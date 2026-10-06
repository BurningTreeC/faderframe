#![allow(clippy::unwrap_used)]
//! Where MIDI effects go: before the instrument on instrument tracks, on
//! MIDI tracks (which take nothing else), never on audio tracks.

use faderframe_core::builtin;
use faderframe_engine::EngineConfig;
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, Session};

fn insert(s: &mut Session, track: faderframe_core::TrackId, index: usize, id: &str) -> bool {
    s.dispatch(Action::InsertPlugin {
        track,
        index,
        plugin: PluginRef::builtin(id, id),
    })
    .is_ok()
}

fn chain(s: &Session, track: faderframe_core::TrackId) -> Vec<String> {
    s.project()
        .track(track)
        .unwrap()
        .inserts
        .iter()
        .map(|p| p.plugin.id.clone())
        .collect()
}

#[test]
fn midi_effects_go_before_the_instrument_and_only_where_notes_are() {
    let mut s = Session::new(Project::new("Song", 48_000), None, EngineConfig::default()).unwrap();
    let keys = s.add_track(TrackKind::Instrument).unwrap();
    assert!(insert(&mut s, keys, 0, builtin::SYNTH));
    assert!(insert(&mut s, keys, 1, builtin::REVERB));
    assert!(
        s.available_plugins()
            .iter()
            .any(|p| p.midi_effect && p.plugin.id == builtin::ARPEGGIATOR)
    );
    assert!(s.is_midi_effect(&PluginRef::builtin(builtin::CHORD, "Chord")));
    // Asked for the end of the chain: placed right before the synth.
    assert!(insert(&mut s, keys, 2, builtin::ARPEGGIATOR));
    assert!(insert(&mut s, keys, 9, builtin::SCALE));
    assert_eq!(
        chain(&s, keys),
        [
            builtin::ARPEGGIATOR,
            builtin::SCALE,
            builtin::SYNTH,
            builtin::REVERB
        ]
    );
    // MIDI tracks take MIDI effects only; audio tracks none.
    let notes = s.add_track(TrackKind::Midi).unwrap();
    assert!(insert(&mut s, notes, 0, builtin::NOTE_ECHO));
    assert!(!insert(&mut s, notes, 1, builtin::COMPRESSOR));
    assert_eq!(chain(&s, notes), [builtin::NOTE_ECHO]);
    let audio = s.add_track(TrackKind::Audio).unwrap();
    assert!(!insert(&mut s, audio, 0, builtin::ARPEGGIATOR));
    assert!(chain(&s, audio).is_empty());
}
