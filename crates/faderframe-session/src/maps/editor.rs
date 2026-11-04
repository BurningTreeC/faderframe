//! The map editor's side of the session: which map it shows ([`MapRef`])
//! and for which track, the maps it can make, and a drum sound learning
//! its pitch from the MIDI inputs (the next note-on played sets it, one
//! undo step; a sound already on that key swaps with it).
//!
//! What the editor shows: the map it was opened on (maps made or imported
//! through [`MapOp`] are shown at once) until the track selection changes
//! and a selected track has a map — then that track's, its articulation
//! map, else its drum map, like the Inspector following the selection. A
//! map gone from the project falls back the same way, to the track it was
//! opened for, else the piano roll clip's.

use super::{DrumMapSource, MapOp};
use crate::{InputChoice, NoticeLevel, Result, Session, SessionError, WorkspaceAction};
use faderframe_core::{ArticulationMapId, DrumMapId, TrackId};
use faderframe_midi::MidiEvent;
use faderframe_project::articulation::ArticulationMap;
use faderframe_project::{Command, TrackKind};
use faderframe_workspace::ViewId;

/// A map of the project, of either kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapRef {
    Articulation(ArticulationMapId),
    Drum(DrumMapId),
}

/// The kinds of map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MapKind {
    Articulation,
    Drum,
}

impl MapRef {
    pub fn kind(self) -> MapKind {
        match self {
            MapRef::Articulation(_) => MapKind::Articulation,
            MapRef::Drum(_) => MapKind::Drum,
        }
    }
}

impl MapKind {
    /// "articulation map", "drum map".
    pub fn noun(self) -> &'static str {
        match self {
            MapKind::Articulation => "articulation map",
            MapKind::Drum => "drum map",
        }
    }
}

/// What the map editor shows and is doing.
#[derive(Clone, Debug, Default)]
pub(crate) struct MapEditorState {
    /// The map it was opened on (gone from the project: the track's).
    shown: Option<MapRef>,
    /// The track it was opened for.
    track: Option<TrackId>,
    /// The tracks selected when it was opened (another selection follows
    /// the selected track).
    selection: Vec<TrackId>,
    /// A drum sound (by its map and pitch) waiting for a key played.
    learning: Option<(DrumMapId, u8)>,
}

impl Session {
    /// The map the map editor shows (see the module).
    pub fn map_editor(&self) -> Option<MapRef> {
        let map_of = |id: TrackId| {
            let t = self.project.track(id)?;
            t.articulation_map
                .map(MapRef::Articulation)
                .or(t.drum_map.map(MapRef::Drum))
        };
        if self.map_selection_moved()
            && let Some(m) = self.selected_playing_track().and_then(map_of)
        {
            return Some(m);
        }
        if let Some(m) = self.map_editor.shown.filter(|m| self.has_map(*m)) {
            return Some(m);
        }
        map_of(self.map_editor_track()?)
    }

    /// The track the map editor edits for: new and imported maps go to it
    /// (instrument and MIDI tracks only).
    pub fn map_editor_track(&self) -> Option<TrackId> {
        if self.map_selection_moved()
            && let Some(t) = self.selected_playing_track()
        {
            return Some(t);
        }
        if let Some(t) = self.map_editor.track.filter(|t| self.plays_notes(*t)) {
            return Some(t);
        }
        self.selected_playing_track().or_else(|| {
            let clip = self.project.clip(self.editor_clip()?)?;
            self.plays_notes(clip.track).then_some(clip.track)
        })
    }

