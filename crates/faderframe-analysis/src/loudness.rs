//! Loudness after ITU-R BS.1770-4 / EBU R128: momentary (400 ms),
//! short-term (3 s), gated integrated loudness, loudness range (EBU Tech
//! 3342) and true peak (4× oversampled, BS.1770 Annex 2).

use std::f64::consts::PI;

/// Absolute gate (LUFS).
const ABSOLUTE_GATE: f64 = -70.0;
/// Relative gate of the integrated loudness (LU below the ungated mean).
const RELATIVE_GATE: f64 = -10.0;
/// Relative gate of the loudness range.
const LRA_GATE: f64 = -20.0;
/// Short-term values kept for the history graph (10 minutes at 10 Hz).
const HISTORY: usize = 6000;

fn loudness(mean_square: f64) -> f64 {
    if mean_square <= 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * mean_square.log10()
    }
}

fn mean_square(l: f64) -> f64 {
    10f64.powf((l + 0.691) / 10.0)
}

/// Direct form I biquad.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// The K-weighting filter (high shelf + RLB high-pass) of one channel,
/// designed for any sample rate.
#[derive(Clone, Copy, Debug)]
struct KWeighting {
    shelf: Biquad,
    highpass: Biquad,
}

impl KWeighting {
    fn new(rate: f64) -> Self {
        let (f0, gain, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / rate).tan();
        let vh = 10f64.powf(gain / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            ..Biquad::default()
        };
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let highpass = Biquad {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            ..Biquad::default()
        };
        Self { shelf, highpass }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f64 {
        self.highpass.run(self.shelf.run(x as f64))
    }
}

/// 4× oversampling peak detector (windowed-sinc polyphase interpolator).
#[derive(Clone, Debug)]
struct TruePeak {
    /// Taps per phase.
    phases: [[f64; TP_TAPS]; 4],
    history: [f64; TP_TAPS],
    pos: usize,
}

const TP_TAPS: usize = 12;

impl TruePeak {
    fn new() -> Self {
        let n = TP_TAPS * 4;
        let mut phases = [[0.0; TP_TAPS]; 4];
        let centre = (n - 1) as f64 / 2.0;
        for i in 0..n {
            let t = (i as f64 - centre) / 4.0;
            let sinc = if t.abs() < 1e-12 {
                1.0
            } else {
                (PI * t).sin() / (PI * t)
            };
            // Blackman window.
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos()
                + 0.08 * (4.0 * PI * i as f64 / (n - 1) as f64).cos();
            phases[i % 4][i / 4] = sinc * w;
        }
        // Each phase passes DC at unity.
        for p in &mut phases {
            let sum: f64 = p.iter().sum();
            for c in p.iter_mut() {
                *c /= sum;
            }
        }
        Self {
            phases,
            history: [0.0; TP_TAPS],
            pos: 0,
        }
    }

    /// Peak of the interpolated signal around the new sample.
    #[inline]
    fn run(&mut self, x: f32) -> f64 {
        self.history[self.pos] = x as f64;
        self.pos = (self.pos + 1) % TP_TAPS;
        let mut peak = 0.0f64;
        for p in &self.phases {
            let mut acc = 0.0;
            for (k, c) in p.iter().enumerate() {
                // Newest sample first.
                let h = self.history[(self.pos + TP_TAPS - 1 - k) % TP_TAPS];
                acc += c * h;
            }
            peak = peak.max(acc.abs());
        }
        peak.max((x as f64).abs())
    }
}

/// EBU R128 loudness of a stereo signal.
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    rate: f64,
    filters: [KWeighting; 2],
    true_peak: [TruePeak; 2],
    /// Frames per 100 ms block.
    block: usize,
    /// Frames into the current block and its energy so far.
    fill: usize,
    energy: f64,
    /// The last 30 block energies (3 s), newest last.
    blocks: std::collections::VecDeque<f64>,
    /// 400 ms gating blocks (75 % overlap) while measuring.
    gating: Vec<f64>,
    /// Short-term values (LUFS) while measuring, for the loudness range.
    short_terms: Vec<f64>,
    /// Short-term loudness every 100 ms (history graph).
    history: std::collections::VecDeque<f32>,
    max_momentary: f64,
    max_short_term: f64,
    max_true_peak: f64,
}

