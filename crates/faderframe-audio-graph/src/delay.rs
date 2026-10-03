//! Delay lines used for plugin delay compensation on graph edges.

use crate::{AudioBuffer, for_each_channel_route};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use std::collections::VecDeque;

/// Fixed-delay ring buffer applied to an audio edge.
///
/// Capacity is `delay + max_block`, which is the minimum that lets a whole
/// block be written before reading the delayed block (see `process_mix`).
#[derive(Debug)]
pub(crate) struct AudioDelay {
    delay: usize,
    ring: Vec<Box<[f32]>>,
    write: usize,
}

impl AudioDelay {
    pub fn new(channels: usize, delay: usize, max_block: usize) -> Self {
        let cap = delay + max_block;
        Self {
            delay,
            ring: (0..channels)
                .map(|_| vec![0.0; cap].into_boxed_slice())
                .collect(),
            write: 0,
        }
    }

    pub fn delay(&self) -> usize {
        self.delay
    }

    /// Push `src` into the ring and add the delayed signal into `dst`.
    pub fn process_mix(&mut self, src: &AudioBuffer, dst: &mut AudioBuffer) {
        let n = src.len().min(dst.len());
        let Some(cap) = self.ring.first().map(|r| r.len()) else {
            return;
        };
        if n == 0 || n > cap {
            return;
        }
        let w = self.write;
        // Write the new block.
        for (c, ring) in self.ring.iter_mut().enumerate().take(src.num_channels()) {
            let input = src.channel(c);
            let first = (cap - w).min(n);
            ring[w..w + first].copy_from_slice(&input[..first]);
            ring[..n - first].copy_from_slice(&input[first..n]);
        }
        // Read the block that was written `delay` frames ago.
        let r = (w + cap - self.delay) % cap;
        let first = (cap - r).min(n);
        let ring = &self.ring;
        for_each_channel_route(
            src.num_channels().min(ring.len()),
            dst.num_channels(),
            |s, d, wgt| {
                let out = dst.channel_mut(d);
                let a = &ring[s][r..r + first];
                let b = &ring[s][..n - first];
                for (o, i) in out[..first].iter_mut().zip(a) {
                    *o += *i * wgt;
                }
                for (o, i) in out[first..n].iter_mut().zip(b) {
                    *o += *i * wgt;
                }
            },
        );
        self.write = (w + n) % cap;
    }
}

/// Fixed-delay queue applied to an event edge.
#[derive(Debug)]
pub(crate) struct EventDelay {
    delay: u64,
    now: u64,
    queue: VecDeque<(u64, MidiEvent)>,
    capacity: usize,
}

impl EventDelay {
    pub fn new(delay: usize, capacity: usize) -> Self {
        Self {
            delay: delay as u64,
            now: 0,
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Queue `src` events and emit those due in this block into `dst`.
    pub fn process_merge(&mut self, src: &MidiBuffer, dst: &mut MidiBuffer, frames: usize) {
        for e in src {
            if self.queue.len() < self.capacity {
                self.queue
                    .push_back((self.now + e.sample_offset as u64 + self.delay, e.event));
            }
        }
        let end = self.now + frames as u64;
        while let Some(&(due, event)) = self.queue.front() {
            if due >= end {
                break;
            }
            self.queue.pop_front();
            let offset = due.saturating_sub(self.now) as u32;
            let _ = dst.push(TimedMidiEvent::new(offset, event));
        }
        self.now = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ChannelLayout;

    #[test]
    fn audio_delay_shifts_impulse_across_blocks() {
        let block = 4;
        let mut d = AudioDelay::new(1, 6, block);
        let mut src = AudioBuffer::new(ChannelLayout::Mono, block);
        let mut dst = AudioBuffer::new(ChannelLayout::Mono, block);
        src.set_len(block);
        dst.set_len(block);
        let mut out = Vec::new();
        for b in 0..4 {
            src.clear();
            if b == 0 {
                src.channel_mut(0)[1] = 1.0;
            }
            dst.clear();
            d.process_mix(&src, &mut dst);
            out.extend_from_slice(dst.channel(0));
        }
        let pos = out.iter().position(|&v| v == 1.0);
        assert_eq!(pos, Some(7));
        assert_eq!(out.iter().filter(|&&v| v != 0.0).count(), 1);
    }

    #[test]
    fn event_delay_shifts_events() {
        let mut d = EventDelay::new(10, 16);
        let mut src = MidiBuffer::with_capacity(4);
        let mut dst = MidiBuffer::with_capacity(4);
        src.push(TimedMidiEvent::new(
            3,
            MidiEvent::NoteOn {
                channel: 0,
                key: 1,
                velocity: 1,
            },
        ))
        .unwrap();
        d.process_merge(&src, &mut dst, 8);
        assert!(dst.is_empty());
        src.clear();
        d.process_merge(&src, &mut dst, 8);
        assert_eq!(dst.as_slice()[0].sample_offset, 5); // 3 + 10 - 8
    }
}
