//! A streaming short-time Fourier transform for the spectral restorers:
//! square-root Hann windows on both sides at 75 % overlap (unity gains give
//! the input back exactly), a frame every quarter frame, `n` samples of
//! latency (all of a frame's work is done on its last sample).
//!
//! Spectra are scaled so a bin's power reads in "per sample" units: white
//! noise of variance σ² has an expected power of σ² in every bin at any
//! frame size ([`Stft::power_scale`]), which is what noise profiles are
//! kept in.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub type C64 = Complex<f64>;

/// A frame size's transforms and window.
pub struct Stft {
    pub n: usize,
    pub hop: usize,
    pub bins: usize,
    window: Vec<f64>,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    scratch_f: Vec<C64>,
    scratch_i: Vec<C64>,
    time: Vec<f64>,
}

impl Stft {
    /// For frames of `n` samples (a power of two of at least 64).
    pub fn new(n: usize) -> Self {
        let n = n.next_power_of_two().max(64);
        let mut planner = RealFftPlanner::<f64>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        Self {
            n,
            hop: n / 4,
            bins: n / 2 + 1,
            window: (0..n)
                .map(|i| (std::f64::consts::PI * i as f64 / n as f64).sin())
                .collect(),
            scratch_f: fwd.make_scratch_vec(),
            scratch_i: inv.make_scratch_vec(),
            fwd,
            inv,
            time: vec![0.0; n],
        }
    }

    /// What a bin's squared magnitude is divided by to read per sample:
    /// the window's energy (`n / 2` for the square-root Hann).
    pub fn power_scale(&self) -> f64 {
        self.n as f64 / 2.0
    }

    /// The frequency of bin `k` at `rate`.
    pub fn freq(&self, k: usize, rate: f64) -> f64 {
        k as f64 * rate / self.n as f64
    }

    /// The windowed spectrum of `frame` (`n` samples) into `out`.
    pub fn forward(&mut self, frame: &[f64], out: &mut [C64]) {
        for ((t, &x), &w) in self.time.iter_mut().zip(frame).zip(&self.window) {
            *t = x * w;
        }
        if self
            .fwd
            .process_with_scratch(&mut self.time, out, &mut self.scratch_f)
            .is_err()
        {
            out.fill(C64::new(0.0, 0.0));
        }
    }

    /// Transform `spectrum` back (it is used as scratch), window it and add
    /// it into `acc` (`n` samples).
    pub fn inverse_add(&mut self, spectrum: &mut [C64], acc: &mut [f64]) {
        // Square-root Hann twice at 75 % overlap sums to 2.
        let norm = 1.0 / (2.0 * self.n as f64);
        spectrum[0].im = 0.0;
        if let Some(last) = spectrum.last_mut() {
            last.im = 0.0;
        }
        if self
            .inv
            .process_with_scratch(spectrum, &mut self.time, &mut self.scratch_i)
            .is_err()
        {
            self.time.fill(0.0);
        }
        for ((a, t), w) in acc.iter_mut().zip(&self.time).zip(&self.window) {
            *a += t * w * norm;
        }
    }
}

/// One channel's input ring and output accumulator.
struct Lane {
    ring: Vec<f64>,
    acc: Vec<f64>,
    out: Vec<f64>,
}

/// Several channels through one frame size, sample by sample.
pub struct Streaming {
    pub stft: Stft,
    lanes: Vec<Lane>,
    /// Where the next input sample goes (the oldest).
    pos: usize,
    /// Samples into the hop.
    at: usize,
    /// Samples pushed since the last reset (up to `n`): older ones read
    /// as silence, so a reset clears nothing large.
    fresh: usize,
    frame: Vec<f64>,
    /// Each channel's spectrum of the frame just finished.
    pub spectra: Vec<Vec<C64>>,
}

impl Streaming {
    pub fn new(n: usize, channels: usize) -> Self {
        let stft = Stft::new(n);
        let (n, hop, bins) = (stft.n, stft.hop, stft.bins);
        Self {
            lanes: (0..channels)
                .map(|_| Lane {
                    ring: vec![0.0; n],
                    acc: vec![0.0; n],
                    out: vec![0.0; hop],
                })
                .collect(),
            pos: 0,
            at: 0,
            fresh: 0,
            frame: vec![0.0; n],
            spectra: (0..channels)
                .map(|_| vec![C64::new(0.0, 0.0); bins])
                .collect(),
            stft,
        }
    }

