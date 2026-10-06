//! Live MIDI input on the audio thread.
//!
//! Once per device callback the processor drains the MIDI input queue
//! ([`faderframe_midi::MidiInputQueue`]) and places every event in the block
//! by its arrival time: an event that arrived one block duration ago lands
//! at frame 0, one that arrived just now at the end. Live input therefore
//! has a constant latency of one block instead of up to a block of jitter,
//! and the timing between notes is preserved. Each processing chunk sees its
//! share of the events in [`EngineContext::midi_input`]; [`MidiInputNode`]s
//! pass the ones for their track (port and channel filter) on to the
//! instrument while the track is live (armed / monitoring / selected, as
//! decided by the session, read from a parameter slot). Recording copies the
//! events of armed tracks inside the record window into a ring
//! ([`MidiRecorder`]) for the session.
//!
//! [`EngineContext::midi_input`]: crate::EngineContext::midi_input

use crate::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_core::TrackId;
use faderframe_midi::{MidiBuffer, MidiEvent, MidiInputQueue, NoteTracker, TimedMidiEvent};
use faderframe_realtime::ParamSlot;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Input events handled per device callback (more are dropped and counted).
pub const MIDI_INPUT_CAPACITY: usize = 2048;

/// A port index that no device has (a track routed to an absent port).
pub const NO_PORT: u16 = u16::MAX;
/// Events the editor plays for auditioning (they reach only the auditioned
/// track's instrument, whatever its live state, and are never recorded).
pub const AUDITION_PORT: u16 = u16::MAX - 1;

/// Which controls MIDI learn mappings use: those do not reach instruments
/// or recordings. Atomic bits per (port, channel): 128 CCs, 128 notes,
/// pitch bend and pressure. Written by the session, read on the audio
/// thread.
#[derive(Debug)]
pub struct ConsumedControls {
    bits: Box<[AtomicU64]>,
}

const WORDS: usize = 5;

/// A control a mapping takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumedControl {
    Cc(u8),
    Note(u8),
    PitchBend,
    ChannelPressure,
}

impl Default for ConsumedControls {
    fn default() -> Self {
        Self {
            bits: (0..faderframe_midi::MAX_MIDI_PORTS * 16 * WORDS)
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }
}

impl ConsumedControls {
    fn word_bit(port: u16, channel: u8, c: ConsumedControl) -> (usize, u64) {
        let port = (port as usize).min(faderframe_midi::MAX_MIDI_PORTS - 1);
        let base = (port * 16 + (channel & 15) as usize) * WORDS;
        let (w, b) = match c {
            ConsumedControl::Cc(n) => ((n & 127) as usize / 64, n as u32 % 64),
            ConsumedControl::Note(k) => (2 + (k & 127) as usize / 64, k as u32 % 64),
            ConsumedControl::PitchBend => (4, 0),
            ConsumedControl::ChannelPressure => (4, 1),
        };
        (base + w, 1u64 << b)
    }

    /// Replace the set: `(port, channel, control)`, `None` = every port
    /// (control thread).
    pub fn set(&self, controls: &[(Option<u16>, u8, ConsumedControl)]) {
        let mut next = vec![0u64; self.bits.len()];
        for &(port, channel, c) in controls {
            let ports: Vec<u16> = match port {
                Some(p) => vec![p],
                None => (0..faderframe_midi::MAX_MIDI_PORTS as u16).collect(),
            };
            for p in ports {
                let (w, b) = Self::word_bit(p, channel, c);
                next[w] |= b;
            }
        }
        for (a, v) in self.bits.iter().zip(next) {
            a.store(v, Ordering::Relaxed);
        }
    }

