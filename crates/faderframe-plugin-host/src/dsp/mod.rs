//! Building blocks the built-in devices share: filters (the EQ's matched
//! designs, a zero-delay-feedback state variable filter, one poles, a DC
//! blocker), oversampling, interpolated delay lines, envelope followers,
//! LFOs (tempo synced too), parameter smoothing, true peak detection and
//! noise. Everything here is allocation free once made.

pub mod delay;
pub mod env;
pub mod filter;
pub mod lfo;
pub mod oversample;
pub mod smooth;
pub mod truepeak;

/// dB to linear gain.
#[inline]
pub fn gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Linear gain to dB (−240 for nothing).
#[inline]
pub fn db(gain: f64) -> f64 {
    20.0 * gain.abs().max(1e-12).log10()
}

/// A denormal-free value.
#[inline]
pub fn flush(x: f64) -> f64 {
    if x.abs() < 1e-25 { 0.0 } else { x }
}

/// White noise (xorshift), −1 to 1.
#[derive(Clone, Copy, Debug)]
pub struct Noise(u32);

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    #[inline]
    pub fn tick(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0
    }
}

/// Equal power crossfade gains for `t` from 0 (all `a`) to 1 (all `b`).
#[inline]
pub fn equal_power(t: f64) -> (f64, f64) {
    let a = t.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}
