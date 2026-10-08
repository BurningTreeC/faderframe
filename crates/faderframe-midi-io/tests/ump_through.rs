//! MIDI 2.0 ports through the real ALSA sequencer (opt-in: `-- --ignored`;
//! needs `/dev/snd/seq` and alsa-lib 1.2.10). Another MIDI 2.0 client
//! stands in for a device: FaderFrame lists it as an input and an output,
//! reads its packets (per-note controllers as expressions) and plays it in
//! MIDI 2.0.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use faderframe_midi::ump::{Attribute, Message, Voice2, of_bend, packets};
use faderframe_midi::{
    ExpressionValue, MidiEvent, MidiOutputEvent, NoteExpressionKind, midi_input_queue,
};
use faderframe_midi_io::{MidiHub, MidiOutputs, UmpClient};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs the ALSA sequencer"]
fn a_midi2_device_is_read_and_played() {
    let ours = Arc::new(UmpClient::open("FaderFrame MIDI 2.0 Test").unwrap());
    let device = UmpClient::open("Test Synth 2").unwrap();
    // Input: the device's output port is listed and connected.
    let (sender, mut queue, _feed) = midi_input_queue(256);
    let mut hub = MidiHub::new(sender);
    hub.start_ump(Arc::clone(&ours));
    hub.refresh();
    let input = hub
        .ports()
        .into_iter()
        .find(|p| p.name.starts_with("Test Synth 2") && p.connected)
        .unwrap_or_else(|| panic!("listed: {:?}", hub.ports()));
    // The device plays a note with a per-note bend (to whoever listens:
    // straight to our input here, from its output port).
    let mut w = device.writer(ours.input_address());
    let on = Message::Midi2 {
        group: 0,
        channel: 1,
        voice: Voice2::NoteOn {
            note: 64,
            velocity: 0xFFFF,
            attribute: Attribute::default(),
        },
    }
    .to_ump();
    let bend = Message::Midi2 {
        group: 0,
        channel: 1,
        voice: Voice2::PerNotePitchBend {
            note: 64,
            value: of_bend(1.0 / 48.0),
        },
    }
    .to_ump();
    assert!(w.write(on.words()) && w.write(bend.words()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = Vec::new();
    while got.len() < 2 && Instant::now() < deadline {
        while let Ok(ev) = queue.consumer.pop() {
            got.push(ev);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got.iter().all(|e| e.port == input.index),
        "from the device's port"
    );
    assert_eq!(
        got[0].event(),
        Some(MidiEvent::NoteOn {
            channel: 1,
            key: 64,
            velocity: 127
        })
    );
    let Some(MidiEvent::NoteExpression {
        kind: NoteExpressionKind::Tuning,
        value,
        key: 64,
        ..
    }) = got[1].event()
    else {
        panic!("{:?}", got[1]);
    };
    assert!((value.get() - 1.0).abs() < 1e-5);

    // Output: the device's input port is an output, played in MIDI 2.0.
    let mut outs = MidiOutputs::new(faderframe_midi::MidiClock::new());
    outs.set_ump(Some(Arc::clone(&ours)));
    let out = outs
        .ports()
        .into_iter()
        .find(|p| p.name.starts_with("Test Synth 2") && p.connected)
        .unwrap_or_else(|| panic!("listed: {:?}", outs.ports()));
    let (tx, rx) = channel();
    device
        .start_input(move |_, words| {
            let _ = tx.send(words.to_vec());
        })
        .unwrap();
    let mut q = outs.renew_queue();
    let now = q.clock.now_ns();
    q.producer
        .push(MidiOutputEvent::new(out.index, now, &[0x93, 67, 100]).unwrap())
        .unwrap();
    q.producer
        .push(MidiOutputEvent::expression(
            out.index,
            now,
            3,
            67,
            NoteExpressionKind::Brightness,
            ExpressionValue::new(0.25),
        ))
        .unwrap();
    let mut received = Vec::new();
    while received.len() < 2 {
        let words = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        received.extend(packets(&words).map(|p| Message::parse(&p)));
    }
    assert!(matches!(
        received[0],
        Message::Midi2 {
            channel: 3,
            voice: Voice2::NoteOn { note: 67, .. },
            ..
        }
    ));
    assert!(matches!(
        received[1],
        Message::Midi2 {
            channel: 3,
            voice: Voice2::RegisteredPerNote {
                note: 67,
                index: 74,
                ..
            },
            ..
        }
    ));
}
