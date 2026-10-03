use std::sync::atomic::{AtomicU32, Ordering};

/// An `f32` stored in an `AtomicU32`.
#[derive(Debug, Default)]
#[repr(transparent)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }

    #[inline]
    pub fn load(&self, order: Ordering) -> f32 {
        f32::from_bits(self.0.load(order))
    }

    #[inline]
    pub fn store(&self, v: f32, order: Ordering) {
        self.0.store(v.to_bits(), order);
    }

    #[inline]
    pub fn swap(&self, v: f32, order: Ordering) -> f32 {
        f32::from_bits(self.0.swap(v.to_bits(), order))
    }

    /// Atomic maximum for **non-negative** finite values.
    ///
    /// For non-negative IEEE-754 floats the bit pattern ordering equals the
    /// numeric ordering, so an integer `fetch_max` is exact and wait-free.
    /// Negative inputs are clamped to zero (meters only store magnitudes).
    #[inline]
    pub fn fetch_max_non_negative(&self, v: f32, order: Ordering) -> f32 {
        let v = if v > 0.0 { v } else { 0.0 };
        f32::from_bits(self.0.fetch_max(v.to_bits(), order))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_max_orders_like_floats() {
        let a = AtomicF32::new(0.0);
        a.fetch_max_non_negative(0.25, Ordering::Relaxed);
        a.fetch_max_non_negative(0.125, Ordering::Relaxed);
        assert_eq!(a.load(Ordering::Relaxed), 0.25);
        a.fetch_max_non_negative(3.5, Ordering::Relaxed);
        a.fetch_max_non_negative(-10.0, Ordering::Relaxed);
        assert_eq!(a.load(Ordering::Relaxed), 3.5);
        assert_eq!(a.swap(0.0, Ordering::Relaxed), 3.5);
    }
}
