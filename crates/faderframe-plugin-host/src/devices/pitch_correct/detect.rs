//! The live pitch detector: YIN (de Cheveigné and Kawahara, 2002) on the
//! input low-passed and decimated to about 8 kHz, every 2 ms, over a
//! window as long as the lowest period of the range, then refined on the
//! full-rate signal around the period found.
//!
//! The cumulative mean normalised difference picks the first dip under
//! [`THRESHOLD`] (YIN's absolute threshold: the shortest period that
//! repeats well, so no octave too low), refined by a parabola. Octave
//! guards: while a note sounds, a period an octave (or a twelfth) from
//! the one before is taken only when the lag near the old one repeats
//! clearly worse — a strong second harmonic or a vowel's first formant on
//! it does not throw the pitch an octave for a frame. Quieter than
//! [`GATE_DB`] or with no clear repetition (aperiodicity over
//! [`UNVOICED`]) is unvoiced. Allocation-free after [`Detector::new`].

/// The analysis rate the input is decimated to (about).
const ANALYSIS_RATE: f64 = 8_000.0;
/// Analysis every this many decimated samples (2 ms).
pub const HOP: usize = 16;
/// YIN's threshold, and the aperiodicity over which a frame is unvoiced.
pub const THRESHOLD: f64 = 0.15;
pub const UNVOICED: f64 = 0.35;
/// The input must be this loud (window RMS, dBFS) to have a pitch.
pub const GATE_DB: f64 = -50.0;
/// A jump by about an octave (log2 within this of ±1) is checked against
/// the period before.
const OCTAVE_REACH: f64 = 0.12;

/// A simple biquad (RBJ low-pass).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LowPass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl LowPass {
    pub fn new(freq: f64, q: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * (freq / rate).min(0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - c) / 2.0 / a0,
            b1: (1.0 - c) / a0,
            b2: (1.0 - c) / 2.0 / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// What one analysis found.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Found {
    /// The input frame (full rate, counted from the start) the window is
    /// centred on.
    pub at: f64,
    /// The period in full-rate frames (0: unvoiced).
    pub period: f64,
    /// 1 − aperiodicity.
    pub clarity: f64,
}

/// The detector's state and buffers.
pub struct Detector {
    rate: f64,
    /// Decimation factor.
    factor: usize,
    /// Anti-alias filters before decimating (two sections).
    aa: [LowPass; 2],
    /// A DC/rumble high-pass (one pole) state.
    hp: (f64, f64),
    hp_coef: f64,
    /// Decimated history (ring) and how many were written.
    ring: Vec<f32>,
    written: usize,
    /// Full-rate mono history (ring) for the refinement.
    full: Vec<f32>,
    full_written: usize,
    /// Decimated lags searched and the window.
    min_lag: usize,
    max_lag: usize,
    window: usize,
    /// Work buffers.
    diff: Vec<f64>,
    cmnd: Vec<f64>,
    frame: Vec<f32>,
    /// Samples into the current decimation step and the current hop.
    phase: usize,
    since: usize,
    /// The last voiced period (decimated lag), for the octave guard.
    last_lag: Option<f64>,
}

