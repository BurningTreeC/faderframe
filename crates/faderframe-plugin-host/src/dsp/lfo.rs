//! LFOs: free running in hertz or locked to the song (note divisions,
//! dotted and triplet), in the usual shapes.

use super::Noise;
use std::f64::consts::TAU;

/// Note divisions for synced rates and times, in quarter notes.
pub const DIVISIONS: [(&str, f64); 21] = [
    ("1/64", 1.0 / 16.0),
    ("1/32T", 1.0 / 12.0),
    ("1/32", 1.0 / 8.0),
    ("1/16T", 1.0 / 6.0),
    ("1/32D", 3.0 / 16.0),
    ("1/16", 0.25),
    ("1/8T", 1.0 / 3.0),
    ("1/16D", 0.375),
    ("1/8", 0.5),
    ("1/4T", 2.0 / 3.0),
    ("1/8D", 0.75),
    ("1/4", 1.0),
    ("1/2T", 4.0 / 3.0),
    ("1/4D", 1.5),
    ("1/2", 2.0),
    ("1/1T", 8.0 / 3.0),
    ("1/2D", 3.0),
    ("1/1", 4.0),
    ("2/1", 8.0),
    ("4/1", 16.0),
    ("8/1", 32.0),
];

/// A division's length in seconds at `tempo` (BPM).
pub fn division_seconds(index: usize, tempo: f64) -> f64 {
    let q = DIVISIONS[index.min(DIVISIONS.len() - 1)].1;
    q * 60.0 / tempo.max(1.0)
}

/// The name of a division.
pub fn division_name(index: usize) -> &'static str {
    DIVISIONS[index.min(DIVISIONS.len() - 1)].0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Sine,
    Triangle,
    Saw,
    Square,
    /// Random steps.
    SampleHold,
    /// Random, smoothly joined.
    Drift,
}

impl Shape {
    pub const ALL: [Shape; 6] = [
        Shape::Sine,
        Shape::Triangle,
        Shape::Saw,
        Shape::Square,
        Shape::SampleHold,
        Shape::Drift,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Shape::Sine)
    }

    pub fn name(self) -> &'static str {
        match self {
            Shape::Sine => "Sine",
            Shape::Triangle => "Triangle",
            Shape::Saw => "Saw",
            Shape::Square => "Square",
            Shape::SampleHold => "S&H",
            Shape::Drift => "Drift",
        }
    }
}

/// An LFO, −1 to 1.
#[derive(Clone, Copy, Debug)]
pub struct Lfo {
    /// 0 to 1.
    pub phase: f64,
    held: f64,
    from: f64,
    noise: Noise,
}

impl Lfo {
    pub fn new(seed: u32) -> Self {
        Self {
            phase: 0.0,
            held: 0.0,
            from: 0.0,
            noise: Noise::new(seed),
        }
    }

    /// The value at `phase + offset` (offset in cycles; stereo spread).
    #[inline]
    pub fn value(&self, shape: Shape, offset: f64) -> f64 {
        let p = (self.phase + offset).rem_euclid(1.0);
        match shape {
            Shape::Sine => (TAU * p).sin(),
            Shape::Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
            Shape::Saw => 2.0 * p - 1.0,
            Shape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Shape::SampleHold => self.held,
            Shape::Drift => {
                // A raised cosine from the last random value to the next.
                let t = 0.5 - 0.5 * (std::f64::consts::PI * p).cos();
                self.from + (self.held - self.from) * t
            }
        }
    }

    /// Move on by `cycles`.
    #[inline]
    pub fn advance(&mut self, cycles: f64) {
        self.phase += cycles;
        if self.phase >= 1.0 {
            self.phase = self.phase.fract();
            self.from = self.held;
            self.held = self.noise.tick();
        }
    }

    /// Lock the phase to the song: `quarters` into it at a period of
    /// `period_quarters`.
    pub fn sync(&mut self, quarters: f64, period_quarters: f64) {
        self.phase = (quarters / period_quarters.max(1e-6)).rem_euclid(1.0);
    }
}
