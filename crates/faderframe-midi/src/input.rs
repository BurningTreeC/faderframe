//! Timestamped MIDI input on its way from device threads to the audio
//! thread.
//!
//! Every input port (hardware, virtual) pushes raw messages stamped with a
//! shared monotonic [`MidiClock`] into one bounded queue; the audio thread
//! drains it wait-free once per callback and places each event in the block
//! by its arrival time (see the engine), so live input has constant latency
//! instead of buffer-sized jitter.
//!
//! Every message also goes to the control thread through a second bounded
//! channel ([`MidiControlFeed`]) — for MIDI learn, controller mappings and
//! activity display, which must work while no audio stream runs. System
//! messages (clock, start/continue/stop, song position, MTC, SysEx) only
//! go there, as [`MidiSystemEvent`]s: synchronisation and SysEx recording
//! happen on the control side with the messages' timestamps.

use crate::MidiEvent;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Ports whose activity is tracked (port indices above share the last).
pub const MAX_MIDI_PORTS: usize = 64;

/// Monotonic clock shared by MIDI input threads and the audio thread.
#[derive(Clone, Copy, Debug)]
pub struct MidiClock {
    base: Instant,
}

impl Default for MidiClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
        }
    }

    /// Nanoseconds since the clock was created (realtime-safe: a vDSO clock
    /// read).
    #[inline]
    pub fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64
    }
}

/// One channel message as received from a port (system messages, SysEx and
/// clock are not passed on).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiInputEvent {
    pub port: u16,
    pub time_ns: u64,
    pub len: u8,
    pub bytes: [u8; 3],
}

impl MidiInputEvent {
    /// A channel message (1–3 bytes, status first); `None` for anything else.
    pub fn new(port: u16, time_ns: u64, msg: &[u8]) -> Option<Self> {
        let status = *msg.first()?;
        if !(0x80..0xF0).contains(&status) || msg.len() > 3 {
            return None;
        }
        let mut bytes = [0u8; 3];
        bytes[..msg.len()].copy_from_slice(msg);
        Some(Self {
            port,
            time_ns,
            len: msg.len() as u8,
            bytes,
        })
    }

    pub fn event(&self) -> Option<MidiEvent> {
        MidiEvent::from_bytes(&self.bytes[..self.len as usize])
    }
}

/// The audio thread's end of all MIDI inputs.
pub struct MidiInputQueue {
    pub consumer: rtrb::Consumer<MidiInputEvent>,
    pub clock: MidiClock,
}

/// A system message (not for instruments: clock, transport, timecode,
/// SysEx).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemMessage {
    /// Timing clock, 24 per quarter note.
    Clock,
    Start,
    Continue,
    Stop,
    /// Song position in sixteenth notes (6 clocks each).
    SongPosition(u16),
    /// MTC quarter frame (the data byte: piece type and value).
    QuarterFrame(u8),
    /// A complete SysEx message, `F0 … F7` included.
    SysEx(Vec<u8>),
}

impl SystemMessage {
    pub fn parse(msg: &[u8]) -> Option<Self> {
        Some(match *msg.first()? {
            0xF8 => Self::Clock,
            0xFA => Self::Start,
            0xFB => Self::Continue,
            0xFC => Self::Stop,
            0xF2 if msg.len() >= 3 => {
                Self::SongPosition((msg[1] & 0x7F) as u16 | ((msg[2] & 0x7F) as u16) << 7)
            }
            0xF1 if msg.len() >= 2 => Self::QuarterFrame(msg[1] & 0x7F),
            0xF0 if msg.len() >= 2 => Self::SysEx(msg.to_vec()),
            _ => return None,
        })
    }
}

/// A system message as received from a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiSystemEvent {
    pub port: u16,
    pub time_ns: u64,
    pub message: SystemMessage,
}

/// The control thread's copy of every input message.
pub struct MidiControlFeed {
    rx: Receiver<MidiInputEvent>,
    system: Receiver<MidiSystemEvent>,
}

