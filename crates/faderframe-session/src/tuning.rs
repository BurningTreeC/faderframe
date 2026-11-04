//! Tunings in the session (see `faderframe_project::tuning`): Scala files
//! read into the project's tuning or a track's, the menus' choices, the
//! piano roll's degree labels, and MIDI Tuning Standard SysEx to external
//! MIDI 1.0 devices.
//!
//! Plugins are retuned in the engine (`faderframe_engine::tuning`); what
//! is left for the session is the external devices on MIDI 1.0 ports a
//! MIDI track plays when it is retuned by MTS (automatically when it is
//! not an MPE track): each gets a bulk tuning dump (program 0) and the
//! whole keyboard as real-time single note changes whenever the table it
//! plays changes or its port appears — straight out, past the timed queue.
//! MIDI 2.0 ports get each note's pitch with the note instead (the
//! engine's per-note pitch).

use crate::{Action, InputChoice, NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::tuning::{EQUAL_TEMPERAMENTS, MORE_EQUAL_TEMPERAMENTS, Retune, trim_hz};
use faderframe_project::tuning::{KeyDegree, KeyboardMap, ScalaScale, Tuning, mts};
use faderframe_project::{Command, TrackKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What a tuning file chooser picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuningFile {
    /// A Scala scale (`.scl`).
    Scale,
    /// A Scala keyboard mapping (`.kbm`).
    Mapping,
}

/// Concert pitches menus offer (A4, Hz).
pub const REFERENCE_PITCHES: [f64; 9] = [
    415.0, 430.0, 432.0, 435.0, 438.0, 440.0, 441.0, 442.0, 443.0,
];

/// What MTS told the external devices: per MIDI track its port and the
/// fingerprint of the table sent.
#[derive(Debug, Default)]
pub(crate) struct TuningState {
    told: HashMap<TrackId, (u16, u64)>,
}

/// How the piano roll labels a tuning's keys (scales other than twelve-tone
/// equal temperament).
#[derive(Clone, Debug, PartialEq)]
pub struct PianoTuning {
    pub name: String,
    /// Notes per period.
    pub notes: usize,
    /// Per key its degree (`None`: unmapped, or left in equal temperament
    /// outside the mapping's range).
    pub keys: [Option<KeyDegree>; 128],
}

impl PianoTuning {
    /// The row label: the degree and its cents ("3 · 386¢").
    pub fn label(&self, key: u8) -> Option<String> {
        self.keys[usize::from(key & 0x7F)]
            .map(|d| format!("{} · {:.0}¢", d.degree, d.cents.round()))
    }

    /// Does the key play a degree of the scale?
    pub fn in_scale(&self, key: u8) -> bool {
        self.keys[usize::from(key & 0x7F)].is_some()
    }

    /// Degree 0 (where a period starts).
    pub fn is_tonic(&self, key: u8) -> bool {
        self.keys[usize::from(key & 0x7F)].is_some_and(|d| d.degree == 0)
    }
}

/// A file's text, whatever its encoding (Scala files are often Latin-1).
fn text_of(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Scala".into())
}

/// Read a Scala scale file.
pub fn read_scale(path: &Path) -> Result<ScalaScale> {
    ScalaScale::parse(&text_of(path)?)
        .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))
}

/// Read a Scala keyboard mapping file.
pub fn read_mapping(path: &Path) -> Result<KeyboardMap> {
    KeyboardMap::parse(&text_of(path)?)
        .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))
}

/// A tuning from a scale file and an optional mapping file, named after
/// them ("ptolemy", "ptolemy (white)").
pub fn load_tuning(scale: &Path, mapping: Option<&Path>) -> Result<Tuning> {
    let scl = read_scale(scale)?;
    let kbm = mapping.map(read_mapping).transpose()?;
    let name = match mapping {
        Some(m) => format!("{} ({})", stem(scale), stem(m)),
        None => stem(scale),
    };
    Tuning::scala(name, scl, kbm).map_err(|e| SessionError::Other(e.to_string()))
}

