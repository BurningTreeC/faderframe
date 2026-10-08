//! Song structure from a jam: the sections of a recording found and laid
//! on the arranger's section lane.
//!
//! The chosen audio clips (one recording, or the tracks of a multitrack
//! jam) are mixed to mono in project time and analysed in a thread
//! (`faderframe_analysis::structure`: bars by the tempo, parts by where
//! harmony, timbre and loudness change, parts alike given one letter and
//! named Intro, Verse, Chorus, Bridge, Solo, Outro). The parts become
//! sections — the sections they overlap go — in one "Song Structure"
//! step, coloured by letter; on the project's bar lines when the project
//! plays at the recording's tempo, else where they are heard. Pauses in a
//! jam stay without a section.

use crate::pitch::mono_of;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_analysis::structure::Structure;
use faderframe_core::{ClipId, SectionId};
use faderframe_project::{ClipContent, Command, Section, TrackColor};
use faderframe_timeline::MusicalTime;
use std::thread::JoinHandle;

/// How close the project's tempo must be to the recording's (relative) for
/// the parts to go on its bar lines.
const SAME_TEMPO: f64 = 0.015;

struct Job {
    /// What was analysed, for the notice.
    what: String,
    /// The project frame the mix starts at.
    from: i64,
    handle: JoinHandle<Option<Structure>>,
}

#[derive(Default)]
pub(crate) struct StructureJobs {
    jobs: Vec<Job>,
}

