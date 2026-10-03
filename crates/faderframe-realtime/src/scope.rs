use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Stereo audio of one source (a track's post-fader output), written on a
/// DSP thread and read by analysers on the control side.
///
/// An overwriting ring: the writer never waits, and a reader that falls
/// more than [`ScopeRing::capacity`] frames behind loses the oldest ones
/// (it is told how many). Samples are stored as `f32` bits in atomics, so
/// no `unsafe` is needed; a frame being overwritten while it is read can
/// tear, which only a reader that has fallen behind ever sees.
#[derive(Debug)]
pub struct ScopeRing {
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    mask: u64,
    /// Frames written in total (published after the samples).
    written: AtomicU64,
    /// Which source writes: an id + 1 (0: none).
    source: AtomicU64,
}

impl ScopeRing {
    /// Room for at least `frames` frames (rounded up to a power of two).
    pub fn new(frames: usize) -> Self {
        let n = frames.max(2).next_power_of_two();
        let zeros = || (0..n).map(|_| AtomicU32::new(0)).collect::<Box<[_]>>();
        Self {
            left: zeros(),
            right: zeros(),
            mask: n as u64 - 1,
            written: AtomicU64::new(0),
            source: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.left.len()
    }

    /// The writing source (`None`: nobody writes).
    #[inline]
    pub fn source(&self) -> Option<u64> {
        self.source.load(Ordering::Relaxed).checked_sub(1)
    }

    pub fn set_source(&self, id: Option<u64>) {
        self.source
            .store(id.map_or(0, |i| i + 1), Ordering::Relaxed);
    }

    /// Append frames (realtime-safe; one writer at a time).
    #[inline]
    pub fn push(&self, left: &[f32], right: &[f32]) {
        let start = self.written.load(Ordering::Relaxed);
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            let at = ((start + i as u64) & self.mask) as usize;
            self.left[at].store(l.to_bits(), Ordering::Relaxed);
            self.right[at].store(r.to_bits(), Ordering::Relaxed);
        }
        let n = left.len().min(right.len()) as u64;
        self.written.store(start + n, Ordering::Release);
    }

    /// Frames written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Append the frames written since `from` to `left`/`right`; returns
    /// the new position and how many frames were lost (overwritten before
    /// they were read).
    pub fn read_since(&self, from: u64, left: &mut Vec<f32>, right: &mut Vec<f32>) -> (u64, u64) {
        let end = self.written();
        let cap = self.capacity() as u64;
        // Keep a block of headroom: the writer may be overwriting the
        // oldest frames right now.
        let oldest = end.saturating_sub(cap - cap / 8);
        let start = from.max(oldest).min(end);
        let lost = start - from.min(start);
        for f in start..end {
            let at = (f & self.mask) as usize;
            left.push(f32::from_bits(self.left[at].load(Ordering::Relaxed)));
            right.push(f32::from_bits(self.right[at].load(Ordering::Relaxed)));
        }
        (end, lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_was_written_and_counts_losses() {
        let ring = ScopeRing::new(16);
        assert_eq!(ring.capacity(), 16);
        ring.push(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let (pos, lost) = ring.read_since(0, &mut l, &mut r);
        assert_eq!((pos, lost), (3, 0));
        assert_eq!(
            (l.as_slice(), r.as_slice()),
            (&[1.0, 2.0, 3.0][..], &[-1.0, -2.0, -3.0][..])
        );
        // Lapped: only the newest frames (minus headroom) remain.
        let block: Vec<f32> = (0..40).map(|i| i as f32).collect();
        ring.push(&block, &block);
        l.clear();
        r.clear();
        let (pos, lost) = ring.read_since(pos, &mut l, &mut r);
        assert_eq!(pos, 43);
        assert_eq!(l.len(), 14);
        assert_eq!(lost, 40 - 14);
        assert_eq!(*l.last().unwrap(), 39.0);
        ring.set_source(Some(7));
        assert_eq!(ring.source(), Some(7));
        ring.set_source(None);
        assert_eq!(ring.source(), None);
    }
}
