//! Articulation (expression) maps and drum maps in the session (see
//! [`faderframe_project::articulation`] and [`faderframe_project::drum_map`]).
//!
//! [`MapOp`] makes, edits, removes and assigns maps (one undo step each:
//! removing a map takes it from the tracks that name it in the same step),
//! reads them from files — FaderFrame's own JSON maps, Cubase expression
//! maps ([`cubase`]) — and writes user files into the user's maps folders
//! ([`user_dir`]). Notes take an articulation (an attribute; a direction
//! chosen for notes goes into the clip's lane at the first of them), and
//! the lane's directions are set and removed here. Drum maps come from the
//! General MIDI key map, a track's Drum Sampler pads, or files. The map
//! editor's own state — the map it shows, a drum sound learning its pitch
//! from MIDI input — is in [`editor`].

pub mod cubase;
pub mod editor;

pub use editor::{MapKind, MapRef};

use crate::{InputChoice, NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ArticulationMapId, ClipId, DrumMapId, NoteId, TrackId};
use faderframe_project::articulation::{
    ArticulationEvent, ArticulationId, ArticulationKind, ArticulationMap,
};
use faderframe_project::drum_map::DrumMap;
use faderframe_project::{Command, TrackKind};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where a new drum map comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrumMapSource {
    /// The General MIDI key map.
    Gm,
    /// The track's Drum Sampler (or the one its MIDI plays): its pads.
    Pads,
    /// No sounds yet.
    Empty,
}

/// Making, editing and using maps.
#[derive(Clone, Debug, PartialEq)]
pub enum MapOp {
    /// A new articulation map: a template's (`ArticulationMap::TEMPLATES`)
    /// or empty, used by `track` when given.
    NewArticulationMap {
        track: Option<TrackId>,
        template: Option<String>,
    },
    /// A map's content replaced (same id; the map editor's edits).
    /// Articulations it no longer has leave the notes and lanes of the
    /// tracks using it in the same step.
    EditArticulationMap(Box<ArticulationMap>),
    /// Remove a map (the tracks that name it name none).
    RemoveArticulationMap(ArticulationMapId),
    UseArticulationMap {
        track: TrackId,
        map: Option<ArticulationMapId>,
    },
    /// Read a map file (FaderFrame's or a Cubase `.expressionmap`) into
    /// the project, used by `track` when given.
    ImportArticulationMap {
        track: Option<TrackId>,
        path: PathBuf,
    },
    /// Write a map as a user file (`None`: into [`user_dir`]).
    SaveArticulationMap {
        map: ArticulationMapId,
        path: Option<PathBuf>,
    },
    /// The articulation of notes (all of the clip's when empty; `None`:
    /// none). An attribute goes on the notes; a direction goes into the
    /// clip's lane where the first of them starts.
    SetNoteArticulation {
        clip: ClipId,
        notes: Vec<NoteId>,
        articulation: Option<ArticulationId>,
    },
    /// A direction in a clip's lane at `at` (clip time), replacing the
    /// one of its group there; `None` removes the directions at `at`.
    SetDirection {
        clip: ClipId,
        at: MusicalTime,
        articulation: Option<ArticulationId>,
    },
    /// Remove the `index`th direction of a clip's lane.
    RemoveDirection {
        clip: ClipId,
        index: usize,
    },
    /// A new drum map, used by `track` when given.
    NewDrumMap {
        track: Option<TrackId>,
        from: DrumMapSource,
    },
    EditDrumMap(Box<DrumMap>),
    RemoveDrumMap(DrumMapId),
    UseDrumMap {
        track: TrackId,
        map: Option<DrumMapId>,
    },
    ImportDrumMap {
        track: Option<TrackId>,
        path: PathBuf,
    },
    SaveDrumMap {
        map: DrumMapId,
        path: Option<PathBuf>,
    },
    /// Show the map editor on `map` (`track`: the track it is edited for;
    /// maps the editor makes or imports go to it).
    OpenEditor {
        map: MapRef,
        track: Option<TrackId>,
    },
    /// Show the map editor on a track's map of a kind, making one first
    /// when the track has none (an empty articulation map, the General
    /// MIDI drum map).
    EditTrackMap {
        track: TrackId,
        kind: MapKind,
    },
    /// The next note played on a MIDI input puts the drum sound on
    /// `pitch` on its key (one undo step; a sound already there swaps).
    LearnDrumPitch {
        map: DrumMapId,
        pitch: u8,
    },
    /// Stop learning a drum sound's pitch.
    CancelLearn,
}