    /// An instrument or MIDI track.
    fn plays_notes(&self, id: TrackId) -> bool {
        self.project
            .track(id)
            .is_some_and(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi))
    }

    /// The first selected instrument or MIDI track (in track order).
    fn selected_playing_track(&self) -> Option<TrackId> {
        self.project
            .tracks
            .iter()
            .map(|t| t.id)
            .find(|t| self.selection.tracks.contains(t) && self.plays_notes(*t))
    }

    /// The track selection is not what it was when the editor was opened.
    fn map_selection_moved(&self) -> bool {
        !self
            .selection
            .tracks
            .iter()
            .copied()
            .eq(self.map_editor.selection.iter().copied())
    }

    /// The drum sound learning its pitch: its map and the pitch it is on.
    pub fn map_learning(&self) -> Option<(DrumMapId, u8)> {
        self.map_editor.learning.filter(|(m, p)| {
            self.project
                .drum_map(*m)
                .is_some_and(|d| d.sound(*p).is_some())
        })
    }

    /// The tracks playing by `map`, in track order.
    pub fn map_users(&self, map: MapRef) -> Vec<TrackId> {
        self.project
            .tracks
            .iter()
            .filter(|t| match map {
                MapRef::Articulation(m) => t.articulation_map == Some(m),
                MapRef::Drum(m) => t.drum_map == Some(m),
            })
            .map(|t| t.id)
            .collect()
    }

    /// The project has the map.
    pub fn has_map(&self, map: MapRef) -> bool {
        match map {
            MapRef::Articulation(m) => self.project.articulation_map(m).is_some(),
            MapRef::Drum(m) => self.project.drum_map(m).is_some(),
        }
    }

    /// A map's name.
    pub fn map_name(&self, map: MapRef) -> Option<&str> {
        match map {
            MapRef::Articulation(m) => self.project.articulation_map(m).map(|m| m.name.as_str()),
            MapRef::Drum(m) => self.project.drum_map(m).map(|m| m.name.as_str()),
        }
    }

    /// The maps the map editor can make for `track` (used by it when
    /// given): articulation maps from the templates or empty, drum maps
    /// from General MIDI, the track's Drum Sampler pads (when it plays
    /// one) or empty.
    pub fn new_map_choices(&self, track: Option<TrackId>) -> Vec<InputChoice> {
        let choice = |label: String, op: MapOp, group_start: bool| InputChoice {
            label,
            action: crate::Action::Maps(op),
            checked: false,
            group_start,
        };
        let mut out: Vec<InputChoice> = ArticulationMap::TEMPLATES
            .iter()
            .map(|name| {
                choice(
                    format!("New Articulation Map: {name}"),
                    MapOp::NewArticulationMap {
                        track,
                        template: Some((*name).into()),
                    },
                    false,
                )
            })
            .collect();
        out.push(choice(
            "New Articulation Map: Empty".into(),
            MapOp::NewArticulationMap {
                track,
                template: None,
            },
            false,
        ));
        out.push(choice(
            "New Drum Map: General MIDI".into(),
            MapOp::NewDrumMap {
                track,
                from: DrumMapSource::Gm,
            },
            true,
        ));
        let pads = track.and_then(|t| self.player_of(t)).is_some_and(|p| {
            p.slots()
                .iter()
                .any(|s| s.plugin.id == faderframe_core::builtin::DRUMS)
        });
        if pads {
            out.push(choice(
                "New Drum Map: Drum Sampler Pads".into(),
                MapOp::NewDrumMap {
                    track,
                    from: DrumMapSource::Pads,
                },
                false,
            ));
        }
        out.push(choice(
            "New Drum Map: Empty".into(),
            MapOp::NewDrumMap {
                track,
                from: DrumMapSource::Empty,
            },
            false,
        ));
        out
    }

    /// The map editor shows `map` from now on (`track`: edited for it).
    pub(crate) fn map_editor_shows(&mut self, map: MapRef, track: Option<TrackId>) {
        self.map_editor.shown = Some(map);
        if track.is_some() {
            self.map_editor.track = track;
        }
        self.map_editor.selection = self.selection.tracks.iter().copied().collect();
        self.revision += 1;
    }

    pub(crate) fn open_map_editor(&mut self, map: MapRef, track: Option<TrackId>) -> Result<()> {
        if !self.has_map(map) {
            return Err(SessionError::Other(format!(
                "no such {}",
                map.kind().noun()
            )));
        }
        self.map_editor_shows(map, track);
        self.workspace_action(WorkspaceAction::ShowView(ViewId::maps()))
    }

    /// Open the map editor on `track`'s map of `kind`, made first when it
    /// has none.
    pub(crate) fn edit_track_map(&mut self, track: TrackId, kind: MapKind) -> Result<()> {
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
            return Err(SessionError::Other(format!(
                "'{}' plays no notes: only instrument and MIDI tracks have maps",
                t.name
            )));
        }
        let has = match kind {
            MapKind::Articulation => t.articulation_map.map(MapRef::Articulation),
            MapKind::Drum => t.drum_map.map(MapRef::Drum),
        };
        let map = match has {
            Some(m) => m,
            None => {
                self.map_op(match kind {
                    MapKind::Articulation => MapOp::NewArticulationMap {
                        track: Some(track),
                        template: None,
                    },
                    MapKind::Drum => MapOp::NewDrumMap {
                        track: Some(track),
                        from: DrumMapSource::Gm,
                    },
                })?;
                let t = self
                    .project
                    .track(track)
                    .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
                match kind {
                    MapKind::Articulation => t.articulation_map.map(MapRef::Articulation),
                    MapKind::Drum => t.drum_map.map(MapRef::Drum),
                }
                .ok_or_else(|| SessionError::Other("the new map is not the track's".into()))?
            }
        };
        self.open_map_editor(map, Some(track))
    }

    pub(crate) fn learn_drum_pitch(&mut self, map: DrumMapId, pitch: u8) -> Result<()> {
        let name = self
            .project
            .drum_map(map)
            .and_then(|m| m.sound(pitch))
            .map(|s| s.name.clone())
            .ok_or_else(|| SessionError::Other(format!("no drum sound on key {pitch}")))?;
        self.map_editor.learning = Some((map, pitch));
        self.notify(
            NoticeLevel::Info,
            format!("Learning: play the key for {name} on a MIDI input (Esc stops)"),
        );
        self.revision += 1;
        Ok(())
    }

    /// Another project: the editor starts over (its map and track were
    /// the old project's).
    pub(crate) fn map_editor_project_replaced(&mut self) {
        self.map_editor = MapEditorState::default();
    }

    pub(crate) fn cancel_map_learn(&mut self) {
        if self.map_editor.learning.take().is_some() {
            self.revision += 1;
        }
    }

    /// A message from a MIDI input while a drum sound learns its pitch:
    /// a note-on puts the sound on its key (the sound there takes the old
    /// key) and ends learning.
    pub(crate) fn map_learn_event(&mut self, event: MidiEvent) {
        let MidiEvent::NoteOn { key, velocity, .. } = event else {
            return;
        };
        if velocity == 0 {
            return;
        }
        let Some((map, pitch)) = self.map_editor.learning.take() else {
            return;
        };
        self.revision += 1;
        let Some(mut m) = self.project.drum_map(map).cloned() else {
            return;
        };
        let Some(index) = m.sounds.iter().position(|s| s.pitch == pitch) else {
            return;
        };
        let key = key & 127;
        if key == pitch {
            return;
        }
        let name = m.sounds[index].name.clone();
        m.set_pitch(index, key);
        let Some(at) = self.project.drum_maps.iter().position(|d| d.id == map) else {
            return;
        };
        let done = self.edit(Command::Batch {
            label: "Learn Drum Sound Pitch".into(),
            commands: vec![Command::SetDrumMap {
                map,
                value: Some(Box::new(m)),
                index: at,
            }],
        });
        match done {
            Ok(()) => self.notify(
                NoticeLevel::Info,
                format!(
                    "{name} is on {} now",
                    faderframe_project::articulation::key_name(key)
                ),
            ),
            Err(e) => self.notify(NoticeLevel::Error, e.to_string()),
        }
    }

    /// The commands taking articulations `map` no longer has off the notes
    /// and lanes of the clips of the tracks using it.
    pub(crate) fn articulations_removed(&self, map: &ArticulationMap) -> Vec<Command> {
        let Some(old) = self.project.articulation_map(map.id) else {
            return Vec::new();
        };
        let gone: Vec<_> = old
            .articulations
            .iter()
            .map(|a| a.id)
            .filter(|id| map.get(*id).is_none())
            .collect();
        if gone.is_empty() {
            return Vec::new();
        }
        let users = self.map_users(MapRef::Articulation(map.id));
        let mut clips: Vec<&faderframe_project::Clip> = self
            .project
            .clips
            .values()
            .filter(|c| users.contains(&c.track))
            .collect();
        clips.sort_by_key(|c| c.id);
        clips
            .into_iter()
            .filter_map(|c| {
                let m = c.as_midi()?;
                let carries = m
                    .notes
                    .iter()
                    .any(|n| n.articulation.is_some_and(|a| gone.contains(&a)))
                    || m.articulations
                        .iter()
                        .any(|e| gone.contains(&e.articulation));
                if !carries {
                    return None;
                }
                let mut m = m.clone();
                for n in &mut m.notes {
                    if n.articulation.is_some_and(|a| gone.contains(&a)) {
                        n.articulation = None;
                    }
                }
                m.articulations.retain(|e| !gone.contains(&e.articulation));
                Some(Command::SetClipContent {
                    clip: c.id,
                    start: c.start,
                    content: Box::new(faderframe_project::ClipContent::Midi(m)),
                })
            })
            .collect()
    }
}
