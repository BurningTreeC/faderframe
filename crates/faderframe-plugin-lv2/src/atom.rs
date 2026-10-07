//! Atom port buffers: sequences of timed events (MIDI, the transport,
//! messages between a UI and its plugin), 8-byte aligned, in preallocated
//! memory (nothing here allocates once a buffer exists).

use crate::sys::LV2_Atom;
use crate::urid::Urids;

const HEADER: usize = 8; // LV2_Atom
const BODY: usize = 8; // LV2_Atom_Sequence_Body
const EVENT: usize = 16; // frames + LV2_Atom

fn pad8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// A sequence buffer of a fixed capacity (bytes).
pub struct AtomBuffer {
    data: Vec<u64>,
}

impl AtomBuffer {
    pub fn new(capacity: usize) -> AtomBuffer {
        AtomBuffer {
            data: vec![0; pad8(capacity.max(HEADER + BODY + EVENT)) / 8],
        }
    }

    pub fn capacity(&self) -> usize {
        self.data.len() * 8
    }

    fn bytes(&self) -> &[u8] {
        as_bytes(&self.data)
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        as_bytes_mut(&mut self.data)
    }

    pub fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        self.data.as_mut_ptr().cast()
    }

    fn header(&self) -> LV2_Atom {
        let b = self.bytes();
        LV2_Atom {
            size: u32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
            type_: u32::from_ne_bytes([b[4], b[5], b[6], b[7]]),
        }
    }

    fn set_header(&mut self, size: u32, type_: u32) {
        let b = self.bytes_mut();
        b[..4].copy_from_slice(&size.to_ne_bytes());
        b[4..8].copy_from_slice(&type_.to_ne_bytes());
    }

    /// An empty input sequence (time in frames).
    pub fn clear_input(&mut self) {
        self.set_header(BODY as u32, Urids::get().atom_sequence);
        self.bytes_mut()[8..16].fill(0);
    }

    /// Room for the plugin to write its output sequence.
    pub fn prepare_output(&mut self) {
        let room = (self.capacity() - HEADER) as u32;
        self.set_header(room, Urids::get().atom_chunk);
    }

    /// Append an event (events must come in time order); `false`: full.
    pub fn push(&mut self, frames: i64, type_: u32, body: &[u8]) -> bool {
        let size = self.header().size as usize;
        let at = HEADER + size;
        let need = EVENT + pad8(body.len());
        if at + need > self.capacity() {
            return false;
        }
        let b = self.bytes_mut();
        b[at..at + 8].copy_from_slice(&frames.to_ne_bytes());
        b[at + 8..at + 12].copy_from_slice(&(body.len() as u32).to_ne_bytes());
        b[at + 12..at + 16].copy_from_slice(&type_.to_ne_bytes());
        b[at + 16..at + 16 + body.len()].copy_from_slice(body);
        b[at + 16 + body.len()..at + need].fill(0);
        self.set_header((size + need) as u32, Urids::get().atom_sequence);
        true
    }

    /// Append an atom object event (`otype`, properties as (key, value
    /// type, value bytes)); `false`: full.
    pub fn push_object(&mut self, frames: i64, otype: u32, props: &[(u32, u32, &[u8])]) -> bool {
        // Object body: id, otype; each property: key, context, atom, value.
        let body_len: usize = 8 + props
            .iter()
            .map(|(_, _, v)| 16 + pad8(v.len()))
            .sum::<usize>();
        let size = self.header().size as usize;
        let at = HEADER + size;
        let need = EVENT + body_len;
        if at + need > self.capacity() {
            return false;
        }
        let u = Urids::get();
        let b = self.bytes_mut();
        b[at..at + 8].copy_from_slice(&frames.to_ne_bytes());
        b[at + 8..at + 12].copy_from_slice(&(body_len as u32).to_ne_bytes());
        b[at + 12..at + 16].copy_from_slice(&u.atom_object.to_ne_bytes());
        let mut p = at + 16;
        b[p..p + 4].copy_from_slice(&0u32.to_ne_bytes());
        b[p + 4..p + 8].copy_from_slice(&otype.to_ne_bytes());
        p += 8;
        for (key, type_, value) in props {
            b[p..p + 4].copy_from_slice(&key.to_ne_bytes());
            b[p + 4..p + 8].copy_from_slice(&0u32.to_ne_bytes());
            b[p + 8..p + 12].copy_from_slice(&(value.len() as u32).to_ne_bytes());
            b[p + 12..p + 16].copy_from_slice(&type_.to_ne_bytes());
            b[p + 16..p + 16 + value.len()].copy_from_slice(value);
            let end = p + 16 + pad8(value.len());
            b[p + 16 + value.len()..end].fill(0);
            p = end;
        }
        self.set_header((size + need) as u32, u.atom_sequence);
        true
    }

    /// The events of the sequence a plugin wrote: (frames, type, body).
    pub fn events(&self) -> Events<'_> {
        let h = self.header();
        let end = if h.type_ == Urids::get().atom_sequence {
            (HEADER + h.size as usize).min(self.capacity())
        } else {
            0
        };
        Events {
            bytes: self.bytes(),
            at: HEADER + BODY,
            end,
        }
    }
}

