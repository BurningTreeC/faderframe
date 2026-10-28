//! Long-term average spectra: the spectral balance of a whole stretch of
//! audio, for comparing a mix with a reference and for EQ Match.
//!
//! Measured as the EQ's analyser averages (its EQ Match lines): the mid
//! (`(L + R) / 2`; the mean of all channels) in Hann-windowed frames of
//! [`SIZE`] with 75 % overlap, power averaged per bin, then on a
//! logarithmic grid each point the mean power of the bins between its
//! neighbours' geometric midpoints — a full-scale sine reads 0 dB.

use crate::spectrum::fft;

/// FFT size.
pub const SIZE: usize = 8192;
/// Grid points per octave of [`Ltas::freqs`].
pub const PER_OCTAVE: f64 = 12.0;
/// The grid's ends (Hz; the top at most 45 % of the rate).
pub const LOW_HZ: f64 = 25.0;
pub const HIGH_HZ: f64 = 18_000.0;
/// The floor (dB).
pub const FLOOR: f64 = -200.0;

/// A logarithmic grid from `lo` to `hi` Hz with `per_octave` points per
/// octave (EQ Match's).
pub fn grid(lo: f64, hi: f64, per_octave: f64) -> Vec<f64> {
    let n = ((hi / lo).log2() * per_octave).ceil().max(1.0) as usize + 1;
    (0..n)
        .map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64))
        .collect()
}

/// A long-term average spectrum.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ltas {
    /// The grid (Hz).
    pub freqs: Vec<f64>,
    /// The level at each (dB).
    pub db: Vec<f64>,
    /// Seconds averaged.
    pub seconds: f64,
}

impl Ltas {
    /// Its level at `hz` (interpolated on the log grid; the ends held).
    pub fn at(&self, hz: f64) -> Option<f64> {
        let (f, d) = (&self.freqs, &self.db);
        if f.is_empty() || d.len() != f.len() {
            return None;
        }
        if hz <= f[0] {
            return Some(d[0]);
        }
        if hz >= f[f.len() - 1] {
            return Some(d[d.len() - 1]);
        }
        let i = f.partition_point(|x| *x <= hz).clamp(1, f.len() - 1);
        let (a, b) = (f[i - 1].ln(), f[i].ln());
        let t = ((hz.ln() - a) / (b - a)).clamp(0.0, 1.0);
        Some(d[i - 1] + (d[i] - d[i - 1]) * t)
    }

    /// Its levels on another grid.
    pub fn on(&self, freqs: &[f64]) -> Option<Vec<f64>> {
        freqs.iter().map(|f| self.at(*f)).collect()
    }

    /// `self − other` on `self`'s grid (`other` interpolated).
    pub fn minus(&self, other: &Ltas) -> Vec<f64> {
        self.freqs
            .iter()
            .zip(&self.db)
            .map(|(f, d)| d - other.at(*f).unwrap_or(*d))
            .collect()
    }
}

/// Accumulates the average power per bin, a frame at a time.
pub struct Averager {
    rate: f64,
    window: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    sum: Vec<f64>,
    frames: u64,
}

impl Averager {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: f64::from(rate.max(1)),
            window: (0..SIZE)
                .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / SIZE as f64).cos())
                .collect(),
            re: vec![0.0; SIZE],
            im: vec![0.0; SIZE],
            sum: vec![0.0; SIZE / 2],
            frames: 0,
        }
    }

    /// Every frame of `channels` (their mean), 75 % overlap.
    pub fn add(&mut self, channels: &[&[f32]]) {
        let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if channels.is_empty() || len < SIZE {
            return;
        }
        let k = channels.len() as f64;
        let norm = 4.0 / SIZE as f64;
        let mut start = 0;
        while start + SIZE <= len {
            for i in 0..SIZE {
                let mid: f64 = channels
                    .iter()
                    .map(|c| f64::from(c[start + i]))
                    .sum::<f64>()
                    / k;
                self.re[i] = mid * self.window[i];
                self.im[i] = 0.0;
            }
            fft(&mut self.re, &mut self.im);
            for b in 0..SIZE / 2 {
                self.sum[b] += (self.re[b].powi(2) + self.im[b].powi(2)) * norm * norm;
            }
            self.frames += 1;
            start += SIZE / 4;
        }
    }

    /// The average on the standard grid (`None`: nothing averaged).
    pub fn finish(&self) -> Option<Ltas> {
        if self.frames == 0 {
            return None;
        }
        let hi = HIGH_HZ.min(0.45 * self.rate);
        let freqs = grid(LOW_HZ, hi, PER_OCTAVE);
        let per_bin = SIZE as f64 / self.rate;
        let bins = self.sum.len();
        let n = self.frames as f64;
        let count = freqs.len();
        let db = (0..count)
            .map(|i| {
                let f = freqs[i];
                let lo = if i == 0 {
                    f * f / freqs.get(1).copied().unwrap_or(f * 1.01)
                } else {
                    freqs[i - 1]
                };
                let hi = if i + 1 == count {
                    f * f / lo.max(1e-3)
                } else {
                    freqs[i + 1]
                };
                let a = (f * lo).sqrt() * per_bin;
                let b = (f * hi).sqrt() * per_bin;
                let k0 = (a.floor().max(1.0) as usize).min(bins - 1);
                let k1 = (b.ceil() as usize).clamp(k0 + 1, bins);
                let p = self.sum[k0..k1].iter().sum::<f64>() / ((k1 - k0) as f64 * n);
                (10.0 * p.max(1e-20).log10()).max(FLOOR)
            })
            .collect();
        Some(Ltas {
            freqs,
            db,
            seconds: n * (SIZE / 4) as f64 / self.rate,
        })
    }
}

/// The long-term average spectrum of `channels` at `rate` (`None`:
/// shorter than one frame).
pub fn ltas(channels: &[&[f32]], rate: u32) -> Option<Ltas> {
    let mut a = Averager::new(rate);
    a.add(channels);
    a.finish()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_sine_reads_its_level_where_it_is() {
        let rate = 48_000;
        let x: Vec<f32> = (0..rate as usize * 2)
            .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(rate)).sin() as f32)
            .collect();
        let s = ltas(&[&x, &x], rate).unwrap();
        let at = s.at(1000.0).unwrap();
        // A full-scale sine spread over a twelfth of an octave of bins.
        assert!(at > -20.0, "{at}");
        assert!(s.at(100.0).unwrap() < at - 60.0);
        assert!(s.at(10_000.0).unwrap() < at - 60.0);
        assert!((s.seconds - 2.0).abs() < 0.2, "{}", s.seconds);
        // A copy 6 dB down differs by 6 dB everywhere it sounds.
        let y: Vec<f32> = x.iter().map(|v| v * 0.5).collect();
        let t = ltas(&[&y], rate).unwrap();
        let d = s.minus(&t);
        let i = s.freqs.partition_point(|f| *f < 1000.0);
        assert!((d[i] - 6.02).abs() < 0.05, "{}", d[i]);
        assert!(ltas(&[&x[..100]], rate).is_none());
    }
}
