//! Clicks and crackle: found where an autoregressive model of the sound
//! cannot predict it, filled by least-squares AR interpolation
//! (Vaseghi and Rayner; Godsill and Rayner, *Digital Audio Restoration*).
//!
//! Per window ([`crate::blocks`]) the model is fitted, the prediction error
//! `e` computed and passed through the model again backwards (the matched
//! filter of an impulse in an AR process: it puts a click's energy back on
//! the click instead of smearing it over the `p` samples after). Samples
//! where that statistic stands over `K` times its robust spread (the median
//! of its magnitude: the music's own peaks barely move it) are clicks; `K`
//! falls from 13.5 to 3.5 as Sensitivity rises. Crackle — dense, tiny
//! impulses — is taken at a lower threshold but only in runs as short as
//! crackle is; only what reaches half the local peak of the statistic
//! counts (the matched filter rings for the model's order). Runs are
//! widened a little, joined when close, continued while the sound after
//! them stays away from the model's extrapolation from before the click (a
//! click's decay is quieter than its edge), and filled when they are no
//! longer than the widest click allowed (longer ones are the music: a
//! transient, a consonant). Two passes: the second fits the model to the
//! repaired window and catches what the first one's clicks hid.

use crate::ar::{ArFit, Interpolator};

/// How clicks are found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickSettings {
    /// 0…1: more clicks found (and more risk of touching the music).
    pub sensitivity: f64,
    /// The widest click repaired (samples).
    pub max_width: usize,
    /// 0…1: crackle taken too, at a lower threshold.
    pub crackle: f64,
    /// The longest run counted as crackle (samples).
    pub crackle_width: usize,
}

/// Marks: a sample over the click threshold, or only over the crackle one.
pub const CLICK: u8 = 1;
pub const CRACKLE: u8 = 2;

/// What of the local peak of the statistic a sample must reach to be part
/// of the click (the rest is the matched filter's ringing).
const SIDELOBE: f64 = 0.5;

/// A run goes on while the sound after it strays from the extrapolation by
/// more than this share of the detection threshold (in the prediction
/// error's own spread).
const EXTEND: f64 = 0.6;

/// How far the allowance for the extrapolation's error may grow (times the
/// one-step error's).
const GROWTH: f64 = 2.0;

/// The threshold factor for a sensitivity.
pub fn threshold(sensitivity: f64) -> f64 {
    3.5 + (1.0 - sensitivity.clamp(0.0, 1.0)) * 10.0
}

/// One channel's click finder and repairer, for windows of one length.
pub struct Declicker {
    fit: ArFit,
    interp: Interpolator,
    stat: Vec<f64>,
    resid: Vec<f64>,
    mags: Vec<f64>,
    /// The statistic's robust spread in the last analysis.
    pub spread: f64,
    /// The prediction error's.
    pub spread_e: f64,
}

impl Declicker {
    /// For windows of `len` samples, a model of `order`, gaps up to
    /// `max_gap`.
    pub fn new(len: usize, order: usize, max_gap: usize) -> Self {
        Self {
            fit: ArFit::new(len, order),
            interp: Interpolator::new(max_gap, order),
            stat: vec![0.0; len],
            resid: vec![0.0; len],
            mags: vec![0.0; len],
            spread: 0.0,
            spread_e: 0.0,
        }
    }

    pub fn order(&self) -> usize {
        self.fit.order()
    }

    /// The detection statistic of the last analysis.
    pub fn statistic(&self) -> &[f64] {
        &self.stat
    }

    /// The model last fitted.
    pub fn model(&self) -> &[f64] {
        &self.fit.a
    }