    pub fn channels(&self) -> usize {
        self.lanes.len()
    }

    /// The delay from input to output (samples).
    pub fn latency(&self) -> usize {
        self.stft.n
    }

    /// One sample in per channel (`x`), one out (`y`, `latency()` later).
    /// `true` when a frame is complete: its spectra are in
    /// [`Self::spectra`]; change them and call [`Self::finish`] before the
    /// next push.
    #[inline]
    pub fn push(&mut self, x: &[f64], y: &mut [f64]) -> bool {
        let p = self.pos;
        for (c, lane) in self.lanes.iter_mut().enumerate() {
            lane.ring[p] = x.get(c).copied().unwrap_or(0.0);
            if let Some(o) = y.get_mut(c) {
                *o = lane.out[self.at];
            }
        }
        let n = self.stft.n;
        self.pos = if p + 1 == n { 0 } else { p + 1 };
        self.fresh = (self.fresh + 1).min(n);
        self.at += 1;
        if self.at < self.stft.hop {
            return false;
        }
        self.at = 0;
        for c in 0..self.lanes.len() {
            // Oldest first; samples from before a reset are silence.
            let lane = &self.lanes[c];
            let stale = n - self.fresh;
            for (i, f) in self.frame.iter_mut().enumerate() {
                *f = if i < stale {
                    0.0
                } else {
                    lane.ring[(self.pos + i) % n]
                };
            }
            self.stft.forward(&self.frame, &mut self.spectra[c]);
        }
        true
    }

    /// The frame's (changed) spectra back into the output.
    pub fn finish(&mut self) {
        let hop = self.stft.hop;
        for (lane, spectrum) in self.lanes.iter_mut().zip(self.spectra.iter_mut()) {
            self.stft.inverse_add(spectrum, &mut lane.acc);
            lane.out.copy_from_slice(&lane.acc[..hop]);
            lane.acc.copy_within(hop.., 0);
            let n = lane.acc.len();
            lane.acc[n - hop..].fill(0.0);
        }
    }

    /// Forget the audio: the ring is not cleared (what was in it reads as
    /// silence), the output starts silent.
    pub fn reset(&mut self) {
        self.fresh = 0;
        self.at = 0;
        for lane in &mut self.lanes {
            lane.acc.fill(0.0);
            lane.out.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_spectra_give_the_input_back_a_frame_later() {
        let mut s = Streaming::new(512, 2);
        let x: Vec<f64> = (0..8000)
            .map(|n| ((n * 37 % 101) as f64 - 50.0) / 60.0)
            .collect();
        let mut out = vec![[0.0; 2]; x.len()];
        for (n, &v) in x.iter().enumerate() {
            let mut y = [0.0; 2];
            if s.push(&[v, -v], &mut y) {
                s.finish();
            }
            out[n] = y;
        }
        let lat = s.latency();
        for n in lat + 512..x.len() {
            assert!((out[n][0] - x[n - lat]).abs() < 1e-9, "at {n}");
            assert!((out[n][1] + x[n - lat]).abs() < 1e-9, "at {n}");
        }
    }

    #[test]
    fn white_noise_reads_its_variance_in_every_bin() {
        let mut stft = Stft::new(1024);
        let mut noise = crate::Noise::new(3);
        let mut spectrum = vec![C64::new(0.0, 0.0); stft.bins];
        let mut mean = vec![0.0; stft.bins];
        let frames = 400;
        for _ in 0..frames {
            let frame: Vec<f64> = (0..1024).map(|_| 0.1 * noise.next()).collect();
            stft.forward(&frame, &mut spectrum);
            for (m, c) in mean.iter_mut().zip(&spectrum) {
                *m += c.norm_sqr() / stft.power_scale() / frames as f64;
            }
        }
        // Unit-variance noise at 0.1: a variance of 0.01.
        let avg = mean[10..500].iter().sum::<f64>() / 490.0;
        assert!((avg / 0.01 - 1.0).abs() < 0.05, "{avg}");
    }
}
