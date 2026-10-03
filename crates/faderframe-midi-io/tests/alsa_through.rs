#![allow(clippy::unwrap_used)]
//! Real sequencer round trip (opt-in, needs ALSA's "Midi Through" port and
//! `aseqsend`): `cargo test -p faderframe-midi-io -- --ignored`.

use faderframe_midi::{MidiEvent, midi_input_queue};
use faderframe_midi_io::MidiHub;
use std::time::{Duration, Instant};

/// The tests share the system's Midi Through port: one at a time.
static PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
#[ignore = "needs an ALSA sequencer with Midi Through and aseqsend"]
fn messages_sent_to_midi_through_arrive() {
    let _one_at_a_time = PORT.lock().unwrap_or_else(|e| e.into_inner());
    let (tx, mut rx, _feed) = midi_input_queue(64);
    let mut hub = MidiHub::new(tx);
    hub.start_system();
    let through = hub
        .ports()
        .into_iter()
        .find(|p| p.key.starts_with("Midi Through"))
        .expect("Midi Through port");
    assert!(through.connected);
    let status = std::process::Command::new("aseqsend")
        .args(["-p", "Midi Through", "90", "3C", "64", "80", "3C", "00"])
        .status()
        .unwrap();
    assert!(status.success());
    let start = Instant::now();
    let mut got = Vec::new();
    while got.len() < 2 && start.elapsed() < Duration::from_secs(2) {
        while let Ok(ev) = rx.consumer.pop() {
            assert_eq!(ev.port, through.index);
            got.push(ev.event().unwrap());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        got,
        vec![
            MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100
            },
            MidiEvent::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0
            }
        ]
    );
}

#[test]
#[ignore = "needs an ALSA sequencer with Midi Through"]
fn output_to_midi_through_comes_back_in() {
    let _one_at_a_time = PORT.lock().unwrap_or_else(|e| e.into_inner());
    use faderframe_midi::MidiOutputEvent;
    use faderframe_midi_io::MidiOutputs;
    let (tx, mut rx, _feed) = midi_input_queue(64);
    let clock = tx.clock();
    let mut hub = MidiHub::new(tx);
    hub.start_system();
    let mut outs = MidiOutputs::new(clock);
    outs.start_system();
    let port = outs
        .ports()
        .into_iter()
        .find(|p| p.key.starts_with("Midi Through"))
        .expect("Midi Through output");
    assert!(port.connected);
    let mut q = outs.renew_queue();
    let due = clock.now_ns() + 20_000_000;
    q.producer
        .push(MidiOutputEvent::new(port.index, due, &[0x91, 72, 99]).unwrap())
        .unwrap();
    let start = Instant::now();
    let mut got = None;
    while got.is_none() && start.elapsed() < Duration::from_secs(2) {
        // (Other tests may play through the same port: wait for ours.)
        while let Ok(ev) = rx.consumer.pop() {
            if matches!(ev.event(), Some(MidiEvent::NoteOn { key: 72, .. })) {
                got = Some(ev);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let ev = got.expect("echoed back");
    assert_eq!(
        ev.event(),
        Some(MidiEvent::NoteOn {
            channel: 1,
            key: 72,
            velocity: 99
        })
    );
    assert!(ev.time_ns + 1_000_000 >= due, "not sent early");
}
