//! Listen Along: the host's mix on a peer's speakers — for someone who
//! lacks media or plug-ins, or simply wants to hear what the host hears.
//!
//! **The host**, while any peer listens, has the engine write its master's
//! post-fader audio into the share ring
//! ([`faderframe_engine::EngineController::set_share_source`]); each tick
//! it takes what was written, codes it as Opus (stereo, 48 kHz, 20 ms
//! frames at [`STREAM_KBPS`]: `faderframe_codec::stream`) and sends each
//! packet, numbered, to the peers listening. A connection keeps a few
//! packets waiting at most: a slow link drops the oldest.
//!
//! **A peer** decodes the packets at its engine's rate (a missing number
//! is a lost packet, concealed by Opus) and writes them into the listening
//! node's ring ([`faderframe_engine::nodes::AlongLink`]): the node plays
//! them in place of the peer's own mix once [`PRIME_SECONDS`] wait, fading
//! between the two. Nothing of it reaches the peer's project, renders or
//! meters.

use super::Side;
use crate::{NoticeLevel, Session};
use faderframe_codec::stream::{STREAM_KBPS, StreamDecoder, StreamEncoder};
use faderframe_collab::{ToHost, ToPeer};
use faderframe_engine::nodes::AlongFeed;

/// Audio a peer waits for before playing the host's mix (jitter).
pub const PRIME_SECONDS: f64 = 0.15;
/// The ring between the control side and the audio thread.
const RING_SECONDS: f64 = 2.0;

#[derive(Default)]
pub(crate) struct ListenState {
    /// A peer listens.
    on: bool,
    decoder: Option<(u32, StreamDecoder)>,
    feed: Option<AlongFeed>,
    last_seq: Option<u64>,
    /// Packets received, and lost on the way.
    received: u64,
    lost: u64,
    left: Vec<f32>,
    right: Vec<f32>,
    /// The host: its coder, at a rate.
    encoder: Option<(u32, StreamEncoder)>,
    /// The host: where its share ring was read up to.
    pos: u64,
    next_seq: u64,
    packets: Vec<Vec<u8>>,
}

impl ListenState {
    pub fn listening(&self) -> bool {
        self.on
    }
}

/// How Listen Along is going on a peer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ListenStatus {
    pub on: bool,
    /// The host's mix is heard now.
    pub playing: bool,
    pub received: u64,
    pub lost: u64,
    /// Times the stream ran dry.
    pub underruns: u64,
    /// Seconds of it waiting to be heard.
    pub buffered: f64,
}

impl Session {
    /// Listen Along on this peer (`None`: not a peer).
    pub fn listen_along_status(&self) -> Option<ListenStatus> {
        if !matches!(self.collab.side, Some(Side::Peer(_))) {
            return None;
        }
        let l = &self.collab.listen;
        let shared = self.engine.listening().link.along().shared();
        let rate = f64::from(self.engine.sample_rate().max(1));
        Some(ListenStatus {
            on: l.on,
            playing: shared.playing(),
            received: l.received,
            lost: l.lost,
            underruns: shared.underruns(),
            // (The ring's fill as the control side sees it: the node's
            // count stands still while no device runs.)
            buffered: l.feed.as_ref().map_or(0, AlongFeed::waiting) as f64 / rate,
        })
    }

    pub(crate) fn set_listen_along(&mut self, on: bool) {
        let Some(Side::Peer(p)) = &self.collab.side else {
            return;
        };
        if on && !faderframe_codec::opus_available() {
            self.notify(
                NoticeLevel::Warning,
                "Listen Along needs the Opus codec, which this build lacks",
            );
            return;
        }
        if let Some(c) = &p.client {
            c.send(&ToHost::Listen(on));
        }
        let along = std::sync::Arc::clone(self.engine.listening().link.along());
        let l = &mut self.collab.listen;
        l.on = on;
        l.last_seq = None;
        if on {
            let rate = self.engine.sample_rate();
            along
                .shared()
                .set_prime((f64::from(rate) * PRIME_SECONDS) as u32);
            l.feed = Some(along.open((f64::from(rate) * RING_SECONDS) as usize));
            l.decoder = None;
        } else {
            l.feed = None;
            l.decoder = None;
        }
        along.shared().set_hear(on);
        self.revision += 1;
    }

