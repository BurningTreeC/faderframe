#![allow(clippy::unwrap_used)]
//! MIDI keyboards and controllers in the session, driven through the
//! virtual keyboard input: live play, the live rule, MIDI learn, mapped
//! controls, transport triggers and recording.

use faderframe_audio::dummy::DummyBackend;
use faderframe_automation::AutomationTarget;
use faderframe_core::{TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_project::{
    ClipContent, Command, MappingTarget, MidiControl, MonitorMode, PluginRef, Project, TrackKind,
    TransportControl,
};
use faderframe_session::{
    Action, AudioPreferences, RecordMode, SelectMode, Session, TransportAction,
};
use std::time::{Duration, Instant};

fn session_with_synth() -> (Session, TrackId) {
    let mut s = Session::new(Project::new("Midi", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef::builtin(builtin::SYNTH, "Synth")),
    })
    .unwrap();
    (s, t)
}

fn start_audio(s: &mut Session) {
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

fn select(s: &mut Session, t: TrackId) {
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })
    .unwrap();
}

#[test]
fn the_selected_or_armed_instrument_plays_live() {
    let (mut s, a) = session_with_synth();
    let b = s.add_track(TrackKind::Instrument).unwrap();
    select(&mut s, a);
    s.tick(0.0);
    assert!(s.midi_live_tracks().contains(&a));
    assert!(!s.midi_live_tracks().contains(&b));
    // An armed track wins over the selection.
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: b,
        on: true,
    }))
    .unwrap();
    s.tick(0.0);
    assert!(s.midi_live_tracks().contains(&b) && !s.midi_live_tracks().contains(&a));
    // Monitoring off: never; Input: always.
    s.dispatch(Action::Edit(Command::SetTrackMonitor {
        track: b,
        mode: MonitorMode::Off,
    }))
    .unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMonitor {
        track: a,
        mode: MonitorMode::Input,
    }))
    .unwrap();
    s.tick(0.0);
    assert!(s.midi_live_tracks().contains(&a) && !s.midi_live_tracks().contains(&b));
}

#[test]
fn keyboard_notes_sound_on_the_live_instrument() {
    let (mut s, t) = session_with_synth();
    select(&mut s, t);
    start_audio(&mut s);
    run(&mut s, Duration::from_millis(100));
    assert!(level(&s, t) <= -70.0, "silent: {}", level(&s, t));
    s.midi_keyboard().send(&[0x90, 60, 110]);
    run(&mut s, Duration::from_millis(250));
    assert!(level(&s, t) > -40.0, "playing: {}", level(&s, t));
    assert!(s.midi_ports().iter().any(|p| p.is_virtual));
    s.midi_keyboard().send(&[0x80, 60, 0]);
    s.stop_audio();
}

#[test]
fn midi_learn_maps_a_controller_and_moves_are_single_undo_steps() {
    let (mut s, t) = session_with_synth();
    let target = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackVolume,
    };
    s.dispatch(Action::MidiLearn(target)).unwrap();
    assert_eq!(s.midi_learning(), Some(target));
    // A note does not map a continuous parameter; a CC does.
    s.midi_keyboard().send(&[0x90, 40, 100]);
    s.tick(0.0);
    assert_eq!(s.midi_learning(), Some(target));
    s.midi_keyboard().send(&[0xB0, 7, 64]);
    s.tick(0.0);
    assert_eq!(s.midi_learning(), None);
    let m = s.midi_mappings_for(target);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].source.control, MidiControl::Cc { number: 7 });
    assert_eq!(m[0].source.channel, 0);
    let before = s.project().track(t).unwrap().volume_db;

    // Turning the knob: the fader follows …
    for v in [80u8, 100, 127] {
        s.midi_keyboard().send(&[0xB0, 7, v]);
        s.tick(0.0);
    }
    let top = s.project().track(t).unwrap().volume_db;
    assert!(top > before, "{top} > {before}");
    s.midi_keyboard().send(&[0xB0, 7, 0]);
    s.tick(0.0);
    assert!(s.project().track(t).unwrap().volume_db < -60.0);
    // … and once it rests, one undo step restores the start.
    run(&mut s, Duration::from_millis(500));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(t).unwrap().volume_db, before);
    // Undo once more: the mapping itself.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.midi_mappings_for(target).is_empty());
}

