//! Singing into MIDI: an instrument track taking MIDI from a voice port
//! plays and records the notes its audio input sings (the dummy device's
//! input carries a steady A3).
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, InputRouting, PluginRef, Project, TrackKind};
use faderframe_session::voice::voice_port_key;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

fn singer() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Voice", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef::builtin(builtin::SYNTH, "Synth")),
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_tone(220.0))],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, Duration::from_millis(100));
    // The device's inputs have voice ports.
    let key = voice_port_key(0);
    assert!(
        s.midi_ports().iter().any(|p| p.key == key),
        "{:?}",
        s.midi_ports()
            .iter()
            .map(|p| p.key.clone())
            .collect::<Vec<_>>()
    );
    (s, t)
}

/// The track takes MIDI from input 1's voice port (or from every port).
fn sing_into(s: &mut Session, t: TrackId, voice: bool) {
    s.dispatch(Action::Edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Midi {
            port: voice.then(|| voice_port_key(0)),
            channel: None,
        },
    }))
    .unwrap();
}

#[test]
fn a_sung_note_plays_and_records_on_the_instrument() {
    let (mut s, t) = singer();
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: t,
        on: true,
    }))
    .unwrap();
    assert_eq!(s.voice_listening(), None, "no track takes the voice yet");
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(100));
    // The steady tone starts its note as soon as the track listens.
    sing_into(&mut s, t, true);
    run(&mut s, Duration::from_millis(400));
    assert_eq!(s.voice_listening(), Some(0), "listening to input 1");
    assert!(
        s.live_midi_notes(t).iter().any(|n| n.key == 57),
        "A3 sounds: {:?}",
        s.live_midi_notes(t)
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let clips: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    assert_eq!(clips.len(), 1, "{clips:?}");
    let ClipContent::Midi(m) = &clips[0].content else {
        panic!("a MIDI clip");
    };
    assert!(!m.notes.is_empty());
    assert!(m.notes.iter().all(|n| n.key == 57), "{:?}", m.notes);
    // Taking the port away stops the listening.
    sing_into(&mut s, t, false);
    run(&mut s, Duration::from_millis(50));
    assert_eq!(s.voice_listening(), None);
    s.stop_audio();
}
