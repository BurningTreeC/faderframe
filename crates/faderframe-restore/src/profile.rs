//! Noise profiles: a noise's power per frequency, kept in [`BANDS`] bands a
//! sixth of an octave wide from 20 Hz to 24 kHz, in dB of power per sample
//! (white noise of variance σ² reads 10·log10 σ² in every band), so one
//! profile serves every frame size and sample rate. A device keeps it in
//! its parameters; the spectral editor learns one per edit at its own
//! resolution ([`Learner::bins`]).

/// Bands of a profile.
pub const BANDS: usize = 64;
/// The lowest band's lower edge and the highest band's upper edge (Hz).
pub const LOW_HZ: f64 = 20.0;
pub const HIGH_HZ: f64 = 24_000.0;
/// What a band holds when nothing was learned (dB).
pub const FLOOR_DB: f64 = -160.0;

/// Band `b`'s edges (Hz).
pub fn band_edges(b: usize) -> (f64, f64) {
    let ratio = HIGH_HZ / LOW_HZ;
    let at = |i: f64| LOW_HZ * ratio.powf(i / BANDS as f64);
    (at(b as f64), at(b as f64 + 1.0))
}

/// Band `b`'s centre (Hz, geometric).
pub fn band_centre(b: usize) -> f64 {
    let (lo, hi) = band_edges(b);
    (lo * hi).sqrt()
}

/// Where `f` falls among the band centres (fractional band index).
fn band_position(f: f64) -> f64 {
    let f = f.max(1e-3);
    (f / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln() * BANDS as f64 - 0.5
}

/// A profile's level at `f` (dB), interpolated between band centres.
pub fn level_at(bands: &[f64], f: f64) -> f64 {
    if bands.is_empty() {
        return FLOOR_DB;
    }
    let u = band_position(f).clamp(0.0, (bands.len() - 1) as f64);
    let i = u.floor() as usize;
    let j = (i + 1).min(bands.len() - 1);
    let t = u - i as f64;
    bands[i] + (bands[j] - bands[i]) * t
}

/// A profile's power per bin (per sample units) for bins of `bin_hz`:
/// `out[k]` for bin `k`.
pub fn to_bins(bands: &[f64], bin_hz: f64, out: &mut [f64]) {
    for (k, o) in out.iter_mut().enumerate() {
        let f = (k as f64 * bin_hz).max(0.5 * bin_hz);
        *o = 10f64.powf(level_at(bands, f) / 10.0);
    }
}

/// Learns a profile from spectra: the mean power of each bin over the
/// frames it is given.
#[derive(Clone, Debug)]
pub struct Learner {
    sum: Vec<f64>,
    frames: u64,
    bin_hz: f64,
}

impl Learner {
    /// For `bins` bins of `bin_hz` each.
    pub fn new(bins: usize, bin_hz: f64) -> Self {
        Self {
            sum: vec![0.0; bins],
            frames: 0,
            bin_hz,
        }
    }

    /// Start again.
    pub fn clear(&mut self) {
        self.sum.fill(0.0);
        self.frames = 0;
    }

    /// One frame's power per bin (per sample units).
    pub fn add(&mut self, power: &[f64]) {
        for (s, p) in self.sum.iter_mut().zip(power) {
            *s += p;
        }
        self.frames += 1;
    }

    /// Frames learned so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The mean power per bin so far.
    pub fn bins(&self, out: &mut [f64]) {
        let n = self.frames.max(1) as f64;
        for (o, s) in out.iter_mut().zip(&self.sum) {
            *o = s / n;
        }
    }

    /// The profile so far: each band the mean power of the bins in it (dB);
    /// bands no bin falls in take their neighbours' levels.
    pub fn bands(&self, out: &mut [f64; BANDS]) {
        let mut sum = [0.0f64; BANDS];
        let mut count = [0u32; BANDS];
        let n = self.frames.max(1) as f64;
        for (k, s) in self.sum.iter().enumerate().skip(1) {
            let f = k as f64 * self.bin_hz;
            if f < LOW_HZ {
                continue;
            }
            let b = ((f / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln() * BANDS as f64).floor() as usize;
            let b = b.min(BANDS - 1);
            sum[b] += s / n;
            count[b] += 1;
        }
        fill_bands(&sum, &count, out);
    }
}

/// Bands from sums and counts, the empty ones interpolated in dB between
/// their filled neighbours (held at the ends). No allocation: a device
/// publishes the profile it learns from the audio thread.
fn fill_bands(sum: &[f64; BANDS], count: &[u32; BANDS], out: &mut [f64; BANDS]) {
    let db = |b: usize| (10.0 * (sum[b] / f64::from(count[b])).max(1e-30).log10()).max(FLOOR_DB);
    for b in 0..BANDS {
        out[b] = if count[b] > 0 {
            db(b)
        } else {
            let before = (0..b).rev().find(|&i| count[i] > 0);
            let after = (b + 1..BANDS).find(|&i| count[i] > 0);
            match (before, after) {
                (Some(i), Some(j)) => {
                    let t = (b - i) as f64 / (j - i) as f64;
                    db(i) + (db(j) - db(i)) * t
                }
                (Some(i), None) => db(i),
                (None, Some(j)) => db(j),
                (None, None) => FLOOR_DB,
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cover_the_range_a_sixth_of_an_octave_each() {
        assert!((band_edges(0).0 - LOW_HZ).abs() < 1e-9);
        assert!((band_edges(BANDS - 1).1 - HIGH_HZ).abs() < 1e-6);
        let octaves = (band_edges(10).1 / band_edges(10).0).log2();
        assert!((octaves - 1.0 / 6.25).abs() < 0.01, "{octaves}");
    }

    #[test]
    fn a_learned_profile_reads_back_at_any_resolution() {
        // A pink-ish noise: −40 dB at 100 Hz falling 3 dB an octave.
        let level = |f: f64| -40.0 - 3.0 * (f / 100.0).log2();
        let bin_hz = 48_000.0 / 2048.0;
        let mut learner = Learner::new(1025, bin_hz);
        let power: Vec<f64> = (0..1025)
            .map(|k| 10f64.powf(level((k as f64 * bin_hz).max(1.0)) / 10.0))
            .collect();
        learner.add(&power);
        let mut bands = [0.0; BANDS];
        learner.bands(&mut bands);
        let mut fine = vec![0.0; 4097];
        to_bins(&bands, 48_000.0 / 8192.0, &mut fine);
        for f in [200.0, 1_000.0, 5_000.0, 15_000.0] {
            let k = (f / (48_000.0 / 8192.0)) as usize;
            let got = 10.0 * fine[k].log10();
            assert!(
                (got - level(f)).abs() < 1.0,
                "{f} Hz: {got} vs {}",
                level(f)
            );
        }
    }
}
