//! Transient (onset) detection.
//!
//! The detection function is the spectral flux of log-compressed magnitude
//! spectra (`ln(1 + |X|)`) (1024-point Hann window, hop 256): how much energy appeared
//! since the previous frame, summed over frequency. Peaks of it above an
//! adaptive threshold (local median plus a margin) at least 30 ms apart
//! are onsets. Each is then moved in the time domain to where the attack
//! starts (the envelope first reaching a fifth of the local peak), so
//! edits and warp markers land just before the hit.
//!
//! Every onset keeps a strength (0–1, the detection peak relative to the
//! signal's strong peaks); a sensitivity setting filters by it without
//! analysing again ([`strength_threshold`]).

use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

const WINDOW: usize = 1024;
const HOP: usize = 256;
/// Flux of a clear attack is in the hundreds (summed log magnitude
/// increases over 513 bins); steady sounds stay well below one.
const MIN_FLUX_NORM: f32 = 40.0;

/// One detected transient.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Onset {
    /// Frame where the attack starts.
    pub frame: i64,
    /// 0–1: how clearly it stands out.
    pub strength: f32,
}

/// The weakest strength kept at `sensitivity` (0: only the clearest
/// transients, 1: everything detected).
pub fn strength_threshold(sensitivity: f32) -> f32 {
    let s = sensitivity.clamp(0.0, 1.0);
    0.6 * (1.0 - s) * (1.0 - s) + 0.02
}

/// Detect onsets in mono audio at `rate`.
pub fn detect(mono: &[f32], rate: u32) -> Vec<Onset> {
    if mono.len() < WINDOW {
        return Vec::new();
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let window: Vec<f32> = (0..WINDOW)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WINDOW as f32).cos())
        .collect();
    let bins = spectrum.len();
    let mut prev = vec![0.0f32; bins];
    let frames = (mono.len() - WINDOW) / HOP + 1;
    let mut odf = Vec::with_capacity(frames);
    for f in 0..frames {
        let at = f * HOP;
        for ((x, s), w) in input.iter_mut().zip(&mono[at..at + WINDOW]).zip(&window) {
            *x = s * w;
        }
        if fft
            .process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .is_err()
        {
            return Vec::new();
        }
        let mut flux = 0.0f32;
        for (c, p) in spectrum.iter().zip(prev.iter_mut()) {
            let mag = c.norm().ln_1p();
            flux += (mag - *p).max(0.0);
            *p = mag;
        }
        // The first frame has nothing to compare with.
        odf.push(if f == 0 { 0.0 } else { flux });
    }
    // Normalise by the strong peaks (98th percentile), not the loudest one
    // — but never below a level real attacks reach, so steady material's
    // tiny fluctuations stay small.
    let mut sorted = odf.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p98 = sorted[(sorted.len() as f32 * 0.98) as usize % sorted.len()];
    let norm = p98.max(MIN_FLUX_NORM);
    for v in &mut odf {
        *v /= norm;
    }
    // Adaptive threshold: median of ±8 frames plus a margin; local maxima
    // of ±3 frames, at least 30 ms apart.
    let min_gap = ((rate as f64 * 0.03) as usize / HOP).max(1);
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    let mut local = Vec::with_capacity(17);
    for i in 0..odf.len() {
        let v = odf[i];
        let (a, b) = (i.saturating_sub(3), (i + 4).min(odf.len()));
        if odf[a..b].iter().any(|&x| x > v) {
            continue;
        }
        local.clear();
        local.extend_from_slice(&odf[i.saturating_sub(8)..(i + 9).min(odf.len())]);
        local.sort_by(|a, b| a.total_cmp(b));
        let median = local[local.len() / 2];
        let strength = v - median;
        if strength < 0.02 {
            continue;
        }
        match peaks.last_mut() {
            Some(last) if i - last.0 < min_gap => {
                if strength > last.1 {
                    *last = (i, strength);
                }
            }
            _ => peaks.push((i, strength)),
        }
    }
    peaks
        .into_iter()
        .map(|(i, strength)| Onset {
            frame: attack_start(mono, i * HOP + WINDOW / 2) as i64,
            strength: strength.min(1.0),
        })
        .collect()
}

/// Where the attack around `center` starts: the first point (going back
/// from the local peak) where the envelope is below a fifth of the peak.
fn attack_start(x: &[f32], center: usize) -> usize {
    let lo = center.saturating_sub(WINDOW / 2);
    let hi = (center + WINDOW / 2).min(x.len());
    if hi <= lo {
        return center.min(x.len().saturating_sub(1));
    }
    let (peak_i, peak) = x[lo..hi]
        .iter()
        .enumerate()
        .map(|(i, v)| (lo + i, v.abs()))
        .fold((lo, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
    if peak <= 1e-6 {
        return center;
    }
    let env = |i: usize| -> f32 {
        x[i.saturating_sub(16)..(i + 16).min(x.len())]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    let mut i = peak_i;
    while i > lo && env(i) > peak * 0.2 {
        i = i.saturating_sub(8);
    }
    i
}

/// Mono mixdown of channels (for detection).
pub fn mixdown(channels: &[&[f32]]) -> Vec<f32> {
    let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let k = channels.len().max(1) as f32;
    (0..n)
        .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / k)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clicks of decaying noise at known positions over quiet noise.
    fn hits(rate: u32, at: &[usize], len: usize) -> Vec<f32> {
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0
        };
        let mut x: Vec<f32> = (0..len).map(|_| noise() * 0.002).collect();
        for &p in at {
            for k in 0..(rate as usize / 10) {
                if p + k < len {
                    x[p + k] += noise() * 0.8 * (-(k as f32) / (rate as f32 * 0.02)).exp();
                }
            }
        }
        x
    }

    #[test]
    fn finds_hits_just_before_they_sound() {
        let rate = 48_000;
        let at = [4_800, 30_000, 41_000, 70_000, 96_123];
        let x = hits(rate, &at, 120_000);
        let found = detect(&x, rate);
        let strong: Vec<i64> = found
            .iter()
            .filter(|o| o.strength >= strength_threshold(0.5))
            .map(|o| o.frame)
            .collect();
        assert_eq!(strong.len(), at.len(), "{found:?}");
        for (f, &a) in strong.iter().zip(&at) {
            let err = *f - a as i64;
            assert!(
                (-64..=16).contains(&err),
                "onset {f} for hit at {a} ({err})"
            );
        }
    }

    #[test]
    fn silence_and_steady_tones_have_no_onsets() {
        let rate = 48_000;
        assert!(detect(&vec![0.0; 48_000], rate).is_empty());
        let tone: Vec<f32> = (0..96_000)
            .map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 0.5)
            .collect();
        let found = detect(&tone, rate);
        assert!(
            found
                .iter()
                .all(|o| o.strength < strength_threshold(0.5) || o.frame < 2048),
            "{found:?}"
        );
    }
}