    /// Is this event taken by a mapping? (realtime-safe)
    #[inline]
    pub fn contains(&self, port: u16, ev: MidiEvent) -> bool {
        let c = match ev {
            MidiEvent::ControlChange { controller, .. } => ConsumedControl::Cc(controller),
            MidiEvent::NoteOn { key, .. } | MidiEvent::NoteOff { key, .. } => {
                ConsumedControl::Note(key)
            }
            MidiEvent::PitchBend { .. } => ConsumedControl::PitchBend,
            MidiEvent::ChannelPressure { .. } => ConsumedControl::ChannelPressure,
            _ => return false,
        };
        let (w, b) = Self::word_bit(port, ev.channel(), c);
        self.bits[w].load(Ordering::Relaxed) & b != 0
    }
}

/// MIDI state shared by the session and the audio thread.
#[derive(Debug, Default)]
pub struct MidiShared {
    /// Raw id of the track the editor auditions (0 = none).
    pub audition_track: AtomicU64,
    pub consumed: ConsumedControls,
    /// Output ports (bit per index < 64) that get MIDI clock.
    pub clock_ports: AtomicU64,
}

/// Input events of the current chunk: (port, event at a chunk offset).
/// SysEx events refer to bytes kept here ([`Self::sysex`]).
#[derive(Debug)]
pub struct MidiInputBlock {
    events: Vec<(u16, TimedMidiEvent)>,
    /// Holds the bytes of the chunk's SysEx messages.
    bytes: MidiBuffer,
}

/// Live SysEx messages per callback, and bytes for them.
const LIVE_SYSEX: usize = 32;

impl MidiInputBlock {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            bytes: MidiBuffer::with_capacities(LIVE_SYSEX, MidiBuffer::DEFAULT_SYSEX_CAPACITY),
        }
    }

    /// The bytes of one of this block's SysEx events.
    pub fn sysex(&self, r: &faderframe_midi::SysexRef) -> Option<&[u8]> {
        self.bytes.sysex(r)
    }

    /// Realtime-safe: dropped when there is no room.
    fn push_sysex(&mut self, port: u16, offset: u32, bytes: &[u8]) -> bool {
        if self.events.len() >= self.events.capacity() {
            return false;
        }
        match self.bytes.push_sysex(offset, bytes) {
            Ok(r) => self.push(port, TimedMidiEvent::new(offset, MidiEvent::SysEx(r))),
            Err(_) => false,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(u16, TimedMidiEvent)> {
        self.events.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn clear(&mut self) {
        self.events.clear();
        self.bytes.clear();
    }

    /// Realtime-safe: never grows past the capacity.
    fn push(&mut self, port: u16, ev: TimedMidiEvent) -> bool {
        if self.events.len() < self.events.capacity() {
            self.events.push((port, ev));
            true
        } else {
            false
        }
    }
}

/// Bytes of live SysEx on their way to the audio thread.
pub(crate) const LIVE_SYSEX_RING: usize = 64 * 1024;
/// A live SysEx frame: port (2 bytes), length (4), then the message.
const FRAME_HEADER: usize = 6;

/// The control side's end of the live SysEx ring.
pub(crate) struct LiveSysexSender(rtrb::Producer<u8>);

impl LiveSysexSender {
    /// Queue one message from input `port` as a whole frame (or not at all
    /// when the ring is full or the message too long).
    pub(crate) fn send(&mut self, port: u16, bytes: &[u8]) -> bool {
        let total = FRAME_HEADER + bytes.len();
        if bytes.is_empty() || bytes.len() > MidiBuffer::DEFAULT_SYSEX_CAPACITY {
            return false;
        }
        let Ok(chunk) = self.0.write_chunk_uninit(total) else {
            return false;
        };
        let header = port
            .to_le_bytes()
            .into_iter()
            .chain((bytes.len() as u32).to_le_bytes());
        // All bytes become visible to the reader at once.
        chunk.fill_from_iter(header.chain(bytes.iter().copied()));
        true
    }
}

/// Device-callback-level input (owned by the processor).
pub(crate) struct MidiInputState {
    queue: Option<Box<MidiInputQueue>>,
    /// Offsets relative to the device callback.
    events: MidiInputBlock,
    /// Live SysEx from the control side, and room to put one together.
    sysex: rtrb::Consumer<u8>,
    scratch: Vec<u8>,
}

impl MidiInputState {
    pub(crate) fn new() -> (Self, LiveSysexSender) {
        let (tx, rx) = rtrb::RingBuffer::new(LIVE_SYSEX_RING);
        (
            Self {
                queue: None,
                events: MidiInputBlock::with_capacity(MIDI_INPUT_CAPACITY),
                sysex: rx,
                scratch: Vec::with_capacity(MidiBuffer::DEFAULT_SYSEX_CAPACITY),
            },
            LiveSysexSender(tx),
        )
    }

    /// Live SysEx at the callback start (audio thread, allocation-free).
    fn take_sysex(&mut self, dropped: &AtomicU64) {
        while self.sysex.slots() >= FRAME_HEADER {
            let Ok(head) = self.sysex.read_chunk(FRAME_HEADER) else {
                return;
            };
            let mut h = [0u8; FRAME_HEADER];
            for (d, s) in h.iter_mut().zip(head) {
                *d = s;
            }
            let port = u16::from_le_bytes([h[0], h[1]]);
            let len = u32::from_le_bytes([h[2], h[3], h[4], h[5]]) as usize;
            // Frames are committed whole by the sender.
            let Ok(body) = self.sysex.read_chunk(len.min(self.sysex.slots())) else {
                return;
            };
            self.scratch.clear();
            if len <= self.scratch.capacity() {
                self.scratch.extend(body);
            } else {
                body.commit_all();
            }
            if self.scratch.len() == len && !self.events.push_sysex(port, 0, &self.scratch) {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Install a queue; returns the previous one (to retire).
    pub(crate) fn replace_queue(
        &mut self,
        q: Option<Box<MidiInputQueue>>,
    ) -> Option<Box<MidiInputQueue>> {
        std::mem::replace(&mut self.queue, q)
    }

    /// The clock MIDI input is stamped with (shared with the control side).
    pub(crate) fn clock(&self) -> Option<faderframe_midi::MidiClock> {
        self.queue.as_ref().map(|q| q.clock)
    }

    /// Drain the queue for a callback of `frames` at `rate` (audio thread).
    pub(crate) fn take(&mut self, frames: usize, rate: f64, dropped: &AtomicU64) {
        self.events.clear();
        if frames == 0 {
            return;
        }
        self.take_sysex(dropped);
        let Some(q) = &mut self.queue else { return };
        let now = q.clock.now_ns();
        let block_ns = frames as f64 * 1e9 / rate.max(1.0);
        while let Ok(raw) = q.consumer.pop() {
            let Some(event) = raw.event() else { continue };
            let age = now.saturating_sub(raw.time_ns) as f64;
            let at = ((block_ns - age).max(0.0) / block_ns * frames as f64) as usize;
            let offset = at.min(frames - 1) as u32;
            if !self
                .events
                .push(raw.port, TimedMidiEvent::new(offset, event))
            {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Stable insertion sort by offset (events of several ports
        // interleave; allocation-free, the lists are short).
        let ev = &mut self.events.events;
        for i in 1..ev.len() {
            let mut j = i;
            while j > 0 && ev[j - 1].1.sample_offset > ev[j].1.sample_offset {
                ev.swap(j - 1, j);
                j -= 1;
            }
        }
    }

    /// The events of the chunk `offset..offset + frames`, shifted to the
    /// chunk start.
    pub(crate) fn chunk(&self, offset: usize, frames: usize, out: &mut MidiInputBlock) {
        out.clear();
        let (a, b) = (offset as u32, (offset + frames) as u32);
        for &(port, ev) in self.events.iter() {
            if ev.sample_offset >= a && ev.sample_offset < b {
                let at = ev.sample_offset - a;
                match ev.event {
                    MidiEvent::SysEx(r) => {
                        if let Some(bytes) = self.events.sysex(&r) {
                            out.push_sysex(port, at, bytes);
                        }
                    }
                    e => {
                        out.push(port, TimedMidiEvent::new(at, e));
                    }
                }
            }
        }
    }
}

/// Which events a track takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiFilter {
    /// `None`: every port.
    pub port: Option<u16>,
    /// `None`: every channel.
    pub channel: Option<u8>,
}

impl MidiFilter {
    #[inline]
    pub fn accepts(&self, port: u16, event: MidiEvent) -> bool {
        self.port.is_none_or(|p| p == port) && self.channel.is_none_or(|c| c == event.channel())
    }
}

/// A track's live MIDI input and editor auditioning (events out: port 0).
pub struct MidiInputNode {
    track: u64,
    /// `None`: the track has no MIDI input (auditioning only).
    filter: Option<MidiFilter>,
    live: ParamSlot,
    /// A MIDI track's mute: no live play while set.
    mute: Option<ParamSlot>,
    held: NoteTracker,
    shared: Arc<MidiShared>,
}

impl MidiInputNode {
    pub fn new(
        track: TrackId,
        filter: Option<MidiFilter>,
        live: ParamSlot,
        shared: Arc<MidiShared>,
    ) -> Self {
        Self {
            track: track.raw(),
            filter,
            live,
            mute: None,
            held: NoteTracker::default(),
            shared,
        }
    }
}

impl MidiInputNode {
    /// No live play while the slot is set (MIDI tracks' mute).
    pub fn with_mute(mut self, mute: ParamSlot) -> Self {
        self.mute = Some(mute);
        self
    }
}

impl Processor<EngineContext> for MidiInputNode {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.events_out.first_mut() else {
            return;
        };
        let muted = self.mute.is_some_and(|m| cx.data.params.get(m) >= 0.5);
        let live = self.filter.is_some() && cx.data.params.get(self.live) >= 0.5 && !muted;
        if !live && self.held.any_active() {
            // No longer live: nothing may keep sounding.
            self.held.release_all(out, 0);
        }
        let audition = self.shared.audition_track.load(Ordering::Relaxed) == self.track;
        for &(port, ev) in cx.data.midi_input.iter() {
            // SysEx from the track's input port (it has no channel).
            if let MidiEvent::SysEx(r) = ev.event {
                if live
                    && self
                        .filter
                        .is_some_and(|f| f.port.is_none_or(|p| p == port))
                    && let Some(bytes) = cx.data.midi_input.sysex(&r)
                {
                    let _ = out.push_sysex(ev.sample_offset, bytes);
                }
                continue;
            }
            if port == AUDITION_PORT {
                if audition {
                    let _ = out.push(ev);
                }
                continue;
            }
            if live
                && self.filter.is_some_and(|f| f.accepts(port, ev.event))
                && !self.shared.consumed.contains(port, ev.event)
            {
                self.held.observe(ev.event);
                let _ = out.push(ev);
            }
        }
    }

    fn reset(&mut self) {
        self.held = NoteTracker::default();
    }
}

/// Sends a track's events to an external MIDI device: copies events in to
/// events out (optionally on another channel); the driver reads the output
/// of this node (`NodeRole::EventOutput`).
pub struct MidiOutputSink {
    channel: Option<u8>,
}

impl MidiOutputSink {
    pub fn new(channel: Option<u8>) -> Self {
        Self { channel }
    }
}

fn on_channel(ev: MidiEvent, ch: u8) -> MidiEvent {
    match ev {
        MidiEvent::NoteOn { key, velocity, .. } => MidiEvent::NoteOn {
            channel: ch,
            key,
            velocity,
        },
        MidiEvent::NoteOff { key, velocity, .. } => MidiEvent::NoteOff {
            channel: ch,
            key,
            velocity,
        },
        MidiEvent::PolyPressure { key, pressure, .. } => MidiEvent::PolyPressure {
            channel: ch,
            key,
            pressure,
        },
        MidiEvent::ControlChange {
            controller, value, ..
        } => MidiEvent::ControlChange {
            channel: ch,
            controller,
            value,
        },
        MidiEvent::ProgramChange { program, .. } => MidiEvent::ProgramChange {
            channel: ch,
            program,
        },
        MidiEvent::ChannelPressure { pressure, .. } => MidiEvent::ChannelPressure {
            channel: ch,
            pressure,
        },
        MidiEvent::PitchBend { value, .. } => MidiEvent::PitchBend { channel: ch, value },
        MidiEvent::NoteExpression {
            key, kind, value, ..
        } => MidiEvent::NoteExpression {
            channel: ch,
            key,
            kind,
            value,
        },
        MidiEvent::SysEx(r) => MidiEvent::SysEx(r),
    }
}

impl Processor<EngineContext> for MidiOutputSink {
    fn process(&mut self, _cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let (Some(input), Some(out)) = (io.events_in.first(), io.events_out.first_mut()) else {
            return;
        };
        for ev in input.iter() {
            if matches!(
                ev.event,
                MidiEvent::NoteExpression { .. } | MidiEvent::SysEx(_)
            ) {
                // No MIDI form (expressions), or sent from the control side
                // (clip SysEx: `session::sysex`).
                continue;
            }
            let e = match self.channel {
                Some(ch) => on_channel(ev.event, ch),
                None => ev.event,
            };
            let _ = out.push(TimedMidiEvent::new(ev.sample_offset, e));
        }
    }
}

/// An armed track whose input is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiRecordTarget {
    pub track: TrackId,
    pub filter: MidiFilter,
}

/// One recorded event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedMidi {
    /// Index into the record targets.
    pub target: u16,
    /// Timeline sample where the event was placed.
    pub position: i64,
    /// Contiguous passes (a loop wrap or locate starts a new one).
    pub pass: u32,
    pub event: MidiEvent,
}

/// Captures armed tracks' MIDI input on the audio thread.
pub struct MidiRecorder {
    targets: Vec<MidiRecordTarget>,
    tx: Producer<RecordedMidi>,
    from: i64,
    to: i64,
    pass: u32,
    next: Option<i64>,
}

/// A recorder for `targets` inside `from..to` and the session's end.
pub fn midi_recording(
    targets: Vec<MidiRecordTarget>,
    from: i64,
    to: i64,
    capacity: usize,
) -> (MidiRecorder, Consumer<RecordedMidi>) {
    let (tx, rx) = RingBuffer::new(capacity.max(64));
    (
        MidiRecorder {
            targets,
            tx,
            from,
            to,
            pass: 0,
            next: None,
        },
        rx,
    )
}

impl MidiRecorder {
    /// Record the chunk starting at timeline sample `pos` (audio thread).
    pub(crate) fn capture(
        &mut self,
        block: &MidiInputBlock,
        pos: i64,
        frames: usize,
        overruns: &AtomicU64,
        consumed: &ConsumedControls,
    ) {
        if self.next.is_some_and(|n| n != pos) {
            self.pass += 1;
        }
        self.next = Some(pos + frames as i64);
        for &(port, ev) in block.iter() {
            let at = pos + ev.sample_offset as i64;
            if at < self.from
                || at >= self.to
                || port == AUDITION_PORT
                || consumed.contains(port, ev.event)
            {
                continue;
            }
            for (i, t) in self.targets.iter().enumerate() {
                if t.filter.accepts(port, ev.event) {
                    let rec = RecordedMidi {
                        target: i as u16,
                        position: at,
                        pass: self.pass,
                        event: ev.event,
                    };
                    if self.tx.push(rec).is_err() {
                        overruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: u8) -> [u8; 3] {
        [0x90, key, 100]
    }

    #[test]
    fn live_sysex_arrives_whole_and_reaches_its_chunk() {
        let (mut st, mut tx) = MidiInputState::new();
        let dropped = AtomicU64::new(0);
        let big = {
            let mut v = vec![0x22u8; 9000];
            (v[0], v[8999]) = (0xF0, 0xF7);
            v
        };
        assert!(tx.send(3, &[0xF0, 1, 2, 0xF7]));
        assert!(tx.send(5, &big));
        assert!(!tx.send(5, &[]), "nothing to send");
        st.take(256, 48_000.0, &dropped);
        let got: Vec<(u16, Vec<u8>)> = st
            .events
            .iter()
            .filter_map(|(p, e)| match e.event {
                MidiEvent::SysEx(r) => st.events.sysex(&r).map(|b| (*p, b.to_vec())),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![(3, vec![0xF0, 1, 2, 0xF7]), (5, big.clone())]);
        // The first chunk carries them (bytes copied), the next none.
        let mut chunk = MidiInputBlock::with_capacity(8);
        st.chunk(0, 128, &mut chunk);
        let sizes: Vec<usize> = chunk
            .iter()
            .filter_map(|(_, e)| match e.event {
                MidiEvent::SysEx(r) => chunk.sysex(&r).map(<[u8]>::len),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, vec![4, 9000]);
        st.chunk(128, 128, &mut chunk);
        assert!(chunk.is_empty());
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn events_are_placed_by_arrival_time() {
        let (tx, q, _feed) = faderframe_midi::midi_input_queue(16);
        let (mut st, _) = MidiInputState::new();
        st.replace_queue(Some(Box::new(q)));
        let dropped = AtomicU64::new(0);
        // 48 kHz, 480 frames = 10 ms. An event sent 5 ms before the callback
        // lands mid-block; a fresh one at the end; an old one at 0.
        tx.send(0, &note(60));
        std::thread::sleep(std::time::Duration::from_millis(5));
        tx.send(1, &note(62));
        st.take(480, 48_000.0, &dropped);
        let offsets: Vec<(u16, u32)> = st
            .events
            .iter()
            .map(|(p, e)| (*p, e.sample_offset))
            .collect();
        assert_eq!(offsets.len(), 2);
        assert!(offsets[0].0 == 0 && offsets[0].1 < 300, "{offsets:?}");
        assert!(offsets[1].0 == 1 && offsets[1].1 > 400, "{offsets:?}");
        std::thread::sleep(std::time::Duration::from_millis(30));
        tx.send(0, &note(64));
        std::thread::sleep(std::time::Duration::from_millis(20));
        st.take(480, 48_000.0, &dropped);
        assert_eq!(st.events.iter().next().map(|e| e.1.sample_offset), Some(0));
        // Chunks see their share, shifted.
        let (mut st2, _) = MidiInputState::new();
        st2.events.push(
            0,
            TimedMidiEvent::new(10, MidiEvent::from_bytes(&note(1)).unwrap()),
        );
        st2.events.push(
            0,
            TimedMidiEvent::new(300, MidiEvent::from_bytes(&note(2)).unwrap()),
        );
        let mut chunk = MidiInputBlock::with_capacity(8);
        st2.chunk(256, 224, &mut chunk);
        let got: Vec<u32> = chunk.iter().map(|e| e.1.sample_offset).collect();
        assert_eq!(got, vec![44]);
    }

    #[test]
    fn filters_take_port_and_channel() {
        let on = |ch| MidiEvent::NoteOn {
            channel: ch,
            key: 60,
            velocity: 1,
        };
        let all = MidiFilter {
            port: None,
            channel: None,
        };
        let ch10 = MidiFilter {
            port: Some(2),
            channel: Some(9),
        };
        assert!(all.accepts(5, on(3)));
        assert!(ch10.accepts(2, on(9)));
        assert!(!ch10.accepts(1, on(9)));
        assert!(!ch10.accepts(2, on(0)));
    }

    #[test]
    fn the_recorder_keeps_the_window_and_counts_passes() {
        let ev = MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity: 90,
        };
        let (mut rec, mut rx) = midi_recording(
            vec![MidiRecordTarget {
                track: TrackId(1),
                filter: MidiFilter {
                    port: None,
                    channel: None,
                },
            }],
            1000,
            5000,
            64,
        );
        let overruns = AtomicU64::new(0);
        let mut block = MidiInputBlock::with_capacity(4);
        block.push(0, TimedMidiEvent::new(10, ev));
        let consumed = ConsumedControls::default();
        rec.capture(&block, 900, 256, &overruns, &consumed); // 910: before the window
        rec.capture(&block, 1156, 256, &overruns, &consumed); // 1166
        rec.capture(&block, 1000, 256, &overruns, &consumed); // loop wrap: 1010, pass 1
        let got: Vec<(i64, u32)> = std::iter::from_fn(|| rx.pop().ok())
            .map(|r| (r.position, r.pass))
            .collect();
        assert_eq!(got, vec![(1166, 0), (1010, 1)]);
    }
}

/// MIDI clock (24 pulses per quarter) with start/stop/continue and song
/// position, generated on the audio thread for every output port with
/// clock enabled.
#[derive(Debug, Default)]
pub(crate) struct ClockGen {
    was_playing: bool,
}

impl ClockGen {
    /// Messages for a chunk of `frames` starting at `info` (offsets in the
    /// chunk).
    pub(crate) fn chunk(
        &mut self,
        info: &faderframe_transport::TransportInfo,
        discontinuity: bool,
        frames: usize,
        mut emit: impl FnMut(u32, &[u8]),
    ) {
        let q0 = info.quarter_position.max(0.0);
        let spp = |q: f64| {
            let sixteenths = (q * 4.0).floor().clamp(0.0, 16383.0) as u16;
            [0xF2, (sixteenths & 0x7F) as u8, (sixteenths >> 7) as u8]
        };
        if info.playing && (!self.was_playing || discontinuity) {
            if self.was_playing {
                emit(0, &[0xFC]);
            }
            if q0 <= 1e-9 {
                emit(0, &[0xFA]);
            } else {
                emit(0, &spp(q0));
                emit(0, &[0xFB]);
            }
        } else if !info.playing && self.was_playing {
            emit(0, &[0xFC]);
        }
        self.was_playing = info.playing;
        if !info.playing || info.tempo <= 0.0 {
            return;
        }
        let frames_per_quarter = 60.0 / info.tempo * info.sample_rate.max(1.0);
        let mut k = (q0 * 24.0).ceil();
        loop {
            let q = k / 24.0;
            let offset = ((q - q0) * frames_per_quarter).round();
            if offset >= frames as f64 {
                break;
            }
            emit(offset.max(0.0) as u32, &[0xF8]);
            k += 1.0;
        }
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    use faderframe_transport::TransportInfo;

    fn info(playing: bool, q: f64) -> TransportInfo {
        TransportInfo {
            playing,
            sample_rate: 48_000.0,
            quarter_position: q,
            tempo: 120.0,
            ..TransportInfo::default()
        }
    }

    #[test]
    fn clock_pulses_start_and_stop() {
        let mut c = ClockGen::default();
        let mut got: Vec<(u32, Vec<u8>)> = Vec::new();
        // 120 BPM: a quarter is 24 000 frames, a pulse every 1000.
        c.chunk(&info(true, 0.0), false, 2500, |o, b| {
            got.push((o, b.to_vec()))
        });
        assert_eq!(got[0], (0, vec![0xFA]), "start from the top");
        let pulses: Vec<u32> = got
            .iter()
            .filter(|(_, b)| b == &[0xF8])
            .map(|(o, _)| *o)
            .collect();
        assert_eq!(pulses, vec![0, 1000, 2000]);
        got.clear();
        c.chunk(&info(false, 0.1), false, 256, |o, b| {
            got.push((o, b.to_vec()))
        });
        assert_eq!(got, vec![(0, vec![0xFC])]);
        got.clear();
        // Continue from bar 2 (quarter 4): song position 16 sixteenths.
        c.chunk(&info(true, 4.0), false, 10, |o, b| {
            got.push((o, b.to_vec()))
        });
        assert_eq!(got[0], (0, vec![0xF2, 16, 0]));
        assert_eq!(got[1], (0, vec![0xFB]));
    }
}
