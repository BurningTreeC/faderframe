//! FFT spectrum of the mid signal: Hann-windowed frames with 75 % overlap,
//! smoothed magnitudes and a decaying peak hold, in dBFS.

use std::f64::consts::PI;

/// In-place iterative radix-2 FFT (`re`/`im` of a power-of-two length).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f64;
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

#[derive(Clone, Debug)]
pub struct Spectrum {
    rate: f64,
    size: usize,
    window: Vec<f64>,
    /// Pending input (mid), oldest first.
    input: Vec<f32>,
    re: Vec<f64>,
    im: Vec<f64>,
    /// Smoothed magnitude per bin (dBFS).
    level: Vec<f32>,
    peak: Vec<f32>,
}

/// Floor of the display (dBFS).
pub const FLOOR_DB: f32 = -120.0;

impl Spectrum {
    pub fn new(sample_rate: u32, size: usize) -> Self {
        let size = size.next_power_of_two().max(256);
        let window: Vec<f64> = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / size as f64).cos())
            .collect();
        Self {
            rate: sample_rate.max(8000) as f64,
            size,
            window,
            input: Vec::with_capacity(size * 2),
            re: vec![0.0; size],
            im: vec![0.0; size],
            level: vec![FLOOR_DB; size / 2],
            peak: vec![FLOOR_DB; size / 2],
        }
    }

    pub fn reset_peaks(&mut self) {
        self.peak.fill(FLOOR_DB);
    }

    /// Feed audio; `dt` (seconds since the last call) decays the peaks.
    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        self.input
            .extend(left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)));
        let hop = self.size / 4;
        while self.input.len() >= self.size {
            self.analyse();
            self.input.drain(..hop);
        }
    }

    fn analyse(&mut self) {
        for i in 0..self.size {
            self.re[i] = self.input[i] as f64 * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        // Hann's coherent gain is 0.5: a full-scale sine reads 0 dBFS.
        let norm = 4.0 / self.size as f64;
        for b in 0..self.size / 2 {
            let mag = (self.re[b].hypot(self.im[b]) * norm).max(1e-12);
            let db = (20.0 * mag.log10()) as f32;
            let db = db.max(FLOOR_DB);
            // Fast attack, slower release.
            let l = &mut self.level[b];
            *l = if db > *l { db } else { *l + (db - *l) * 0.3 };
            self.peak[b] = self.peak[b].max(*l);
        }
    }

    /// Let peaks fall by `db`.
    pub fn decay_peaks(&mut self, db: f32) {
        for (p, l) in self.peak.iter_mut().zip(&self.level) {
            *p = (*p - db).max(*l);
        }
    }

    /// Frequency of bin `b`.
    pub fn bin_hz(&self, b: usize) -> f64 {
        b as f64 * self.rate / self.size as f64
    }

    /// `points` values (dBFS) on a log frequency axis from `lo` to `hi` Hz:
    /// the loudest bin in each point's band, interpolated where bins are
    /// sparser than points. Returns (level, peak).
    pub fn curve(&self, points: usize, lo: f64, hi: f64) -> (Vec<f32>, Vec<f32>) {
        let bins = self.level.len();
        let hz_per_bin = self.rate / self.size as f64;
        let ratio = (hi / lo).ln();
        let at = |p: f64| lo * (ratio * p).exp() / hz_per_bin;
        let sample = |v: &[f32], i: usize| {
            let a = at(i as f64 / points as f64);
            let b = at((i + 1) as f64 / points as f64);
            if b - a < 1.0 {
                // Between bins: interpolate.
                let x = ((a + b) * 0.5).clamp(0.0, (bins - 1) as f64);
                let k = x.floor() as usize;
                let t = (x - k as f64) as f32;
                let next = v[(k + 1).min(bins - 1)];
                v[k] + (next - v[k]) * t
            } else {
                let (k0, k1) = (a.round() as usize, (b.round() as usize).min(bins));
                v[k0.min(bins - 1)..k1.max(k0 + 1).min(bins)]
                    .iter()
                    .copied()
                    .fold(FLOOR_DB, f32::max)
            }
        };
        (
            (0..points).map(|i| sample(&self.level, i)).collect(),
            (0..points).map(|i| sample(&self.peak, i)).collect(),
        )
    }

    /// The loudest bin (for tests and readouts).
    pub fn loudest(&self) -> (f64, f32) {
        let (b, db) = self
            .level
            .iter()
            .enumerate()
            .skip(1)
            .fold(
                (0, FLOOR_DB),
                |m, (b, &db)| if db > m.1 { (b, db) } else { m },
            );
        (self.bin_hz(b), db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_of_an_impulse_is_flat() {
        let mut re = vec![0.0; 8];
        let mut im = vec![0.0; 8];
        re[0] = 1.0;
        fft(&mut re, &mut im);
        assert!(re.iter().all(|v| (v - 1.0).abs() < 1e-12));
        assert!(im.iter().all(|v| v.abs() < 1e-12));
    }

    #[test]
    fn a_sine_shows_at_its_frequency_and_level() {
        let rate = 48_000;
        let mut s = Spectrum::new(rate, 8192);
        let tone: Vec<f32> = (0..rate)
            .map(|i| (0.5 * (2.0 * PI * 1000.0 * i as f64 / rate as f64).sin()) as f32)
            .collect();
        s.process(&tone, &tone);
        let (hz, db) = s.loudest();
        assert!((hz - 1000.0).abs() < 10.0, "{hz}");
        assert!((db + 6.02).abs() < 1.6, "{db}");
        let (curve, peaks) = s.curve(200, 20.0, 20_000.0);
        assert_eq!(curve.len(), 200);
        let at_1k = curve[(200.0 * (1000f64 / 20.0).ln() / 1000f64.ln()) as usize];
        assert!(at_1k > -10.0, "{at_1k}");
        assert!(peaks.iter().zip(&curve).all(|(p, c)| p >= c));
    }
}
