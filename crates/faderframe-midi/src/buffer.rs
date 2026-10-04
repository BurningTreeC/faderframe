use crate::event::{MidiEvent, SysexRef, TimedMidiEvent};
use std::sync::atomic::{AtomicU32, Ordering};

/// Returned when a [`MidiBuffer`] is at capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiBufferFull;

/// Fixed-capacity, always-sorted list of block events.
///
/// Events are ordered by sample offset; events at the same offset keep
/// insertion order except that note-offs are placed before note-ons.
/// System exclusive messages keep their bytes in the buffer's own byte
/// area ([`MidiEvent::SysEx`]): they are pushed with [`Self::push_sysex`]
/// and copied to other buffers with [`Self::push_from`] (or
/// [`Self::merge_from`]); a SysEx event of another buffer pushed with
/// [`Self::push`] is dropped, never misread. Never allocates after
/// construction.
#[derive(Debug)]
pub struct MidiBuffer {
    events: Vec<TimedMidiEvent>,
    /// The bytes of this block's SysEx messages.
    sysex: Vec<u8>,
    /// Tells this buffer's SysEx references from others'.
    id: u32,
    dropped: u64,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

impl Clone for MidiBuffer {
    /// The same events and room (the copy's SysEx references stay valid:
    /// it keeps the id while the contents are the same).
    fn clone(&self) -> Self {
        let mut events = Vec::with_capacity(self.events.capacity());
        events.extend_from_slice(&self.events);
        let mut sysex = Vec::with_capacity(self.sysex.capacity());
        sysex.extend_from_slice(&self.sysex);
        Self {
            events,
            sysex,
            id: self.id,
            dropped: self.dropped,
        }
    }
}

impl MidiBuffer {
    /// Default capacity used for graph event ports.
    pub const DEFAULT_CAPACITY: usize = 1024;
    /// Default room for SysEx bytes per block (a DX7 bulk dump is 4104).
    pub const DEFAULT_SYSEX_CAPACITY: usize = 16 * 1024;

    /// Allocate a buffer (control thread only) with room for `capacity`
    /// events and [`Self::DEFAULT_SYSEX_CAPACITY`] SysEx bytes.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacities(capacity, Self::DEFAULT_SYSEX_CAPACITY)
    }

    /// Room for `events` events and `sysex_bytes` bytes of SysEx.
    pub fn with_capacities(events: usize, sysex_bytes: usize) -> Self {
        Self {
            events: Vec::with_capacity(events.max(1)),
            sysex: Vec::with_capacity(sysex_bytes),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            dropped: 0,
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.events.clear();
        self.sysex.clear();
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.events.capacity()
    }

    /// Total number of events dropped because the buffer was full.
    #[inline]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    #[inline]
    pub fn as_slice(&self) -> &[TimedMidiEvent] {
        &self.events
    }

    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, TimedMidiEvent> {
        self.events.iter()
    }

    /// Insert an event, keeping the buffer sorted. Realtime-safe. A SysEx
    /// event must be this buffer's (see [`Self::push_from`]).
    pub fn push(&mut self, event: TimedMidiEvent) -> Result<(), MidiBufferFull> {
        if let MidiEvent::SysEx(r) = event.event
            && r.buffer != self.id
        {
            self.dropped += 1;
            return Err(MidiBufferFull);
        }
        if self.events.len() >= self.events.capacity() {
            self.dropped += 1;
            return Err(MidiBufferFull);
        }
        let key = event.sort_key();
        match self.events.last() {
            Some(last) if last.sort_key() > key => {
                let at = self.events.partition_point(|e| e.sort_key() <= key);
                // Capacity checked above: `insert` cannot reallocate.
                self.events.insert(at, event);
            }
            _ => self.events.push(event),
        }
        Ok(())
    }

    /// Add a SysEx message (`F0 … F7`) at `sample_offset`, its bytes copied
    /// into this buffer. Realtime-safe: a message that does not fit is
    /// dropped and counted.
    pub fn push_sysex(
        &mut self,
        sample_offset: u32,
        bytes: &[u8],
    ) -> Result<SysexRef, MidiBufferFull> {
        let start = self.sysex.len();
        if bytes.is_empty()
            || start + bytes.len() > self.sysex.capacity()
            || self.events.len() >= self.events.capacity()
            || u32::try_from(start + bytes.len()).is_err()
        {
            self.dropped += 1;
            return Err(MidiBufferFull);
        }
        // Within capacity: no reallocation.
        self.sysex.extend_from_slice(bytes);
        let r = SysexRef {
            buffer: self.id,
            start: start as u32,
            len: bytes.len() as u32,
        };
        if let Err(e) = self.push(TimedMidiEvent::new(sample_offset, MidiEvent::SysEx(r))) {
            self.sysex.truncate(start);
            return Err(e);
        }
        Ok(r)
    }

    /// The bytes of one of this buffer's SysEx messages.
    pub fn sysex(&self, r: &SysexRef) -> Option<&[u8]> {
        if r.buffer != self.id {
            return None;
        }
        self.sysex
            .get(r.start as usize..r.start as usize + r.len as usize)
    }

    /// Add an event of `from` (SysEx bytes are copied). Realtime-safe.
    pub fn push_from(
        &mut self,
        from: &MidiBuffer,
        event: TimedMidiEvent,
    ) -> Result<(), MidiBufferFull> {
        match event.event {
            MidiEvent::SysEx(r) => match from.sysex(&r) {
                Some(bytes) => self.push_sysex(event.sample_offset, bytes).map(|_| ()),
                None => {
                    self.dropped += 1;
                    Err(MidiBufferFull)
                }
            },
            _ => self.push(event),
        }
    }

    /// Merge all events of `other` into `self` (shifted by `offset_shift`).
    /// Events that do not fit are dropped and counted.
    pub fn merge_from(&mut self, other: &MidiBuffer, offset_shift: u32) {
        for e in other.iter() {
            let _ = self.push_from(
                other,
                TimedMidiEvent::new(e.sample_offset.saturating_add(offset_shift), e.event),
            );
        }
    }

    /// Events with `start <= offset < end`.
    pub fn range(&self, start: u32, end: u32) -> &[TimedMidiEvent] {
        let a = self.events.partition_point(|e| e.sample_offset < start);
        let b = self.events.partition_point(|e| e.sample_offset < end);
        &self.events[a..b.max(a)]
    }
}

impl<'a> IntoIterator for &'a MidiBuffer {
    type Item = &'a TimedMidiEvent;
    type IntoIter = std::slice::Iter<'a, TimedMidiEvent>;
    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MidiEvent;

