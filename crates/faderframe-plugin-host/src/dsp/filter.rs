//! Filters: cascades designed by the EQ (analog-shaped to Nyquist), a
//! zero-delay-feedback state variable filter for fast modulation, one pole
//! low and high passes and a DC blocker.

pub use crate::eq::design::{BandShape, BandType, Coefs, MAX_SECTIONS};
use std::f64::consts::PI;

/// The state of one transposed direct form II biquad.
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    pub fn run(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    pub fn flush(&mut self) {
        if self.z1.abs() < 1e-25 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-25 {
            self.z2 = 0.0;
        }
    }
}

/// A band of the EQ's kind on `C` channels: designed on [`Filter::set`]
/// when the shape changes.
#[derive(Clone, Debug)]
pub struct Filter<const C: usize> {
    sections: [Coefs; MAX_SECTIONS],
    used: usize,
    state: [[State; MAX_SECTIONS]; C],
    designed: Option<(BandShape, f64)>,
}

impl<const C: usize> Default for Filter<C> {
    fn default() -> Self {
        Self {
            sections: [Coefs::IDENTITY; MAX_SECTIONS],
            used: 0,
            state: [[State::default(); MAX_SECTIONS]; C],
            designed: None,
        }
    }
}

impl<const C: usize> Filter<C> {
    pub fn new(shape: BandShape, rate: f64) -> Self {
        let mut f = Self::default();
        f.set(shape, rate);
        f
    }

    /// Follow `shape` (redesigned only when it changed).
    pub fn set(&mut self, shape: BandShape, rate: f64) {
        if self.designed == Some((shape, rate)) {
            return;
        }
        self.designed = Some((shape, rate));
        self.used = crate::eq::design::design(&shape, rate, &mut self.sections);
    }

    /// Pass everything (no sections).
    pub fn clear(&mut self) {
        self.used = 0;
        self.designed = None;
    }

    pub fn is_active(&self) -> bool {
        self.used > 0
    }

    #[inline]
    pub fn process(&mut self, ch: usize, x: f64) -> f64 {
        let mut y = x;
        for (s, c) in self.state[ch]
            .iter_mut()
            .zip(&self.sections)
            .take(self.used)
        {
            y = s.run(c, y);
        }
        y
    }

    pub fn reset(&mut self) {
        self.state = [[State::default(); MAX_SECTIONS]; C];
    }

    pub fn flush(&mut self) {
        for s in self.state.iter_mut().flatten() {
            s.flush();
        }
    }

    /// The magnitude (dB) at `freq`, as designed.
    pub fn db_at(&self, freq: f64) -> f64 {
        let rate = self.designed.map_or(48_000.0, |(_, r)| r);
        crate::eq::design::sections_db(&self.sections[..self.used], rate, freq)
    }
}

/// The outputs of a [`Svf`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SvfOut {
    pub low: f64,
    pub band: f64,
    pub high: f64,
}

impl SvfOut {
    pub fn notch(&self) -> f64 {
        self.low + self.high
    }
}

/// A trapezoidal (zero delay feedback) state variable filter (after
/// Andrew Simper): stable and click free under fast modulation.
#[derive(Clone, Copy, Debug)]
pub struct Svf {
    g: f64,
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
    ic1: f64,
    ic2: f64,
}

impl Default for Svf {
    fn default() -> Self {
        let mut s = Self {
            g: 0.0,
            k: 1.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            ic1: 0.0,
            ic2: 0.0,
        };
        s.set(1_000.0, 0.707, 48_000.0);
        s
    }
}

impl Svf {
    /// Cutoff `freq` (Hz) and resonance `q` at `rate`.
    #[inline]
    pub fn set(&mut self, freq: f64, q: f64, rate: f64) {
        self.g = (PI * freq.clamp(5.0, 0.49 * rate) / rate).tan();
        self.k = 1.0 / q.max(0.05);
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> SvfOut {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        SvfOut {
            low: v2,
            band: v1,
            high: x - self.k * v1 - v2,
        }
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    pub fn flush(&mut self) {
        self.ic1 = super::flush(self.ic1);
        self.ic2 = super::flush(self.ic2);
    }
}

/// A one pole low pass (`process`) whose high pass is the rest.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    a: f64,
    z: f64,
}

impl OnePole {
    pub fn new(freq: f64, rate: f64) -> Self {
        let mut p = Self::default();
        p.set(freq, rate);
        p
    }

    pub fn set(&mut self, freq: f64, rate: f64) {
        self.a = 1.0 - (-2.0 * PI * freq.clamp(0.1, 0.49 * rate) / rate).exp();
    }

    #[inline]
    pub fn low(&mut self, x: f64) -> f64 {
        self.z += self.a * (x - self.z);
        self.z
    }

    #[inline]
    pub fn high(&mut self, x: f64) -> f64 {
        x - self.low(x)
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    pub fn flush(&mut self) {
        self.z = super::flush(self.z);
    }
}

/// Takes DC off (a 5 Hz first order high pass).
#[derive(Clone, Copy, Debug)]
pub struct DcBlock {
    r: f64,
    x1: f64,
    y1: f64,
}

impl DcBlock {
    pub fn new(rate: f64) -> Self {
        Self {
            r: (-2.0 * PI * 5.0 / rate).exp(),
            x1: 0.0,
            y1: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = super::flush(y);
        y
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The level of a sine through `f` (from its RMS over whole periods:
    /// a sample peak misses the crest).
    fn gain_of(mut f: impl FnMut(f64) -> f64, freq: f64) -> f64 {
        let rate = 48_000.0;
        let mut sum = 0.0f64;
        for n in 0..48_000 {
            let y = f((2.0 * PI * freq * n as f64 / rate).sin());
            if n >= 24_000 {
                sum += y * y;
            }
        }
        10.0 * (2.0 * sum / 24_000.0).log10()
    }

    #[test]
    fn the_svf_cuts_and_passes() {
        let mut s = Svf::default();
        s.set(1_000.0, std::f64::consts::FRAC_1_SQRT_2, 48_000.0);
        assert!(gain_of(|x| s.process(x).low, 100.0).abs() < 0.1);
        let mut s = Svf::default();
        s.set(1_000.0, std::f64::consts::FRAC_1_SQRT_2, 48_000.0);
        assert!((gain_of(|x| s.process(x).low, 1_000.0) + 3.0).abs() < 0.2);
        let mut s = Svf::default();
        s.set(1_000.0, std::f64::consts::FRAC_1_SQRT_2, 48_000.0);
        assert!(gain_of(|x| s.process(x).high, 10_000.0).abs() < 0.1);
    }

    #[test]
    fn a_designed_filter_follows_its_shape() {
        let shape = BandShape {
            kind: BandType::HighCut,
            freq: 2_000.0,
            gain: 0.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            slope: 24.0,
        };
        let mut f: Filter<1> = Filter::new(shape, 48_000.0);
        assert!((f.db_at(2_000.0) + 3.01).abs() < 0.1);
        let got = gain_of(|x| f.process(0, x), 8_000.0);
        assert!(
            (got - f.db_at(8_000.0)).abs() < 0.1,
            "{got} vs {}",
            f.db_at(8_000.0)
        );
        let mut dc = DcBlock::new(48_000.0);
        let mut last = 1.0;
        for _ in 0..48_000 {
            last = dc.process(1.0);
        }
        assert!(last.abs() < 1e-3);
    }
}
