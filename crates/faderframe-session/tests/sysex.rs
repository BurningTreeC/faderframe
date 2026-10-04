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
fn stopping_before_it_cancels_scheduled_sysex() {
    let (mut s, captured, _) = setup();
    audio(&mut s);
    // Start 450 ms before the message, stop 50 ms before it (by the
    // transport, not by sleeping, which overshoots on busy machines): it
    // was already scheduled (100 ms ahead) but must not go out.
    let start = MusicalTime::from_quarters(1.0 - 0.9);
    s.dispatch(Action::Transport(TransportAction::Locate(start)))
        .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let stop_at = (0.1 * 0.5 * 48_000.0 + 0.4 * 48_000.0) as i64;
    let begun = std::time::Instant::now();
    while s.transport().position < stop_at && begun.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
        s.tick(0.002);
    }
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
