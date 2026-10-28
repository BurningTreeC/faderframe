//! Stereo measurements from a profile's band frames: mid and side power
//! per band, the correlation of left and right, and what a mono fold-down
//! ((L+R)/2) does to each band.
//!
//! With `mid = (L+R)/2`, `P_mid = (P_L + P_R + 2ρ·√(P_L·P_R)) / 4` (ρ the
//! band's correlation). What the fold-down *loses* is measured against
//! what it would hold if left and right added without cancelling,
//! `(√P_L + √P_R)² / 4`: `10·log10(P_mid / that)` is 0 dB for a centred
//! band and for a source panned anywhere (one side alone loses nothing to
//! cancellation, only its other side's level), −3 dB for fully
//! uncorrelated channels of equal level, and falls without bound as left
//! and right become inverted copies. Powers are averaged over the frames
//! first (the band's long-term picture), or taken per frame for
//! durations.

use crate::profile::{FLOOR_DB, TrackProfile, power_db};

/// A band's stereo picture over a stretch of frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandStereo {
    pub mid: f64,
    pub side: f64,
    /// The mid power if left and right added without cancelling
    /// (`(√P_L + √P_R)² / 4`).
    pub coherent: f64,
    /// Mean of the two channels' powers.
    pub power: f64,
    /// Power-weighted mean correlation of left and right (−1 to 1).
    pub correlation: f64,
}

impl BandStereo {
    /// What the fold-down loses to cancellation (dB, ≤ 0; [`FLOOR_DB`]
    /// when all of it cancels; 0 in silence).
    pub fn fold_db(&self) -> f64 {
        if self.coherent <= 0.0 {
            0.0
        } else if self.mid <= 0.0 {
            FLOOR_DB
        } else {
            (10.0 * (self.mid / self.coherent).log10()).clamp(FLOOR_DB, 0.0)
        }
    }

    /// Frame `t`'s band `b`.
    pub fn of_frame(p: &TrackProfile, t: usize, b: usize) -> Self {
        let (m, s) = p.frames.mid_side(t, b);
        let (l, r) = p.frames.powers(t, b);
        Self {
            mid: m,
            side: s,
            coherent: (l.sqrt() + r.sqrt()).powi(2) / 4.0,
            power: 0.5 * (l + r),
            correlation: p.frames.correlation(t, b),
        }
    }

    /// Side over mid (dB; how wide the band is).
    pub fn width_db(&self) -> f64 {
        power_db(self.side) - power_db(self.mid)
    }
}

/// Every band's stereo picture over `frames` (all when `None`).
pub fn bands(p: &TrackProfile, frames: Option<std::ops::Range<usize>>) -> Vec<BandStereo> {
    let range = frames.unwrap_or(0..p.frames.frames());
    let n = range.len().max(1) as f64;
    (0..p.frames.bands())
        .map(|b| {
            let (mut mid, mut side, mut coherent, mut power, mut cw) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for t in range.clone() {
                let x = BandStereo::of_frame(p, t, b);
                mid += x.mid;
                side += x.side;
                coherent += x.coherent;
                power += x.power;
                cw += x.power * x.correlation;
            }
            BandStereo {
                mid: mid / n,
                side: side / n,
                coherent: coherent / n,
                power: power / n,
                correlation: if power > 0.0 { cw / power } else { 0.0 },
            }
        })
        .collect()
}

/// The broadband correlation of frame `t` (−1 to 1; 0 in silence) and
/// the two channels' total powers.
pub fn frame_correlation(p: &TrackProfile, t: usize) -> (f64, f64, f64) {
    let (mut l, mut r, mut cross) = (0.0, 0.0, 0.0);
    for b in 0..p.frames.bands() {
        let (pl, pr) = p.frames.powers(t, b);
        l += pl;
        r += pr;
        cross += p.frames.correlation(t, b) * (pl * pr).sqrt();
    }
    let c = if l > 0.0 && r > 0.0 {
        (cross / (l * r).sqrt()).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (c, l, r)
}

/// The power-weighted correlation over all frames.
pub fn overall_correlation(p: &TrackProfile) -> f64 {
    let (mut cw, mut w) = (0.0, 0.0);
    for t in 0..p.frames.frames() {
        let (c, l, r) = frame_correlation(p, t);
        cw += c * (l + r);
        w += l + r;
    }
    if w > 0.0 { cw / w } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::profile;
    use std::f64::consts::TAU;

    fn sine(hz: f64, phase: f64) -> Vec<f32> {
        (0..96_000)
            .map(|i| (0.4 * (TAU * hz * i as f64 / 48_000.0 + phase).sin()) as f32)
            .collect()
    }

    #[test]
    fn fold_down_of_known_relationships() {
        let s = sine(200.0, 0.0);
        let b = |p: &TrackProfile| p.layout.index_of(200.0).unwrap_or(0);
        let mono = profile(&[&s, &s]);
        assert!(bands(&mono, None)[b(&mono)].fold_db().abs() < 0.01);
        assert!((overall_correlation(&mono) - 1.0).abs() < 1e-3);
        let inv: Vec<f32> = s.iter().map(|v| -v).collect();
        let inverted = profile(&[&s, &inv]);
        assert!(bands(&inverted, None)[b(&inverted)].fold_db() < -60.0);
        assert!((overall_correlation(&inverted) + 1.0).abs() < 1e-3);
        // 90° apart: uncorrelated, −3 dB in mono.
        let quad = profile(&[&s, &sine(200.0, std::f64::consts::FRAC_PI_2)]);
        let f = bands(&quad, None)[b(&quad)].fold_db();
        assert!((f + 3.01).abs() < 0.2, "{f}");
        // One side only (or panned anywhere): nothing cancels.
        let z = vec![0.0f32; s.len()];
        let left = profile(&[&s, &z]);
        let f = bands(&left, None)[b(&left)].fold_db();
        assert!(f.abs() < 0.01, "{f}");
        let half: Vec<f32> = s.iter().map(|v| 0.3 * v).collect();
        let panned = profile(&[&s, &half]);
        let f = bands(&panned, None)[b(&panned)].fold_db();
        assert!(f.abs() < 0.01, "{f}");
        // Silence: nothing changes.
        let silent = profile(&[&z, &z]);
        assert_eq!(bands(&silent, None)[b(&silent)].fold_db(), 0.0);
        assert_eq!(overall_correlation(&silent), 0.0);
    }
}
