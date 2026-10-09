//! Singing into MIDI: an instrument track taking MIDI from a voice port
//! plays and records the notes its audio input sings (the dummy device's
//! input carries a steady A3).
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, InputRouting, PluginRef, Project, TrackKind};
use faderframe_session::voice::{voice_all_port_key, voice_port_key};
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
    assert!(
        s.voice_listening().is_empty(),
        "no track takes the voice yet"
    );
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(100));
    // The steady tone starts its note as soon as the track listens.
    sing_into(&mut s, t, true);
    run(&mut s, Duration::from_millis(400));
    assert_eq!(s.voice_listening(), [0], "listening to input 1");
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
    assert!(s.voice_listening().is_empty());
    s.stop_audio();
}

#[test]
fn changing_polyphonic_mode_releases_and_restarts_the_listener() {
    let (mut s, t) = singer();
    sing_into(&mut s, t, true);
    run(&mut s, Duration::from_millis(200));
    assert_eq!(s.held_midi_keys(), 1u128 << 57);

    let mut settings = s.voice_settings();
    settings.polyphonic = true;
    s.set_voice_settings(settings);
    run(&mut s, Duration::from_millis(250));
    assert_eq!(s.voice_listening(), [0]);
    // The dummy's pure sine has no harmonic evidence for chord mode.
    // Its old mono note must still release, including the capture feed.
    assert_eq!(s.held_midi_keys(), 0, "the retired feed releases A3");

    settings.polyphonic = false;
    s.set_voice_settings(settings);
    run(&mut s, Duration::from_millis(150));
    assert_eq!(s.held_midi_keys(), 1u128 << 57);
    s.stop_audio();
}

/// A track per port, recording, on a device whose input n sings A3 +
/// 4n semitones (A3, C♯4, F4, A4, …) from a second in, after the
/// recording began.
fn choir(ports: &[String]) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new("Choir", 48_000), None, EngineConfig::default()).unwrap();
    let mut ts = Vec::new();
    for port in ports {
        let t = s.add_track(TrackKind::Instrument).unwrap();
        s.dispatch(Action::SetInstrumentPlugin {
            track: t,
            plugin: Some(PluginRef::builtin(builtin::SYNTH, "Synth")),
        })
        .unwrap();
        s.dispatch(Action::Edit(Command::SetTrackRecordArm {
            track: t,
            on: true,
        }))
        .unwrap();
        take_from(&mut s, t, port.clone());
        ts.push(t);
    }
    s.start_audio(
        vec![Box::new(DummyBackend::with_input_spread(220.0, 4.0))],
        &AudioPreferences::default(),
    )
    .unwrap();
    run(&mut s, Duration::from_millis(100));
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    // The voices begin.
    run(&mut s, Duration::from_millis(1300));
    (s, ts)
}

fn take_from(s: &mut Session, t: TrackId, port: String) {
    s.dispatch(Action::Edit(Command::SetTrackInput {
        track: t,
        input: InputRouting::Midi {
            port: Some(port),
            channel: None,
        },
    }))
    .unwrap();
}

/// The notes of a track's recorded clip: (key, channel).
fn recorded(s: &Session, t: TrackId) -> Vec<(u8, u8)> {
    let clips: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    assert_eq!(clips.len(), 1, "{clips:?}");
    let ClipContent::Midi(m) = &clips[0].content else {
        panic!("a MIDI clip");
    };
    let mut notes: Vec<(u8, u8)> = m.notes.iter().map(|n| (n.key, n.channel)).collect();
    notes.sort_unstable();
    notes.dedup();
    notes
}

#[test]
fn two_inputs_sing_into_two_tracks_at_once() {
    let (mut s, ts) = choir(&[voice_port_key(0), voice_port_key(1)]);
    assert_eq!(s.voice_listening(), [0, 1]);
    assert_eq!(
        s.held_midi_keys(),
        1u128 << 57 | 1u128 << 61,
        "A3 and C♯4 sound together"
    );
    // One stops listening; the other sings on.
    take_from(&mut s, ts[1], "virtual:FaderFrame Keyboard".into());
    run(&mut s, Duration::from_millis(150));
    assert_eq!(s.voice_listening(), [0]);
    assert_eq!(s.held_midi_keys(), 1u128 << 57, "C♯4 ended");
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    // Each track recorded its own input.
    assert_eq!(recorded(&s, ts[0]), [(57, 0)]);
    assert_eq!(recorded(&s, ts[1]), [(61, 0)]);
    s.stop_audio();
}

#[test]
fn the_all_inputs_port_puts_each_input_on_its_own_channel() {
    let (mut s, ts) = choir(&[voice_all_port_key()]);
    // The dummy device has eight inputs.
    assert_eq!(s.voice_listening(), (0..8).collect::<Vec<u16>>());
    let keys: Vec<u8> = (0..8).map(|n| 57 + 4 * n).collect();
    assert_eq!(
        s.held_midi_keys(),
        keys.iter().fold(0u128, |m, k| m | 1u128 << k),
        "every input sounds"
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    // Input n on MIDI channel n + 1 (0-based: n + 1, the first left free).
    let want: Vec<(u8, u8)> = keys.iter().zip(1u8..).map(|(k, c)| (*k, c)).collect();
    assert_eq!(recorded(&s, ts[0]), want);
    // Taking the port away ends every note.
    take_from(&mut s, ts[0], voice_port_key(0));
    run(&mut s, Duration::from_millis(150));
    assert_eq!(s.voice_listening(), [0]);
    assert_eq!(s.held_midi_keys(), 1u128 << 57);
    s.stop_audio();
}
