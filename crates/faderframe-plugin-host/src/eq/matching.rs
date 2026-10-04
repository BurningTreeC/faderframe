//! EQ Match: bands that make one spectrum sound like another.
//!
//! The caller gives the difference between the reference's long-term
//! spectrum and the input's on a logarithmic frequency grid. It is
//! smoothed (a third of an octave), its average taken off (matching the
//! balance, not the level) and limited; then bands are placed one at a
//! time where most is left to correct — a bell at the largest remaining
//! difference, as wide as that difference's half-height, or a shelf when it
//! sits at either end — and each new band and then all of them together
//! are refined by a pattern search on their frequency, gain and Q against
//! the whole curve. It stops at the band budget or once nothing larger than
//! half a decibel remains.

use super::design::{AnalogBand, BandShape, BandType};

/// The largest correction a band is given (dB).
const LIMIT: f64 = 18.0;
/// Differences under this are left alone (dB).
const GOOD_ENOUGH: f64 = 0.5;

/// A logarithmic grid from `lo` to `hi` Hz with `per_octave` points per
/// octave.
pub fn grid(lo: f64, hi: f64, per_octave: f64) -> Vec<f64> {
    let n = ((hi / lo).log2() * per_octave).ceil() as usize + 1;
    (0..n)
        .map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64))
        .collect()
}

/// Smooth a curve over `octaves` (on a grid with `per_octave` points per
/// octave), take its average off and limit it: the curve EQ Match aims at.
pub fn target(difference: &[f64], per_octave: f64, octaves: f64) -> Vec<f64> {
    let half = ((octaves * per_octave) / 2.0).round().max(0.0) as usize;
    let n = difference.len();
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            difference[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let mean = out.iter().sum::<f64>() / n.max(1) as f64;
    for v in &mut out {
        *v = (*v - mean).clamp(-LIMIT, LIMIT);
    }
    out
}

/// A band being fitted: its shape and its curve on the grid.
struct Fit {
    shape: BandShape,
    curve: Vec<f64>,
}

impl Fit {
    fn new(shape: BandShape, freqs: &[f64]) -> Self {
        let mut f = Self {
            shape,
            curve: vec![0.0; freqs.len()],
        };
        f.update(freqs);
        f
    }

    fn update(&mut self, freqs: &[f64]) {
        let a = AnalogBand::new(&self.shape);
        for (c, f) in self.curve.iter_mut().zip(freqs) {
            *c = a.db(*f);
        }
    }
}

/// Squared error of `target` against all fits but `skip`, plus `with`.
fn error(target: &[f64], fits: &[Fit], skip: usize, with: &[f64]) -> f64 {
    target
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let sum: f64 = fits
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != skip)
                .map(|(_, f)| f.curve[i])
                .sum::<f64>()
                + with[i];
            (t - sum).powi(2)
        })
        .sum()
}

/// Refine band `i` by a pattern search over log frequency, gain and log Q.
fn refine(target: &[f64], freqs: &[f64], fits: &mut [Fit], i: usize, passes: usize) {
    let (lo, hi) = (freqs[0], freqs[freqs.len() - 1]);
    let mut steps = [0.25, 1.0, 0.25];
    let mut best = error(target, fits, i, &fits[i].curve.clone());
    let mut trial_curve = vec![0.0; freqs.len()];
    for _ in 0..passes {
        let mut improved = false;
        for param in 0..3 {
            for dir in [-1.0, 1.0] {
                let mut s = fits[i].shape;
                match param {
                    0 => s.freq = (s.freq * 2f64.powf(dir * steps[0])).clamp(lo, hi),
                    1 => s.gain = (s.gain + dir * steps[1]).clamp(-LIMIT * 1.5, LIMIT * 1.5),
                    _ => s.q = (s.q * 2f64.powf(dir * steps[2])).clamp(0.1, 12.0),
                }
                let a = AnalogBand::new(&s);
                for (c, f) in trial_curve.iter_mut().zip(freqs) {
                    *c = a.db(*f);
                }
                let e = error(target, fits, i, &trial_curve);
                if e < best {
                    best = e;
                    fits[i].shape = s;
                    fits[i].curve.copy_from_slice(&trial_curve);
                    improved = true;
                }
            }
        }
        if !improved {
            for s in &mut steps {
                *s *= 0.5;
            }
            if steps[1] < 0.02 {
                break;
            }
        }
    }
}

