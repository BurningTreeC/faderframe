//! Two MIDI 2.0 clients of CoreMIDI in one process (macOS only; runs on
//! the macOS CI runners): one lists the other's destination as a MIDI 2.0
//! port and writes to it, bit for bit.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)]

use faderframe_midi::ump::{Attribute, Message, Ump, Voice2, packets};
use faderframe_midi_ump::UmpClient;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

#[test]
fn a_midi2_destination_is_listed_and_written_to() {
    let us = UmpClient::open("FaderFrame UMP Test").expect("CoreMIDI opens a MIDI 2.0 client");
    let peer = UmpClient::open("FaderFrame UMP Peer").unwrap();
    let (tx, rx) = channel();
    peer.start_input(move |from, words| {
        let _ = tx.send((from, words.to_vec()));
    })
    .unwrap();
    let name = peer.input_name().unwrap();
    // The peer's destination, once CoreMIDI lists it.
    let start = Instant::now();
    let port = loop {
        if let Some(p) = us
            .ports()
            .into_iter()
            .find(|p| p.writable && p.client_name == name)
        {
            break p;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{name} is not listed: {:?}",
            us.ports()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(port.endpoint && !port.readable);
    assert!(!port.hardware, "a program's destination");
    assert!(us.claimed().contains(&name), "midir leaves it to MIDI 2.0");
    assert_eq!(
        us.ports()
            .into_iter()
            .find(|p| p.client_name == name)
            .map(|p| p.address()),
        Some(port.address()),
        "the address stays"
    );
    // Our own destination is not among the ports.
    let ours = us.input_name().unwrap();
    assert!(us.ports().iter().all(|p| p.client_name != ours));
    let on = Message::Midi2 {
        group: 0,
        channel: 3,
        voice: Voice2::NoteOn {
            note: 60,
            velocity: 0x1234,
            attribute: Attribute { kind: 1, data: 7 },
        },
    }
    .to_ump();
    let cc = Message::Midi2 {
        group: 0,
        channel: 3,
        voice: Voice2::ControlChange {
            index: 1,
            value: 0x8765_4321,
        },
    }
    .to_ump();
    let mut w = us.writer(port.address());
    let words: Vec<u32> = [on, cc].iter().flat_map(|p| p.words().to_vec()).collect();
    assert!(w.write(&words));
    let mut got: Vec<Ump> = Vec::new();
    while got.len() < 2 {
        let (from, words) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(from, peer.input_address(), "sent straight to the peer");
        got.extend(packets(&words));
    }
    assert_eq!(got, [on, cc], "bit for bit, full resolution");
    // A writer to nothing says so.
    assert!(!us.writer((250, 1)).write(on.words()));
}
