//! Signal analysis for the Tools view (mastering meters): EBU R128
//! loudness with true peak ([`LoudnessMeter`]), peak/RMS levels with
//! K-System scales ([`LevelMeter`]), stereo correlation and goniometer
//! points ([`PhaseMeter`]) and an FFT [`Spectrum`].
//!
//! Everything works on blocks of stereo audio handed over from the audio
//! thread (see `faderframe_realtime::ScopeRing`) and runs on the control
//! side; [`Analyzer`] combines them.

#![forbid(unsafe_code)]

pub mod chroma;
pub mod delivery;
mod dynamics;
mod loudness;
pub mod melody;
pub mod pitch;
pub mod polyvoice;
mod spectrum;
pub mod structure;
pub mod tempo;
pub mod vinyl;
pub mod voice;

pub use dynamics::{Dynamics, DynamicsMeter};
pub use loudness::{Loudness, LoudnessMeter, integrated_weighted, speaker_weight};
pub use spectrum::{FLOOR_DB, Spectrum, fft};

use std::collections::VecDeque;

/// Sample peak and RMS (300 ms) per channel, with peak hold.
#[derive(Clone, Debug)]
pub struct LevelMeter {
    /// Mean squares of 10 ms slices over the last 300 ms, per channel.
    slices: [VecDeque<f64>; 2],
    slice: usize,
    fill: usize,
    acc: [f64; 2],
    peak: [f32; 2],
    hold: [f32; 2],
    /// Highest peak since the reset (linear).
    max: f32,
}

/// Levels of one channel (dBFS).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub peak: f32,
    pub rms: f32,
    pub hold: f32,
}

fn db(v: f64) -> f32 {
    if v <= 1e-10 {
        -200.0
    } else {
        (20.0 * v.log10()) as f32
    }
}

impl LevelMeter {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            slices: [VecDeque::with_capacity(31), VecDeque::with_capacity(31)],
            slice: (sample_rate as usize / 100).max(1),
            fill: 0,
            acc: [0.0; 2],
            peak: [0.0; 2],
            hold: [0.0; 2],
            max: 0.0,
        }
    }

    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        for (&l, &r) in left.iter().zip(right) {
            for (c, v) in [l, r].into_iter().enumerate() {
                self.acc[c] += (v as f64) * (v as f64);
                self.peak[c] = self.peak[c].max(v.abs());
            }
            self.fill += 1;
            if self.fill == self.slice {
                for c in 0..2 {
                    if self.slices[c].len() == 30 {
                        self.slices[c].pop_front();
                    }
                    self.slices[c].push_back(self.acc[c] / self.slice as f64);
                    self.acc[c] = 0.0;
                }
                self.fill = 0;
            }
        }
    }

    /// Current levels; the peak (and its hold) fall with `dt` seconds.
    pub fn read(&mut self, dt: f32) -> [Level; 2] {
        let mut out = [Level {
            peak: -200.0,
            rms: -200.0,
            hold: -200.0,
        }; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let p = self.peak[c];
            self.max = self.max.max(p);
            // 20 dB/s fall-back, 2 s hold decay.
            self.hold[c] = (self.hold[c] * 10f32.powf(-dt * 6.0 / 20.0)).max(p);
            let n = self.slices[c].len().max(1) as f64;
            o.rms = db((self.slices[c].iter().sum::<f64>() / n).sqrt());
            o.peak = db(p as f64);
            o.hold = db(self.hold[c] as f64);
            self.peak[c] = p * 10f32.powf(-dt * 20.0 / 20.0);
        }
        out
    }

    /// Highest sample peak since the reset (dBFS).
    pub fn max_peak(&self) -> f32 {
        db(self.max as f64)
    }

    pub fn reset(&mut self) {
        self.max = 0.0;
        self.hold = [0.0; 2];
    }
}

/// Stereo correlation (−1 … +1) and recent samples for a goniometer.
#[derive(Clone, Debug)]
pub struct PhaseMeter {
    lr: f64,
    ll: f64,
    rr: f64,
    correlation: f32,
    points: VecDeque<(f32, f32)>,
}

/// Goniometer points kept.
pub const GONIO_POINTS: usize = 2048;

