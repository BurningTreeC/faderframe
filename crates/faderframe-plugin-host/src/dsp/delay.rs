//! Delay lines read at fractional delays (cubic Hermite interpolation:
//! smooth enough for chorus and tape-like modulation, cheap enough for many
//! taps).

/// A delay line of up to `capacity` samples.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f64>,
    mask: usize,
    /// Where the next sample goes.
    pos: usize,
}

impl DelayLine {
    pub fn new(capacity: usize) -> Self {
        let len = (capacity + 4).next_power_of_two();
        Self {
            buf: vec![0.0; len],
            mask: len - 1,
            pos: 0,
        }
    }

    /// The longest delay it can read (samples).
    pub fn capacity(&self) -> usize {
        self.buf.len() - 4
    }

    #[inline]
    pub fn push(&mut self, x: f64) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }

    /// The sample `n` samples ago (`n = 0`: the last one pushed, `n ≥ 1`
    /// further back).
    #[inline]
    pub fn tap(&self, n: usize) -> f64 {
        self.buf[(self.pos.wrapping_sub(1 + n)) & self.mask]
    }

    /// The signal `delay` samples back (at least one), interpolated.
    #[inline]
    pub fn read(&self, delay: f64) -> f64 {
        let d = delay.clamp(1.0, self.capacity() as f64);
        let i = d.floor() as usize;
        let t = d - i as f64;
        // Points i−1 .. i+2 samples back (i−1 is newer).
        let y0 = self.tap(i - 1);
        let y1 = self.tap(i);
        let y2 = self.tap(i + 1);
        let y3 = self.tap(i + 2);
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + y1
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_and_fractional_delays() {
        let mut d = DelayLine::new(100);
        for n in 0..50 {
            d.push(n as f64);
        }
        // The last pushed is 49: one sample back reads 48.
        assert_eq!(d.tap(0), 49.0);
        assert_eq!(d.read(1.0), 48.0);
        assert!(
            (d.read(10.5) - 38.5).abs() < 1e-9,
            "a ramp interpolates exactly"
        );
    }
}