pub struct Events<'a> {
    bytes: &'a [u8],
    at: usize,
    end: usize,
}

impl<'a> Iterator for Events<'a> {
    type Item = (i64, u32, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.at + EVENT > self.end {
            return None;
        }
        let b = self.bytes;
        let at = self.at;
        let frames = i64::from_ne_bytes(b[at..at + 8].try_into().ok()?);
        let size = u32::from_ne_bytes(b[at + 8..at + 12].try_into().ok()?) as usize;
        let type_ = u32::from_ne_bytes(b[at + 12..at + 16].try_into().ok()?);
        let body_end = at + EVENT + size;
        if body_end > self.end {
            return None;
        }
        self.at = at + EVENT + pad8(size);
        Some((frames, type_, &b[at + EVENT..body_end]))
    }
}

fn as_bytes(words: &[u64]) -> &[u8] {
    // SAFETY: every byte pattern is a valid u8, and u8 has the smallest
    // alignment, so the whole slice is the middle part.
    let (prefix, bytes, _) = unsafe { words.align_to::<u8>() };
    debug_assert!(prefix.is_empty());
    bytes
}

fn as_bytes_mut(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: as in `as_bytes`; any bytes written make valid u64s.
    let (prefix, bytes, _) = unsafe { words.align_to_mut::<u8>() };
    debug_assert!(prefix.is_empty());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_holds_events_in_order_and_objects() {
        let u = Urids::get();
        let mut b = AtomBuffer::new(256);
        b.clear_input();
        assert!(b.push(3, u.midi_event, &[0x90, 60, 100]));
        assert!(b.push(10, u.midi_event, &[0x80, 60, 0]));
        assert!(b.push_object(
            0,
            u.time_position,
            &[(u.time_speed, u.atom_float, &1.0f32.to_ne_bytes())]
        ));
        let ev: Vec<_> = b.events().collect();
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0], (3, u.midi_event, &[0x90, 60, 100][..]));
        assert_eq!(ev[1].0, 10);
        assert_eq!(ev[2].1, u.atom_object);
        // Object body: id 0, otype, one property of 16 + 8 bytes.
        assert_eq!(ev[2].2.len(), 8 + 16 + 8);
        // Full: refused.
        let mut small = AtomBuffer::new(40);
        small.clear_input();
        assert!(small.push(0, u.midi_event, &[1, 2, 3]));
        assert!(!small.push(0, u.midi_event, &[1, 2, 3]));
        // An output buffer offers its room as a chunk.
        b.prepare_output();
        assert_eq!(b.events().count(), 0);
    }
}
