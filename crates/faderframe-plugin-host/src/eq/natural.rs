//! The natural phase mode: the analog filter's magnitude *and* phase.
//!
//! The matched minimum phase sections already follow the analog magnitude
//! closely and its phase low down; what is left is a small difference in
//! the top octaves (phase, and a little magnitude). After the sections a
//! short FIR corrects it: per frequency, the ratio of the analog response
//! to the digital one (a 2 × 2 system when bands work on one side, mid or
//! side), delayed by [`PRE`] samples so the correction may act a little
//! before the main tap, windowed to [`LENGTH`] taps. The correction is
//! nearly a pure delay below the top octaves, so it is short: the mode
//! costs [`LATENCY`] samples and no noticeable pre-ring, where linear phase
//! costs thousands.
//!
//! Dynamic bands are corrected at rest; spectral bands run in their own
//! (linear phase) stage.

use super::design::{self, AnalogBand, C64, Coefs, MAX_SECTIONS};
use super::{BANDS, BandParams, Placement, global, linear};
use crate::ParamValues;
use realfft::RealFftPlanner;

/// Taps of the correction.
pub const LENGTH: usize = 1024;
/// Samples the correction may act before its main tap.
pub const PRE: usize = 64;
/// Samples per convolution block.
pub const PARTITION: usize = 64;
/// The mode's latency.
pub const LATENCY: u32 = (PRE + PARTITION) as u32;

pub(crate) fn key(params: &ParamValues) -> Vec<u32> {
    linear::band_key(params)
}

/// A 2 × 2 complex matrix (stereo paths: rows are outputs).
#[derive(Clone, Copy, Debug)]
struct M2([[C64; 2]; 2]);

impl M2 {
    const IDENTITY: M2 = M2([
        [C64::new(1.0, 0.0), C64::new(0.0, 0.0)],
        [C64::new(0.0, 0.0), C64::new(1.0, 0.0)],
    ]);

    /// A band with response `h` placed on part of the signal.
    fn band(placement: Placement, h: C64) -> M2 {
        let one = C64::new(1.0, 0.0);
        let zero = C64::new(0.0, 0.0);
        match placement {
            Placement::Stereo => M2([[h, zero], [zero, h]]),
            Placement::Left => M2([[h, zero], [zero, one]]),
            Placement::Right => M2([[one, zero], [zero, h]]),
            // l' = (M + S), r' = (M − S) with M filtered.
            Placement::Mid => {
                let (a, b) = ((h + one) * 0.5, (h - one) * 0.5);
                M2([[a, b], [b, a]])
            }
            Placement::Side => {
                let (a, b) = ((one + h) * 0.5, (one - h) * 0.5);
                M2([[a, b], [b, a]])
            }
        }
    }

    fn mul(&self, o: &M2) -> M2 {
        let a = &self.0;
        let b = &o.0;
        M2([
            [
                a[0][0] * b[0][0] + a[0][1] * b[1][0],
                a[0][0] * b[0][1] + a[0][1] * b[1][1],
            ],
            [
                a[1][0] * b[0][0] + a[1][1] * b[1][0],
                a[1][0] * b[0][1] + a[1][1] * b[1][1],
            ],
        ])
    }

    fn adjoint(&self) -> M2 {
        let a = &self.0;
        M2([
            [a[0][0].conj(), a[1][0].conj()],
            [a[0][1].conj(), a[1][1].conj()],
        ])
    }

    fn inverse(&self) -> M2 {
        let a = &self.0;
        let det = a[0][0] * a[1][1] - a[0][1] * a[1][0];
        let inv = if det.norm_sqr() > 0.0 {
            C64::new(1.0, 0.0) / det
        } else {
            C64::new(0.0, 0.0)
        };
        M2([
            [a[1][1] * inv, -a[0][1] * inv],
            [-a[1][0] * inv, a[0][0] * inv],
        ])
    }

    /// `self (D^H D + εI)^-1 D^H`: `self / D`, regularised where `D`
    /// has (next to) nothing.
    fn over(&self, d: &M2) -> M2 {
        const EPS: f64 = 1e-10;
        let dh = d.adjoint();
        let mut g = dh.mul(d);
        g.0[0][0] += EPS;
        g.0[1][1] += EPS;
        self.mul(&g.inverse()).mul(&dh)
    }
}

/// The correction's four paths (left to left, right to left, left to
/// right, right to right) as impulse responses of [`LENGTH`] taps.
pub(crate) fn impulses(
    params: &ParamValues,
    rate: f64,
    planner: &mut RealFftPlanner<f64>,
) -> [Option<Vec<f64>>; 4] {
    let scale = f64::from(params.get(global::GAIN_SCALE));
    let interact = params.get(global::GAIN_Q) >= 0.5;
    // The bands the sections run, in their order, at rest.
    let mut bands: Vec<(Placement, [Coefs; MAX_SECTIONS], usize, AnalogBand)> = Vec::new();
    for b in 0..BANDS {
        let p = BandParams::read(params, b);
        if !p.enabled || p.is_spectral() {
            continue;
        }
        let shape = p.shape(scale, interact);
        let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
        let n = design::design(&shape, rate, &mut s);
        bands.push((p.placement, s, n, AnalogBand::new(&shape)));
    }
    let bins = LENGTH / 2 + 1;
    let mut spectra: [Vec<C64>; 4] = std::array::from_fn(|_| vec![C64::new(0.0, 0.0); bins]);
    for k in 0..bins {
        let w = std::f64::consts::TAU * k as f64 / LENGTH as f64;
        // DC: just above it (a cut has nothing there to divide by).
        let f = rate * k.max(1) as f64 / LENGTH as f64 * if k == 0 { 0.25 } else { 1.0 };
        let wd = if k == 0 {
            std::f64::consts::TAU * f / rate
        } else {
            w
        };
        let (mut digital, mut analog) = (M2::IDENTITY, M2::IDENTITY);
        for (placement, s, n, a) in &bands {
            let hd = s[..*n]
                .iter()
                .fold(C64::new(1.0, 0.0), |acc, c| acc * c.response(wd));
            digital = M2::band(*placement, hd).mul(&digital);
            analog = M2::band(*placement, a.response(f)).mul(&analog);
        }
        let delay = C64::from_polar(1.0, -w * PRE as f64);
        let c = analog.over(&digital);
        for (path, spectrum) in spectra.iter_mut().enumerate() {
            let (out, input) = (path / 2, path % 2);
            spectrum[k] = c.0[out][input] * delay;
        }
    }
    let inverse = planner.plan_fft_inverse(LENGTH);
    spectra.map(|mut spectrum| {
        // A real response: real DC and Nyquist.
        spectrum[0].im = 0.0;
        spectrum[bins - 1].im = 0.0;
        let mut h = vec![0.0; LENGTH];
        if inverse.process(&mut spectrum, &mut h).is_err() {
            return None;
        }
        let tail = LENGTH / 2;
        let mut peak = 0.0f64;
        for (i, v) in h.iter_mut().enumerate() {
            let w = if i < PRE {
                // Rising into the main tap.
                (0.5 * std::f64::consts::PI * i as f64 / PRE as f64)
                    .sin()
                    .powi(2)
            } else if i >= LENGTH - tail {
                let t = (LENGTH - i) as f64 / tail as f64;
                (0.5 * std::f64::consts::PI * t).sin().powi(2)
            } else {
                1.0
            };
            *v *= w / LENGTH as f64;
            peak = peak.max(v.abs());
        }
        (peak > 1e-9).then_some(h)
    })
}
