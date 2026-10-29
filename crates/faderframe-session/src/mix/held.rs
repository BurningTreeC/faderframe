//! Signals held from a render, so the phase and timing pairs ([`super::extra`])
//! read their excerpts without rendering the mix again.
//!
//! A render of the analysis keeps each tapped signal it measures (and each
//! source it renders) here as well, when the analysis' share of memory
//! allows ([`HOLD_BYTES`]): 16 bits a sample with a scale per [`BLOCK`]
//! frames of each channel — the block's peak at full scale, so what is lost
//! lies about 96 dB under the block's own peak, far below anything a pair
//! measurement reads. Signals are held under the same key as their profile
//! (the signal's fingerprint, the range, the rate …): the session keeps the
//! newest of them up to [`KEEP_BYTES`], so after a change the pairs of an
//! unchanged track find its audio as the last analysis rendered it, and only
//! the tracks the change reached are rendered again.

use super::CacheKey;
use std::sync::Arc;

/// Frames of a channel sharing one scale.
pub(super) const BLOCK: usize = 1024;
/// Signals held during one analysis at most (bytes).
pub const HOLD_BYTES: usize = 512 << 20;
/// Signals kept between analyses (bytes; least recently used out).
pub const KEEP_BYTES: usize = 192 << 20;

/// A signal held at 16 bits (see the module docs).
#[derive(Debug)]
pub(crate) struct HeldAudio {
    frames: usize,
    samples: Vec<Vec<i16>>,
    /// Per channel, one scale per block.
    scales: Vec<Vec<f32>>,
    /// The block being gathered, per channel.
    pending: Vec<Vec<f32>>,
}

impl HeldAudio {
    /// Room for `frames` frames of `channels` channels.
    pub(super) fn new(channels: usize, frames: usize) -> Self {
        let channels = channels.max(1);
        Self {
            frames: 0,
            samples: (0..channels).map(|_| Vec::with_capacity(frames)).collect(),
            scales: (0..channels)
                .map(|_| Vec::with_capacity(frames.div_ceil(BLOCK)))
                .collect(),
            pending: (0..channels).map(|_| Vec::with_capacity(BLOCK)).collect(),
        }
    }

    /// What `frames` frames of `channels` channels take.
    pub(super) fn bytes_for(channels: usize, frames: usize) -> usize {
        channels.max(1) * (2 * frames + 4 * frames.div_ceil(BLOCK))
    }

    pub(super) fn bytes(&self) -> usize {
        Self::bytes_for(self.samples.len(), self.frames)
    }

    pub(super) fn channels(&self) -> usize {
        self.samples.len()
    }

    pub(super) fn frames(&self) -> usize {
        self.frames
    }

    /// The next frames (every channel, as a render hands them).
    pub(super) fn push(&mut self, piece: &[&[f32]]) {
        let n = piece.first().map_or(0, |c| c.len());
        let mut done = 0;
        while done < n {
            let have = self.pending[0].len();
            let take = (BLOCK - have).min(n - done);
            for (c, p) in self.pending.iter_mut().enumerate() {
                match piece.get(c) {
                    Some(src) => p.extend_from_slice(&src[done..done + take]),
                    None => p.extend(std::iter::repeat_n(0.0, take)),
                }
            }
            done += take;
            if have + take == BLOCK {
                self.flush();
            }
        }
    }

    /// Hold what is still gathered (the last, short block).
    pub(super) fn finish(mut self) -> Self {
        if !self.pending[0].is_empty() {
            self.flush();
        }
        self
    }

    fn flush(&mut self) {
        let n = self.pending[0].len();
        for ((p, s), q) in self
            .pending
            .iter_mut()
            .zip(&mut self.scales)
            .zip(&mut self.samples)
        {
            // Invalid samples are held as silence (the health check reports
            // them; a pair measurement cannot read them anyway).
            let peak = p
                .iter()
                .filter(|v| v.is_finite())
                .fold(0.0f32, |m, v| m.max(v.abs()));
            let scale = peak / f32::from(i16::MAX);
            s.push(scale);
            let inv = if scale > 0.0 { 1.0 / scale } else { 0.0 };
            q.extend(p.iter().map(|v| {
                if v.is_finite() {
                    (v * inv)
                        .round()
                        .clamp(-f32::from(i16::MAX), f32::from(i16::MAX)) as i16
                } else {
                    0
                }
            }));
            p.clear();
        }
        self.frames += n;
    }

