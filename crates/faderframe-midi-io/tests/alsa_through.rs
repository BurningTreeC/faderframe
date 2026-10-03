#![allow(clippy::unwrap_used)]
//! Real sequencer round trip (opt-in, needs ALSA's "Midi Through" port and
//! `aseqsend`): `cargo test -p faderframe-midi-io -- --ignored`.

use faderframe_midi::{MidiEvent, midi_input_queue};
use faderframe_midi_io::MidiHub;
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs an ALSA sequencer with Midi Through and aseqsend"]
fn messages_sent_to_midi_through_arrive() {
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
