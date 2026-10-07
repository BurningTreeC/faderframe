//! SysEx: played to external devices on time, cancelled on stop, recorded
//! from inputs.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::ClipId;
use faderframe_engine::EngineConfig;
use faderframe_project::{ClipContent, Command, MidiOutputRouting, Project, SysexEvent, TrackKind};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

fn run(s: &mut Session, d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::thread::sleep(Duration::from_millis(3));
        s.tick(0.003);
    }
}

fn audio(s: &mut Session) {
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            threads: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    run(s, Duration::from_millis(80));
}

const DUMP: [u8; 6] = [0xF0, 0x43, 0x10, 0x4C, 0x00, 0xF7];

/// A MIDI track playing to a virtual output, with a clip carrying `DUMP`
/// half a second in (120 BPM: one beat).
fn setup() -> (Session, faderframe_midi_io::Captured, ClipId) {
    let mut s = Session::new(Project::new("SysEx", 48_000), None, EngineConfig::default()).unwrap();
    let captured = s.add_virtual_midi_output("Synth");
    let t = s.add_track(TrackKind::Midi).unwrap();
    s.dispatch(Action::Edit(Command::SetTrackMidiOutput {
        track: t,
        output: Some(MidiOutputRouting {
            port: "virtual:Synth".into(),
            channel: None,
        }),
    }))
    .unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters_i(8),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let messages = SysexEvent::split_messages(&[&DUMP[..], &[0xF0, 1, 2, 0xF7]].concat());
    assert_eq!(messages.len(), 2);
    s.dispatch(Action::AddSysex {
        clip,
        at: MusicalTime::from_quarters_i(1),
        messages: vec![messages[0].clone()],
    })
    .unwrap();
    assert_eq!(s.history().undo_label(), Some("Add SysEx"));
    (s, captured, clip)
}

fn sent(c: &faderframe_midi_io::Captured) -> Vec<(u64, Vec<u8>)> {
    c.lock()
        .unwrap()
        .iter()
        .filter(|(_, b)| b.first() == Some(&0xF0))
        .cloned()
        .collect()
}

/// Run until `n` SysEx messages went out (slow CI machines lag behind the
/// wall clock), at most `limit`.
fn until_sent(
    s: &mut Session,
    c: &faderframe_midi_io::Captured,
    n: usize,
    limit: Duration,
) -> Vec<(u64, Vec<u8>)> {
    let start = Instant::now();
    while sent(c).len() < n && start.elapsed() < limit {
        run(s, Duration::from_millis(20));
    }
    sent(c)
}

#[test]
fn clip_sysex_goes_out_when_its_time_comes() {
    let (mut s, captured, _) = setup();
    audio(&mut s);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert!(sent(&captured).is_empty(), "not before its beat");
    let got = until_sent(&mut s, &captured, 1, Duration::from_secs(3));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].1, DUMP.to_vec());
    // Locate back: it plays again.
    s.dispatch(Action::Transport(TransportAction::Locate(
        MusicalTime::ZERO,
    )))
    .unwrap();
    let again = until_sent(&mut s, &captured, 2, Duration::from_secs(3));
    assert_eq!(again.len(), 2, "again after the locate");
    s.stop_audio();
}

#[test]
fn looped_sysex_goes_out_once_per_pass() {
    let (mut s, captured, _) = setup();
    // A two-second loop; the message is half a second in.
    s.dispatch(Action::Transport(TransportAction::SetLoop(
        faderframe_project::MusicalRange::new(MusicalTime::ZERO, MusicalTime::from_quarters_i(4)),
    )))
    .unwrap();
    assert!(s.project().loop_enabled);
    audio(&mut s);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let got = until_sent(&mut s, &captured, 3, Duration::from_secs(12));
    s.stop_audio();
    assert!(got.len() >= 3, "{got:?}");
    // Never twice in a pass: a pass sent again comes at once (a clock
    // wobble taken for a jump re-sent within milliseconds). The dummy
    // device catches up after oversleeping (1.5 s of audio at once on a
    // busy CI Mac), which brings two passes closer on the wall clock -- but
    // not to within a quarter of a second.
    for pair in got.windows(2) {
        let apart = pair[1].0.saturating_sub(pair[0].0);
        assert!(
            apart > 250_000_000,
            "sent {} ms apart: {got:?}",
            apart / 1_000_000
        );
    }
}

#[test]
fn stopping_before_it_cancels_scheduled_sysex() {
    let (mut s, captured, _) = setup();
    audio(&mut s);
    // Start 450 ms before the message and stop as soon as it is scheduled
    // (100 ms ahead of its time — waiting on that rather than the clock
    // keeps busy machines from overshooting): it must not go out.
    let start = MusicalTime::from_quarters(1.0 - 0.9);
    s.dispatch(Action::Transport(TransportAction::Locate(start)))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let before = s.sysex_scheduled();
    let begun = std::time::Instant::now();
    while s.sysex_scheduled() == before && begun.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(1));
        s.tick(0.001);
    }
    assert!(s.sysex_scheduled() > before, "it was scheduled");
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    run(&mut s, Duration::from_millis(300));
    assert!(sent(&captured).is_empty(), "{:?}", sent(&captured));
    s.stop_audio();
}

#[test]
fn sysex_from_the_input_is_recorded_into_the_take() {
    let mut s = Session::new(Project::new("Rec", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Midi).unwrap();
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
    run(&mut s, Duration::from_millis(200));
    s.midi_keyboard().send(&DUMP);
    run(&mut s, Duration::from_millis(100));
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    s.wait_for_recordings();
    let clip = s.project().clips_of(t)[0].clone();
    let ClipContent::Midi(m) = &clip.content else {
        panic!("a MIDI take")
    };
    assert_eq!(m.sysex.len(), 1);
    assert_eq!(m.sysex[0].data, DUMP.to_vec());
    s.stop_audio();
}