#[test]
fn pads_toggle_switches_and_trigger_transport() {
    let (mut s, t) = session_with_synth();
    start_audio(&mut s);
    let mute = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackMute,
    };
    s.dispatch(Action::MidiLearn(mute)).unwrap();
    s.midi_keyboard().send(&[0x99, 36, 100]); // pad, channel 10
    s.tick(0.0);
    let play = MappingTarget::Transport {
        control: TransportControl::PlayStop,
    };
    s.dispatch(Action::MidiLearn(play)).unwrap();
    s.midi_keyboard().send(&[0x99, 37, 100]);
    s.tick(0.0);
    assert_eq!(s.project().midi_mappings.len(), 2);

    s.midi_keyboard().send(&[0x99, 36, 90]);
    s.tick(0.0);
    assert!(s.project().track(t).unwrap().mute);
    s.midi_keyboard().send(&[0x89, 36, 0]); // release: nothing
    s.midi_keyboard().send(&[0x99, 36, 90]);
    s.tick(0.0);
    assert!(!s.project().track(t).unwrap().mute);

    s.midi_keyboard().send(&[0x99, 37, 100]);
    let start = Instant::now();
    while !s.transport().playing && start.elapsed() < Duration::from_secs(3) {
        run(&mut s, Duration::from_millis(10));
    }
    assert!(s.transport().playing, "pad started playback");
    s.stop_audio();
}

#[test]
fn recording_an_instrument_track_makes_a_midi_clip() {
    let (mut s, t) = session_with_synth();
    start_audio(&mut s);
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: t,
        on: true,
    }))
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::ToggleRecord))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(200));
    for key in [60u8, 64, 67] {
        s.midi_keyboard().send(&[0x90, key, 100]);
        run(&mut s, Duration::from_millis(120));
        s.midi_keyboard().send(&[0x80, key, 0]);
        run(&mut s, Duration::from_millis(60));
    }
    // A held key at the end is closed on stop.
    s.midi_keyboard().send(&[0x90, 72, 80]);
    run(&mut s, Duration::from_millis(100));
    assert!(
        !s.live_midi_notes(t).is_empty(),
        "notes show while recording"
    );
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let clips: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    assert_eq!(clips.len(), 1, "{clips:?}");
    let ClipContent::Midi(m) = &clips[0].content else {
        panic!("a MIDI clip");
    };
    let keys: Vec<u8> = m.notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, vec![60, 64, 67, 72]);
    assert!(m.notes.windows(2).all(|w| w[0].start < w[1].start));
    assert!(m.notes.iter().all(|n| n.length.0 > 0 && n.velocity > 0));
    // Undo removes the take in one step.
    s.dispatch(Action::Undo).unwrap();
    assert!(s.project().clips_of(t).is_empty());
    s.stop_audio();
}

#[test]
fn replace_mode_overwrites_earlier_midi() {
    let (mut s, t) = session_with_synth();
    start_audio(&mut s);
    let mut settings = s.record;
    settings.mode = RecordMode::Replace;
    s.dispatch(Action::SetRecordSettings(settings)).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackRecordArm {
        track: t,
        on: true,
    }))
    .unwrap();
    for pass in 0..2u8 {
        s.dispatch(Action::Transport(TransportAction::ReturnToStart))
            .unwrap();
        s.dispatch(Action::Transport(TransportAction::ToggleRecord))
            .unwrap();
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        run(&mut s, Duration::from_millis(150));
        s.midi_keyboard().send(&[0x90, 50 + pass, 100]);
        run(&mut s, Duration::from_millis(100));
        s.midi_keyboard().send(&[0x80, 50 + pass, 0]);
        run(&mut s, Duration::from_millis(50));
        s.dispatch(Action::Transport(TransportAction::Stop))
            .unwrap();
        s.wait_for_recordings();
    }
    let clips: Vec<_> = s.project().clips_of(t).into_iter().cloned().collect();
    assert_eq!(clips.len(), 1, "the second take replaced the first");
    let ClipContent::Midi(m) = &clips[0].content else {
        panic!()
    };
    assert_eq!(m.notes.iter().map(|n| n.key).collect::<Vec<_>>(), vec![51]);
    s.stop_audio();
}

#[test]
fn mappings_are_saved_with_the_project() {
    let (mut s, t) = session_with_synth();
    let pan = MappingTarget::Parameter {
        track: t,
        target: AutomationTarget::TrackPan,
    };
    s.dispatch(Action::MidiLearn(pan)).unwrap();
    s.midi_keyboard().send(&[0xB2, 10, 0]);
    s.tick(0.0);
    let dir = std::env::temp_dir().join(format!("ff-midi-map-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Map.ffproj");
    s.save_as(&path).unwrap();
    let mut back = Session::new(Project::new("x", 48_000), None, EngineConfig::default()).unwrap();
    back.open(&path).unwrap();
    let m = back.midi_mappings_for(pan);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].source.channel, 2);
    assert_eq!(m[0].source.control, MidiControl::Cc { number: 10 });
    let _ = std::fs::remove_dir_all(&dir);
}
