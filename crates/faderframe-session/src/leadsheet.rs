//! Lead sheets from a recording: a clip's melody (heard by the pitch
//! analysis in a thread for audio, read for MIDI), the bars it sits in,
//! the chord track's chords (or, without them, the chords the other
//! tracks' MIDI plays), the key, the tempo and the lyrics — written out by
//! [`faderframe_leadsheet`] and engraved for the Lead Sheet view, exported
//! as MusicXML or PDF.

use crate::to_midi::{Found, melody};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::ClipId;
pub use faderframe_leadsheet::Grid;
use faderframe_leadsheet::engrave::{Layout, Page, engrave};
use faderframe_leadsheet::{Bar, ChordAt, Input, LeadSheet, Line, Note};
use faderframe_project::ClipContent;
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// A lead sheet made: its clip, how it was written and its pages.
#[derive(Clone, Debug)]
pub struct LeadSheetDoc {
    pub clip: ClipId,
    pub grid: Grid,
    pub sheet: LeadSheet,
    pub pages: Vec<Page>,
    /// The part's name (the clip's track).
    pub part: String,
}

/// A melody being heard: its clip, the grid asked for, the thread.
type Job = (ClipId, Grid, JoinHandle<Option<Vec<Found>>>);

/// The lead sheet and the melody being heard for one.
#[derive(Default)]
pub(crate) struct LeadSheets {
    job: Option<Job>,
    /// The last melody heard (clip, notes in quarters), so a new grid does
    /// not listen again.
    heard: Option<(ClipId, Vec<Note>)>,
    pub(crate) doc: Option<LeadSheetDoc>,
}