impl MapOp {
    /// It may change the project (saving a user file, showing the editor
    /// and learning do not by themselves).
    pub fn edits(&self) -> bool {
        !matches!(
            self,
            MapOp::SaveArticulationMap { .. }
                | MapOp::SaveDrumMap { .. }
                | MapOp::OpenEditor { .. }
                | MapOp::LearnDrumPitch { .. }
                | MapOp::CancelLearn
        )
    }
}

static USER_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The user's maps live under this folder (tests set their own).
pub fn set_user_dir(dir: Option<PathBuf>) {
    if let Ok(mut d) = USER_DIR.lock() {
        *d = dir;
    }
}

/// The folder of the user's maps: `articulation-maps` and `drum-maps`
/// in it (in the data folder unless [`set_user_dir`] chose another).
pub fn user_dir() -> PathBuf {
    USER_DIR
        .lock()
        .ok()
        .and_then(|d| d.clone())
        .unwrap_or_else(crate::media::data_dir)
}

fn articulation_dir() -> PathBuf {
    user_dir().join("articulation-maps")
}

fn drum_dir() -> PathBuf {
    user_dir().join("drum-maps")
}

fn file_name(name: &str) -> String {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if n.is_empty() { "Map".into() } else { n }
}

/// The user's map files in a folder: (name, path), by name.
fn listed(dir: &Path, extensions: &[&str]) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    out.sort_by_key(|a| a.0.to_lowercase());
    out
}

/// The user's articulation maps (FaderFrame's and Cubase's files).
pub fn user_articulation_maps() -> Vec<(String, PathBuf)> {
    listed(&articulation_dir(), &["json", "expressionmap"])
}

/// The user's drum maps.
pub fn user_drum_maps() -> Vec<(String, PathBuf)> {
    listed(&drum_dir(), &["json"])
}

/// Read an articulation map file of either kind, giving it `id`.
pub fn read_articulation_map(id: ArticulationMapId, path: &Path) -> Result<ArticulationMap> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
    let cubase = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("expressionmap"))
        || text.trim_start().starts_with('<');
    let map = if cubase {
        cubase::parse(id, &text)
    } else {
        ArticulationMap::from_file(id, &text)
    };
    map.map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))
}

