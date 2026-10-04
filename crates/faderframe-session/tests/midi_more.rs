#![allow(clippy::unwrap_used)]
//! The rest of the MIDI work: auditioning, step input, encoder modes, soft
//! takeover, consumed controls, controller recording, external MIDI output
//! and clock.

use faderframe_audio::dummy::DummyBackend;
use faderframe_automation::AutomationTarget;
use faderframe_core::{ClipId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_midi::MidiEvent;
use faderframe_project::{
    ClipContent, Command, MappingMode, MappingTarget, MidiController, MidiOutputRouting, PluginRef,
    Project, TrackKind,
};
use faderframe_session::{
    Action, AudioPreferences, SelectMode, Session, StepInput, TransportAction,
};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn synth() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("M", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef::builtin(builtin::SYNTH, "Synth")),
    })
    .unwrap();
    (s, t)
}

fn audio(s: &mut Session) {
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
}

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(5));
        s.tick(0.005);
    }
}

fn level(s: &Session, t: TrackId) -> f32 {
    s.meter(t).left.level_db.max(s.meter(t).right.level_db)
}

fn midi_clip(s: &mut Session, t: TrackId) -> ClipId {
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters_i(8),
    })
    .unwrap();
    s.project().clips_of(t)[0].id
}

#[test]
fn auditioning_sounds_without_live_input() {
    let (mut s, t) = synth();
    // Nothing selected or armed: the track is not live.
    s.dispatch(Action::SelectTracks {
        tracks: vec![],
        mode: SelectMode::Replace,
    })
    .unwrap();
    audio(&mut s);
    run(&mut s, Duration::from_millis(80));
    assert!(!s.midi_live_tracks().contains(&t));
    s.dispatch(Action::Audition {
        track: t,
        key: 60,
        velocity: 110,
        channel: 0,
    })
    .unwrap();
    run(&mut s, Duration::from_millis(200));
    let playing = level(&s, t);
    assert!(playing > -40.0, "audition sounds: {playing}");
    s.dispatch(Action::AuditionOff).unwrap();
    // Released: the (smoothed) meter falls far below the playing level
    // (within seconds even where the device clock lags behind).
    let start = Instant::now();
    while level(&s, t) >= playing - 30.0 && start.elapsed() < Duration::from_secs(15) {
        run(&mut s, Duration::from_millis(100));
    }
    assert!(level(&s, t) < playing - 30.0, "and stops: {}", level(&s, t));
    s.stop_audio();
}

#[test]
fn step_input_enters_chords_and_advances() {
    let (mut s, t) = synth();
    let clip = midi_clip(&mut s, t);
    let step = MusicalTime::from_quarters(0.5);
    s.dispatch(Action::SetStepInput(Some(StepInput {
        clip,
        cursor: MusicalTime::ZERO,
        step,
    })))
    .unwrap();
    // A C major chord, keys overlapping.
    for k in [60u8, 64, 67] {
        s.midi_keyboard().send(&[0x90, k, 90]);
    }
    s.tick(0.0);
    for k in [60u8, 64, 67] {
        s.midi_keyboard().send(&[0x80, k, 0]);
    }
    s.tick(0.0);
    // Then a single note.
    s.midi_keyboard().send(&[0x90, 72, 80]);
    s.midi_keyboard().send(&[0x80, 72, 0]);
    s.tick(0.0);
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    let at = |q: f64| {
        let mut v: Vec<u8> = m
            .notes
            .iter()
            .filter(|n| n.start == MusicalTime::from_quarters(q))
            .map(|n| n.key)
            .collect();
        v.sort();
        v
    };
    assert_eq!(at(0.0), vec![60, 64, 67]);
    assert_eq!(at(0.5), vec![72]);
    assert!(m.notes.iter().all(|n| n.length == step));
    assert_eq!(
        s.step_input().unwrap().cursor,
        MusicalTime::from_quarters(1.0)
    );
    // One undo step per chord.
    s.dispatch(Action::Undo).unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap();
    assert_eq!(m.notes.len(), 3);
}

fn learn(s: &mut Session, target: MappingTarget, msg: &[u8]) {
    s.dispatch(Action::MidiLearn(target)).unwrap();
    s.midi_keyboard().send(msg);
    s.tick(0.0);
    assert!(s.midi_learning().is_none());
}

fn set_mode(s: &mut Session, target: MappingTarget, mode: MappingMode) {
    let mut m = s.midi_mappings_for(target)[0].clone();
    m.mode = mode;
    s.dispatch(Action::Edit(Command::UpdateMidiMapping { mapping: m }))
        .unwrap();
}

#[test]
fn relative_encoders_step_from_the_current_value() {
    let (mut s, t) = synth();
    let pan = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackPan,
    };
    learn(&mut s, pan, &[0xB0, 20, 1]);
    set_mode(&mut s, pan, MappingMode::RelativeTwosComplement);
    let before = s.project().track(t).unwrap().pan;
    for _ in 0..16 {
        s.midi_keyboard().send(&[0xB0, 20, 1]);
    }
    s.tick(0.0);
    let right = s.project().track(t).unwrap().pan;
    // 16 ticks of 1/128 of the range (-1..1): +0.25.
    assert!((right - before - 0.25).abs() < 0.02, "{before} → {right}");
    for _ in 0..32 {
        s.midi_keyboard().send(&[0xB0, 20, 127]);
    }
    s.tick(0.0);
    let left = s.project().track(t).unwrap().pan;
    assert!((left - (right - 0.5)).abs() < 0.02, "{right} → {left}");
}

