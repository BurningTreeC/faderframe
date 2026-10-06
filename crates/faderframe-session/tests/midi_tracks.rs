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