/// What a [`LoudnessMeter`] reads now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    pub momentary: f64,
    pub short_term: f64,
    /// `-inf` until enough has been measured.
    pub integrated: f64,
    /// Loudness range (LU).
    pub range: f64,
    pub max_momentary: f64,
    pub max_short_term: f64,
    /// Highest true peak since the reset (dBTP).
    pub true_peak: f64,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = sample_rate.max(8000) as f64;
        Self {
            rate,
            filters: [KWeighting::new(rate); 2],
            true_peak: [TruePeak::new(), TruePeak::new()],
            block: (rate / 10.0).round() as usize,
            fill: 0,
            energy: 0.0,
            blocks: std::collections::VecDeque::with_capacity(31),
            gating: Vec::new(),
            short_terms: Vec::new(),
            history: std::collections::VecDeque::with_capacity(HISTORY),
            max_momentary: f64::NEG_INFINITY,
            max_short_term: f64::NEG_INFINITY,
            max_true_peak: f64::NEG_INFINITY,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate as u32
    }

    /// Forget the integrated measurement, loudness range and maxima.
    pub fn reset(&mut self) {
        self.gating.clear();
        self.short_terms.clear();
        self.history.clear();
        self.max_momentary = f64::NEG_INFINITY;
        self.max_short_term = f64::NEG_INFINITY;
        self.max_true_peak = f64::NEG_INFINITY;
    }

    /// Feed audio; `measuring` adds it to the integrated loudness, the
    /// range and the maxima (while playing).
    pub fn process(&mut self, left: &[f32], right: &[f32], measuring: bool) {
        for (&l, &r) in left.iter().zip(right) {
            let (kl, kr) = (self.filters[0].run(l), self.filters[1].run(r));
            self.energy += kl * kl + kr * kr;
            if measuring {
                let tp = self.true_peak[0].run(l).max(self.true_peak[1].run(r));
                if tp > 0.0 {
                    self.max_true_peak = self.max_true_peak.max(20.0 * tp.log10());
                }
            }
            self.fill += 1;
            if self.fill == self.block {
                self.end_block(measuring);
            }
        }
    }

    fn end_block(&mut self, measuring: bool) {
        let ms = self.energy / self.block as f64;
        self.energy = 0.0;
        self.fill = 0;
        if self.blocks.len() == 30 {
            self.blocks.pop_front();
        }
        self.blocks.push_back(ms);
        let momentary = self.window(4);
        let short_term = self.window(30);
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(loudness(short_term) as f32);
        if !measuring {
            return;
        }
        if self.blocks.len() >= 4 {
            self.gating.push(momentary);
            self.max_momentary = self.max_momentary.max(loudness(momentary));
        }
        if self.blocks.len() >= 30 {
            let st = loudness(short_term);
            self.short_terms.push(st);
            self.max_short_term = self.max_short_term.max(st);
        }
    }

    /// Mean square of the last `n` blocks (fewer while starting).
    fn window(&self, n: usize) -> f64 {
        let n = n.min(self.blocks.len());
        if n == 0 {
            return 0.0;
        }
        self.blocks.iter().rev().take(n).sum::<f64>() / n as f64
    }

    pub fn read(&self) -> Loudness {
        Loudness {
            momentary: loudness(self.window(4)),
            short_term: loudness(self.window(30)),
            integrated: self.integrated(),
            range: self.range(),
            max_momentary: self.max_momentary,
            max_short_term: self.max_short_term,
            true_peak: self.max_true_peak,
        }
    }

    /// Gated integrated loudness (LUFS).
    pub fn integrated(&self) -> f64 {
        let above: Vec<f64> = self
            .gating
            .iter()
            .copied()
            .filter(|&e| loudness(e) > ABSOLUTE_GATE)
            .collect();
        if above.is_empty() {
            return f64::NEG_INFINITY;
        }
        let gate = loudness(above.iter().sum::<f64>() / above.len() as f64) + RELATIVE_GATE;
        let kept: Vec<f64> = above.into_iter().filter(|&e| loudness(e) > gate).collect();
        if kept.is_empty() {
            return f64::NEG_INFINITY;
        }
        loudness(kept.iter().sum::<f64>() / kept.len() as f64)
    }

    /// Loudness range (LU): the spread of gated short-term values between
    /// their 10th and 95th percentiles.
    pub fn range(&self) -> f64 {
        let above: Vec<f64> = self
            .short_terms
            .iter()
            .copied()
            .filter(|&l| l > ABSOLUTE_GATE)
            .collect();
        if above.len() < 2 {
            return 0.0;
        }
        let mean = above.iter().map(|&l| mean_square(l)).sum::<f64>() / above.len() as f64;
        let gate = loudness(mean) + LRA_GATE;
        let mut kept: Vec<f64> = above.into_iter().filter(|&l| l > gate).collect();
        if kept.len() < 2 {
            return 0.0;
        }
        kept.sort_by(f64::total_cmp);
        let at = |p: f64| kept[((kept.len() - 1) as f64 * p).round() as usize];
        at(0.95) - at(0.10)
    }

    /// Short-term loudness every 100 ms, oldest first.
    pub fn history(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.history.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, db: f64, secs: f64, phase: f64) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..(rate as f64 * secs) as usize)
            .map(|i| (a * (2.0 * PI * hz * i as f64 / rate as f64 + phase).sin()) as f32)
            .collect()
    }

    #[test]
    fn ebu_tech_3341_case_1_stereo_sine_at_minus_23() {
        for rate in [44_100, 48_000, 96_000] {
            let mut m = LoudnessMeter::new(rate);
            let s = sine(rate, 1000.0, -23.0, 20.0, 0.0);
            m.process(&s, &s, true);
            let r = m.read();
            for (name, v) in [
                ("momentary", r.momentary),
                ("short-term", r.short_term),
                ("integrated", r.integrated),
            ] {
                assert!((v + 23.0).abs() < 0.1, "{rate} Hz {name}: {v}");
            }
            assert!(r.range < 0.1, "steady tone has no range: {}", r.range);
        }
    }

    #[test]
    fn gating_drops_silence_and_quiet_parts() {
        let rate = 48_000;
        let mut m = LoudnessMeter::new(rate);
        let quiet = sine(rate, 1000.0, -46.0, 20.0, 0.0);
        let loud = sine(rate, 1000.0, -26.0, 20.0, 0.0);
        let silence = vec![0.0f32; rate as usize * 10];
        for part in [&quiet, &silence, &loud] {
            m.process(part, part, true);
        }
        // 20 LU below the ungated mean: under the relative gate.
        let r = m.read();
        assert!((r.integrated + 26.0).abs() < 0.2, "{}", r.integrated);
        m.reset();
        assert_eq!(m.read().integrated, f64::NEG_INFINITY);
    }

    #[test]
    fn ebu_tech_3342_case_1_range_of_two_levels() {
        let rate = 48_000;
        let mut m = LoudnessMeter::new(rate);
        let a = sine(rate, 1000.0, -20.0, 20.0, 0.0);
        let b = sine(rate, 1000.0, -30.0, 20.0, 0.0);
        m.process(&a, &a, true);
        m.process(&b, &b, true);
        let r = m.read();
        assert!((r.range - 10.0).abs() < 0.2, "{}", r.range);
    }

    #[test]
    fn true_peak_sees_inter_sample_peaks() {
        let rate = 48_000;
        let mut m = LoudnessMeter::new(rate);
        // fs/4 at 45°: every sample is ±0.707, the waveform peaks at 1.0.
        let s = sine(rate, 12_000.0, 0.0, 1.0, PI / 4.0);
        let sample_peak = s.iter().fold(0.0f32, |p, v| p.max(v.abs()));
        assert!((20.0 * sample_peak.log10() + 3.01).abs() < 0.05);
        m.process(&s, &s, true);
        let tp = m.read().true_peak;
        assert!(tp > -0.5 && tp < 0.3, "true peak {tp} dBTP");
    }
}