#[test]
fn soft_takeover_waits_for_the_control_to_reach_the_parameter() {
    let (mut s, t) = synth();
    let vol = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackVolume,
    };
    learn(&mut s, vol, &[0xB0, 7, 0]);
    set_mode(&mut s, vol, MappingMode::Pickup);
    // The fader is at 0 dB (high up); the knob far below does nothing …
    s.dispatch(Action::Edit(Command::SetTrackVolume { track: t, db: 0.0 }))
        .unwrap();
    let param = s
        .automation_param(t, AutomationTarget::TrackVolume)
        .unwrap();
    let at = param.to_normal(0.0);
    s.midi_keyboard().send(&[0xB0, 7, 10]);
    s.tick(0.0);
    assert_eq!(
        s.project().track(t).unwrap().volume_db,
        0.0,
        "not picked up"
    );
    // … until it passes the fader's position; then it follows.
    let near = (at * 127.0).round() as u8;
    s.midi_keyboard().send(&[0xB0, 7, near]);
    s.tick(0.0);
    s.midi_keyboard().send(&[0xB0, 7, near.saturating_sub(20)]);
    s.tick(0.0);
    assert!(
        s.project().track(t).unwrap().volume_db < -3.0,
        "follows now"
    );
}

#[test]
fn mapped_controls_are_taken_from_instruments() {
    let (mut s, t) = synth();
    let mute = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackMute,
    };
    learn(&mut s, mute, &[0x99, 36, 100]);
    s.tick(0.0);
    let port = s.midi_ports().iter().position(|p| p.is_virtual).unwrap() as u16;
    let consumed = &s.engine().midi_shared().consumed;
    let pad = MidiEvent::NoteOn {
        channel: 9,
        key: 36,
        velocity: 100,
    };
    let other = MidiEvent::NoteOn {
        channel: 9,
        key: 37,
        velocity: 100,
    };
    assert!(consumed.contains(port, pad));
    assert!(!consumed.contains(port, other));
}

#[test]
fn controller_moves_are_recorded_into_lanes() {
    let (mut s, t) = synth();
    audio(&mut s);
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: t,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(150));
    s.midi_keyboard().send(&[0x90, 60, 100]);
    for v in [10u8, 40, 80, 120] {
        s.midi_keyboard().send(&[0xB0, 1, v]);
        run(&mut s, Duration::from_millis(40));
    }
    s.midi_keyboard().send(&[0xE0, 0x00, 0x60]); // pitch bend up
    run(&mut s, Duration::from_millis(40));
    s.midi_keyboard().send(&[0x80, 60, 0]);
    run(&mut s, Duration::from_millis(50));
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let clip = s.project().clips_of(t)[0].clone();
    let ClipContent::Midi(m) = &clip.content else {
        panic!()
    };
    let wheel = m
        .lane(MidiController::MOD_WHEEL, 0)
        .expect("mod wheel lane");
    let values: Vec<u16> = wheel.points.iter().map(|p| p.value).collect();
    assert_eq!(values, vec![10, 40, 80, 120]);
    assert!(wheel.points.windows(2).all(|w| w[0].time < w[1].time));
    let bend = m.lane(MidiController::PitchBend, 0).expect("bend lane");
    assert_eq!(bend.points[0].value, 0x60 << 7);
    s.stop_audio();
}

#[test]
fn midi_tracks_play_external_devices_and_send_clock() {
    let mut s = Session::new(Project::new("Ext", 48_000), None, EngineConfig::default()).unwrap();
    let captured = s.add_virtual_midi_output("Capture");
    let t = s.add_track(TrackKind::Midi).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMidiOutput {
        track: t,
        output: Some(MidiOutputRouting {
            port: "virtual:Capture".into(),
            channel: None,
        }),
    }))
    .unwrap();
    assert!(
        s.midi_output_choices(t)
            .iter()
            .any(|c| c.label == "No External MIDI Output")
    );
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })
    .unwrap();
    s.set_midi_clock_output("virtual:Capture", true);
    audio(&mut s);
    run(&mut s, Duration::from_millis(100));
    s.midi_keyboard().send(&[0x92, 48, 77]);
    run(&mut s, Duration::from_millis(200));
    let got = captured.lock().unwrap().clone();
    assert!(
        got.iter().any(|(_, b)| b == &vec![0x92, 48, 77]),
        "the live note went out: {got:?}"
    );
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(400));
    let got = captured.lock().unwrap().clone();
    assert!(got.iter().any(|(_, b)| b == &vec![0xFA]), "start");
    let pulses = got.iter().filter(|(_, b)| b == &vec![0xF8]).count();
    assert!(pulses >= 8, "clock pulses: {pulses}");
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, Duration::from_millis(200));
    assert!(
        captured
            .lock()
            .unwrap()
            .iter()
            .any(|(_, b)| b == &vec![0xFC]),
        "stop"
    );
    assert!(
        s.midi_preferences()
            .clock_outputs
            .contains(&"virtual:Capture".to_string())
    );
    s.stop_audio();
}
