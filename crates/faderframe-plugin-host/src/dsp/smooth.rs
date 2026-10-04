//! Parameter smoothing: values move to where they are set over a few
//! milliseconds instead of jumping (no zipper noise).

/// An exponentially smoothed value.
#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    pub value: f64,
    pub target: f64,
    k: f64,
}

impl Smoothed {
    /// Starting at `value`, settling over `ms` at `rate`.
    pub fn new(value: f64, ms: f64, rate: f64) -> Self {
        Self {
            value,
            target: value,
            k: super::env::coeff(ms, rate),
        }
    }

    #[inline]
    pub fn set(&mut self, target: f64) {
        self.target = target;
    }

    /// Jump to the target.
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    #[inline]
    pub fn tick(&mut self) -> f64 {
        self.value += (self.target - self.value) * self.k;
        if (self.value - self.target).abs() < 1e-9 {
            self.value = self.target;
        }
        self.value
    }

    pub fn settled(&self) -> bool {
        self.value == self.target
    }
}