    fn on(off: u32, key: u8) -> TimedMidiEvent {
        TimedMidiEvent::new(
            off,
            MidiEvent::NoteOn {
                channel: 0,
                key,
                velocity: 100,
            },
        )
    }

    fn off(off: u32, key: u8) -> TimedMidiEvent {
        TimedMidiEvent::new(
            off,
            MidiEvent::NoteOff {
                channel: 0,
                key,
                velocity: 0,
            },
        )
    }

    #[test]
    fn keeps_events_sorted_and_note_offs_first() {
        let mut buf = MidiBuffer::with_capacity(8);
        buf.push(on(10, 60)).unwrap();
        buf.push(on(2, 61)).unwrap();
        buf.push(off(10, 60)).unwrap();
        buf.push(on(5, 62)).unwrap();
        let offsets: Vec<_> = buf.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![2, 5, 10, 10]);
        assert!(matches!(buf.as_slice()[2].event, MidiEvent::NoteOff { .. }));
        assert_eq!(buf.range(3, 11).len(), 3);
    }

    #[test]
    fn overflow_drops_without_reallocating() {
        let mut buf = MidiBuffer::with_capacity(2);
        let cap = buf.capacity();
        buf.push(on(0, 1)).unwrap();
        buf.push(on(1, 2)).unwrap();
        while buf.len() < cap {
            buf.push(on(2, 3)).unwrap();
        }
        assert_eq!(buf.push(on(3, 4)), Err(MidiBufferFull));
        assert_eq!(buf.dropped(), 1);
        assert_eq!(buf.capacity(), cap);
    }

    #[test]
    fn sysex_bytes_travel_with_their_buffer() {
        let mut a = MidiBuffer::with_capacities(8, 16);
        let mut b = MidiBuffer::with_capacities(8, 16);
        a.push_sysex(3, &[0xF0, 1, 2, 0xF7]).unwrap();
        a.push(on(1, 60)).unwrap();
        let MidiEvent::SysEx(r) = a.as_slice()[1].event else {
            panic!("{:?}", a.as_slice());
        };
        assert_eq!(a.sysex(&r), Some(&[0xF0, 1, 2, 0xF7][..]));
        // Another buffer cannot read it, and a plain push of it is dropped.
        assert_eq!(b.sysex(&r), None);
        assert_eq!(b.push(a.as_slice()[1]), Err(MidiBufferFull));
        // Merging copies the bytes.
        b.push_sysex(0, &[0xF0, 9, 0xF7]).unwrap();
        b.merge_from(&a, 10);
        let copied: Vec<Vec<u8>> = b
            .iter()
            .filter_map(|e| match e.event {
                MidiEvent::SysEx(r) => b.sysex(&r).map(<[u8]>::to_vec),
                _ => None,
            })
            .collect();
        assert_eq!(copied, vec![vec![0xF0, 9, 0xF7], vec![0xF0, 1, 2, 0xF7]]);
        // No room: dropped, the buffer never grows.
        let cap = b.sysex.capacity();
        assert_eq!(b.push_sysex(0, &[0xF0; 20]), Err(MidiBufferFull));
        assert_eq!(b.sysex.capacity(), cap);
        b.clear();
        assert!(b.sysex.is_empty());
    }

    #[test]
    fn merge_shifts_offsets() {
        let mut a = MidiBuffer::with_capacity(8);
        let mut b = MidiBuffer::with_capacity(8);
        a.push(on(4, 1)).unwrap();
        b.push(on(1, 2)).unwrap();
        a.merge_from(&b, 10);
        let offsets: Vec<_> = a.iter().map(|e| e.sample_offset).collect();
        assert_eq!(offsets, vec![4, 11]);
    }
}
