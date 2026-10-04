//! True peak (ITU-R BS.1770 Annex 2): the signal's peaks between samples,
//! found by 4× polyphase interpolation (a windowed sinc of 48 taps).

const TAPS: usize = 12;

/// How far behind its input the interpolated peaks are (samples).
pub const LATENCY: usize = 6;

#[derive(Clone, Debug)]
pub struct TruePeak {
    phases: [[f64; TAPS]; 4],
    history: [f64; TAPS],
    pos: usize,
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeak {
    pub fn new() -> Self {
        let n = TAPS * 4;
        let mut phases = [[0.0; TAPS]; 4];
        let centre = (n - 1) as f64 / 2.0;
        for i in 0..n {
            let t = (i as f64 - centre) / 4.0;
            let sinc = if t.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
            };
            let x = i as f64 / (n - 1) as f64;
            let w = 0.42 - 0.5 * (std::f64::consts::TAU * x).cos()
                + 0.08 * (2.0 * std::f64::consts::TAU * x).cos();
            phases[i % 4][i / 4] = sinc * w;
        }
        for p in &mut phases {
            let sum: f64 = p.iter().sum();
            for c in p.iter_mut() {
                *c /= sum;
            }
        }
        Self {
            phases,
            history: [0.0; TAPS],
            pos: 0,
        }
    }

    /// The highest interpolated level round the newest sample.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.history[self.pos] = x;
        self.pos = (self.pos + 1) % TAPS;
        let mut peak = 0.0f64;
        for p in &self.phases {
            let mut acc = 0.0;
            for (k, c) in p.iter().enumerate() {
                acc += c * self.history[(self.pos + TAPS - 1 - k) % TAPS];
            }
            peak = peak.max(acc.abs());
        }
        peak
    }

    pub fn reset(&mut self) {
        self.history = [0.0; TAPS];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_peaking_between_samples_reads_its_true_level() {
        // A quarter of the rate, phase-shifted by 45°: every sample sits at
        // 0.707 of the true peak.
        let mut tp = TruePeak::new();
        let mut sample_peak = 0.0f64;
        let mut true_peak = 0.0f64;
        for n in 0..2_000 {
            let x = (std::f64::consts::FRAC_PI_2 * n as f64 + std::f64::consts::FRAC_PI_4).sin();
            sample_peak = sample_peak.max(x.abs());
            true_peak = true_peak.max(tp.process(x));
        }
        assert!((sample_peak - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert!((true_peak - 1.0).abs() < 0.02, "{true_peak}");
    }
}