/// A tuning's name without the mapping in brackets.
fn scale_name(name: &str) -> &str {
    match name.rsplit_once(" (") {
        Some((head, tail)) if tail.ends_with(')') => head,
        _ => name,
    }
}

impl Session {
    /// The tuning a choice sets: the project's (`track: None`) or a
    /// track's.
    fn set_tuning(&mut self, track: Option<TrackId>, tuning: Option<Tuning>) -> Result<()> {
        self.edit(Command::SetTuning { track, tuning })
    }

    /// [`Action::LoadTuning`].
    pub(crate) fn load_tuning_files(
        &mut self,
        track: Option<TrackId>,
        scale: &Path,
        mapping: Option<&Path>,
    ) -> Result<()> {
        match load_tuning(scale, mapping) {
            Ok(t) => {
                let name = t.name.clone();
                self.set_tuning(track, Some(t))?;
                self.notify(NoticeLevel::Info, format!("Tuning: {name}"));
                Ok(())
            }
            Err(e) => {
                self.notify(NoticeLevel::Error, e.to_string());
                Err(e)
            }
        }
    }

    /// [`Action::LoadKeyboardMapping`]: the tuning played (the track's,
    /// else the project's; 12-TET when none) with another mapping.
    pub(crate) fn load_keyboard_mapping(
        &mut self,
        track: Option<TrackId>,
        mapping: Option<&Path>,
    ) -> Result<()> {
        let base = match track {
            Some(t) => self.project.tuning_of(t).cloned(),
            None => self.project.tuning.clone(),
        }
        .unwrap_or_else(Tuning::twelve_tet);
        let kbm = match mapping.map(read_mapping).transpose() {
            Ok(k) => k,
            Err(e) => {
                self.notify(NoticeLevel::Error, e.to_string());
                return Err(e);
            }
        };
        let mut tuning = match base.with_mapping(kbm) {
            Ok(t) => t,
            Err(e) => {
                let e = SessionError::Other(e.to_string());
                self.notify(NoticeLevel::Error, e.to_string());
                return Err(e);
            }
        };
        let name = scale_name(&tuning.name).to_string();
        tuning.name = match mapping {
            Some(m) => format!("{name} ({})", stem(m)),
            None => name,
        };
        // 12-TET on the standard mapping is no tuning of the project's own.
        let tuning = (track.is_some() || !tuning.is_twelve_tet()).then_some(tuning);
        self.set_tuning(track, tuning)
    }

    /// [`Action::PromptTuning`].
    pub(crate) fn prompt_tuning(&mut self, track: Option<TrackId>, file: TuningFile) {
        self.ui_requests
            .push(UiRequest::ChooseTuning { track, file });
    }