    /// `frames` frames from `start` (silence past the end).
    pub(super) fn excerpt(&self, start: usize, frames: usize) -> Vec<Vec<f32>> {
        self.samples
            .iter()
            .zip(&self.scales)
            .map(|(q, s)| {
                (start..start + frames)
                    .map(|i| match q.get(i) {
                        Some(&v) => f32::from(v) * s[i / BLOCK],
                        None => 0.0,
                    })
                    .collect()
            })
            .collect()
    }
}

/// Signals kept between analyses (see the module docs).
#[derive(Default)]
pub(crate) struct HeldStore {
    entries: Vec<(CacheKey, Arc<HeldAudio>, u64)>,
    tick: u64,
}

impl HeldStore {
    /// Every signal kept (for a job to look its signals up in).
    pub(super) fn snapshot(&self) -> Vec<(CacheKey, Arc<HeldAudio>)> {
        self.entries
            .iter()
            .map(|e| (e.0, Arc::clone(&e.1)))
            .collect()
    }

    /// A job used these.
    pub(super) fn touch(&mut self, keys: &[CacheKey]) {
        for k in keys {
            self.tick += 1;
            let tick = self.tick;
            if let Some(e) = self.entries.iter_mut().find(|e| e.0 == *k) {
                e.2 = tick;
            }
        }
    }

    /// Keep `audio` (the oldest go when there is no room).
    pub(super) fn put(&mut self, key: CacheKey, audio: Arc<HeldAudio>) {
        self.tick += 1;
        self.entries.retain(|e| e.0 != key);
        self.entries.push((key, audio, self.tick));
        let mut bytes: usize = self.entries.iter().map(|e| e.1.bytes()).sum();
        while bytes > KEEP_BYTES {
            let Some(i) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.2)
                .map(|(i, _)| i)
            else {
                break;
            };
            bytes -= self.entries.remove(i).1.bytes();
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_signal_comes_back_within_the_blocks_resolution() {
        let rate = 48_000.0;
        let n = 10_000;
        let left: Vec<f32> = (0..n)
            .map(|i| (0.6 * (std::f64::consts::TAU * 440.0 * i as f64 / rate).sin()) as f32)
            .collect();
        // Quiet, with a loud stretch and an invalid sample.
        let mut right: Vec<f32> = (0..n)
            .map(|i| (1e-3 * (std::f64::consts::TAU * 97.0 * i as f64 / rate).cos()) as f32)
            .collect();
        right[5000] = f32::NAN;
        let mut h = HeldAudio::new(2, n);
        // Pieces of odd sizes, as renders give them.
        let mut at = 0;
        while at < n {
            let end = (at + 777).min(n);
            h.push(&[&left[at..end], &right[at..end]]);
            at = end;
        }
        let h = h.finish();
        assert_eq!(h.frames(), n);
        assert_eq!(h.channels(), 2);
        // Half of what the samples take as floats, and a scale a block.
        assert_eq!(h.bytes(), 2 * (2 * n + 4 * n.div_ceil(BLOCK)));
        let back = h.excerpt(100, 9_000);
        for (got, want) in back.iter().zip([&left, &right]) {
            for (i, (g, w)) in got.iter().zip(&want[100..9_100]).enumerate() {
                let w = if w.is_finite() { *w } else { 0.0 };
                // Each block's peak at full scale: 96 dB under it.
                let peak = want[((100 + i) / BLOCK) * BLOCK..]
                    .iter()
                    .take(BLOCK)
                    .filter(|v| v.is_finite())
                    .fold(0.0f32, |m, v| m.max(v.abs()));
                assert!((g - w).abs() <= peak / 32767.0, "{i}: {g} {w}");
            }
        }
        // Past the end: silence.
        let tail = h.excerpt(n - 10, 20);
        assert!(tail[0][10..].iter().all(|v| *v == 0.0));
    }
}