    /// Fit the model to `x` and compute the detection statistic; its spread
    /// is measured over `from..to`.
    pub fn analyse(&mut self, x: &[f64], from: usize, to: usize) {
        let p = self.order();
        let len = x.len().min(self.stat.len());
        self.fit.fit(&x[..len]);
        self.resid.fill(0.0);
        for n in p..len {
            self.resid[n] = self.fit.residual(x, n);
        }
        self.stat.fill(0.0);
        let a = &self.fit.a;
        for n in p..len.saturating_sub(p) {
            let mut d = 0.0;
            for (i, ai) in a.iter().enumerate() {
                d += ai * self.resid[n + i];
            }
            self.stat[n] = d;
        }
        let (from, to) = (from.max(p), to.min(len.saturating_sub(p)));
        let count = to.saturating_sub(from);
        if count == 0 {
            self.spread = 0.0;
            return;
        }
        for (m, s) in self.mags[..count].iter_mut().zip(&self.stat[from..to]) {
            *m = s.abs();
        }
        let mid = count / 2;
        let (_, median, _) = self.mags[..count].select_nth_unstable_by(mid, f64::total_cmp);
        // A floor under the spread: a sound the model predicts perfectly (a
        // pure tone) would make rounding errors clicks.
        let rms = (x[from..to].iter().map(|v| v * v).sum::<f64>() / count as f64).sqrt();
        let gain: f64 = self.fit.a.iter().map(|a| a * a).sum();
        self.spread = (*median / 0.6745).max(1e-6 * rms * gain);
        for (m, s) in self.mags[..count].iter_mut().zip(&self.resid[from..to]) {
            *m = s.abs();
        }
        let (_, median, _) = self.mags[..count].select_nth_unstable_by(mid, f64::total_cmp);
        self.spread_e = (*median / 0.6745).max(1e-6 * rms);
    }

    /// Mark the samples of `from..to` over the thresholds (`CLICK`, or
    /// `CRACKLE` when only over the lower one) into `marks` (kept where
    /// already marked: channels can share their marks).
    pub fn mark(&self, s: &ClickSettings, marks: &mut [u8], from: usize, to: usize) {
        if self.spread <= 1e-15 {
            return;
        }
        let k = threshold(s.sensitivity) * self.spread;
        let kc = k * (1.0 - 0.6 * s.crackle.clamp(0.0, 1.0));
        let crackle = s.crackle > 0.0;
        let to = to.min(self.stat.len()).min(marks.len());
        let p = self.order();
        let floor = if crackle { kc } else { k };
        for (n, mark) in marks.iter_mut().enumerate().take(to).skip(from) {
            let d = self.stat[n].abs();
            if d <= floor {
                continue;
            }
            // The matched filter rings for the model's order either side
            // of a click: only what stands near the local peak is the
            // click itself.
            let (a, b) = (n.saturating_sub(p), (n + p + 1).min(self.stat.len()));
            let peak = self.stat[a..b].iter().fold(0.0f64, |m, v| m.max(v.abs()));
            if d < SIDELOBE * peak {
                continue;
            }
            if d > k {
                *mark = CLICK;
            } else if *mark == 0 {
                *mark = CRACKLE;
            }
        }
    }

    /// Fill the runs of `marks` that start in `from..to` (at most
    /// `max_width` long) in `x`; returns how many. A click's decay can be
    /// quieter than its edge and smooth enough to pass for the music: a run
    /// goes on while the sound after it stays away from what the model
    /// predicts from before the click alone (extrapolated through the run),
    /// up to `max_width`, and `marks` follow.
    pub fn repair(
        &mut self,
        x: &mut [f64],
        marks: &mut [u8],
        from: usize,
        to: usize,
        max_width: usize,
        s: &ClickSettings,
    ) -> usize {
        let mut count = 0;
        let mut n = from;
        let p = self.order();
        let len = marks.len().min(x.len());
        let limit = EXTEND * threshold(s.sensitivity) * self.spread_e;
        let look = (p / 4).max(4);
        while n < to.min(len) {
            if marks[n] == 0 {
                n += 1;
                continue;
            }
            let start = n;
            while n < len && marks[n] != 0 {
                n += 1;
            }
            let mut end = n;
            if end - start > max_width || start < p {
                continue;
            }
            // The model run on from before the click, and how far that may
            // stray from the music k samples in (the k-step prediction
            // error grows by the model's impulse response). `resid` and
            // `mags` are free here: the analysis is done with them.
            let reach = (start + max_width + look).min(len);
            let span = reach - start;
            let mut energy = 0.0;
            for k in 0..span {
                let mut h = if k == 0 { 1.0 } else { 0.0 };
                for (j, a) in self.fit.a.iter().enumerate().skip(1).take(k) {
                    h -= a * self.stat[k - j];
                }
                // (`stat` holds the impulse response from here on.)
                self.stat[k] = h;
                energy += h * h;
                // Held within twice the one-step error: a tonal model's
                // response barely decays, and the allowance would soon
                // swallow the click.
                self.mags[k] = limit * energy.sqrt().min(GROWTH);
            }
            for i in start..reach {
                let mut y = 0.0;
                for (j, a) in self.fit.a.iter().enumerate().skip(1) {
                    let k = i - j;
                    y -= a * if k < start { x[k] } else { self.resid[k] };
                }
                self.resid[i] = y;
            }
            while end - start < max_width && end + look <= reach {
                let away =
                    (end..end + look).any(|i| (x[i] - self.resid[i]).abs() > self.mags[i - start]);
                if !away {
                    break;
                }
                end += 1;
            }
            if !self.interp.fill(x, start, end, &self.fit.a) {
                continue;
            }
            marks[start..end].fill(CLICK);
            n = n.max(end);
            count += 1;
        }
        count
    }
}