    /// The tuning menu of the project (`None`) or of a track: the
    /// project's (tracks), twelve-tone equal temperament, the common equal
    /// temperaments, then Scala files. More equal temperaments are in
    /// [`Self::more_tuning_choices`].
    pub fn tuning_choices(&self, track: Option<TrackId>) -> Vec<InputChoice> {
        let own = match track {
            Some(t) => match self.project.track(t) {
                Some(t) if t.takes_tuning() => t.tuning.as_ref(),
                _ => return Vec::new(),
            },
            None => self.project.tuning.as_ref(),
        };
        let mut out = Vec::new();
        if track.is_some() {
            let project = self
                .project
                .tuning
                .as_ref()
                .map_or("12-TET", |t| t.name.as_str());
            out.push(InputChoice {
                label: format!("Project Tuning ({project})"),
                action: Action::Edit(Command::SetTuning {
                    track,
                    tuning: None,
                }),
                checked: own.is_none(),
                group_start: false,
            });
        }
        for (i, steps) in EQUAL_TEMPERAMENTS.into_iter().enumerate() {
            let t = Tuning::equal(steps);
            let checked = match own {
                Some(o) => *o == t,
                // The project without a tuning plays 12-TET.
                None => track.is_none() && steps == 12,
            };
            let label = if steps == 12 {
                "12-TET (Standard)".to_string()
            } else {
                t.name.clone()
            };
            // The project's "no tuning" is 12-TET.
            let tuning = (track.is_some() || steps != 12).then_some(t);
            out.push(InputChoice {
                label,
                action: Action::Edit(Command::SetTuning { track, tuning }),
                checked,
                group_start: i == 0 && track.is_some(),
            });
        }
        let loaded = own.filter(|t| {
            !EQUAL_TEMPERAMENTS
                .iter()
                .chain(&MORE_EQUAL_TEMPERAMENTS)
                .any(|n| **t == Tuning::equal(*n))
        });
        if let Some(t) = loaded {
            out.push(InputChoice {
                label: t.name.clone(),
                action: Action::Edit(Command::SetTuning {
                    track,
                    tuning: Some(t.clone()),
                }),
                checked: true,
                group_start: true,
            });
        }
        out.push(InputChoice {
            label: "Load Scala Scale…".into(),
            action: Action::PromptTuning {
                track,
                file: TuningFile::Scale,
            },
            checked: false,
            group_start: loaded.is_none(),
        });
        out.push(InputChoice {
            label: "Load Keyboard Mapping…".into(),
            action: Action::PromptTuning {
                track,
                file: TuningFile::Mapping,
            },
            checked: false,
            group_start: false,
        });
        if own.is_some_and(|t| t.mapping.is_some()) {
            out.push(InputChoice {
                label: "Standard Keyboard Mapping".into(),
                action: Action::LoadKeyboardMapping {
                    track,
                    mapping: None,
                },
                checked: false,
                group_start: false,
            });
        }
        out
    }

    /// The less common equal temperaments (a submenu).
    pub fn more_tuning_choices(&self, track: Option<TrackId>) -> Vec<InputChoice> {
        let own = match track {
            Some(t) => match self.project.track(t) {
                Some(t) if t.takes_tuning() => t.tuning.as_ref(),
                _ => return Vec::new(),
            },
            None => self.project.tuning.as_ref(),
        };
        MORE_EQUAL_TEMPERAMENTS
            .into_iter()
            .map(|steps| {
                let t = Tuning::equal(steps);
                InputChoice {
                    label: t.name.clone(),
                    checked: own == Some(&t),
                    action: Action::Edit(Command::SetTuning {
                        track,
                        tuning: Some(t),
                    }),
                    group_start: false,
                }
            })
            .collect()
    }

    /// How a track's plugins and MIDI outputs are retuned (instrument and
    /// MIDI tracks).
    pub fn retune_choices(&self, track: TrackId) -> Vec<InputChoice> {
        let Some(t) = self.project.track(track).filter(|t| t.takes_tuning()) else {
            return Vec::new();
        };
        Retune::ALL
            .into_iter()
            .filter(|r| *r != Retune::Mpe || t.mpe.is_some())
            .map(|retune| InputChoice {
                label: retune.label().into(),
                action: Action::Edit(Command::SetTrackRetune { track, retune }),
                checked: t.retune == retune,
                group_start: retune == Retune::Off,
            })
            .collect()
    }

    /// Concert pitches to choose from (the project's own among them).
    pub fn reference_choices(&self) -> Vec<InputChoice> {
        let now = self.project.reference_hz();
        let mut pitches: Vec<f64> = REFERENCE_PITCHES.to_vec();
        if !pitches.contains(&now) {
            pitches.push(now);
            pitches.sort_by(f64::total_cmp);
        }
        pitches
            .into_iter()
            .map(|hz| InputChoice {
                label: format!("A = {} Hz", trim_hz(hz)),
                action: Action::Edit(Command::SetReferencePitch { hz }),
                checked: hz == now,
                group_start: false,
            })
            .collect()
    }

    /// The degrees the piano roll labels `clip`'s rows with (`None`: its
    /// track plays twelve-tone equal temperament — at any reference pitch —
    /// and the rows keep their note names).
    pub fn piano_tuning(&self, clip: ClipId) -> Option<PianoTuning> {
        let track = self.project.clip(clip)?.track;
        let t = self.project.tuning_of(track)?;
        if t.is_twelve_tet() {
            return None;
        }
        Some(PianoTuning {
            name: t.name.clone(),
            notes: t.notes(),
            keys: std::array::from_fn(|k| t.key_degree(k as u8)),
        })
    }

