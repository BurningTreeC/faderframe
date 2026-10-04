//! Level detection: one pole coefficients for attack and release times,
//! peak and RMS followers, and the program dependent release analog
//! compressors are loved for (a fast stage riding on a slow one).

/// The one pole factor that settles to 63 % in `ms` at `rate`.
#[inline]
pub fn coeff(ms: f64, rate: f64) -> f64 {
    1.0 - (-1.0 / (ms.max(0.001) * 0.001 * rate)).exp()
}

/// A peak follower: instant to rise at `attack`, falling at `release`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Follower {
    pub env: f64,
    attack: f64,
    release: f64,
}

impl Follower {
    pub fn new(attack_ms: f64, release_ms: f64, rate: f64) -> Self {
        let mut f = Self::default();
        f.set(attack_ms, release_ms, rate);
        f
    }

    pub fn set(&mut self, attack_ms: f64, release_ms: f64, rate: f64) {
        self.attack = coeff(attack_ms, rate);
        self.release = coeff(release_ms, rate);
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let k = if x > self.env {
            self.attack
        } else {
            self.release
        };
        self.env += (x - self.env) * k;
        self.env
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
    }

    pub fn flush(&mut self) {
        self.env = super::flush(self.env);
    }
}

/// A running mean square over a time constant.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeanSquare {
    pub value: f64,
    k: f64,
}

impl MeanSquare {
    pub fn new(ms: f64, rate: f64) -> Self {
        Self {
            value: 0.0,
            k: coeff(ms, rate),
        }
    }

    /// A new time constant (the level stays).
    pub fn set_time(&mut self, ms: f64, rate: f64) {
        self.k = coeff(ms, rate);
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.value += (x * x - self.value) * self.k;
        self.value
    }

    pub fn reset(&mut self) {
        self.value = 0.0;
    }

    pub fn flush(&mut self) {
        self.value = super::flush(self.value);
    }
}

/// Gain smoothing in the dB domain with a program dependent release: a
/// fast release for transients rides on a slow one that tracks how long
/// the signal has been compressed, so sustained material releases slowly
/// and transients quickly.
#[derive(Clone, Copy, Debug, Default)]
pub struct DualRelease {
    /// The smoothed reduction (dB, ≥ 0) and its slow companion.
    pub fast: f64,
    pub slow: f64,
    attack: f64,
    release: f64,
    slow_release: f64,
}

impl DualRelease {
    pub fn set(&mut self, attack_ms: f64, release_ms: f64, rate: f64) {
        self.attack = coeff(attack_ms, rate);
        self.release = coeff(release_ms, rate);
        self.slow_release = coeff(release_ms * 8.0, rate);
    }

    /// The reduction to apply for a wanted reduction `target` (dB ≥ 0).
    #[inline]
    pub fn process(&mut self, target: f64, auto: bool) -> f64 {
        if target > self.fast {
            self.fast += (target - self.fast) * self.attack;
        } else {
            self.fast += (target - self.fast) * self.release;
        }
        if !auto {
            return self.fast;
        }
        // The slow stage follows the reduction up gently and lets go
        // slowly; the result never releases below it.
        let k = if self.fast > self.slow {
            self.release * 0.25
        } else {
            self.slow_release
        };
        self.slow += (self.fast.min(target.max(self.fast * 0.5)) - self.slow) * k;
        self.fast.max(self.slow * 0.7)
    }

    pub fn reset(&mut self) {
        self.fast = 0.0;
        self.slow = 0.0;
    }
}