/// Fit up to `max_bands` bands to `target` (dB on `freqs`).
pub fn fit(freqs: &[f64], target: &[f64], max_bands: usize) -> Vec<BandShape> {
    let n = freqs.len().min(target.len());
    if n < 3 {
        return Vec::new();
    }
    let (freqs, target) = (&freqs[..n], &target[..n]);
    let mut fits: Vec<Fit> = Vec::new();
    let mut residual = target.to_vec();
    while fits.len() < max_bands {
        let Some((peak, &value)) = residual
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        else {
            break;
        };
        if value.abs() < GOOD_ENOUGH {
            break;
        }
        // The half-height span round the peak.
        let half = value.abs() / 2.0;
        let same = |v: f64| v.signum() == value.signum() && v.abs() > half;
        let mut a = peak;
        while a > 0 && same(residual[a - 1]) {
            a -= 1;
        }
        let mut b = peak;
        while b + 1 < n && same(residual[b + 1]) {
            b += 1;
        }
        let edge = 2.max(n / 40);
        let shape = if a == 0 && peak < edge + n / 8 {
            BandShape {
                kind: BandType::LowShelf,
                freq: freqs[b],
                gain: value,
                q: std::f64::consts::FRAC_1_SQRT_2,
                slope: 12.0,
            }
        } else if b == n - 1 && peak + edge + n / 8 > n {
            BandShape {
                kind: BandType::HighShelf,
                freq: freqs[a],
                gain: value,
                q: std::f64::consts::FRAC_1_SQRT_2,
                slope: 12.0,
            }
        } else {
            let octaves = (freqs[b] / freqs[a]).log2().max(0.1);
            let r = 2f64.powf(octaves);
            BandShape {
                kind: BandType::Bell,
                freq: freqs[peak],
                gain: value,
                q: (r.sqrt() / (r - 1.0)).clamp(0.1, 12.0),
                slope: 12.0,
            }
        };
        fits.push(Fit::new(shape, freqs));
        let last = fits.len() - 1;
        refine(target, freqs, &mut fits, last, 24);
        for (r, (t, i)) in residual.iter_mut().zip(target.iter().zip(0..)) {
            *r = t - fits.iter().map(|f| f.curve[i]).sum::<f64>();
        }
    }
    // All together.
    for _ in 0..2 {
        for i in 0..fits.len() {
            refine(target, freqs, &mut fits, i, 8);
        }
    }
    // Bands that ended up doing nothing go.
    fits.retain(|f| f.shape.gain.abs() >= 0.2);
    fits.into_iter().map(|f| f.shape).collect()
}

/// The largest difference left between `target` and `bands` (dB).
pub fn worst_error(freqs: &[f64], target: &[f64], bands: &[BandShape]) -> f64 {
    let analog: Vec<AnalogBand> = bands.iter().map(AnalogBand::new).collect();
    freqs
        .iter()
        .zip(target)
        .map(|(f, t)| (t - analog.iter().map(|a| a.db(*f)).sum::<f64>()).abs())
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_finds_the_bands_that_made_a_difference() {
        let freqs = grid(20.0, 20_000.0, 12.0);
        let made = [
            BandShape {
                kind: BandType::Bell,
                freq: 250.0,
                gain: -4.0,
                q: 1.2,
                slope: 12.0,
            },
            BandShape {
                kind: BandType::Bell,
                freq: 3_000.0,
                gain: 5.0,
                q: 0.8,
                slope: 12.0,
            },
            BandShape {
                kind: BandType::HighShelf,
                freq: 10_000.0,
                gain: 3.0,
                q: 0.707,
                slope: 12.0,
            },
        ];
        let curve: Vec<f64> = freqs
            .iter()
            .map(|f| made.iter().map(|b| AnalogBand::new(b).db(*f)).sum())
            .collect();
        let bands = fit(&freqs, &curve, 8);
        assert!(!bands.is_empty() && bands.len() <= 8, "{}", bands.len());
        let e = worst_error(&freqs, &curve, &bands);
        assert!(e < 0.6, "worst error {e:.2} dB with {} bands", bands.len());
        // A budget of one gets the biggest feature.
        let one = fit(&freqs, &curve, 1);
        assert_eq!(one.len(), 1);
        assert!(one[0].gain > 2.0);
    }

    #[test]
    fn the_target_is_smoothed_and_centred() {
        let freqs = grid(20.0, 20_000.0, 12.0);
        let raw: Vec<f64> = freqs
            .iter()
            .enumerate()
            .map(|(i, _)| 6.0 + if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let t = target(&raw, 12.0, 1.0 / 3.0);
        let mean = t.iter().sum::<f64>() / t.len() as f64;
        assert!(mean.abs() < 1e-9);
        assert!(t.iter().all(|v| v.abs() < 0.5), "ripple smoothed out");
        // Nothing to do: no bands.
        assert!(fit(&freqs, &t, 8).is_empty());
    }
}