impl Session {
    /// Make, edit or use maps (see [`MapOp`]).
    pub(crate) fn map_op(&mut self, op: MapOp) -> Result<()> {
        match op {
            MapOp::NewArticulationMap { track, template } => {
                let id: ArticulationMapId = self.project.ids.allocate();
                let map = match template.as_deref() {
                    Some(t) => ArticulationMap::template(id, t).ok_or_else(|| {
                        SessionError::Other(format!("no articulation map template '{t}'"))
                    })?,
                    None => {
                        let mut m = ArticulationMap::new(id, "Articulations");
                        m.add(
                            "Natural",
                            "nat.",
                            0,
                            ArticulationKind::Direction,
                            Default::default(),
                        );
                        m
                    }
                };
                self.add_articulation_map(map, track, "New Articulation Map")
            }
            MapOp::EditArticulationMap(map) => {
                let index = self
                    .project
                    .articulation_maps
                    .iter()
                    .position(|m| m.id == map.id)
                    .ok_or_else(|| {
                        SessionError::Other(format!("no articulation map {}", map.id))
                    })?;
                // Articulations it no longer has leave the notes and lanes
                // of the tracks using it (their ids may be given again).
                let mut commands = self.articulations_removed(&map);
                commands.insert(
                    0,
                    Command::SetArticulationMap {
                        map: map.id,
                        value: Some(map),
                        index,
                    },
                );
                self.edit(Command::Batch {
                    label: "Edit Articulation Map".into(),
                    commands,
                })
            }
            MapOp::RemoveArticulationMap(map) => {
                let mut commands: Vec<Command> = self
                    .project
                    .tracks
                    .iter()
                    .filter(|t| t.articulation_map == Some(map))
                    .map(|t| Command::SetTrackArticulationMap {
                        track: t.id,
                        map: None,
                    })
                    .collect();
                commands.push(Command::SetArticulationMap {
                    map,
                    value: None,
                    index: 0,
                });
                self.edit(Command::Batch {
                    label: "Delete Articulation Map".into(),
                    commands,
                })
            }
            MapOp::UseArticulationMap { track, map } => {
                self.edit(Command::SetTrackArticulationMap { track, map })
            }
            MapOp::ImportArticulationMap { track, path } => {
                let id: ArticulationMapId = self.project.ids.allocate();
                let map = read_articulation_map(id, &path)?;
                self.add_articulation_map(map, track, "Import Articulation Map")
            }
            MapOp::SaveArticulationMap { map, path } => {
                let m = self
                    .project
                    .articulation_map(map)
                    .ok_or_else(|| SessionError::Other(format!("no articulation map {map}")))?;
                let path = path.unwrap_or_else(|| {
                    articulation_dir().join(format!("{}.json", file_name(&m.name)))
                });
                let text = m.to_file();
                write_map(&path, &text)?;
                self.notify(
                    NoticeLevel::Info,
                    format!("saved the articulation map to {}", path.display()),
                );
                Ok(())
            }
            MapOp::SetNoteArticulation {
                clip,
                notes,
                articulation,
            } => self.set_note_articulation(clip, &notes, articulation),
            MapOp::SetDirection {
                clip,
                at,
                articulation,
            } => self.set_direction(clip, at, articulation),
            MapOp::RemoveDirection { clip, index } => {
                let (_, mut m) = self.midi_clip(clip)?;
                if index >= m.articulations.len() {
                    return Ok(());
                }
                m.articulations.remove(index);
                self.set_midi_clip(clip, "Remove Articulation", m)
            }
            MapOp::NewDrumMap { track, from } => {
                let id: DrumMapId = self.project.ids.allocate();
                let map = match from {
                    DrumMapSource::Gm => DrumMap::gm(id),
                    DrumMapSource::Empty => DrumMap::new(id, "Drum Map"),
                    DrumMapSource::Pads => {
                        let t = track.ok_or_else(|| {
                            SessionError::Other("whose Drum Sampler pads?".into())
                        })?;
                        self.pads_map(id, t)?
                    }
                };
                self.add_drum_map(map, track, "New Drum Map")
            }
            MapOp::EditDrumMap(map) => {
                let index = self
                    .project
                    .drum_maps
                    .iter()
                    .position(|m| m.id == map.id)
                    .ok_or_else(|| SessionError::Other(format!("no drum map {}", map.id)))?;
                self.edit(Command::Batch {
                    label: "Edit Drum Map".into(),
                    commands: vec![Command::SetDrumMap {
                        map: map.id,
                        value: Some(map),
                        index,
                    }],
                })
            }
            MapOp::RemoveDrumMap(map) => {
                let mut commands: Vec<Command> = self
                    .project
                    .tracks
                    .iter()
                    .filter(|t| t.drum_map == Some(map))
                    .map(|t| Command::SetTrackDrumMap {
                        track: t.id,
                        map: None,
                    })
                    .collect();
                commands.push(Command::SetDrumMap {
                    map,
                    value: None,
                    index: 0,
                });
                self.edit(Command::Batch {
                    label: "Delete Drum Map".into(),
                    commands,
                })
            }
            MapOp::UseDrumMap { track, map } => self.edit(Command::SetTrackDrumMap { track, map }),
            MapOp::ImportDrumMap { track, path } => {
                let id: DrumMapId = self.project.ids.allocate();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
                let map = DrumMap::from_file(id, &text)
                    .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
                self.add_drum_map(map, track, "Import Drum Map")
            }
            MapOp::SaveDrumMap { map, path } => {
                let m = self
                    .project
                    .drum_map(map)
                    .ok_or_else(|| SessionError::Other(format!("no drum map {map}")))?;
                let path =
                    path.unwrap_or_else(|| drum_dir().join(format!("{}.json", file_name(&m.name))));
                let text = m.to_file();
                write_map(&path, &text)?;
                self.notify(
                    NoticeLevel::Info,
                    format!("saved the drum map to {}", path.display()),
                );
                Ok(())
            }
            MapOp::OpenEditor { map, track } => self.open_map_editor(map, track),
            MapOp::EditTrackMap { track, kind } => self.edit_track_map(track, kind),
            MapOp::LearnDrumPitch { map, pitch } => self.learn_drum_pitch(map, pitch),
            MapOp::CancelLearn => {
                self.cancel_map_learn();
                Ok(())
            }
        }
    }

