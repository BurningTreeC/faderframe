//! The EQ's linear phase mode.
//!
//! The static bands become one zero-phase FIR per path, designed from the
//! bands' *analog* magnitude (exact to Nyquist, no cramping): sampled on
//! `N / 2 + 1` bins, inverse transformed, centred and windowed. Bands that
//! work on one side or on mid or side make the stereo path a 2 × 2 system
//! (left to left, right to left, …), still linear phase. The FIRs run as a
//! uniformly partitioned convolution in blocks of [`PARTITION`] samples
//! ([`super::fir`]), so the latency is `N / 2 + PARTITION` whatever the
//! host's block size. Dynamic bands stay minimum phase sections after the
//! FIR; spectral bands are linear phase in their own stage.

use super::design::AnalogBand;
use super::{BANDS, BandParams, global};
use crate::ParamValues;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Samples per convolution block.
pub const PARTITION: usize = 256;
/// FIR lengths of the resolutions (low to maximum).
pub const LENGTHS: [usize; 5] = [4096, 8192, 16384, 32768, 65536];

/// The latency of linear phase at a resolution (samples).
pub fn latency(quality: usize) -> u32 {
    (LENGTHS[quality.min(LENGTHS.len() - 1)] / 2 + PARTITION) as u32
}

/// A snapshot of what the bands' kernels depend on.
pub(crate) fn band_key(params: &ParamValues) -> Vec<u32> {
    let mut k: Vec<u32> = (0..BANDS)
        .flat_map(|b| {
            let p = BandParams::read(params, b);
            [
                u32::from(p.enabled),
                p.kind.index() as u32,
                (p.freq as f32).to_bits(),
                (p.gain as f32).to_bits(),
                (p.q as f32).to_bits(),
                (p.kind.snap_slope(p.slope) as f32).to_bits(),
                p.placement.index() as u32,
                u32::from(p.dynamic()),
                u32::from(p.is_spectral()),
            ]
        })
        .collect();
    k.push(params.get(global::GAIN_SCALE).to_bits());
    k.push(params.get(global::GAIN_Q).to_bits());
    k
}

pub(crate) fn key(params: &ParamValues) -> Vec<u32> {
    band_key(params)
}

/// The zero-phase magnitudes of the static bands on each part of the
/// signal (both, left, right, mid, side), at `n / 2 + 1` bins up to Nyquist.
fn magnitudes(params: &ParamValues, rate: f64, n: usize) -> [Vec<f64>; 5] {
    let bins = n / 2 + 1;
    let scale = f64::from(params.get(global::GAIN_SCALE));
    let interact = params.get(global::GAIN_Q) >= 0.5;
    let mut m: [Vec<f64>; 5] = std::array::from_fn(|_| vec![1.0; bins]);
    for b in 0..BANDS {
        let p = BandParams::read(params, b);
        if !p.enabled || p.dynamic() || p.is_spectral() {
            continue;
        }
        let analog = AnalogBand::new(&p.shape(scale, interact));
        let target = p.placement.index();
        for (k, v) in m[target].iter_mut().enumerate() {
            // DC: just above it.
            let f = k.max(1) as f64 * rate / n as f64 * if k == 0 { 0.25 } else { 1.0 };
            *v *= analog.magnitude2(f).sqrt();
        }
    }
    m
}

/// The four paths' impulse responses (left to left, right to left, left to
/// right, right to right).
pub(crate) fn impulses(
    params: &ParamValues,
    rate: f64,
    n: usize,
    planner: &mut RealFftPlanner<f64>,
) -> [Option<Vec<f64>>; 4] {
    let [st, l, r, mid, side] = magnitudes(params, rate, n);
    let bins = n / 2 + 1;
    let ms = mid.iter().zip(&side).any(|(a, b)| (a - b).abs() > 1e-9);
    let path = |f: &dyn Fn(usize) -> f64| -> Vec<f64> { (0..bins).map(f).collect() };
    // Mid/side: m' = M m, s' = S s; as left/right, a = (M + S)/2 on the
    // same side, b = (M − S)/2 across.
    let a = |k: usize| 0.5 * (mid[k] + side[k]);
    let b = |k: usize| 0.5 * (mid[k] - side[k]);
    let lc = |k: usize| st[k] * l[k];
    let rc = |k: usize| st[k] * r[k];
    let ll = path(&|k| a(k) * lc(k));
    let rr = path(&|k| a(k) * rc(k));
    let (rl, lr) = if ms {
        (Some(path(&|k| b(k) * rc(k))), Some(path(&|k| b(k) * lc(k))))
    } else {
        (None, None)
    };
    let mut centred = |mag: Option<Vec<f64>>| mag.map(|m| centred(&m, n, planner));
    [
        centred(Some(ll)),
        centred(rl),
        centred(lr),
        centred(Some(rr)),
    ]
}

/// A zero-phase magnitude turned into a centred, windowed FIR of length
/// `n`.
fn centred(magnitude: &[f64], n: usize, planner: &mut RealFftPlanner<f64>) -> Vec<f64> {
    let inverse = planner.plan_fft_inverse(n);
    let mut spectrum: Vec<Complex<f64>> = magnitude.iter().map(|m| Complex::new(*m, 0.0)).collect();
    let mut h = vec![0.0; n];
    // A zero-phase spectrum is real; its DC and Nyquist bins must be too.
    if inverse.process(&mut spectrum, &mut h).is_err() {
        return vec![0.0; n];
    }
    // Centre the impulse (time N/2) and window it (Blackman).
    let mut out = vec![0.0; n];
    for (i, c) in out.iter_mut().enumerate() {
        let src = (i + n / 2) % n;
        let x = i as f64 / (n - 1) as f64;
        let w = 0.42 - 0.5 * (std::f64::consts::TAU * x).cos()
            + 0.08 * (2.0 * std::f64::consts::TAU * x).cos();
        *c = h[src] / n as f64 * w;
    }
    out
}
