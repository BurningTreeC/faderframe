//! Loudness as a device hears it: ITU-R BS.1770 K-weighting and the
//! momentary (400 ms) and short-term (3 s) loudness of what it puts out,
//! kept in 100 ms buckets so a device can read them after every block
//! without allocating or looking back over its output.

use std::f64::consts::PI;

/// A direct form I biquad.
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

    fn reset(&mut self) {
        self.x = [0.0; 2];
        self.y = [0.0; 2];
    }

    fn flush(&mut self) {
        for v in self.x.iter_mut().chain(self.y.iter_mut()) {
            *v = super::flush(*v);
        }
    }
}

/// The K-weighting filter (the head's high shelf, then the RLB high
/// pass) of one channel, designed for any sample rate.
#[derive(Clone, Copy, Debug)]
pub struct KWeighting {
    shelf: Biquad,
    highpass: Biquad,
}

impl KWeighting {
    pub fn new(rate: f64) -> Self {
        let rate = rate.max(1.0);
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
            b: [1.0 / a0, -2.0 / a0, 1.0 / a0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            ..Biquad::default()
        };
        Self { shelf, highpass }
    }

    #[inline]
    pub fn run(&mut self, x: f64) -> f64 {
        self.highpass.run(self.shelf.run(x))
    }

    pub fn reset(&mut self) {
        self.shelf.reset();
        self.highpass.reset();
    }

    pub fn flush(&mut self) {
        self.shelf.flush();
        self.highpass.flush();
    }
}

/// Buckets of 100 ms a window is made of: 4 for momentary, 30 for short
/// term.
const BUCKETS: usize = 30;
const MOMENTARY: usize = 4;

/// Momentary and short-term loudness (LUFS) of up to two channels.
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    weight: [KWeighting; 2],
    /// K-weighted energy summed over the channels per bucket.
    buckets: [f64; BUCKETS],
    head: usize,
    /// Buckets filled since the reset (a window reads nothing before it is
    /// complete).
    filled: usize,
    /// The bucket being filled: its energy and samples so far.
    acc: f64,
    count: usize,
    bucket_len: usize,
}

impl LoudnessMeter {
    pub fn new(rate: f64) -> Self {
        Self {
            weight: [KWeighting::new(rate); 2],
            buckets: [0.0; BUCKETS],
            head: 0,
            filled: 0,
            acc: 0.0,
            count: 0,
            bucket_len: ((rate.max(1.0) * 0.1).round() as usize).max(1),
        }
    }

    /// One frame of (up to) two channels.
    #[inline]
    pub fn add(&mut self, x: [f64; 2], channels: usize) {
        let mut e = 0.0;
        for (k, v) in self.weight.iter_mut().zip(x).take(channels.clamp(1, 2)) {
            let w = k.run(v);
            e += w * w;
        }
        self.acc += e;
        self.count += 1;
        if self.count >= self.bucket_len {
            self.buckets[self.head] = self.acc / self.count as f64;
            self.head = (self.head + 1) % BUCKETS;
            self.filled = (self.filled + 1).min(BUCKETS);
            self.acc = 0.0;
            self.count = 0;
        }
    }

    fn over(&self, buckets: usize) -> f64 {
        if self.filled < buckets {
            return f64::NEG_INFINITY;
        }
        let power: f64 = (1..=buckets)
            .map(|i| self.buckets[(self.head + BUCKETS - i) % BUCKETS])
            .sum::<f64>()
            / buckets as f64;
        -0.691 + 10.0 * power.max(1e-20).log10()
    }

    /// The loudness of the last 400 ms (LUFS; −∞ before there are any).
    pub fn momentary(&self) -> f64 {
        self.over(MOMENTARY)
    }

    /// The loudness of the last 3 s (LUFS; −∞ before there are any).
    pub fn short_term(&self) -> f64 {
        self.over(BUCKETS)
    }

    pub fn flush(&mut self) {
        self.weight.iter_mut().for_each(KWeighting::flush);
    }

    pub fn reset(&mut self) {
        self.weight.iter_mut().for_each(KWeighting::reset);
        self.buckets = [0.0; BUCKETS];
        self.head = 0;
        self.filled = 0;
        self.acc = 0.0;
        self.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_at_minus_twenty_reads_as_bs1770_says() {
        // BS.1770: a 997 Hz sine at −20 dBFS in one channel reads
        // −23.0 LUFS (the sine's power is −23 dB, K-weighting is unity
        // there).
        let rate = 48_000.0;
        let mut m = LoudnessMeter::new(rate);
        for n in 0..(4.0 * rate) as usize {
            let x = 0.1 * (std::f64::consts::TAU * 997.0 * n as f64 / rate).sin();
            m.add([x, 0.0], 2);
        }
        assert!((m.momentary() + 23.0).abs() < 0.1, "{}", m.momentary());
        assert!((m.short_term() + 23.0).abs() < 0.1, "{}", m.short_term());
        // Both channels: 3 dB more.
        let mut m = LoudnessMeter::new(44_100.0);
        for n in 0..(4.0 * 44_100.0) as usize {
            let x = 0.1 * (std::f64::consts::TAU * 997.0 * n as f64 / 44_100.0).sin();
            m.add([x, x], 2);
        }
        assert!((m.short_term() + 20.0).abs() < 0.1, "{}", m.short_term());
        // The high pass takes the lows out: 40 Hz reads much quieter.
        let mut m = LoudnessMeter::new(rate);
        for n in 0..(4.0 * rate) as usize {
            let x = 0.1 * (std::f64::consts::TAU * 40.0 * n as f64 / rate).sin();
            m.add([x, 0.0], 2);
        }
        assert!(m.short_term() < -26.0, "{}", m.short_term());
        m.reset();
        assert_eq!(m.momentary(), f64::NEG_INFINITY);
    }
}