/// Today as YYYY-MM-DD (UTC).
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Session {
    /// Make a lead sheet of `clip` (an audio clip is listened to first;
    /// the result arrives from the tick).
    pub fn make_lead_sheet(&mut self, clip: ClipId, grid: Grid) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        match &c.content {
            ClipContent::Midi(m) => {
                let notes = m
                    .notes
                    .iter()
                    .filter(|n| !n.muted)
                    .map(|n| Note {
                        start: (c.start + n.start).quarters(),
                        end: (c.start + n.start + n.length).quarters(),
                        key: n.key,
                    })
                    .collect();
                self.lead_sheets.heard = Some((clip, notes));
                self.write_lead_sheet(clip, grid)
            }
            ClipContent::Audio(_) | ClipContent::Takes(_) => {
                if let Some((heard, _)) = &self.lead_sheets.heard
                    && *heard == clip
                {
                    return self.write_lead_sheet(clip, grid);
                }
                let Some(a) = c.as_audio() else {
                    return Err(SessionError::Other("the clip has no audio".into()));
                };
                let source = self
                    .sources
                    .get(&a.source)
                    .cloned()
                    .ok_or_else(|| SessionError::Other("the clip's audio is missing".into()))?;
                let (from, span) = (a.source_offset, a.source_span());
                let project_rate = self.project.sample_rate as f64;
                let job = std::thread::Builder::new()
                    .name("faderframe-lead-sheet".into())
                    .spawn(move || {
                        let (mono, rate) = crate::pitch::mono_of(&source)?;
                        let k = rate / project_rate;
                        let a = ((from as f64 * k) as usize).min(mono.len());
                        let b = (((from + span) as f64 * k) as usize).clamp(a, mono.len());
                        Some(melody(&mono[a..b], rate))
                    })
                    .map_err(|e| SessionError::Other(e.to_string()))?;
                self.lead_sheets.job = Some((clip, grid, job));
                self.notify(
                    NoticeLevel::Info,
                    format!("Listening to the melody of ‘{}’…", c.name),
                );
                self.revision += 1;
                Ok(())
            }
        }
    }

    /// Whether a melody is being heard for a lead sheet.
    pub fn making_lead_sheet(&self) -> bool {
        self.lead_sheets.job.is_some()
    }

    /// The lead sheet made last.
    pub fn lead_sheet(&self) -> Option<&LeadSheetDoc> {
        self.lead_sheets.doc.as_ref()
    }

    /// The melody heard: write the sheet (from the tick).
    pub(crate) fn poll_lead_sheets(&mut self) {
        let finished = self
            .lead_sheets
            .job
            .as_ref()
            .is_some_and(|(_, _, j)| j.is_finished());
        if !finished {
            return;
        }
        let Some((clip, grid, job)) = self.lead_sheets.job.take() else {
            return;
        };
        self.revision += 1;
        let Some(found) = job.join().ok().flatten() else {
            self.notify(NoticeLevel::Warning, "the clip's audio could not be read");
            return;
        };
        let Some(c) = self.project.clip(clip).cloned() else {
            return;
        };
        let Some(a) = c.as_audio().cloned() else {
            return;
        };
        let p = &self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let at = |s: f64| {
            let f = a.source_offset + (s * rate).round() as i64;
            let out = match &a.warp {
                Some(w) => w.output_of(a.source_offset, a.length, f),
                None => f - a.source_offset,
            };
            p.timeline
                .to_musical(base + out.clamp(0, a.length), rate)
                .quarters()
        };
        let notes = found
            .iter()
            .map(|n| Note {
                start: at(n.start),
                end: at(n.end),
                key: n.key,
            })
            .collect();
        self.lead_sheets.heard = Some((clip, notes));
        if let Err(e) = self.write_lead_sheet(clip, grid) {
            self.notify(NoticeLevel::Warning, e.to_string());
        }
    }

    /// Wait for a melody being heard (scripts and tests).
    pub fn wait_for_lead_sheet(&mut self) {
        while let Some((_, _, j)) = &self.lead_sheets.job {
            if j.is_finished() {
                self.poll_lead_sheets();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Write the sheet from the melody heard and the project around it.
    fn write_lead_sheet(&mut self, clip: ClipId, grid: Grid) -> Result<()> {
        let Some((_, notes)) = self.lead_sheets.heard.clone() else {
            return Err(SessionError::Other("no melody heard yet".into()));
        };
        let p = &self.project;
        let c = p
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        if notes.is_empty() {
            return Err(SessionError::Other(format!(
                "no melody heard in ‘{}’",
                c.name
            )));
        }
        let meter = &p.timeline.meter;
        let start = c.start;
        let end = c.end(&p.timeline, p.sample_rate);
        // The bars from the clip's first to its last.
        let (first, last) = (
            meter.bar_at(start),
            meter.bar_at(end - MusicalTime::from_ticks(1).max(MusicalTime::ZERO)),
        );
        let bars: Vec<Bar> = (first..=last.max(first))
            .map(|b| {
                let sig = meter.signature_of_bar(b);
                Bar {
                    start: meter.bar_start(b).quarters(),
                    time: (sig.numerator, sig.denominator),
                }
            })
            .collect();
        let from = meter.bar_start(first);
        let to = meter.bar_start(last.max(first) + 1);
        // Chords: the chord track's, else what the other tracks' MIDI plays.
        let mut chords: Vec<ChordAt> = p
            .chords
            .iter()
            .filter(|ch| ch.end > from && ch.start < to)
            .map(|ch| ChordAt {
                start: ch.start.max(from).quarters(),
                chord: ch.chord,
            })
            .collect();
        if chords.is_empty() {
            let mut sounding = Vec::new();
            for t in &p.tracks {
                if t.id == c.track {
                    continue;
                }
                for other in p.clips_of(t.id) {
                    if let ClipContent::Midi(m) = &other.content {
                        for n in m.notes.iter().filter(|n| !n.muted) {
                            sounding.push(faderframe_project::harmony::Sounding {
                                start: other.start + n.start,
                                end: other.start + n.start + n.length,
                                key: n.key,
                            });
                        }
                    }
                }
            }
            let half = meter.signature_at(from).bar_length() / 2;
            chords = faderframe_project::harmony::detect_chords(&sounding, from, to, half)
                .into_iter()
                .map(|ch| ChordAt {
                    start: ch.start.quarters(),
                    chord: ch.chord,
                })
                .collect();
        }
        let key = p
            .keys
            .iter()
            .rev()
            .find(|k| k.at <= start)
            .or(p.keys.first())
            .map(|k| k.key);
        let lines = p
            .lyrics
            .iter()
            .filter(|l| l.end > from && l.start < to)
            .map(|l| Line {
                start: l.start.quarters(),
                end: l.end.quarters(),
                text: l.text.clone(),
            })
            .collect();
        let part = p
            .track(c.track)
            .map_or_else(String::new, |t| t.name.clone());
        let title = if p.name.trim().is_empty() {
            c.name.clone()
        } else {
            p.name.clone()
        };
        let input = Input {
            title,
            composer: String::new(),
            key,
            bars,
            tempo: Some(p.timeline.tempo.bpm_at(start)),
            notes,
            chords,
            lines,
            grid,
        };
        let sheet = faderframe_leadsheet::build(&input);
        let pages = engrave(&sheet, &Layout::default());
        let count = sheet.measures.len();
        self.lead_sheets.doc = Some(LeadSheetDoc {
            clip,
            grid,
            sheet,
            pages,
            part,
        });
        self.workspace_action(crate::WorkspaceAction::ShowView(
            faderframe_workspace::ViewId::lead_sheet(),
        ))?;
        self.notify(
            NoticeLevel::Info,
            format!("Lead sheet of ‘{}’: {count} bars", c.name),
        );
        self.revision += 1;
        Ok(())
    }

    /// Write the lead sheet to `path`: MusicXML (`.musicxml`, `.xml`) or
    /// PDF (`.pdf`).
    pub fn export_lead_sheet(&mut self, path: &Path) -> Result<PathBuf> {
        let doc = self
            .lead_sheets
            .doc
            .as_ref()
            .ok_or_else(|| SessionError::Other("make a lead sheet first".into()))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let bytes = match ext.as_deref() {
            Some("pdf") => faderframe_leadsheet::pdf::write(&doc.pages, &doc.sheet.title),
            Some("musicxml" | "xml") => {
                faderframe_leadsheet::musicxml::write(&doc.sheet, &doc.part, &today()).into_bytes()
            }
            _ => {
                return Err(SessionError::Other(
                    "a lead sheet is written as .pdf or .musicxml".into(),
                ));
            }
        };
        std::fs::write(path, bytes).map_err(|e| SessionError::Other(e.to_string()))?;
        self.notify(
            NoticeLevel::Info,
            format!("Lead sheet written to {}", path.display()),
        );
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_are_civil() {
        assert_eq!(super::today().len(), 10);
    }
}