    fn add_articulation_map(
        &mut self,
        map: ArticulationMap,
        track: Option<TrackId>,
        label: &str,
    ) -> Result<()> {
        let id = map.id;
        let mut commands = vec![Command::SetArticulationMap {
            map: id,
            value: Some(Box::new(map)),
            index: self.project.articulation_maps.len(),
        }];
        if let Some(track) = track {
            commands.push(Command::SetTrackArticulationMap {
                track,
                map: Some(id),
            });
        }
        self.edit(Command::Batch {
            label: label.into(),
            commands,
        })?;
        self.map_editor_shows(MapRef::Articulation(id), track);
        Ok(())
    }

    fn add_drum_map(&mut self, map: DrumMap, track: Option<TrackId>, label: &str) -> Result<()> {
        let id = map.id;
        let mut commands = vec![Command::SetDrumMap {
            map: id,
            value: Some(Box::new(map)),
            index: self.project.drum_maps.len(),
        }];
        if let Some(track) = track {
            commands.push(Command::SetTrackDrumMap {
                track,
                map: Some(id),
            });
        }
        self.edit(Command::Batch {
            label: label.into(),
            commands,
        })?;
        self.map_editor_shows(MapRef::Drum(id), track);
        Ok(())
    }

    /// The track that plays `track`'s notes: itself, or the instrument
    /// track a MIDI track plays.
    fn player_of(&self, track: TrackId) -> Option<&faderframe_project::Track> {
        let t = self.project.track(track)?;
        match t.output {
            faderframe_project::OutputRouting::Track { track: to } if t.kind == TrackKind::Midi => {
                self.project.track(to)
            }
            _ => Some(t),
        }
    }

    /// A drum map of the pads of `track`'s Drum Sampler (named by their
    /// samples).
    fn pads_map(&self, id: DrumMapId, track: TrackId) -> Result<DrumMap> {
        use faderframe_plugin_host::devices::drums;
        let player = self.player_of(track);
        let slot = player
            .and_then(|t| {
                t.slots()
                    .into_iter()
                    .find(|s| s.plugin.id == faderframe_core::builtin::DRUMS)
            })
            .ok_or_else(|| SessionError::Other("the track plays no Drum Sampler".into()))?;
        let first = slot
            .parameters
            .iter()
            .find(|p| p.id.0 == drums::id::BASE_NOTE)
            .map_or(36.0, |p| p.value)
            .round()
            .clamp(0.0, 127.0) as u8;
        let doc = crate::samples::doc_of(slot);
        let names: Vec<Option<String>> = (0..drums::PADS)
            .map(|p| {
                doc.file(p).map(|f| {
                    Path::new(f)
                        .file_stem()
                        .map_or_else(|| f.to_string(), |s| s.to_string_lossy().into_owned())
                })
            })
            .collect();
        Ok(DrumMap::pads(id, first, &names, drums::PADS))
    }