/// Turn raw marks into runs to repair: crackle-only runs longer than
/// crackle is dropped, runs widened by `pad` each side and joined across
/// gaps shorter than `join`. Only `from..to` is looked at.
pub fn shape_runs(
    marks: &mut [u8],
    from: usize,
    to: usize,
    pad: usize,
    join: usize,
    crackle_width: usize,
) {
    let to = to.min(marks.len());
    // Drop crackle-only runs that are too long to be crackle.
    let mut n = from;
    while n < to {
        if marks[n] == 0 {
            n += 1;
            continue;
        }
        let s = n;
        let mut strong = false;
        while n < to && marks[n] != 0 {
            strong |= marks[n] == CLICK;
            n += 1;
        }
        if !strong && n - s > crackle_width {
            marks[s..n].fill(0);
        }
    }
    // Widen: a pass marks pad either side (as CLICK, the run is decided).
    let mut n = from;
    while n < to {
        if marks[n] == 0 {
            n += 1;
            continue;
        }
        let s = n;
        while n < to && marks[n] != 0 {
            n += 1;
        }
        let a = s.saturating_sub(pad).max(from);
        let b = (n + pad).min(to);
        marks[a..b].fill(CLICK);
        n = b;
    }
    // Join across short gaps.
    let mut last_end: Option<usize> = None;
    let mut n = from;
    while n < to {
        if marks[n] == 0 {
            n += 1;
            continue;
        }
        let s = n;
        if let Some(e) = last_end
            && s - e < join
        {
            marks[e..s].fill(CLICK);
        }
        while n < to && marks[n] != 0 {
            n += 1;
        }
        last_end = Some(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Music-like: two tones and a little noise.
    fn music(len: usize, rate: f64) -> Vec<f64> {
        let mut noise = crate::Noise::new(9);
        (0..len)
            .map(|n| {
                let t = n as f64 / rate;
                0.3 * (std::f64::consts::TAU * 220.0 * t).sin()
                    + 0.2 * (std::f64::consts::TAU * 657.0 * t).sin()
                    + 0.002 * noise.next()
            })
            .collect()
    }

    #[test]
    fn a_click_is_found_and_filled() {
        let rate = 48_000.0;
        let clean = music(2048, rate);
        let mut x = clean.clone();
        for (i, v) in x[1000..1006].iter_mut().enumerate() {
            *v += if i % 2 == 0 { 0.6 } else { -0.4 };
        }
        let mut d = Declicker::new(2048, 32, 256);
        let s = ClickSettings {
            sensitivity: 0.5,
            max_width: 96,
            crackle: 0.0,
            crackle_width: 12,
        };
        d.analyse(&x, 256, 1792);
        let mut marks = vec![0u8; 2048];
        d.mark(&s, &mut marks, 256, 1792);
        assert!(marks[1000..1006].iter().any(|&m| m != 0), "found");
        let false_marks = marks[256..900].iter().filter(|&&m| m != 0).count()
            + marks[1100..1792].iter().filter(|&&m| m != 0).count();
        assert_eq!(false_marks, 0, "nothing else");
        shape_runs(&mut marks, 256, 1792, 3, 8, 12);
        assert_eq!(d.repair(&mut x, &mut marks, 256, 1792, 96, &s), 1);
        // The second pass: the model fitted again without the click.
        d.analyse(&x, 256, 1792);
        assert_eq!(d.repair(&mut x, &mut marks, 256, 1792, 96, &s), 1);
        // The click's error was 1.25; what is left is about the noise's
        // own (the model cannot know the noise under the click).
        let err: f64 = (990..1016)
            .map(|n| (x[n] - clean[n]).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!(err < 0.04, "residual {err}");
    }
}
