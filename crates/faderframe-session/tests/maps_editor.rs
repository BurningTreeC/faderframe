//! The map editor's side of the session: it opens on a track's map (made
//! first when the track has none) or on the map chosen, follows the
//! selection otherwise and shows maps made or imported at once; an edit
//! that removes articulations takes them off the notes and lanes in the
//! same step; a drum sound learns its pitch from the next key played on a
//! MIDI input (one step; a sound already there swaps).
#![allow(clippy::unwrap_used)]

use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::{MidiNote, PluginRef, Project, TrackKind};
use faderframe_session::maps::{MapKind, MapOp, MapRef};
use faderframe_session::{Action, SelectMode, Session};
use faderframe_timeline::MusicalTime;
use faderframe_workspace::ViewId;

const SR: u32 = 48_000;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

/// A track with a clip of three notes (beats 1, 2, 3).
fn with_notes(kind: TrackKind) -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Maps", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(kind).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: q(8.0),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let notes = [(1.0, 60), (2.0, 62), (3.0, 64)]
        .into_iter()
        .map(|(at, key)| MidiNote {
            time_locked: false,
            id: faderframe_core::NoteId(0),
            start: q(at),
            length: q(0.25),
            key,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
            articulation: None,
            fine: None,
        })
        .collect();
    s.dispatch(Action::AddNotes { clip, notes }).unwrap();
    (s, t, clip)
}

fn maps(op: MapOp) -> Action {
    Action::Maps(op)
}

#[test]
fn the_editor_opens_on_a_tracks_map_and_follows_the_selection() {
    let (mut s, t, _) = with_notes(TrackKind::Instrument);
    let other = s.add_track(TrackKind::Midi).unwrap();
    let audio = s.add_track(TrackKind::Audio).unwrap();
    assert_eq!(s.map_editor(), None);
    // A track with no map: one is made (one step) and shown.
    s.dispatch(maps(MapOp::EditTrackMap {
        track: t,
        kind: MapKind::Articulation,
    }))
    .unwrap();
    let art = s.project().track(t).unwrap().articulation_map.unwrap();
    assert_eq!(s.map_editor(), Some(MapRef::Articulation(art)));
    assert_eq!(s.map_editor_track(), Some(t));
    assert!(
        s.workspace().active_layout().is_placed(&ViewId::maps()),
        "the editor is shown"
    );
    assert_eq!(s.history().undo_label(), Some("New Articulation Map"));
    assert_eq!(
        s.project().articulation_map(art).unwrap().articulations[0].name,
        "Natural"
    );
    // A drum map for the same track: the General MIDI one.
    s.dispatch(maps(MapOp::EditTrackMap {
        track: t,
        kind: MapKind::Drum,
    }))
    .unwrap();
    let drums = s.project().track(t).unwrap().drum_map.unwrap();
    assert_eq!(s.map_editor(), Some(MapRef::Drum(drums)));
    assert_eq!(s.project().drum_map(drums).unwrap().sounds.len(), 47);
    // Opening it again makes nothing new.
    let steps = s.history().undo_labels().len();
    s.dispatch(maps(MapOp::EditTrackMap {
        track: t,
        kind: MapKind::Articulation,
    }))
    .unwrap();
    assert_eq!(s.history().undo_labels().len(), steps);
    assert_eq!(s.map_editor(), Some(MapRef::Articulation(art)));
    // Audio tracks have no maps.
    assert!(
        s.dispatch(maps(MapOp::EditTrackMap {
            track: audio,
            kind: MapKind::Drum,
        }))
        .is_err()
    );
    // The track menus open it.
    let edit = s
        .articulation_map_choices(t)
        .into_iter()
        .find(|c| c.label == "Edit Map…")
        .unwrap();
    assert_eq!(
        edit.action,
        maps(MapOp::OpenEditor {
            map: MapRef::Articulation(art),
            track: Some(t),
        })
    );
    assert!(
        s.drum_map_choices(t)
            .iter()
            .any(|c| c.label == "Edit Map…" && c.group_start)
    );
    assert!(
        !s.articulation_map_choices(other)
            .iter()
            .any(|c| c.label == "Edit Map…"),
        "no map, nothing to edit"
    );
    // A map deleted: the editor shows the track's other map.
    s.dispatch(maps(MapOp::RemoveArticulationMap(art))).unwrap();
    assert_eq!(s.map_editor(), Some(MapRef::Drum(drums)));
    s.dispatch(Action::Undo).unwrap();
    // Opened on nothing in particular, it follows the selected track.
    let mut fresh = Session::new(Project::new("Sel", SR), None, EngineConfig::default()).unwrap();
    let a = fresh.add_track(TrackKind::Midi).unwrap();
    let b = fresh.add_track(TrackKind::Midi).unwrap();
    fresh.use_drum_map_named(a, "gm").unwrap();
    fresh
        .dispatch(maps(MapOp::NewArticulationMap {
            track: Some(b),
            template: Some("Brass".into()),
        }))
        .unwrap();
    let brass = fresh.project().track(b).unwrap().articulation_map.unwrap();
    assert_eq!(
        fresh.map_editor(),
        Some(MapRef::Articulation(brass)),
        "a new map shows at once"
    );
    assert!(fresh.has_map(MapRef::Articulation(brass)));
    assert_eq!(fresh.map_name(MapRef::Articulation(brass)), Some("Brass"));
    assert_eq!(fresh.map_users(MapRef::Articulation(brass)), vec![b]);
    // Another selection: the selected track's map (like the Inspector);
    // a selected track without one keeps the map shown.
    let a_map = fresh.project().track(a).unwrap().drum_map.unwrap();
    let c = fresh.add_track(TrackKind::Midi).unwrap();
    for (pick, shown, track) in [
        (a, MapRef::Drum(a_map), a),
        (b, MapRef::Articulation(brass), b),
        (c, MapRef::Articulation(brass), c),
    ] {
        fresh
            .dispatch(Action::SelectTracks {
                tracks: vec![pick],
                mode: SelectMode::Replace,
            })
            .unwrap();
        assert_eq!(fresh.map_editor(), Some(shown));
        assert_eq!(fresh.map_editor_track(), Some(track));
    }
    // Opened from a track's menu: that map until the selection changes.
    fresh
        .dispatch(maps(MapOp::OpenEditor {
            map: MapRef::Drum(a_map),
            track: Some(a),
        }))
        .unwrap();
    assert_eq!(fresh.map_editor(), Some(MapRef::Drum(a_map)));
    assert_eq!(fresh.map_editor_track(), Some(a));
    fresh
        .dispatch(Action::SelectTracks {
            tracks: vec![b],
            mode: SelectMode::Replace,
        })
        .unwrap();
    assert_eq!(fresh.map_editor(), Some(MapRef::Articulation(brass)));
}