impl Detector {
    /// For input at `rate`, pitches from `lowest` to `highest` Hz
    /// (allocates; control thread).
    pub fn new(rate: f64, lowest: f64, highest: f64) -> Self {
        let factor = ((rate / ANALYSIS_RATE).floor() as usize).max(1);
        let low_rate = rate / factor as f64;
        let max_lag = (low_rate / lowest).ceil() as usize + 2;
        let min_lag = ((low_rate / highest).floor() as usize).max(2);
        let window = max_lag;
        let span = window + max_lag + 2;
        let full_span = (span + 4) * factor;
        Self {
            rate,
            factor,
            aa: [
                LowPass::new(0.38 * low_rate, 0.541_196_1, rate),
                LowPass::new(0.38 * low_rate, 1.306_563, rate),
            ],
            hp: (0.0, 0.0),
            hp_coef: (-std::f64::consts::TAU * 0.5 * lowest / rate).exp(),
            ring: vec![0.0; span.next_power_of_two()],
            written: 0,
            full: vec![0.0; full_span.next_power_of_two()],
            full_written: 0,
            min_lag,
            max_lag,
            window,
            diff: vec![0.0; max_lag + 2],
            cmnd: vec![0.0; max_lag + 2],
            frame: vec![0.0; span],
            phase: 0,
            since: 0,
            last_lag: None,
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.aa {
            f.reset();
        }
        self.hp = (0.0, 0.0);
        self.ring.fill(0.0);
        self.written = 0;
        self.full.fill(0.0);
        self.full_written = 0;
        self.phase = 0;
        self.since = 0;
        self.last_lag = None;
    }

    /// Full-rate frames between analyses.
    pub fn hop_frames(&self) -> usize {
        HOP * self.factor
    }

    /// Take one input sample; `Some` when an analysis ran on it.
    pub fn push(&mut self, x: f32) -> Option<Found> {
        // Rumble and DC out.
        let x = f64::from(x);
        let y = self.hp_coef * (self.hp.1 + x - self.hp.0);
        self.hp = (x, y);
        let fm = self.full.len() - 1;
        self.full[self.full_written & fm] = y as f32;
        self.full_written += 1;
        let first = self.aa[0].run(y);
        let low = self.aa[1].run(first);
        self.phase += 1;
        if self.phase < self.factor {
            return None;
        }
        self.phase = 0;
        let m = self.ring.len() - 1;
        self.ring[self.written & m] = low as f32;
        self.written += 1;
        self.since += 1;
        if self.since < HOP {
            return None;
        }
        self.since = 0;
        Some(self.analyse())
    }

    /// YIN on the newest window.
    fn analyse(&mut self) -> Found {
        let span = self.window + self.max_lag;
        // The window's centre, in full-rate frames.
        let at = (self.full_written as f64) - (span as f64 / 2.0) * self.factor as f64;
        if self.written < span {
            return Found {
                at,
                ..Found::default()
            };
        }
        let m = self.ring.len() - 1;
        let from = self.written - span;
        for (k, f) in self.frame[..span].iter_mut().enumerate() {
            *f = self.ring[(from + k) & m];
        }
        let x = &self.frame[..span];
        let w = self.window;
        let energy: f64 = x[..w].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / w as f64;
        let level_db = 10.0 * energy.max(1e-20).log10();
        if level_db < GATE_DB {
            self.last_lag = None;
            return Found {
                at,
                ..Found::default()
            };
        }
        // The difference function and its cumulative mean normalisation.
        let max = self.max_lag.min(span - w);
        self.diff[0] = 0.0;
        self.cmnd[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=max {
            let mut d = 0.0f64;
            for j in 0..w {
                let e = f64::from(x[j]) - f64::from(x[j + tau]);
                d += e * e;
            }
            self.diff[tau] = d;
            running += d;
            self.cmnd[tau] = if running > 0.0 {
                d * tau as f64 / running
            } else {
                1.0
            };
        }
        let Some(lag) = self.pick(max) else {
            self.last_lag = None;
            return Found {
                at,
                ..Found::default()
            };
        };
        let aperiodicity = self.cmnd_at(lag);
        if aperiodicity > UNVOICED {
            self.last_lag = None;
            return Found {
                at,
                ..Found::default()
            };
        }
        self.last_lag = Some(lag);
        let period = self.refine(lag * self.factor as f64);
        Found {
            at,
            period,
            clarity: (1.0 - aperiodicity).clamp(0.0, 1.0),
        }
    }

    /// The normalised difference at a fractional lag (linear between
    /// lags).
    fn cmnd_at(&self, lag: f64) -> f64 {
        let i = (lag.floor() as usize).min(self.cmnd.len() - 2);
        let f = lag - i as f64;
        self.cmnd[i] + (self.cmnd[i + 1] - self.cmnd[i]) * f
    }

    /// The bottom of the dip around `tau` (walking down), refined by a
    /// parabola.
    fn dip(&self, mut tau: usize, max: usize) -> f64 {
        while tau < max && self.cmnd[tau + 1] < self.cmnd[tau] {
            tau += 1;
        }
        while tau > self.min_lag && self.cmnd[tau - 1] < self.cmnd[tau] {
            tau -= 1;
        }
        if tau <= 1 || tau >= max {
            return tau as f64;
        }
        let (a, b, c) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
        let den = a - 2.0 * b + c;
        if den.abs() < 1e-12 {
            tau as f64
        } else {
            tau as f64 + (0.5 * (a - c) / den).clamp(-0.5, 0.5)
        }
    }

    /// The lag (decimated, fractional) of the period.
    fn pick(&self, max: usize) -> Option<f64> {
        let lo = self.min_lag.min(max);
        let first = (lo..max).find(|&t| self.cmnd[t] < THRESHOLD);
        let lag = match first {
            Some(t) => self.dip(t, max),
            None => {
                // No clear repetition: the deepest dip, judged by caller.
                let (t, _) = (lo..=max)
                    .map(|t| (t, self.cmnd[t]))
                    .min_by(|a, b| a.1.total_cmp(&b.1))?;
                self.dip(t, max)
            }
        };
        // The octave guard: a jump to about twice or half (or a third)
        // of the period before keeps the old one while it repeats nearly
        // as well.
        if let Some(last) = self.last_lag {
            let ratio = (lag / last).log2();
            let jumped = [(-1.0f64), 1.0, -(3f64.log2()), 3f64.log2()]
                .iter()
                .any(|r| (ratio - r).abs() < OCTAVE_REACH);
            if jumped {
                let near = (last.round() as usize).clamp(lo, max);
                let kept = self.dip(near, max);
                if (kept / last).log2().abs() < OCTAVE_REACH
                    && self.cmnd_at(kept) < (self.cmnd_at(lag) + 0.1).max(THRESHOLD)
                {
                    return Some(kept);
                }
            }
        }
        Some(lag)
    }

    /// The period at full rate near `guess` frames: the plain difference
    /// over the window's span, its minimum refined by a parabola.
    fn refine(&self, guess: f64) -> f64 {
        let f = self.factor;
        if f == 1 {
            return guess;
        }
        let m = self.full.len() - 1;
        let w = (self.window * f).min(self.full_written / 2);
        let lo = (guess.floor() as usize).saturating_sub(f).max(2);
        let hi = guess.ceil() as usize + f;
        let end = self.full_written;
        if end < w + hi + 1 {
            return guess;
        }
        let start = end - w - hi;
        let d = |tau: usize| {
            let mut s = 0.0f64;
            for j in 0..w {
                let a = f64::from(self.full[(start + j) & m]);
                let b = f64::from(self.full[(start + j + tau) & m]);
                s += (a - b) * (a - b);
            }
            s
        };
        let mut best = (lo, f64::MAX);
        for tau in lo..=hi {
            let v = d(tau);
            if v < best.1 {
                best = (tau, v);
            }
        }
        let tau = best.0;
        if tau <= lo || tau >= hi {
            return tau as f64;
        }
        let (a, b, c) = (d(tau - 1), best.1, d(tau + 1));
        let den = a - 2.0 * b + c;
        if den.abs() < 1e-18 {
            tau as f64
        } else {
            tau as f64 + (0.5 * (a - c) / den).clamp(-0.5, 0.5)
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(det: &mut Detector, x: impl Fn(usize) -> f32, n: usize) -> Vec<Found> {
        (0..n).filter_map(|i| det.push(x(i))).collect()
    }

    #[test]
    fn a_steady_tone_is_found_to_a_cent() {
        let rate = 48_000.0;
        for hz in [82.0, 110.0, 196.0, 330.0, 440.0, 880.0] {
            let mut det = Detector::new(rate, 60.0, 1500.0);
            let found = run(
                &mut det,
                |i| (0.3 * (std::f64::consts::TAU * hz * i as f64 / rate).sin()) as f32,
                24_000,
            );
            let last = found.last().copied().unwrap_or_default();
            assert!(last.period > 0.0, "{hz} Hz unvoiced");
            let cents = 1200.0 * (rate / last.period / hz).log2();
            assert!(cents.abs() < 1.0, "{hz} Hz off by {cents} cents");
        }
    }

    #[test]
    fn a_strong_second_harmonic_does_not_throw_the_pitch_an_octave() {
        let rate = 48_000.0;
        let hz = 150.0;
        let mut det = Detector::new(rate, 60.0, 1500.0);
        let found = run(
            &mut det,
            |i| {
                let t = std::f64::consts::TAU * hz * i as f64 / rate;
                (0.15 * t.sin() + 0.3 * (2.0 * t).sin() + 0.1 * (3.0 * t).sin()) as f32
            },
            48_000,
        );
        let voiced: Vec<f64> = found
            .iter()
            .skip(10)
            .filter(|f| f.period > 0.0)
            .map(|f| rate / f.period)
            .collect();
        assert!(voiced.len() > 100);
        assert!(
            voiced.iter().all(|f| (f / hz - 1.0).abs() < 0.01),
            "{voiced:?}"
        );
    }

    #[test]
    fn noise_and_silence_have_no_pitch() {
        let rate = 48_000.0;
        let mut det = Detector::new(rate, 60.0, 1500.0);
        let found = run(&mut det, |_| 0.0, 9_600);
        assert!(found.iter().all(|f| f.period == 0.0));
        let found = run(
            &mut det,
            |i| (crate::devices::rig::white(i, 3) * 0.2) as f32,
            24_000,
        );
        let voiced = found.iter().filter(|f| f.period > 0.0).count();
        assert!(voiced * 10 < found.len(), "{voiced} of {}", found.len());
    }
}
