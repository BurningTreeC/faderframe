//! Clipped peaks rebuilt: runs of samples stuck at the clip level are
//! filled by least-squares AR interpolation constrained to stay at or
//! beyond what was recorded, on the side they were clipped (the waveform
//! went further than the converter could follow: at a hard clip that is
//! the level, within the tolerance below it the sample as it came).
//!
//! The constraint is kept by an active set: the unconstrained solution
//! first; samples that fall short are pinned at the level and become known;
//! the stretches between pinned samples are solved again, a few rounds
//! (each stretch is Toeplitz, solved in `O(m²)`; stretches sweep in turn,
//! Gauss–Seidel). The model is fitted twice when quality asks: once with
//! the flat tops in the window, then again on the first rebuild, the tops
//! put back and rebuilt with the better model.

use crate::ar::{ArFit, Interpolator};

/// Rounds of the active set.
const ROUNDS: usize = 4;

/// Mark clipped runs whose first sample is in `from..to`: at least two
/// samples in a row at or beyond `threshold` (linear) on one side, each
/// run at most `max_len` (longer ones are left: nothing can be rebuilt
/// from so little). Returns the number of samples marked. Marks are 1 for
/// a positive run, 2 for a negative one.
pub fn mark(
    x: &[f64],
    threshold: f64,
    from: usize,
    to: usize,
    max_len: usize,
    marks: &mut [u8],
) -> usize {
    let len = x.len().min(marks.len());
    let mut n = from;
    let mut marked = 0;
    let to = to.min(len);
    // A run already under way at `from` belongs to the window before.
    while n < to
        && n > 0
        && x[n].abs() >= threshold
        && x[n - 1].abs() >= threshold
        && x[n].signum() == x[n - 1].signum()
    {
        n += 1;
    }
    while n < to {
        if x[n].abs() < threshold {
            n += 1;
            continue;
        }
        let s = n;
        let sign = x[n].signum();
        while n < len && x[n].abs() >= threshold && x[n].signum() == sign {
            n += 1;
        }
        let run = n - s;
        if run >= 2 && run <= max_len {
            marks[s..n].fill(if sign > 0.0 { 1 } else { 2 });
            marked += run;
        }
    }
    marked
}

/// One channel's rebuilder, for windows of one length.
pub struct Declipper {
    fit: ArFit,
    interp: Interpolator,
    pinned: Vec<bool>,
}

impl Declipper {
    pub fn new(len: usize, order: usize, max_len: usize) -> Self {
        Self {
            fit: ArFit::new(len, order),
            interp: Interpolator::new(max_len, order),
            pinned: vec![false; max_len],
        }
    }

    pub fn order(&self) -> usize {
        self.fit.order()
    }

    /// Fit the model to the window `x`.
    pub fn fit(&mut self, x: &[f64]) {
        self.fit.fit(x);
    }

    /// Rebuild the marked runs starting in `from..to`, each sample at least
    /// as far out as `observed` (the input as it came): returns the runs
    /// rebuilt.
    pub fn repair(
        &mut self,
        x: &mut [f64],
        observed: &[f64],
        marks: &[u8],
        from: usize,
        to: usize,
    ) -> usize {
        let len = x.len().min(marks.len());
        let mut runs = 0;
        let mut n = from;
        while n < to.min(len) {
            if marks[n] == 0 {
                n += 1;
                continue;
            }
            let s = n;
            let tag = marks[n];
            while n < len && marks[n] == tag {
                n += 1;
            }
            let sign = if tag == 1 { 1.0 } else { -1.0 };
            if self.rebuild(x, observed, s, n, sign) {
                runs += 1;
            }
        }
        runs
    }

    /// One run `[s, e)` clipped on the side of `sign`.
    fn rebuild(&mut self, x: &mut [f64], observed: &[f64], s: usize, e: usize, sign: f64) -> bool {
        let m = e - s;
        if m > self.pinned.len() || e > observed.len() || !self.interp.fill(x, s, e, &self.fit.a) {
            return false;
        }
        let pinned = &mut self.pinned[..m];
        pinned.fill(false);
        for _ in 0..ROUNDS {
            let mut short = false;
            for (i, pin) in pinned.iter_mut().enumerate() {
                let bound = observed[s + i];
                if !*pin && x[s + i] * sign < bound * sign {
                    *pin = true;
                    x[s + i] = bound;
                    short = true;
                }
            }
            if !short {
                break;
            }
            // The stretches between pinned samples again.
            let mut i = 0;
            while i < m {
                if pinned[i] {
                    i += 1;
                    continue;
                }
                let a = i;
                while i < m && !pinned[i] {
                    i += 1;
                }
                self.interp.fill(x, s + a, s + i, &self.fit.a);
            }
        }
        // Whatever is still short stands where it was recorded.
        for (v, &bound) in x[s..e].iter_mut().zip(&observed[s..e]) {
            if *v * sign < bound * sign {
                *v = bound;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clipped_sine_gets_its_peaks_back() {
        let rate = 48_000.0;
        let clean: Vec<f64> = (0..3072)
            .map(|n| {
                let t = n as f64 / rate;
                0.9 * (std::f64::consts::TAU * 300.0 * t).sin()
                    + 0.3 * (std::f64::consts::TAU * 910.0 * t).sin()
            })
            .collect();
        let level = 0.7;
        let mut x: Vec<f64> = clean.iter().map(|v| v.clamp(-level, level)).collect();
        let mut marks = vec![0u8; x.len()];
        let marked = mark(&x, level * 0.999, 512, 2560, 200, &mut marks);
        assert!(marked > 100, "{marked}");
        let mut d = Declipper::new(3072, 32, 200);
        d.fit(&x);
        let observed = x.clone();
        assert!(d.repair(&mut x, &observed, &marks, 512, 2560) > 2);
        let before: f64 = (600..2400)
            .map(|n| (clean[n] - clean[n].clamp(-level, level)).powi(2))
            .sum();
        let after: f64 = (600..2400).map(|n| (clean[n] - x[n]).powi(2)).sum();
        assert!(after < 0.2 * before, "error {after:.4} of {before:.4}");
        assert!(x[600..2400].iter().any(|v| v.abs() > level + 0.1));
    }
}