    fn set_note_articulation(
        &mut self,
        clip: ClipId,
        notes: &[NoteId],
        articulation: Option<ArticulationId>,
    ) -> Result<()> {
        let track = self
            .project
            .clip(clip)
            .map(|c| c.track)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let map = self.project.track_articulation_map(track).cloned();
        let (_, mut m) = self.midi_clip(clip)?;
        let chosen = |n: &faderframe_project::MidiNote| notes.is_empty() || notes.contains(&n.id);
        match articulation {
            Some(id) => {
                let map = map.ok_or_else(|| {
                    SessionError::Other("the track has no articulation map".into())
                })?;
                let a = map
                    .get(id)
                    .ok_or_else(|| SessionError::Other("no such articulation".into()))?;
                if a.kind == ArticulationKind::Direction {
                    let at = m
                        .notes
                        .iter()
                        .filter(|n| chosen(n))
                        .map(|n| n.start)
                        .min()
                        .ok_or_else(|| SessionError::Other("no notes chosen".into()))?;
                    return self.set_direction(clip, at, Some(id));
                }
                for n in m.notes.iter_mut().filter(|n| chosen(n)) {
                    n.articulation = Some(id);
                }
            }
            None => {
                for n in m.notes.iter_mut().filter(|n| chosen(n)) {
                    n.articulation = None;
                }
            }
        }
        self.set_midi_clip(clip, "Set Articulation", m)
    }

    fn set_direction(
        &mut self,
        clip: ClipId,
        at: MusicalTime,
        articulation: Option<ArticulationId>,
    ) -> Result<()> {
        let track = self
            .project
            .clip(clip)
            .map(|c| c.track)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let (_, mut m) = self.midi_clip(clip)?;
        let at = at.max(MusicalTime::ZERO);
        match articulation {
            Some(id) => {
                let map = self.project.track_articulation_map(track).ok_or_else(|| {
                    SessionError::Other("the track has no articulation map".into())
                })?;
                let group = map
                    .get(id)
                    .map(|a| a.group)
                    .ok_or_else(|| SessionError::Other("no such articulation".into()))?;
                let group_of = |a: ArticulationId| map.get(a).map(|x| x.group);
                m.articulations
                    .retain(|e| e.time != at || group_of(e.articulation) != Some(group));
                m.articulations.push(ArticulationEvent {
                    time: at,
                    articulation: id,
                });
                m.articulations.sort_by_key(|e| e.time);
            }
            None => m.articulations.retain(|e| e.time != at),
        }
        let label = if articulation.is_some() {
            "Set Articulation"
        } else {
            "Remove Articulation"
        };
        self.set_midi_clip(clip, label, m)
    }

    /// The articulation map of the clip the editor has open.
    pub fn editor_articulation_map(&self) -> Option<&ArticulationMap> {
        let clip = self.project.clip(self.editor_clip()?)?;
        self.project.track_articulation_map(clip.track)
    }

    /// The drum map of the clip the editor has open.
    pub fn editor_drum_map(&self) -> Option<&DrumMap> {
        let clip = self.project.clip(self.editor_clip()?)?;
        self.project.track_drum_map(clip.track)
    }

