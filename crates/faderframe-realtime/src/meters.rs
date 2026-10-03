use crate::AtomicF32;
use std::sync::atomic::Ordering;

/// A contiguous range of meter channels in a [`MeterBank`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeterRange {
    pub first: u32,
    pub channels: u16,
}

impl MeterRange {
    #[inline]
    pub fn channel(self, ch: usize) -> Option<u32> {
        (ch < self.channels as usize).then(|| self.first + ch as u32)
    }
}

#[derive(Debug, Default)]
struct MeterChannel {
    /// Max absolute sample value since the last read.
    peak: AtomicF32,
    /// Max per-block mean square since the last read.
    mean_square: AtomicF32,
}

/// One channel's values as consumed by the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    /// Linear sample peak since the previous read.
    pub peak: f32,
    /// Linear RMS (of the loudest block) since the previous read.
    pub rms: f32,
}

/// Lock-free meter values flowing from the audio thread to the UI.
///
/// The audio thread *accumulates* (atomic max) and the single control-side
/// reader *consumes* (atomic swap to zero), so no peak is ever lost between
/// UI frames regardless of the relative rates. The GUI never touches DSP
/// state directly.
#[derive(Debug)]
pub struct MeterBank {
    channels: Box<[MeterChannel]>,
}

impl MeterBank {
    pub fn new(capacity: u32) -> Self {
        Self {
            channels: (0..capacity).map(|_| MeterChannel::default()).collect(),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.channels.len() as u32
    }

    /// Accumulate a block's statistics (audio thread).
    #[inline]
    pub fn accumulate(&self, index: u32, peak: f32, mean_square: f32) {
        if let Some(c) = self.channels.get(index as usize) {
            c.peak.fetch_max_non_negative(peak, Ordering::Relaxed);
            c.mean_square
                .fetch_max_non_negative(mean_square, Ordering::Relaxed);
        }
    }

    /// Compute peak/mean-square of `samples` and accumulate (audio thread).
    #[inline]
    pub fn measure(&self, index: u32, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut peak = 0.0f32;
        let mut sum = 0.0f32;
        for &s in samples {
            peak = peak.max(s.abs());
            sum += s * s;
        }
        self.accumulate(index, peak, sum / samples.len() as f32);
    }

    /// Consume the accumulated values (control thread).
    #[inline]
    pub fn take(&self, index: u32) -> MeterReading {
        match self.channels.get(index as usize) {
            Some(c) => MeterReading {
                peak: c.peak.swap(0.0, Ordering::Relaxed),
                rms: c.mean_square.swap(0.0, Ordering::Relaxed).sqrt(),
            },
            None => MeterReading::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_until_taken() {
        let bank = MeterBank::new(4);
        bank.measure(1, &[0.1, -0.5, 0.2]);
        bank.measure(1, &[0.3, 0.3]);
        let r = bank.take(1);
        assert_eq!(r.peak, 0.5);
        // Loudest block: mean square of [0.1,-0.5,0.2] = 0.1
        assert!((r.rms - 0.1f32.sqrt()).abs() < 1e-6);
        assert_eq!(bank.take(1), MeterReading::default());
        bank.measure(99, &[1.0]);
    }
}
