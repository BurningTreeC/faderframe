//! Two MIDI 2.0 clients of the real ALSA sequencer (needs `/dev/snd/seq`
//! and alsa-lib 1.2.10: opt-in, `-- --ignored`).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use faderframe_midi::ump::{Attribute, Message, Ump, Voice2, packets};
use faderframe_midi_ump::UmpClient;
use std::sync::mpsc::channel;
use std::time::Duration;

#[test]
#[ignore = "needs the ALSA sequencer"]
fn packets_go_from_one_client_to_another() {
    let us = UmpClient::open("FaderFrame UMP Test").unwrap();
    let peer = UmpClient::open("FaderFrame UMP Peer").unwrap();
    // We see the peer's ports (it is a MIDI 2.0 client).
    let theirs: Vec<_> = us
        .ports()
        .into_iter()
        .filter(|p| p.client == peer.client_id())
        .collect();
    assert!(theirs.iter().any(|p| p.readable), "{theirs:?}");
    assert!(theirs.iter().any(|p| p.writable), "{theirs:?}");
    assert!(us.ump_clients().contains(&peer.client_id()));
    let (tx, rx) = channel();
    us.start_input(move |from, words| {
        let _ = tx.send((from, words.to_vec()));
    })
    .unwrap();
    // The peer writes a MIDI 2.0 note-on with a 16-bit velocity and a
    // per-note pitch bend straight to our input.
    let on = Message::Midi2 {
        group: 0,
        channel: 3,
        voice: Voice2::NoteOn {
            note: 60,
            velocity: 0x1234,
            attribute: Attribute::default(),
        },
    }
    .to_ump();
    let bend = Message::Midi2 {
        group: 0,
        channel: 3,
        voice: Voice2::PerNotePitchBend {
            note: 60,
            value: 0x9000_0000,
        },
    }
    .to_ump();
    let mut w = peer.writer(us.input_address());
    let words: Vec<u32> = [on, bend].iter().flat_map(|p| p.words().to_vec()).collect();
    assert!(w.write(&words));
    let mut got: Vec<Ump> = Vec::new();
    while got.len() < 2 {
        let (from, words) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(from.0, peer.client_id());
        got.extend(packets(&words));
    }
    assert_eq!(got, [on, bend], "bit for bit, full resolution");
    // And our output reaches the peer.
    let (tx, rx) = channel();
    peer.start_input(move |_, words| {
        let _ = tx.send(words.to_vec());
    })
    .unwrap();
    let mut out = us.writer(peer.input_address());
    assert!(out.write(on.words()));
    let words = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(words, on.words());
}