    /// The Articulation Map submenu of a track: none, the project's maps,
    /// new ones from templates, the user's files, removing the one in use.
    pub fn articulation_map_choices(&self, track: TrackId) -> Vec<InputChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            return Vec::new();
        }
        let mut out = vec![InputChoice {
            label: "None".into(),
            action: crate::Action::Maps(MapOp::UseArticulationMap { track, map: None }),
            checked: t.articulation_map.is_none(),
            group_start: false,
        }];
        for (k, m) in self.project.articulation_maps.iter().enumerate() {
            out.push(InputChoice {
                label: m.name.clone(),
                action: crate::Action::Maps(MapOp::UseArticulationMap {
                    track,
                    map: Some(m.id),
                }),
                checked: t.articulation_map == Some(m.id),
                group_start: k == 0,
            });
        }
        for (k, name) in ArticulationMap::TEMPLATES.iter().enumerate() {
            out.push(InputChoice {
                label: format!("New: {name}"),
                action: crate::Action::Maps(MapOp::NewArticulationMap {
                    track: Some(track),
                    template: Some((*name).into()),
                }),
                checked: false,
                group_start: k == 0,
            });
        }
        out.push(InputChoice {
            label: "New: Empty".into(),
            action: crate::Action::Maps(MapOp::NewArticulationMap {
                track: Some(track),
                template: None,
            }),
            checked: false,
            group_start: false,
        });
        for (k, (name, path)) in user_articulation_maps().into_iter().enumerate() {
            out.push(InputChoice {
                label: format!("From File: {name}"),
                action: crate::Action::Maps(MapOp::ImportArticulationMap {
                    track: Some(track),
                    path,
                }),
                checked: false,
                group_start: k == 0,
            });
        }
        if let Some(map) = t.articulation_map {
            out.push(InputChoice {
                label: "Edit Map…".into(),
                action: crate::Action::Maps(MapOp::OpenEditor {
                    map: MapRef::Articulation(map),
                    track: Some(track),
                }),
                checked: false,
                group_start: true,
            });
            out.push(InputChoice {
                label: "Save as User Map".into(),
                action: crate::Action::Maps(MapOp::SaveArticulationMap { map, path: None }),
                checked: false,
                group_start: false,
            });
        }
        out
    }

    /// The Drum Map submenu of a track: none, the project's maps, new
    /// ones (General MIDI, the Drum Sampler's pads), the user's files.
    pub fn drum_map_choices(&self, track: TrackId) -> Vec<InputChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            return Vec::new();
        }
        let mut out = vec![InputChoice {
            label: "None".into(),
            action: crate::Action::Maps(MapOp::UseDrumMap { track, map: None }),
            checked: t.drum_map.is_none(),
            group_start: false,
        }];
        for (k, m) in self.project.drum_maps.iter().enumerate() {
            out.push(InputChoice {
                label: m.name.clone(),
                action: crate::Action::Maps(MapOp::UseDrumMap {
                    track,
                    map: Some(m.id),
                }),
                checked: t.drum_map == Some(m.id),
                group_start: k == 0,
            });
        }
        out.push(InputChoice {
            label: "New: General MIDI".into(),
            action: crate::Action::Maps(MapOp::NewDrumMap {
                track: Some(track),
                from: DrumMapSource::Gm,
            }),
            checked: false,
            group_start: true,
        });
        let has_pads = self.player_of(track).is_some_and(|p| {
            p.slots()
                .iter()
                .any(|s| s.plugin.id == faderframe_core::builtin::DRUMS)
        });
        if has_pads {
            out.push(InputChoice {
                label: "New: Drum Sampler Pads".into(),
                action: crate::Action::Maps(MapOp::NewDrumMap {
                    track: Some(track),
                    from: DrumMapSource::Pads,
                }),
                checked: false,
                group_start: false,
            });
        }
        for (k, (name, path)) in user_drum_maps().into_iter().enumerate() {
            out.push(InputChoice {
                label: format!("From File: {name}"),
                action: crate::Action::Maps(MapOp::ImportDrumMap {
                    track: Some(track),
                    path,
                }),
                checked: false,
                group_start: k == 0,
            });
        }
        if let Some(map) = t.drum_map {
            out.push(InputChoice {
                label: "Edit Map…".into(),
                action: crate::Action::Maps(MapOp::OpenEditor {
                    map: MapRef::Drum(map),
                    track: Some(track),
                }),
                checked: false,
                group_start: true,
            });
            out.push(InputChoice {
                label: "Save as User Map".into(),
                action: crate::Action::Maps(MapOp::SaveDrumMap { map, path: None }),
                checked: false,
                group_start: false,
            });
        }
        out
    }

    /// The articulations notes of `clip` can take (an Articulation submenu
    /// for `notes`): none, then the map's, checked where every one of the
    /// notes carries it.
    pub fn articulation_choices(&self, clip: ClipId, notes: &[NoteId]) -> Vec<InputChoice> {
        let Some(c) = self.project.clip(clip) else {
            return Vec::new();
        };
        let (Some(map), Some(m)) = (self.project.track_articulation_map(c.track), c.as_midi())
        else {
            return Vec::new();
        };
        let chosen: Vec<&faderframe_project::MidiNote> =
            m.notes.iter().filter(|n| notes.contains(&n.id)).collect();
        let all = |a: Option<ArticulationId>| {
            !chosen.is_empty() && chosen.iter().all(|n| n.articulation == a)
        };
        let set = |a: Option<ArticulationId>| {
            crate::Action::Maps(MapOp::SetNoteArticulation {
                clip,
                notes: notes.to_vec(),
                articulation: a,
            })
        };
        let mut out = vec![InputChoice {
            label: "None".into(),
            action: set(None),
            checked: all(None),
            group_start: false,
        }];
        let mut group = None;
        for a in &map.articulations {
            let label = match a.kind {
                ArticulationKind::Attribute => a.name.clone(),
                ArticulationKind::Direction => format!("{} (from here on)", a.name),
            };
            out.push(InputChoice {
                label,
                action: set(Some(a.id)),
                checked: all(Some(a.id)),
                group_start: group != Some(a.group),
            });
            group = Some(a.group);
        }
        out
    }

    /// Find a map by name for a track (the dev action's way): the
    /// project's, a template's, a user file's or a file's path; used by the
    /// track.
    pub fn use_articulation_map_named(&mut self, track: TrackId, name: &str) -> Result<()> {
        let name = name.trim();
        if name.eq_ignore_ascii_case("none") {
            return self.map_op(MapOp::UseArticulationMap { track, map: None });
        }
        if let Some(m) = self
            .project
            .articulation_maps
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
        {
            let map = Some(m.id);
            return self.map_op(MapOp::UseArticulationMap { track, map });
        }
        if let Some(t) = ArticulationMap::TEMPLATES
            .iter()
            .find(|t| t.eq_ignore_ascii_case(name))
        {
            return self.map_op(MapOp::NewArticulationMap {
                track: Some(track),
                template: Some((*t).into()),
            });
        }
        let path = user_articulation_maps()
            .into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, p)| p)
            .or_else(|| Some(PathBuf::from(name)).filter(|p| p.is_file()))
            .ok_or_else(|| SessionError::Other(format!("no articulation map '{name}'")))?;
        self.map_op(MapOp::ImportArticulationMap {
            track: Some(track),
            path,
        })
    }

    /// The dev action's drum map: `gm`, `pads`, `none`, a project map's
    /// or user file's name, or a file's path.
    pub fn use_drum_map_named(&mut self, track: TrackId, name: &str) -> Result<()> {
        let name = name.trim();
        match name.to_ascii_lowercase().as_str() {
            "none" => return self.map_op(MapOp::UseDrumMap { track, map: None }),
            "gm" => {
                return self.map_op(MapOp::NewDrumMap {
                    track: Some(track),
                    from: DrumMapSource::Gm,
                });
            }
            "pads" => {
                return self.map_op(MapOp::NewDrumMap {
                    track: Some(track),
                    from: DrumMapSource::Pads,
                });
            }
            _ => {}
        }
        if let Some(m) = self
            .project
            .drum_maps
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
        {
            let map = Some(m.id);
            return self.map_op(MapOp::UseDrumMap { track, map });
        }
        let path = user_drum_maps()
            .into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, p)| p)
            .or_else(|| Some(PathBuf::from(name)).filter(|p| p.is_file()))
            .ok_or_else(|| SessionError::Other(format!("no drum map '{name}'")))?;
        self.map_op(MapOp::ImportDrumMap {
            track: Some(track),
            path,
        })
    }
}

fn write_map(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
    }
    std::fs::write(path, text).map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))
}