    /// External MIDI 1.0 devices retuned by MTS are told when the table
    /// they play changes (every tick; cheap when nothing changed).
    pub(crate) fn tick_tuning(&mut self) {
        let wants_mts = |t: &faderframe_project::Track| match t.retune {
            Retune::Mts => true,
            Retune::Auto => t.mpe.is_none(),
            _ => false,
        };
        if self.tuning.told.is_empty()
            && !self
                .project
                .tracks
                .iter()
                .any(|t| t.kind == TrackKind::Midi && t.midi_output.is_some() && wants_mts(t))
        {
            return;
        }
        let connected: HashMap<String, u16> = self
            .midi
            .outputs
            .ports()
            .into_iter()
            .filter(|p| p.connected && p.enabled)
            .map(|p| (p.key, p.index))
            .collect();
        let mut told = HashMap::new();
        let mut sends = Vec::new();
        for t in &self.project.tracks {
            let Some(out) = t.midi_output.as_ref().filter(|_| wants_mts(t)) else {
                continue;
            };
            let Some(&port) = connected.get(&out.port) else {
                continue;
            };
            if self.midi.outputs.is_ump(port) {
                continue;
            }
            let table = self.project.tuning_table(t.id);
            let fp = table.fingerprint();
            let before = self.tuning.told.get(&t.id).copied();
            told.insert(t.id, (port, fp));
            if before == Some((port, fp)) || (before.is_none() && table.is_standard()) {
                continue;
            }
            sends.push((port, table, self.project.tuning_name(t.id)));
        }
        self.tuning.told = told;
        for (port, table, name) in sends {
            self.midi
                .outputs
                .send_now(port, &mts::bulk_dump(&table, 0, &name));
            for m in mts::note_changes(&table, 0) {
                self.midi.outputs.send_now(port, &m);
            }
        }
    }

    /// The project's tuning as the transport and menus name it ("19-EDO ·
    /// A = 432 Hz"; `None`: 12-TET at 440 Hz).
    pub fn project_tuning_name(&self) -> Option<String> {
        let a4 = self.project.reference_hz();
        if self.project.tuning.is_none() && a4 == faderframe_midi::tuning::A4_HZ {
            return None;
        }
        let name = self
            .project
            .tuning
            .as_ref()
            .map_or("12-TET", |t| t.name.as_str());
        Some(if a4 == faderframe_midi::tuning::A4_HZ {
            name.to_string()
        } else {
            format!("{name} · A = {} Hz", trim_hz(a4))
        })
    }
}

/// Where a dev action's `scl=<path>[;kbm=<path>]` points.
pub fn parse_tuning_files(arg: &str) -> Option<(PathBuf, Option<PathBuf>)> {
    let mut scale = None;
    let mut mapping = None;
    for part in arg.split(';') {
        match part.split_once('=') {
            Some(("scl", p)) => scale = Some(PathBuf::from(p.trim())),
            Some(("kbm", p)) => mapping = Some(PathBuf::from(p.trim())),
            _ => return None,
        }
    }
    Some((scale?, mapping))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_action_paths_and_names() {
        assert_eq!(
            parse_tuning_files("scl=/a/b.scl;kbm=/a/c.kbm"),
            Some((PathBuf::from("/a/b.scl"), Some(PathBuf::from("/a/c.kbm"))))
        );
        assert_eq!(
            parse_tuning_files("scl=x.scl"),
            Some((PathBuf::from("x.scl"), None))
        );
        assert_eq!(parse_tuning_files("kbm=x.kbm"), None);
        assert_eq!(parse_tuning_files("nonsense"), None);
        assert_eq!(scale_name("ptolemy (white)"), "ptolemy");
        assert_eq!(scale_name("19-EDO"), "19-EDO");
    }
}