impl MidiControlFeed {
    /// Every channel message received since the last call (never blocks).
    pub fn drain(&self) -> Vec<MidiInputEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Every system message received since the last call (never blocks).
    pub fn drain_system(&self) -> Vec<MidiSystemEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.system.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// The input threads' end, shared by every port.
#[derive(Clone)]
pub struct MidiInputSender {
    tx: Arc<Mutex<rtrb::Producer<MidiInputEvent>>>,
    control: SyncSender<MidiInputEvent>,
    system: SyncSender<MidiSystemEvent>,
    clock: MidiClock,
    capacity: usize,
    dropped: Arc<AtomicU64>,
}

impl MidiInputSender {
    /// Stamp `msg` now and queue it; `false` if it was not a channel
    /// message or the queue was full (counted in [`Self::dropped`]).
    /// System messages go to the control feed only.
    pub fn send(&self, port: u16, msg: &[u8]) -> bool {
        let now = self.clock.now_ns();
        if let Some(message) = SystemMessage::parse(msg) {
            return self
                .system
                .try_send(MidiSystemEvent {
                    port,
                    time_ns: now,
                    message,
                })
                .is_ok();
        }
        let Some(ev) = MidiInputEvent::new(port, now, msg) else {
            return false;
        };
        // The control copy may be lost when nobody reads it; that is fine.
        let _ = self.control.try_send(ev);
        let pushed = self
            .tx
            .lock()
            .map(|mut tx| tx.push(ev).is_ok())
            .unwrap_or(false);
        if !pushed {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        pushed
    }

    /// A fresh audio-thread end (for a new engine); every port keeps
    /// sending, now into it. The old consumer receives nothing more.
    pub fn renew(&self) -> MidiInputQueue {
        let (tx, rx) = rtrb::RingBuffer::new(self.capacity);
        if let Ok(mut old) = self.tx.lock() {
            *old = tx;
        }
        MidiInputQueue {
            consumer: rx,
            clock: self.clock,
        }
    }

    pub fn clock(&self) -> MidiClock {
        self.clock
    }

    /// Messages lost because the audio thread did not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A queue for `capacity` messages between input threads and the audio
/// thread, plus the control thread's feed.
pub fn midi_input_queue(capacity: usize) -> (MidiInputSender, MidiInputQueue, MidiControlFeed) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    let (ctx, crx) = sync_channel(capacity.max(1));
    // Clock alone is ~100 messages a second; SysEx dumps come in bursts.
    let (stx, srx) = sync_channel(4096);
    let clock = MidiClock::new();
    (
        MidiInputSender {
            tx: Arc::new(Mutex::new(tx)),
            control: ctx,
            system: stx,
            clock,
            capacity,
            dropped: Arc::new(AtomicU64::new(0)),
        },
        MidiInputQueue {
            consumer: rx,
            clock,
        },
        MidiControlFeed {
            rx: crx,
            system: srx,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_pass_others_do_not() {
        let (tx, mut rx, feed) = midi_input_queue(4);
        assert!(tx.send(3, &[0x90, 60, 100]));
        // System messages only reach the control side.
        assert!(tx.send(0, &[0xF8]), "clock");
        assert!(tx.send(0, &[0xF0, 1, 2, 3, 0xF7]), "SysEx");
        assert!(!tx.send(0, &[]));
        assert!(!tx.send(0, &[0xFE]), "active sensing is dropped");
        let system: Vec<SystemMessage> =
            feed.drain_system().into_iter().map(|e| e.message).collect();
        assert_eq!(
            system,
            vec![
                SystemMessage::Clock,
                SystemMessage::SysEx(vec![0xF0, 1, 2, 3, 0xF7])
            ]
        );
        let ev = rx.consumer.pop().unwrap();
        assert_eq!(ev.port, 3);
        assert_eq!(
            ev.event(),
            Some(MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100
            })
        );
        assert!(ev.time_ns <= rx.clock.now_ns());
        assert_eq!(feed.drain(), vec![ev], "the control thread sees it too");
        assert!(feed.drain().is_empty());
    }

    #[test]
    fn renewing_redirects_every_sender() {
        let (tx, mut old, _feed) = midi_input_queue(4);
        let other = tx.clone();
        let mut new = tx.renew();
        assert!(other.send(1, &[0x90, 1, 1]));
        assert!(old.consumer.pop().is_err());
        assert_eq!(new.consumer.pop().map(|e| e.port), Ok(1));
    }

    #[test]
    fn a_full_queue_drops_and_counts() {
        let (tx, _rx, _feed) = midi_input_queue(2);
        for _ in 0..5 {
            tx.send(0, &[0xB0, 1, 2]);
        }
        assert_eq!(tx.dropped(), 3);
    }
}
