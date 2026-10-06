//! Tempo and key from a clip: the project's tempo set to a clip's, its key
//! to the key a clip is in, or a clip warped to the project's tempo.
//!
//! Audio clips are analysed in a thread (their part of the source's mono
//! mix: `faderframe_analysis::tempo` and `::chroma`, the key by
//! `faderframe_midi::theory::detect_key`) and the edit applied once it is
//! done, as one undo step, with a notice saying what was found; MIDI clips
//! give their key from their notes at once.

use crate::pitch::mono_of;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_analysis::tempo::Tempo;
use faderframe_core::ClipId;
use faderframe_project::harmony::{self, Key, KeyChange, Sounding};
use faderframe_project::{ClipContent, Command, Warp, WarpAlgorithm};
use faderframe_timeline::MusicalTime;
use std::thread::JoinHandle;

/// What to do with what a clip is found to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromClip {
    /// The project's tempo becomes the clip's.
    SetTempo,
    /// The clip is warped to play at the project's tempo.
    WarpToTempo,
    /// The project's key (from the clip's start on) becomes the clip's.
    SetKey,
}

/// What an audio clip was found to be.
struct Facts {
    tempo: Option<Tempo>,
    key: Option<Key>,
}

/// Analyses running.
#[derive(Default)]
pub(crate) struct ClipAnalyses {
    jobs: Vec<(ClipId, FromClip, JoinHandle<Option<Facts>>)>,
}

/// Least confidence a tempo is used with.
const SURE: f64 = 0.1;

fn key_name(k: Key) -> String {
    k.name()
}

impl Session {
    /// Find a clip's tempo or key and use it (see [`FromClip`]).
    pub fn from_clip(&mut self, clip: ClipId, what: FromClip) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        match &c.content {
            ClipContent::Midi(m) => {
                if what != FromClip::SetKey {
                    return Err(SessionError::Other(
                        "the tempo is found in audio clips".into(),
                    ));
                }
                let notes: Vec<Sounding> = m
                    .notes
                    .iter()
                    .map(|n| Sounding {
                        key: n.key,
                        start: n.start,
                        end: n.start + n.length,
                    })
                    .collect();
                let key = harmony::detect_key(&notes);
                self.use_facts(clip, what, Facts { tempo: None, key })
            }
            ClipContent::Audio(a) => {
                let Some(source) = self.sources.get(&a.source).cloned() else {
                    return Err(SessionError::Other("the clip's audio is missing".into()));
                };
                let (from, span) = (a.source_offset, a.source_span());
                let project_rate = self.project.sample_rate as f64;
                let job = std::thread::Builder::new()
                    .name("faderframe-tempo-key".into())
                    .spawn(move || {
                        let (mono, rate) = mono_of(&source)?;
                        // The clip's part (project frames to the source's).
                        let k = rate / project_rate;
                        let a = ((from as f64 * k) as usize).min(mono.len());
                        let b = (((from + span) as f64 * k) as usize).clamp(a, mono.len());
                        let x = &mono[a..b];
                        let profile = faderframe_analysis::chroma::profile(x, rate);
                        Some(Facts {
                            tempo: faderframe_analysis::tempo::detect(x, rate),
                            key: faderframe_midi::theory::detect_key(&profile),
                        })
                    })
                    .map_err(|e| SessionError::Other(e.to_string()))?;
                self.clip_analyses.jobs.push((clip, what, job));
                self.revision += 1;
                Ok(())
            }
            ClipContent::Takes(_) => Err(SessionError::Other(
                "flatten the comp to find its tempo or key".into(),
            )),
        }
    }

    /// Clip analyses still running.
    pub fn analysing_clips(&self) -> bool {
        !self.clip_analyses.jobs.is_empty()
    }

    /// Apply finished analyses (from the session tick).
    pub(crate) fn poll_clip_analyses(&mut self) {
        let mut i = 0;
        while i < self.clip_analyses.jobs.len() {
            if !self.clip_analyses.jobs[i].2.is_finished() {
                i += 1;
                continue;
            }
            let (clip, what, job) = self.clip_analyses.jobs.swap_remove(i);
            let result = match job.join().ok().flatten() {
                Some(facts) => self.use_facts(clip, what, facts),
                None => Err(SessionError::Other(
                    "the clip's audio could not be read".into(),
                )),
            };
            if let Err(e) = result {
                self.notify(NoticeLevel::Warning, e.to_string());
            }
            self.revision += 1;
        }
    }

    fn use_facts(&mut self, clip: ClipId, what: FromClip, facts: Facts) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(());
        };
        let tempo = facts.tempo.filter(|t| t.confidence >= SURE);
        match what {
            FromClip::SetTempo => {
                let Some(t) = tempo else {
                    return Err(SessionError::Other(format!(
                        "‘{}’ has no clear beat to take a tempo from",
                        c.name
                    )));
                };
                self.edit(Command::SetTempo { bpm: t.bpm })?;
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "‘{}’ plays at {:.2} BPM: the project's tempo now",
                        c.name, t.bpm
                    ),
                );
            }
            FromClip::WarpToTempo => {
                let Some(t) = tempo else {
                    return Err(SessionError::Other(format!(
                        "‘{}’ has no clear beat to warp by",
                        c.name
                    )));
                };
                let ClipContent::Audio(mut a) = c.content.clone() else {
                    return Ok(());
                };
                let project = self.project.timeline.tempo.bpm_at(c.start);
                let span = a.source_span();
                a.length = ((span as f64 * t.bpm / project).round() as i64).max(1);
                a.warp = Some(Warp {
                    source_length: span,
                    markers: Vec::new(),
                    algorithm: a
                        .warp
                        .as_ref()
                        .map_or(WarpAlgorithm::Polyphonic, |w| w.algorithm),
                });
                self.set_audio("Warp to Tempo", &c, c.start, a)?;
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "‘{}’ ({:.2} BPM) now plays at the project's {:.2} BPM",
                        c.name, t.bpm, project
                    ),
                );
            }
            FromClip::SetKey => {
                let Some(key) = facts.key else {
                    return Err(SessionError::Other(format!(
                        "‘{}’ has no key to hear",
                        c.name
                    )));
                };
                let keys = if self.project.keys.is_empty() {
                    vec![KeyChange {
                        at: MusicalTime::ZERO,
                        key,
                    }]
                } else {
                    harmony::set_key(&self.project.keys, c.start, Some(key))
                };
                self.edit(Command::SetKeys { keys })?;
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "‘{}’ is in {}: the project's key now",
                        c.name,
                        key_name(key)
                    ),
                );
            }
        }
        Ok(())
    }
}
