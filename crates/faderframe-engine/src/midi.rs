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
use faderframe_midi::{MidiEvent, MidiInputQueue, NoteTracker, TimedMidiEvent};
use faderframe_realtime::ParamSlot;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering};

/// Input events handled per device callback (more are dropped and counted).
pub const MIDI_INPUT_CAPACITY: usize = 2048;

/// A port index that no device has (a track routed to an absent port).
pub const NO_PORT: u16 = u16::MAX;

/// Input events of the current chunk: (port, event at a chunk offset).
#[derive(Debug)]
pub struct MidiInputBlock {
    events: Vec<(u16, TimedMidiEvent)>,
}

impl MidiInputBlock {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
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

/// Device-callback-level input (owned by the processor).
pub(crate) struct MidiInputState {
    queue: Option<Box<MidiInputQueue>>,
    /// Offsets relative to the device callback.
    events: MidiInputBlock,
}

impl MidiInputState {
    pub(crate) fn new() -> Self {
        Self {
            queue: None,
            events: MidiInputBlock::with_capacity(MIDI_INPUT_CAPACITY),
        }
    }

    /// Install a queue; returns the previous one (to retire).
    pub(crate) fn replace_queue(
        &mut self,
        q: Option<Box<MidiInputQueue>>,
    ) -> Option<Box<MidiInputQueue>> {
        std::mem::replace(&mut self.queue, q)
    }

    /// Drain the queue for a callback of `frames` at `rate` (audio thread).
    pub(crate) fn take(&mut self, frames: usize, rate: f64, dropped: &AtomicU64) {
        self.events.clear();
        let Some(q) = &mut self.queue else { return };
        if frames == 0 {
            return;
        }
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
                out.push(port, TimedMidiEvent::new(ev.sample_offset - a, ev.event));
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

/// A track's live MIDI input (events out: port 0).
pub struct MidiInputNode {
    filter: MidiFilter,
    live: ParamSlot,
    held: NoteTracker,
}

impl MidiInputNode {
    pub fn new(filter: MidiFilter, live: ParamSlot) -> Self {
        Self {
            filter,
            live,
            held: NoteTracker::default(),
        }
    }
}

impl Processor<EngineContext> for MidiInputNode {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.events_out.first_mut() else {
            return;
        };
        if cx.data.params.get(self.live) < 0.5 {
            // No longer live: nothing may keep sounding.
            if self.held.any_active() {
                self.held.release_all(out, 0);
            }
            return;
        }
        for &(port, ev) in cx.data.midi_input.iter() {
            if self.filter.accepts(port, ev.event) {
                self.held.observe(ev.event);
                let _ = out.push(ev);
            }
        }
    }

    fn reset(&mut self) {
        self.held = NoteTracker::default();
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
    ) {
        if self.next.is_some_and(|n| n != pos) {
            self.pass += 1;
        }
        self.next = Some(pos + frames as i64);
        for &(port, ev) in block.iter() {
            let at = pos + ev.sample_offset as i64;
            if at < self.from || at >= self.to {
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
    fn events_are_placed_by_arrival_time() {
        let (tx, q, _feed) = faderframe_midi::midi_input_queue(16);
        let mut st = MidiInputState::new();
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
        let mut st2 = MidiInputState::new();
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
        rec.capture(&block, 900, 256, &overruns); // 910: before the window
        rec.capture(&block, 1156, 256, &overruns); // 1166
        rec.capture(&block, 1000, 256, &overruns); // loop wrap: 1010, pass 1
        let got: Vec<(i64, u32)> = std::iter::from_fn(|| rx.pop().ok())
            .map(|r| (r.position, r.pass))
            .collect();
        assert_eq!(got, vec![(1166, 0), (1010, 1)]);
    }
}
