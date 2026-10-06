//! MIDI tracks play an instrument track.
#![allow(clippy::unwrap_used)]

use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_project::{OutputRouting, TrackKind};
use faderframe_session::{Action, SelectMode, Session};

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

#[test]
fn a_midi_track_is_routed_to_an_instrument_track() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    // Added with an instrument track selected: it plays that one.
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    let midi = s.add_track(TrackKind::Midi).unwrap();
    assert_eq!(
        s.project().track(midi).unwrap().output,
        OutputRouting::Track { track: lead }
    );
    // Added with an audio track selected: none until chosen.
    let bass = track(&s, "Bass");
    s.selection.select_tracks(&[bass], SelectMode::Replace);
    let other = s.add_track(TrackKind::Midi).unwrap();
    assert_eq!(
        s.project().track(other).unwrap().output,
        OutputRouting::None
    );

    let choices = s.midi_instrument_choices(other);
    let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["None", "Lead Synth (Synth)"]);
    assert!(choices[0].checked);
    s.dispatch(choices[1].action.clone()).unwrap();
    assert_eq!(
        s.project().track(other).unwrap().output,
        OutputRouting::Track { track: lead }
    );
    assert!(s.midi_instrument_choices(other)[1].checked);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project().track(other).unwrap().output,
        OutputRouting::None
    );
    // Only MIDI tracks choose an instrument.
    assert!(s.midi_instrument_choices(lead).is_empty());
}

fn plugin(s: &Session, id: &str) -> faderframe_project::PluginRef {
    s.available_plugins()
        .into_iter()
        .find(|p| p.plugin.id == id)
        .unwrap()
        .plugin
}

fn insert(s: &mut Session, track: TrackId, id: &str) {
    let plugin = plugin(s, id);
    let index = s.project().track(track).unwrap().inserts.len();
    s.dispatch(Action::InsertPlugin {
        track,
        index,
        plugin,
    })
    .unwrap();
}

fn names(s: &Session, track: TrackId) -> Vec<String> {
    s.project()
        .track(track)
        .unwrap()
        .inserts
        .iter()
        .map(|p| p.plugin.id.clone())
        .collect()
}

#[test]
fn midi_effects_move_between_midi_and_instrument_tracks() {
    use faderframe_core::builtin::{ARPEGGIATOR, CHORD, COMPRESSOR};
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    let bass = track(&s, "Bass");
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    let midi = s.add_track(TrackKind::Midi).unwrap();
    insert(&mut s, midi, ARPEGGIATOR);
    insert(&mut s, midi, CHORD);
    // Reordered on the MIDI track.
    let chord = s.project().track(midi).unwrap().inserts[1].id;
    s.dispatch(Action::MovePlugin {
        track: midi,
        plugin: chord,
        to: midi,
        index: 0,
    })
    .unwrap();
    assert_eq!(names(&s, midi), [CHORD, ARPEGGIATOR]);
    // Not onto an audio track.
    assert!(
        s.dispatch(Action::CopyPlugin {
            track: midi,
            plugin: chord,
            to: bass,
            index: 0,
        })
        .is_err()
    );
    // Onto the instrument track: before its instrument, wherever dropped.
    let before = s.project().track(lead).unwrap().inserts.len();
    s.dispatch(Action::CopyPlugin {
        track: midi,
        plugin: chord,
        to: lead,
        index: 99,
    })
    .unwrap();
    let t = s.project().track(lead).unwrap();
    assert_eq!(t.inserts.len(), before + 1);
    let at = t.inserts.iter().position(|p| p.plugin.id == CHORD).unwrap();
    if let Some(inst) = t
        .inserts
        .iter()
        .position(|p| s.engine_plugin_is_instrument(p.id))
    {
        assert!(at < inst);
    }
    // An audio effect cannot go onto the MIDI track.
    insert(&mut s, bass, COMPRESSOR);
    let comp = s.project().track(bass).unwrap().inserts.last().unwrap().id;
    assert!(
        s.dispatch(Action::MovePlugin {
            track: bass,
            plugin: comp,
            to: midi,
            index: 0,
        })
        .is_err()
    );
    assert_eq!(names(&s, midi), [CHORD, ARPEGGIATOR]);
}

#[test]
fn a_midi_track_shows_the_keys_it_plays() {
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let lead = track(&s, "Lead Synth");
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    let midi = s.add_track(TrackKind::Midi).unwrap();
    // Selected, it plays live: the held keys light.
    s.midi_keyboard().send(&[0x90, 60, 100]);
    s.midi_keyboard().send(&[0x90, 64, 100]);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while s.sounding_keys(midi) != (1 << 60) | (1 << 64) {
        assert!(
            std::time::Instant::now() < end,
            "{:x}",
            s.sounding_keys(midi)
        );
        s.tick(0.016);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Another track selected: the MIDI track no longer plays live.
    s.selection.select_tracks(&[lead], SelectMode::Replace);
    s.tick(0.016);
    assert_eq!(s.sounding_keys(midi), 0);
    s.midi_keyboard().send(&[0x80, 60, 0]);
    s.midi_keyboard().send(&[0x80, 64, 0]);
}
