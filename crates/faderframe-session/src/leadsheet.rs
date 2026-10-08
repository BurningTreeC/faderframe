//! Lead sheets from a recording: the melody of a clip or of a whole track
//! (audio heard by the pitch analysis in a thread, MIDI read), the bars it
//! sits in, the chords — the chord track's; without one, what the other
//! tracks' MIDI plays; without that, what the other audio tracks play,
//! heard by basic-pitch —, the key, the tempo and the lyrics, written out by
//! [`faderframe_leadsheet`] and engraved for the Lead Sheet view, exported
//! as MusicXML or PDF.

use crate::to_midi::{Found, harmony, melody};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::Source;
pub use faderframe_leadsheet::Grid;
use faderframe_leadsheet::engrave::{Layout, Page, engrave};
use faderframe_leadsheet::{Bar, ChordAt, Input, LeadSheet, Line, Note};
use faderframe_project::{Clip, ClipContent, Project};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// What a lead sheet is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeadSheetOf {
    Clip(ClipId),
    /// All of a track's clips, the whole song.
    Track(TrackId),
}

/// A lead sheet made: what of, how it was written and its pages.
#[derive(Clone, Debug)]
pub struct LeadSheetDoc {
    pub of: LeadSheetOf,
    pub grid: Grid,
    pub sheet: LeadSheet,
    pub pages: Vec<Page>,
    /// The part's name (the track).
    pub part: String,
}

/// Audio to listen to: a clip's part of its source.
struct Part {
    clip: ClipId,
    source: Source,
    from: i64,
    span: i64,
}

/// What a listening thread hears: the melody's notes and the chords'
/// notes, each by clip (seconds into the clip's part).
type Heard = (Vec<(ClipId, Vec<Found>)>, Vec<(ClipId, Vec<Found>)>);

/// How audio is heard as notes (the melody, or chords).
type Listener = dyn Fn(&[f32], f64) -> Option<Vec<Found>>;

/// A listening job: what of, the grid, the MIDI melody already read, the
/// thread.
type Job = (LeadSheetOf, Grid, Vec<Note>, JoinHandle<Heard>);

/// The melody and the chords heard for a lead sheet (kept, so another
/// grid does not listen again).
#[derive(Clone)]
struct Kept {
    of: LeadSheetOf,
    melody: Vec<Note>,
    chords: Vec<ChordAt>,
}