#[test]
fn removing_an_articulation_takes_it_off_the_notes_and_lanes_in_one_step() {
    let (mut s, t, clip) = with_notes(TrackKind::Midi);
    s.use_articulation_map_named(t, "strings").unwrap();
    let map = s.project().track_articulation_map(t).unwrap().clone();
    let stacc = map.find("Staccato").unwrap().id;
    let pizz = map.find("Pizzicato").unwrap().id;
    let notes: Vec<_> = s
        .project()
        .clip(clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone();
    s.dispatch(maps(MapOp::SetNoteArticulation {
        clip,
        notes: vec![notes[0].id],
        articulation: Some(stacc),
    }))
    .unwrap();
    s.dispatch(maps(MapOp::SetDirection {
        clip,
        at: q(2.0),
        articulation: Some(pizz),
    }))
    .unwrap();
    // Both taken out of the map in one edit.
    let mut edited = map.clone();
    edited
        .articulations
        .retain(|a| a.id != stacc && a.id != pizz);
    s.dispatch(maps(MapOp::EditArticulationMap(Box::new(edited))))
        .unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    assert!(m.notes.iter().all(|n| n.articulation.is_none()));
    assert!(m.articulations.is_empty(), "the lane's direction went too");
    assert_eq!(s.history().undo_label(), Some("Edit Articulation Map"));
    // One step back: the map and the clip as they were.
    s.dispatch(Action::Undo).unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    assert_eq!(m.notes[0].articulation, Some(stacc));
    assert_eq!(m.articulations.len(), 1);
    assert_eq!(s.project().track_articulation_map(t).unwrap(), &map);
    // An edit keeping every articulation leaves the clips alone.
    let mut renamed = map.clone();
    renamed.name = "Strings II".into();
    s.dispatch(maps(MapOp::EditArticulationMap(Box::new(renamed))))
        .unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap();
    assert_eq!(m.notes[0].articulation, Some(stacc));
}

#[test]
fn a_drum_sound_learns_its_pitch_from_the_next_key_played() {
    let (mut s, t, _) = with_notes(TrackKind::Instrument);
    s.use_drum_map_named(t, "gm").unwrap();
    let map = s.project().track_drum_map(t).unwrap().clone();
    // The kick (36) learns: the snare's key (38) is played.
    s.dispatch(maps(MapOp::LearnDrumPitch {
        map: map.id,
        pitch: 36,
    }))
    .unwrap();
    assert_eq!(s.map_learning(), Some((map.id, 36)));
    let steps = s.history().undo_labels().len();
    // Other messages and note-offs leave it waiting.
    s.midi_keyboard().send(&[0xB0, 1, 64]);
    s.midi_keyboard().send(&[0x90, 38, 0]);
    s.tick(0.0);
    assert_eq!(s.map_learning(), Some((map.id, 36)));
    s.midi_keyboard().send(&[0x99, 38, 100]);
    s.midi_keyboard().send(&[0x89, 38, 0]);
    s.tick(0.0);
    assert_eq!(s.map_learning(), None, "learnt");
    let now = s.project().track_drum_map(t).unwrap();
    assert_eq!(now.name_of(38), Some("Bass Drum 1"));
    assert_eq!(now.name_of(36), Some("Acoustic Snare"), "the two swap");
    assert_eq!(now.order(), {
        let mut o = map.order();
        o.swap(0, 2);
        o
    });
    assert_eq!(s.history().undo_labels().len(), steps + 1);
    assert_eq!(s.history().undo_label(), Some("Learn Drum Sound Pitch"));
    // A later key does nothing more.
    s.midi_keyboard().send(&[0x90, 50, 100]);
    s.tick(0.0);
    assert_eq!(s.history().undo_labels().len(), steps + 1);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track_drum_map(t).unwrap(), &map);
    // Stopped: nothing learnt.
    s.dispatch(maps(MapOp::LearnDrumPitch {
        map: map.id,
        pitch: 42,
    }))
    .unwrap();
    s.dispatch(maps(MapOp::CancelLearn)).unwrap();
    s.midi_keyboard().send(&[0x90, 44, 100]);
    s.tick(0.0);
    assert_eq!(s.project().track_drum_map(t).unwrap(), &map);
    // A key not in the map: the sound moves there.
    s.dispatch(maps(MapOp::LearnDrumPitch {
        map: map.id,
        pitch: 42,
    }))
    .unwrap();
    s.midi_keyboard().send(&[0x90, 100, 100]);
    s.tick(0.0);
    let now = s.project().track_drum_map(t).unwrap();
    assert_eq!(now.name_of(100), Some("Closed Hi-Hat"));
    assert_eq!(now.sound(42), None);
    // Only sounds there are learn.
    assert!(
        s.dispatch(maps(MapOp::LearnDrumPitch {
            map: map.id,
            pitch: 42,
        }))
        .is_err()
    );
}

