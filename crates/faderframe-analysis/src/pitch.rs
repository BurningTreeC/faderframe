//! Pitch detection: McLeod's method (the normalised square difference
//! function of "A Smarter Way to Find Pitch"). Over a window of up to 4096
//! frames, the autocorrelation (by FFT) normalised by the energy of the
//! overlapping parts; the first peak within 90 % of the highest, refined by
//! a parabola, is the period, and its height the clarity. The Tuner shows
//! it live; [`root_key`] finds the note a recorded sample plays.

use crate::fft;

/// Frames analysed.
pub const FRAMES: usize = 4096;

/// A pitch and how clear it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pitch {
    pub freq: f64,
    pub clarity: f64,
}

/// Work buffers for [`detect`].
pub struct Detector {
    re: Vec<f64>,
    im: Vec<f64>,
    nsdf: Vec<f64>,
    /// The peaks found (room for every one: no allocation per call).
    peaks: Vec<(usize, f64)>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Self {
            re: vec![0.0; 2 * FRAMES],
            im: vec![0.0; 2 * FRAMES],
            nsdf: vec![0.0; FRAMES],
            peaks: Vec::with_capacity(FRAMES / 2 + 1),
        }
    }

    /// The pitch of `x` (at most `FRAMES` long) at `rate`, from 25 Hz to
    /// 4.2 kHz.
    pub fn detect(&mut self, x: &[f32], rate: f64) -> Option<Pitch> {
        let n = x.len().min(FRAMES);
        let x = &x[x.len() - n..];
        let mean = x.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
        let energy: f64 = x.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum();
        if energy / (n as f64) < 1e-6 {
            return None;
        }
        // The autocorrelation: the power spectrum's transform.
        let size = (2 * n).next_power_of_two();
        self.re.resize(size, 0.0);
        self.im.resize(size, 0.0);
        self.re.fill(0.0);
        self.im.fill(0.0);
        for (r, v) in self.re.iter_mut().zip(x) {
            *r = f64::from(*v) - mean;
        }
        fft(&mut self.re, &mut self.im);
        for i in 0..size {
            self.re[i] = self.re[i].powi(2) + self.im[i].powi(2);
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        let r0 = self.re[0] / size as f64;
        // The overlap's energy, shrinking with the lag.
        let min_lag = (rate / 4_200.0).floor() as usize;
        let max_lag = ((rate / 25.0).ceil() as usize).min(n - 2);
        let mut m = 2.0 * r0;
        self.nsdf.resize(n, 0.0);
        self.nsdf[0] = 1.0;
        for t in 1..=max_lag {
            let a = f64::from(x[t - 1]) - mean;
            let b = f64::from(x[n - t]) - mean;
            m -= a * a + b * b;
            let r = self.re[t] / size as f64;
            self.nsdf[t] = if m > 1e-12 { 2.0 * r / m } else { 0.0 };
        }
        // The peaks between the positive zero crossings.
        let mut peaks = std::mem::take(&mut self.peaks);
        peaks.clear();
        let mut t = 1;
        while t < max_lag && self.nsdf[t] > 0.0 {
            t += 1;
        }
        while t < max_lag {
            while t < max_lag && self.nsdf[t] <= 0.0 {
                t += 1;
            }
            let mut best = (t, f64::MIN);
            while t < max_lag && self.nsdf[t] > 0.0 {
                if self.nsdf[t] > best.1 {
                    best = (t, self.nsdf[t]);
                }
                t += 1;
            }
            if best.1 > 0.0 && best.0 >= min_lag {
                peaks.push(best);
            }
        }
        let highest = peaks.iter().map(|p| p.1).fold(0.0, f64::max);
        let found = peaks.iter().find(|p| p.1 >= 0.9 * highest).copied();
        self.peaks = peaks;
        let (k, v) = found?;
        // A parabola through the peak and its neighbours.
        let (a, b, c) = (self.nsdf[k - 1], v, self.nsdf[(k + 1).min(max_lag)]);
        let d = a - 2.0 * b + c;
        let shift = if d.abs() > 1e-12 {
            0.5 * (a - c) / d
        } else {
            0.0
        };
        let lag = k as f64 + shift.clamp(-0.5, 0.5);
        let clarity = b - 0.25 * (a - c) * shift;
        Some(Pitch {
            freq: rate / lag,
            clarity,
        })
    }
}

/// A note (MIDI number, may be fractional) for a frequency at a reference.
pub fn note_at(freq: f64, reference: f64) -> f64 {
    69.0 + 12.0 * (freq / reference).log2()
}

/// The note (MIDI number with its cents as the fraction) a sample of
/// `x` at `rate` plays: the median pitch of the clear windows over its
/// first four seconds, once the attack has passed. `None` when too little
/// of it has a clear pitch (drums, noise, chords).
pub fn root_key(x: &[f32], rate: f64) -> Option<f64> {
    let hop = FRAMES / 2;
    let limit = x.len().min((rate * 4.0) as usize);
    let x = &x[..limit];
    if x.len() < FRAMES / 2 {
        return None;
    }
    let mut d = Detector::new();
    let mut found = Vec::new();
    let mut windows = 0usize;
    let mut at = 0;
    loop {
        let w = &x[at..(at + FRAMES).min(x.len())];
        let rms = (w.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / w.len() as f64).sqrt();
        // Skip silence (under −50 dBFS).
        if rms > 0.003 {
            windows += 1;
            if let Some(p) = d.detect(w, rate)
                && p.clarity >= 0.85
            {
                found.push(note_at(p.freq, 440.0));
            }
        }
        at += hop;
        if at + FRAMES / 2 > x.len() {
            break;
        }
    }
    if found.len() < 2 || found.len() * 2 < windows {
        return None;
    }
    found.sort_by(f64::total_cmp);
    let median = found[found.len() / 2];
    // Most windows must agree on the note (a glide or a chord does not).
    let agree = found.iter().filter(|n| (**n - median).abs() < 0.5).count();
    (agree * 3 >= found.len() * 2).then_some(median)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: f64, seconds: f64) -> Vec<f32> {
        (0..(rate * seconds) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                // Harmonics and a decay, like a plucked string.
                let env = (-t * 1.5).exp();
                ((0.5 * (std::f64::consts::TAU * freq * t).sin()
                    + 0.25 * (std::f64::consts::TAU * 2.0 * freq * t).sin()
                    + 0.12 * (std::f64::consts::TAU * 3.0 * freq * t).sin())
                    * env) as f32
            })
            .collect()
    }

    #[test]
    fn a_sample_plays_its_note() {
        let rate = 48_000.0;
        // A2, and C4 a fifth of a semitone sharp.
        let a = root_key(&tone(110.0, rate, 2.0), rate).unwrap();
        assert!((a - 45.0).abs() < 0.05, "{a}");
        let c = root_key(&tone(261.63 * 2f64.powf(0.2 / 12.0), rate, 1.0), rate).unwrap();
        assert!((c - 60.2).abs() < 0.05, "{c}");
    }

    #[test]
    fn noise_and_silence_have_no_note() {
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect();
        assert_eq!(root_key(&noise, 48_000.0), None);
        assert_eq!(root_key(&vec![0.0; 48_000], 48_000.0), None);
        assert_eq!(root_key(&[0.1; 100], 48_000.0), None);
    }
}
