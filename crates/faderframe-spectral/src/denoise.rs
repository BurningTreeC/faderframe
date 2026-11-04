//! "Learn Noise and Remove": a `Denoise` edit's noise is learned from its
//! shape's stretch of the source as recorded (noise alone, chosen by the
//! user), per channel and bin, then taken out of the whole source by the
//! De-noise device's DSP ([`faderframe_restore::denoise`], its Balanced
//! frame of about 43 ms), only in the shape's band (with the frequency
//! feather; the edit's time extent is where it learned, not where it
//! works).
//!
//! A source is de-noised in chunks with a few seconds of run-in before each
//! (the gains' state settles long before), so a long clip never sits in
//! memory whole.

use faderframe_project::spectral::SpectralEdit;
use faderframe_restore::denoise::{Denoiser, Settings};
use faderframe_restore::stft::{C64, Stft};

use crate::Read;

/// Seconds de-noised before each chunk and thrown away (the gains settle).
pub const RUN_IN_SECONDS: f64 = 4.0;

/// The frame length at `rate` (the device's Balanced mode).
pub fn frame(rate: f64) -> usize {
    faderframe_restore::frame_for(2048.0 / 48_000.0, rate)
}

/// The noise per bin of each channel (per sample units) where `edit`'s
/// shape is: the mean power of the frames whose centres fall in it (one
/// frame round it when it is shorter than a frame).
pub fn learn(
    edit: &SpectralEdit,
    channels: usize,
    rate: f64,
    read: &mut Read<'_>,
) -> Result<Vec<Vec<f64>>, String> {
    let mut stft = Stft::new(frame(rate));
    let (n, hop, bins) = (stft.n, stft.hop, stft.bins);
    let (a, b) = edit.shape.frames();
    let (a, b) = if b - a < n as i64 {
        let mid = (a + b) / 2;
        (mid, mid + 1)
    } else {
        (a + n as i64 / 2, b - n as i64 / 2 + 1)
    };
    let first = a - n as i64 / 2;
    let len = (b - a).max(1) as usize + n;
    let mut buffer = vec![vec![0.0f32; len]; channels];
    read(first, &mut buffer)?;
    let mut noise = vec![vec![0.0f64; bins]; channels];
    let mut spectrum = vec![C64::new(0.0, 0.0); bins];
    let mut frame_buf = vec![0.0f64; n];
    let scale = stft.power_scale();
    let mut count = 0usize;
    let mut s = 0usize;
    while s + n <= len {
        for (c, x) in buffer.iter().enumerate() {
            for (f, v) in frame_buf.iter_mut().zip(&x[s..s + n]) {
                *f = f64::from(*v);
            }
            stft.forward(&frame_buf, &mut spectrum);
            for (acc, z) in noise[c].iter_mut().zip(&spectrum) {
                *acc += z.norm_sqr() / scale;
            }
        }
        count += 1;
        s += hop;
    }
    for ch in &mut noise {
        for v in ch.iter_mut() {
            *v /= count.max(1) as f64;
        }
    }
    Ok(noise)
}

/// How much each bin belongs to the edit's band: 1 inside, a raised cosine
/// over the frequency feather outside.
pub fn band_weights(edit: &SpectralEdit, rate: f64, bins: usize) -> Vec<f64> {
    let (lo, hi) = edit.shape.band();
    let (lo, hi) = (
        f64::from(lo).max(1e-3).log2(),
        f64::from(hi).max(1e-3).log2(),
    );
    let feather = f64::from(edit.feather_st.max(0.0)) / 12.0;
    let n = (bins.max(2) - 1) * 2;
    (0..bins)
        .map(|k| {
            let f = (k as f64 * rate / n as f64).max(1e-3).log2();
            let d = if f < lo {
                lo - f
            } else if f > hi {
                f - hi
            } else {
                0.0
            };
            if d <= 0.0 {
                1.0
            } else if feather <= 0.0 || d >= feather {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * d / feather).cos())
            }
        })
        .collect()
}

/// De-noises channels of audio with learned noise.
pub struct Pass {
    stft: Stft,
    settings: Settings,
    rate: f64,
    spectrum: Vec<C64>,
    frame: Vec<f64>,
    power: Vec<f64>,
    gains: Vec<f64>,
    acc: Vec<f64>,
}

impl Pass {
    /// At `rate`, taking at most `reduction_db`.
    pub fn new(rate: f64, reduction_db: f32) -> Self {
        let stft = Stft::new(frame(rate));
        let bins = stft.bins;
        let n = stft.n;
        Self {
            settings: Settings {
                reduction_db: f64::from(reduction_db.abs()),
                ..Settings::default()
            },
            rate,
            spectrum: vec![C64::new(0.0, 0.0); bins],
            frame: vec![0.0; n],
            power: vec![0.0; bins],
            gains: vec![1.0; bins],
            acc: Vec::new(),
            stft,
        }
    }

    pub fn bins(&self) -> usize {
        self.stft.bins
    }

    /// De-noise `x` (one channel, from its start: the gains start fresh)
    /// against `noise` per bin, `weights` per bin from the band.
    pub fn run(&mut self, x: &mut [f32], noise: &[f64], weights: &[f64]) {
        let (n, hop, bins) = (self.stft.n, self.stft.hop, self.stft.bins);
        let len = x.len();
        let mut den = Denoiser::new(bins, hop, self.rate);
        self.acc.clear();
        self.acc.resize(len + 2 * n, 0.0);
        let scale = self.stft.power_scale();
        // Frames from a frame before the start (zeros outside) to past the
        // end: every sample gets all four of its frames.
        let mut s = -(n as i64);
        while s < len as i64 {
            for (i, f) in self.frame.iter_mut().enumerate() {
                let at = s + i as i64;
                *f = if at >= 0 && (at as usize) < len {
                    f64::from(x[at as usize])
                } else {
                    0.0
                };
            }
            self.stft.forward(&self.frame, &mut self.spectrum);
            for (p, z) in self.power.iter_mut().zip(&self.spectrum) {
                *p = z.norm_sqr() / scale;
            }
            den.gains(&self.power, noise, &self.settings, &mut self.gains);
            for ((z, g), w) in self.spectrum.iter_mut().zip(&self.gains).zip(weights) {
                *z *= 1.0 - w * (1.0 - g);
            }
            let at = (s + n as i64) as usize;
            self.stft
                .inverse_add(&mut self.spectrum, &mut self.acc[at..at + n]);
            s += hop as i64;
        }
        for (i, v) in x.iter_mut().enumerate() {
            *v = self.acc[i + n] as f32;
        }
    }
}