impl Default for PhaseMeter {
    fn default() -> Self {
        Self {
            lr: 0.0,
            ll: 0.0,
            rr: 0.0,
            correlation: 0.0,
            points: VecDeque::with_capacity(GONIO_POINTS),
        }
    }
}

impl PhaseMeter {
    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        for (&l, &r) in left.iter().zip(right) {
            // Exponential averages (~100 ms at 48 kHz).
            const A: f64 = 1.0 / 4800.0;
            let (l64, r64) = (l as f64, r as f64);
            self.lr += (l64 * r64 - self.lr) * A;
            self.ll += (l64 * l64 - self.ll) * A;
            self.rr += (r64 * r64 - self.rr) * A;
        }
        let norm = (self.ll * self.rr).sqrt();
        self.correlation = if norm > 1e-12 {
            (self.lr / norm).clamp(-1.0, 1.0) as f32
        } else {
            0.0
        };
        let skip = left.len().saturating_sub(GONIO_POINTS);
        for (&l, &r) in left.iter().zip(right).skip(skip) {
            if self.points.len() == GONIO_POINTS {
                self.points.pop_front();
            }
            self.points.push_back((l, r));
        }
    }

    pub fn correlation(&self) -> f32 {
        self.correlation
    }

    /// Recent (left, right) samples, oldest first.
    pub fn points(&self) -> impl ExactSizeIterator<Item = (f32, f32)> + '_ {
        self.points.iter().copied()
    }
}

/// All meters of the Tools view over one stereo source.
#[derive(Clone, Debug)]
pub struct Analyzer {
    pub loudness: LoudnessMeter,
    /// Crest factor and DR value of what was measured.
    pub dynamics: DynamicsMeter,
    pub level: LevelMeter,
    pub phase: PhaseMeter,
    pub spectrum: Spectrum,
}

impl Analyzer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            loudness: LoudnessMeter::new(sample_rate),
            dynamics: DynamicsMeter::new(sample_rate),
            level: LevelMeter::new(sample_rate),
            phase: PhaseMeter::default(),
            spectrum: Spectrum::new(sample_rate, 8192),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.loudness.sample_rate()
    }

    /// Feed audio; `measuring` (playing) accumulates the integrated
    /// loudness, range and maxima.
    pub fn process(&mut self, left: &[f32], right: &[f32], measuring: bool) {
        self.loudness.process(left, right, measuring);
        if measuring {
            self.dynamics.process(left, right);
        }
        self.level.process(left, right);
        self.phase.process(left, right);
        self.spectrum.process(left, right);
    }

    /// Start a new measurement.
    pub fn reset(&mut self) {
        self.loudness.reset();
        self.dynamics.reset();
        self.level.reset();
        self.spectrum.reset_peaks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_of_mono_inverted_and_unrelated_signals() {
        let a: Vec<f32> = (0..20_000).map(|i| ((i as f32) * 0.05).sin()).collect();
        let b: Vec<f32> = (0..20_000).map(|i| ((i as f32) * 0.0731).cos()).collect();
        let neg: Vec<f32> = a.iter().map(|v| -v).collect();
        let corr = |l: &[f32], r: &[f32]| {
            let mut m = PhaseMeter::default();
            m.process(l, r);
            m.correlation()
        };
        assert!(corr(&a, &a) > 0.99);
        assert!(corr(&a, &neg) < -0.99);
        assert!(corr(&a, &b).abs() < 0.3);
        let mut m = PhaseMeter::default();
        m.process(&a, &b);
        assert_eq!(m.points().len(), GONIO_POINTS);
    }

    #[test]
    fn levels_of_a_full_scale_sine() {
        let rate = 48_000;
        let tone: Vec<f32> = (0..rate)
            .map(|i| (2.0 * std::f32::consts::PI * 997.0 * i as f32 / rate as f32).sin())
            .collect();
        let mut m = LevelMeter::new(rate);
        m.process(&tone, &tone);
        let [l, _] = m.read(0.0);
        assert!(l.peak > -0.01, "{}", l.peak);
        assert!((l.rms + 3.01).abs() < 0.05, "{}", l.rms);
        assert!(m.max_peak() > -0.01);
    }
}