impl Session {
    /// Find the song structure of audio clips (see the module docs).
    pub(crate) fn song_structure(&mut self, clips: &[ClipId]) -> Result<()> {
        let rate = f64::from(self.project.sample_rate.max(1));
        let tl = &self.project.timeline;
        // Each clip: its source, its part of it, where it plays.
        let mut pieces = Vec::new();
        let mut names = Vec::new();
        for &id in clips {
            let Some(c) = self.project.clip(id) else {
                continue;
            };
            let ClipContent::Audio(a) = &c.content else {
                continue;
            };
            let Some(source) = self.sources.get(&a.source).cloned() else {
                continue;
            };
            let at = tl.to_samples(c.start, rate);
            pieces.push((source, a.source_offset, a.length, at));
            names.push(c.name.clone());
        }
        if pieces.is_empty() {
            return Err(SessionError::Other(
                "the song structure is found in audio clips".into(),
            ));
        }
        let from = pieces.iter().map(|p| p.3).min().unwrap_or(0);
        let to = pieces.iter().map(|p| p.3 + p.2).max().unwrap_or(from);
        let first = self.project.timeline.to_musical(from, rate);
        let sig = self.project.timeline.meter.signature_at(first);
        let quarters = sig.bar_length().quarters();
        let what = if names.len() == 1 {
            format!("‘{}’", names[0])
        } else {
            format!("{} recordings", names.len())
        };
        let project_rate = rate;
        let handle = std::thread::Builder::new()
            .name("faderframe-structure".into())
            .spawn(move || {
                let len = (to - from).max(0) as usize;
                let mut mix = vec![0.0f32; len];
                for (source, offset, length, at) in pieces {
                    let (mono, source_rate) = mono_of(&source)?;
                    // Project frames to the source's.
                    let k = source_rate / project_rate;
                    let start = (at - from).max(0) as usize;
                    for i in 0..(length.max(0) as usize).min(len.saturating_sub(start)) {
                        let s = ((offset + i as i64) as f64 * k) as usize;
                        if let Some(v) = mono.get(s) {
                            mix[start + i] += *v;
                        }
                    }
                }
                Some(faderframe_analysis::structure::analyse(
                    &mix,
                    project_rate,
                    quarters,
                ))
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.structure_jobs.jobs.push(Job { what, from, handle });
        self.notify(NoticeLevel::Info, "Finding the song structure…".to_string());
        self.revision += 1;
        Ok(())
    }

    /// Song structures being found.
    pub fn finding_structure(&self) -> bool {
        !self.structure_jobs.jobs.is_empty()
    }

    /// Wait for the song structures being found (tests, scripts).
    pub fn wait_for_structure(&mut self) {
        let start = std::time::Instant::now();
        while self.finding_structure() && start.elapsed().as_secs() < 300 {
            self.poll_structure();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Place finished structures (from the session tick).
    pub(crate) fn poll_structure(&mut self) {
        let mut i = 0;
        while i < self.structure_jobs.jobs.len() {
            if !self.structure_jobs.jobs[i].handle.is_finished() {
                i += 1;
                continue;
            }
            let job = self.structure_jobs.jobs.remove(i);
            match job.handle.join().ok().flatten() {
                Some(s) => {
                    if let Err(e) = self.place_structure(&job.what, job.from, &s) {
                        self.notify(NoticeLevel::Error, format!("song structure: {e}"));
                    }
                }
                None => self.notify(
                    NoticeLevel::Error,
                    format!("song structure: {} could not be read", job.what),
                ),
            }
            self.revision += 1;
        }
    }

    fn place_structure(&mut self, what: &str, from: i64, s: &Structure) -> Result<()> {
        if s.parts.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                format!("No parts found in {what} (too short, or silent)"),
            );
            return Ok(());
        }
        let rate = f64::from(self.project.sample_rate.max(1));
        let tl = &self.project.timeline;
        let at = |seconds: f64| tl.to_musical(from + (seconds * rate).round() as i64, rate);
        // On the project's bars when it plays at the recording's tempo.
        let start = at(s.parts[0].start);
        let same_tempo = s
            .tempo
            .is_some_and(|t| (tl.tempo.bpm_at(start) - t.bpm).abs() / t.bpm < SAME_TEMPO);
        let snap = |t: MusicalTime| -> MusicalTime {
            if !same_tempo {
                return t;
            }
            let bar = tl.meter.bar_at(t);
            let (a, b) = (tl.meter.bar_start(bar), tl.meter.bar_start(bar + 1));
            let near = if (t - a).ticks().abs() <= (b - t).ticks().abs() {
                a
            } else {
                b
            };
            // Only small moves (a quarter of a bar).
            if (near - t).ticks().abs() * 4 <= (b - a).ticks().abs() {
                near
            } else {
                t
            }
        };
        let spans: Vec<(MusicalTime, MusicalTime)> = s
            .parts
            .iter()
            .map(|p| (snap(at(p.start)), snap(at(p.end))))
            .collect();
        let (first, last) = (
            spans.first().map_or(MusicalTime::ZERO, |p| p.0),
            spans.last().map_or(MusicalTime::ZERO, |p| p.1),
        );
        let mut commands: Vec<Command> = self
            .project
            .sections
            .iter()
            .filter(|x| x.start < last && x.end > first)
            .map(|x| Command::RemoveSection { section: x.id })
            .collect();
        for (p, (a, b)) in s.parts.iter().zip(&spans) {
            if b <= a {
                continue;
            }
            let id: SectionId = self.project.ids.allocate();
            commands.push(Command::AddSection {
                section: Section {
                    id,
                    name: p.name.clone(),
                    start: *a,
                    end: *b,
                    color: TrackColor::palette(p.label * 3 + 1),
                },
            });
        }
        self.batch("Song Structure", commands)?;
        let names: Vec<&str> = s.parts.iter().map(|p| p.name.as_str()).collect();
        let tempo = s
            .tempo
            .map(|t| format!(" at {:.1} BPM", t.bpm))
            .unwrap_or_default();
        self.notify(
            NoticeLevel::Info,
            format!(
                "Song structure of {what}{tempo}: {}{}",
                names.join(", "),
                if same_tempo {
                    ""
                } else {
                    " (the project plays at another tempo: the sections are where the parts are heard)"
                }
            ),
        );
        Ok(())
    }
}