#[derive(Default)]
pub(crate) struct LeadSheets {
    job: Option<Job>,
    kept: Option<Kept>,
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

/// A clip's MIDI notes in quarters on the timeline.
fn midi_notes(c: &Clip) -> Vec<Note> {
    match &c.content {
        ClipContent::Midi(m) => m
            .notes
            .iter()
            .filter(|n| !n.muted)
            .map(|n| Note {
                start: (c.start + n.start).quarters(),
                end: (c.start + n.start + n.length).quarters(),
                key: n.key,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Seconds into an audio clip's part → quarters on the timeline.
fn clip_quarters(p: &Project, c: &Clip, secs: f64) -> f64 {
    let Some(a) = c.as_audio() else {
        return c.start.quarters();
    };
    let rate = p.sample_rate as f64;
    let base = p.timeline.to_samples(c.start, rate);
    let f = a.source_offset + (secs * rate).round() as i64;
    let out = match &a.warp {
        Some(w) => w.output_of(a.source_offset, a.length, f),
        None => f - a.source_offset,
    };
    p.timeline
        .to_musical(base + out.clamp(0, a.length), rate)
        .quarters()
}

/// Tracks whose name says they are drums (no chords in them).
fn drum_track(name: &str) -> bool {
    let n = name.to_lowercase();
    ["drum", "kick", "snare", "hat", "perc", "cymbal", "tom"]
        .iter()
        .any(|w| n.contains(w))
}

impl Session {
    /// The clips a lead sheet covers (in order) and its part's track.
    fn lead_sheet_clips(&self, of: LeadSheetOf) -> Result<(Vec<Clip>, TrackId)> {
        let p = &self.project;
        match of {
            LeadSheetOf::Clip(id) => {
                let c = p
                    .clip(id)
                    .cloned()
                    .ok_or_else(|| SessionError::Other(format!("no clip {id}")))?;
                let t = c.track;
                Ok((vec![c], t))
            }
            LeadSheetOf::Track(t) => {
                let mut clips: Vec<Clip> = p
                    .clips_of(t)
                    .into_iter()
                    .filter(|c| !c.muted)
                    .cloned()
                    .collect();
                clips.sort_by_key(|c| c.start);
                if clips.is_empty() {
                    return Err(SessionError::Other("the track has no clips".into()));
                }
                Ok((clips, t))
            }
        }
    }

    /// The span a lead sheet covers.
    fn lead_sheet_span(&self, clips: &[Clip]) -> (MusicalTime, MusicalTime) {
        let p = &self.project;
        let start = clips
            .iter()
            .map(|c| c.start)
            .min()
            .unwrap_or(MusicalTime::ZERO);
        let end = clips
            .iter()
            .map(|c| c.end(&p.timeline, p.sample_rate))
            .max()
            .unwrap_or(start);
        (start, end)
    }

    /// The chords the chord track or the other tracks' MIDI give over
    /// `from..to` (empty: none).
    fn known_chords(&self, part: TrackId, from: MusicalTime, to: MusicalTime) -> Vec<ChordAt> {
        let p = &self.project;
        let chords: Vec<ChordAt> = p
            .chords
            .iter()
            .filter(|ch| ch.end > from && ch.start < to)
            .map(|ch| ChordAt {
                start: ch.start.max(from).quarters(),
                chord: ch.chord,
            })
            .collect();
        if !chords.is_empty() {
            return chords;
        }
        let mut sounding = Vec::new();
        for t in &p.tracks {
            if t.id == part || drum_track(&t.name) {
                continue;
            }
            for other in p.clips_of(t.id) {
                for n in midi_notes(other) {
                    sounding.push(faderframe_project::harmony::Sounding {
                        start: MusicalTime::from_quarters(n.start),
                        end: MusicalTime::from_quarters(n.end),
                        key: n.key,
                    });
                }
            }
        }
        self.chords_of(&sounding, from, to)
    }

    fn chords_of(
        &self,
        sounding: &[faderframe_project::harmony::Sounding],
        from: MusicalTime,
        to: MusicalTime,
    ) -> Vec<ChordAt> {
        let meter = &self.project.timeline.meter;
        let half = meter.signature_at(from).bar_length() / 2;
        faderframe_project::harmony::detect_chords(sounding, from, to, half)
            .into_iter()
            .map(|ch| ChordAt {
                start: ch.start.quarters(),
                chord: ch.chord,
            })
            .collect()
    }

    /// Make a lead sheet (audio is listened to first: the melody's clips,
    /// and the other audio tracks when nothing else gives the chords; the
    /// result arrives from the tick).
    pub fn make_lead_sheet(&mut self, of: LeadSheetOf, grid: Grid) -> Result<()> {
        if let Some(k) = &self.lead_sheets.kept
            && k.of == of
        {
            return self.write_lead_sheet(of, grid);
        }
        let (clips, part) = self.lead_sheet_clips(of)?;
        let (start, end) = self.lead_sheet_span(&clips);
        let source = |c: &Clip| -> Option<Part> {
            let a = c.as_audio()?;
            Some(Part {
                clip: c.id,
                source: self.sources.get(&a.source)?.clone(),
                from: a.source_offset,
                span: a.source_span(),
            })
        };
        let melody_parts: Vec<Part> = clips.iter().filter_map(source).collect();
        let midi: Vec<Note> = clips.iter().flat_map(midi_notes).collect();
        // Chords to hear: the other audio tracks' clips over the span, when
        // neither the chord track nor MIDI gives any.
        let chord_parts: Vec<Part> = if self.known_chords(part, start, end).is_empty() {
            let p = &self.project;
            p.tracks
                .iter()
                .filter(|t| t.id != part && !drum_track(&t.name))
                .flat_map(|t| p.clips_of(t.id))
                .filter(|c| !c.muted && c.start < end && c.end(&p.timeline, p.sample_rate) > start)
                .filter_map(source)
                .collect()
        } else {
            Vec::new()
        };
        if melody_parts.is_empty() && chord_parts.is_empty() {
            self.lead_sheets.kept = Some(Kept {
                of,
                melody: midi,
                chords: Vec::new(),
            });
            return self.write_lead_sheet(of, grid);
        }
        let project_rate = self.project.sample_rate as f64;
        let listen_chords = !chord_parts.is_empty();
        let job = std::thread::Builder::new()
            .name("faderframe-lead-sheet".into())
            .spawn(move || {
                let hear = |part: &Part, how: &Listener| {
                    let (mono, rate) = crate::pitch::mono_of(&part.source)?;
                    let k = rate / project_rate;
                    let a = ((part.from as f64 * k) as usize).min(mono.len());
                    let b = (((part.from + part.span) as f64 * k) as usize).clamp(a, mono.len());
                    how(&mono[a..b], rate)
                };
                let melody_found = melody_parts
                    .iter()
                    .filter_map(|p| Some((p.clip, hear(p, &|x, r| Some(melody(x, r)))?)))
                    .collect();
                let chords_found = chord_parts
                    .iter()
                    .filter_map(|p| Some((p.clip, hear(p, &harmony)?)))
                    .collect();
                (melody_found, chords_found)
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.lead_sheets.job = Some((of, grid, midi, job));
        let name = match of {
            LeadSheetOf::Clip(_) => clips[0].name.clone(),
            LeadSheetOf::Track(t) => self
                .project
                .track(t)
                .map_or_else(String::new, |t| t.name.clone()),
        };
        self.notify(
            NoticeLevel::Info,
            if listen_chords {
                format!("Listening to the melody of ‘{name}’ and to the chords around it…")
            } else {
                format!("Listening to the melody of ‘{name}’…")
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Whether audio is being listened to for a lead sheet.
    pub fn making_lead_sheet(&self) -> bool {
        self.lead_sheets.job.is_some()
    }

    /// The lead sheet made last.
    pub fn lead_sheet(&self) -> Option<&LeadSheetDoc> {
        self.lead_sheets.doc.as_ref()
    }

    /// The audio heard: write the sheet (from the tick).
    pub(crate) fn poll_lead_sheets(&mut self) {
        let finished = self
            .lead_sheets
            .job
            .as_ref()
            .is_some_and(|(_, _, _, j)| j.is_finished());
        if !finished {
            return;
        }
        let Some((of, grid, midi, job)) = self.lead_sheets.job.take() else {
            return;
        };
        self.revision += 1;
        let Ok((melody_found, chords_found)) = job.join() else {
            self.notify(NoticeLevel::Warning, "listening to the audio failed");
            return;
        };
        let p = &self.project;
        let mut melody: Vec<Note> = midi;
        for (clip, found) in &melody_found {
            let Some(c) = p.clip(*clip) else { continue };
            melody.extend(found.iter().map(|n| Note {
                start: clip_quarters(p, c, n.start),
                end: clip_quarters(p, c, n.end),
                key: n.key,
            }));
        }
        let mut sounding = Vec::new();
        for (clip, found) in &chords_found {
            let Some(c) = p.clip(*clip) else { continue };
            sounding.extend(found.iter().map(|n| faderframe_project::harmony::Sounding {
                start: MusicalTime::from_quarters(clip_quarters(p, c, n.start)),
                end: MusicalTime::from_quarters(clip_quarters(p, c, n.end)),
                key: n.key,
            }));
        }
        let chords = match self.lead_sheet_clips(of) {
            Ok((clips, _)) if !sounding.is_empty() => {
                let (start, end) = self.lead_sheet_span(&clips);
                let meter = &self.project.timeline.meter;
                let from = meter.bar_start(meter.bar_at(start));
                self.chords_of(&sounding, from, end)
            }
            _ => Vec::new(),
        };
        self.lead_sheets.kept = Some(Kept { of, melody, chords });
        if let Err(e) = self.write_lead_sheet(of, grid) {
            self.notify(NoticeLevel::Warning, e.to_string());
        }
    }

    /// Wait for the audio being heard (scripts and tests).
    pub fn wait_for_lead_sheet(&mut self) {
        while let Some((_, _, _, j)) = &self.lead_sheets.job {
            if j.is_finished() {
                self.poll_lead_sheets();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Write the sheet from what was heard and the project around it.
    fn write_lead_sheet(&mut self, of: LeadSheetOf, grid: Grid) -> Result<()> {
        let Some(kept) = self.lead_sheets.kept.clone().filter(|k| k.of == of) else {
            return Err(SessionError::Other("nothing heard yet".into()));
        };
        let (clips, part_track) = self.lead_sheet_clips(of)?;
        let (start, end) = self.lead_sheet_span(&clips);
        let p = &self.project;
        let what = match of {
            LeadSheetOf::Clip(_) => clips[0].name.clone(),
            LeadSheetOf::Track(t) => p.track(t).map_or_else(String::new, |t| t.name.clone()),
        };
        if kept.melody.is_empty() {
            return Err(SessionError::Other(format!("no melody heard in ‘{what}’")));
        }
        let meter = &p.timeline.meter;
        // The bars from the first clip's to the last's.
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
        let mut chords = self.known_chords(part_track, from, to);
        if chords.is_empty() {
            chords = kept.chords.clone();
        }
        let p = &self.project;
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
            .track(part_track)
            .map_or_else(String::new, |t| t.name.clone());
        let title = if p.name.trim().is_empty() {
            what.clone()
        } else {
            p.name.clone()
        };
        let input = Input {
            title,
            composer: String::new(),
            key,
            bars,
            tempo: Some(p.timeline.tempo.bpm_at(start)),
            notes: kept.melody,
            chords,
            lines,
            grid,
        };
        let sheet = faderframe_leadsheet::build(&input);
        let pages = engrave(&sheet, &Layout::default());
        let count = sheet.measures.len();
        self.lead_sheets.doc = Some(LeadSheetDoc {
            of,
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
            format!("Lead sheet of ‘{what}’: {count} bars"),
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
