//! Audio on the media clock: a ring of frames indexed by media frame (PTP
//! time in samples), where the network side and the audio side meet.
//!
//! A receiver writes each packet's frames at the media frames their RTP
//! timestamp names; the audio side reads the frames whose time has come
//! (now less the link offset). A sender's ring is the other way round.
//! Each slot carries the media frame it holds, so a frame that never
//! came (a lost packet), or one from a lap ago, reads as silence and is
//! counted — no sequence bookkeeping, and the two sides never wait for
//! each other. One writer and one reader; samples and tags are atomics
//! (plain loads and stores on the machines we run on), the tag stored
//! after the samples (release) and checked before them (acquire).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// A media-clock ring (see the module docs).
#[derive(Debug)]
pub struct MediaRing {
    channels: usize,
    mask: usize,
    /// Frame-major samples (f32 bits).
    samples: Box<[AtomicU32]>,
    /// Per slot: the media frame it holds + 1 (0: never written).
    tags: Box<[AtomicU64]>,
}

impl MediaRing {
    /// A ring of at least `frames` frames (rounded up to a power of two).
    pub fn new(channels: usize, frames: usize) -> Self {
        let len = frames.max(64).next_power_of_two();
        let channels = channels.max(1);
        Self {
            channels,
            mask: len - 1,
            samples: (0..len * channels).map(|_| AtomicU32::new(0)).collect(),
            tags: (0..len).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames it holds.
    pub fn len(&self) -> usize {
        self.mask + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Write `frames` frames from media frame `start`, sample `(frame,
    /// channel)` from `sample`. Realtime safe.
    #[inline]
    pub fn write(&self, start: u64, frames: usize, mut sample: impl FnMut(usize, usize) -> f32) {
        for i in 0..frames {
            let frame = start + i as u64;
            let slot = (frame as usize) & self.mask;
            let base = slot * self.channels;
            for c in 0..self.channels {
                self.samples[base + c].store(sample(i, c).to_bits(), Ordering::Relaxed);
            }
            self.tags[slot].store(frame + 1, Ordering::Release);
        }
    }

    /// Whether media frame `frame` is there.
    #[inline]
    pub fn has(&self, frame: u64) -> bool {
        self.tags[(frame as usize) & self.mask].load(Ordering::Acquire) == frame + 1
    }

    /// Channel `channel` of `out.len()` frames from media frame `start`
    /// into `out` (silence where a frame is missing). Returns the frames
    /// missing. Realtime safe.
    #[inline]
    pub fn read_channel(&self, start: u64, channel: usize, out: &mut [f32]) -> usize {
        let mut missing = 0;
        let c = channel.min(self.channels - 1);
        for (i, o) in out.iter_mut().enumerate() {
            let frame = start + i as u64;
            let slot = (frame as usize) & self.mask;
            let tag = &self.tags[slot];
            if tag.load(Ordering::Acquire) == frame + 1 {
                *o = f32::from_bits(self.samples[slot * self.channels + c].load(Ordering::Relaxed));
                // Overwritten meanwhile (a writer a lap ahead): not ours.
                if tag.load(Ordering::Acquire) != frame + 1 {
                    *o = 0.0;
                    missing += 1;
                }
            } else {
                *o = 0.0;
                missing += 1;
            }
        }
        missing
    }

    /// Every channel of `frames` frames from `start`, given to `put(frame,
    /// channel, value)` (silence where missing). Returns the frames
    /// missing. Realtime safe.
    #[inline]
    pub fn read(&self, start: u64, frames: usize, mut put: impl FnMut(usize, usize, f32)) -> usize {
        let mut missing = 0;
        for i in 0..frames {
            let frame = start + i as u64;
            let slot = (frame as usize) & self.mask;
            let base = slot * self.channels;
            if self.tags[slot].load(Ordering::Acquire) == frame + 1 {
                for c in 0..self.channels {
                    put(
                        i,
                        c,
                        f32::from_bits(self.samples[base + c].load(Ordering::Relaxed)),
                    );
                }
            } else {
                missing += 1;
                for c in 0..self.channels {
                    put(i, c, 0.0);
                }
            }
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_read_where_they_were_written() {
        let ring = MediaRing::new(2, 100);
        assert_eq!(ring.len(), 128);
        let start = 81_600_000_000_123u64;
        ring.write(start, 48, |i, c| (i * 2 + c) as f32);
        let mut out = [9.0f32; 60];
        // Ten frames before what was written are missing, as are the two
        // after it.
        let missing = ring.read_channel(start - 10, 1, &mut out);
        assert_eq!(missing, 12);
        assert_eq!(out[..10], [0.0; 10]);
        assert_eq!(out[10], 1.0);
        assert_eq!(out[57], 95.0);
        assert_eq!(out[58], 0.0);
        // A lap later the same slots hold other frames: old ones read as
        // missing.
        ring.write(start + 128, 4, |_, _| 7.0);
        assert!(!ring.has(start));
        assert!(ring.has(start + 128));
        let mut got = Vec::new();
        let m = ring.read(start, 3, |i, c, v| got.push((i, c, v)));
        assert_eq!(m, 3);
        assert!(got.iter().all(|&(_, _, v)| v == 0.0));
    }
}