#[test]
fn the_editor_offers_new_maps_of_both_kinds() {
    let (mut s, t, _) = with_notes(TrackKind::Instrument);
    let labels = |s: &Session| -> Vec<String> {
        s.new_map_choices(Some(t))
            .into_iter()
            .map(|c| c.label)
            .collect()
    };
    assert!(labels(&s).contains(&"New Articulation Map: Strings".to_string()));
    assert!(labels(&s).contains(&"New Drum Map: General MIDI".to_string()));
    assert!(!labels(&s).iter().any(|l| l.contains("Pads")));
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(faderframe_core::builtin::DRUMS, "Drum Sampler"),
    })
    .unwrap();
    let pads = s
        .new_map_choices(Some(t))
        .into_iter()
        .find(|c| c.label == "New Drum Map: Drum Sampler Pads")
        .unwrap();
    s.dispatch(pads.action).unwrap();
    let map = s.project().track(t).unwrap().drum_map.unwrap();
    assert_eq!(s.map_editor(), Some(MapRef::Drum(map)));
    assert_eq!(s.project().drum_map(map).unwrap().sounds.len(), 16);
    // An empty one, for no track.
    let empty = s
        .new_map_choices(None)
        .into_iter()
        .find(|c| c.label == "New Drum Map: Empty")
        .unwrap();
    s.dispatch(empty.action).unwrap();
    let MapRef::Drum(id) = s.map_editor().unwrap() else {
        panic!("a drum map");
    };
    assert!(s.project().drum_map(id).unwrap().sounds.is_empty());
    assert!(s.map_users(MapRef::Drum(id)).is_empty());
}