    pub(crate) fn stop_listen_along(&mut self) {
        let along = self.engine.listening().link.along();
        along.shared().set_hear(false);
        let l = &mut self.collab.listen;
        l.on = false;
        l.feed = None;
        l.decoder = None;
        l.encoder = None;
        self.engine.set_share_source(None);
    }

    pub(crate) fn tick_listen_along(&mut self) {
        self.engine.listening().link.along().collect();
        if matches!(self.collab.side, Some(Side::Host(_))) {
            self.share_mix();
        }
    }

    /// The host: the master's new audio coded and sent to the listeners.
    fn share_mix(&mut self) {
        let listeners: Vec<faderframe_collab::ConnId> = match &self.collab.side {
            Some(Side::Host(h)) => h
                .guests
                .values()
                .filter(|g| g.listening)
                .filter_map(|g| g.conn)
                .filter(|c| h.conns.contains_key(c))
                .collect(),
            _ => return,
        };
        let master = self.project.master_id();
        if listeners.is_empty() || master.is_none() {
            if self.collab.listen.encoder.take().is_some() {
                self.engine.set_share_source(None);
            }
            return;
        }
        let share = std::sync::Arc::clone(self.engine.share());
        if share.source() != master.map(|m| m.raw()) {
            self.engine.set_share_source(master);
            self.collab.listen.pos = share.written();
        }
        let rate = self.engine.sample_rate();
        let l = &mut self.collab.listen;
        if l.encoder.as_ref().is_none_or(|(r, _)| *r != rate) {
            match StreamEncoder::new(rate, STREAM_KBPS) {
                Ok(e) => l.encoder = Some((rate, e)),
                Err(e) => {
                    tracing::warn!("collaboration: Listen Along: {e}");
                    return;
                }
            }
        }
        l.left.clear();
        l.right.clear();
        let (pos, _lost) = share.read_since(l.pos, &mut l.left, &mut l.right);
        l.pos = pos;
        if l.left.is_empty() {
            return;
        }
        l.packets.clear();
        if let Some((_, e)) = l.encoder.as_mut()
            && let Err(e) = e.push(&l.left, &l.right, &mut l.packets)
        {
            tracing::warn!("collaboration: Listen Along: {e}");
        }
        let packets = std::mem::take(&mut l.packets);
        let Some(Side::Host(h)) = &self.collab.side else {
            return;
        };
        for packet in packets {
            let seq = self.collab.listen.next_seq;
            self.collab.listen.next_seq += 1;
            if let Ok(m) = faderframe_collab::wire::encode(&ToPeer::Audio { seq, packet }) {
                for &c in &listeners {
                    h.host.send_audio(c, m.clone());
                }
            }
        }
    }

    /// A peer: a packet of the host's mix.
    pub(crate) fn listen_packet(&mut self, seq: u64, packet: &[u8]) {
        let rate = self.engine.sample_rate();
        let l = &mut self.collab.listen;
        if !l.on || l.feed.is_none() {
            return;
        }
        if l.decoder.as_ref().is_none_or(|(r, _)| *r != rate) {
            match StreamDecoder::new(rate) {
                Ok(d) => l.decoder = Some((rate, d)),
                Err(e) => {
                    tracing::warn!("collaboration: Listen Along: {e}");
                    return;
                }
            }
        }
        let Some((_, d)) = l.decoder.as_mut() else {
            return;
        };
        l.left.clear();
        l.right.clear();
        // Lost on the way (at most a few concealed; a long gap starts over).
        if let Some(last) = l.last_seq
            && seq > last + 1
        {
            let missing = seq - last - 1;
            l.lost += missing;
            for _ in 0..missing.min(5) {
                let _ = d.decode(None, &mut l.left, &mut l.right);
            }
        }
        if l.last_seq.is_some_and(|last| seq <= last) {
            return;
        }
        l.last_seq = Some(seq);
        l.received += 1;
        if d.decode(Some(packet), &mut l.left, &mut l.right).is_ok()
            && let Some(f) = l.feed.as_mut()
        {
            f.push(&l.left, &l.right);
        }
    }
}
