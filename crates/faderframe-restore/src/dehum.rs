//! Hum and buzz: a notch at the mains frequency and each harmonic, the
//! notches following the mains as it drifts.
//!
//! The notches are biquads of a constant bandwidth in Hz (`width`), so the
//! upper harmonics are notched as narrowly as the fundamental; Depth mixes
//! the notched signal with the input (`x − depth · (x − notched)`).
//!
//! The mains is found and followed by complex demodulation: each of the
//! first harmonics is shifted to 0 Hz and low-passed (8 Hz); what is left
//! turns at the hum's offset from the nominal frequency, so the phase step
//! between readings 32 samples apart is the offset. The readings of the
//! harmonics are weighed by how tonal each is (its narrow band against the
//! signal's level round it) and averaged into the fundamental, slowly
//! (half a second), held within ±3 % of the nominal frequency. In Auto, the
//! 50 Hz family is compared with the 60 Hz one over a second or so (3 dB
//! of hysteresis) to choose the nominal frequency.

use std::f64::consts::TAU;

/// The most harmonics notched (the fundamental is the first).
pub const MAX_HARMONICS: usize = 32;
/// Harmonics demodulated for tracking and for the 50/60 Hz decision.
const TRACKED: usize = 4;
/// Samples between tracker readings (and notch updates).
const STEP: usize = 32;
/// The tracker's low pass (Hz; three one-pole stages).
const TRACK_LP: f64 = 6.0;
/// How far the tracked fundamental may move from the nominal one.
const TRACK_RANGE: f64 = 0.03;

/// How hum is taken out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HumSettings {
    /// The nominal fundamental (Hz); `None`: decide between 50 and 60.
    pub fundamental: Option<f64>,
    /// Harmonics notched, the fundamental counted.
    pub harmonics: usize,
    /// Each notch's bandwidth (Hz).
    pub width_hz: f64,
    /// 0…1: how much of the notched part is taken.
    pub depth: f64,
    /// Follow the mains as it drifts.
    pub track: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    /// A notch at `f` Hz, `width` Hz wide (−3 dB), unity elsewhere.
    fn notch(f: f64, width: f64, rate: f64) -> Self {
        let w0 = TAU * f / rate;
        let q = (f / width.max(0.05)).max(0.1);
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let c = -2.0 * w0.cos();
        Self {
            b0: 1.0 / a0,
            b1: c / a0,
            b2: 1.0 / a0,
            a1: c / a0,
            a2: (1.0 - alpha) / a0,
        }
    }
}

/// One harmonic shifted to 0 Hz and low-passed.
#[derive(Clone, Copy, Debug)]
struct Demod {
    /// Phase increment per sample (radians) and phase now.
    inc: f64,
    phase: f64,
    /// Three one-pole stages on the complex signal.
    s1: (f64, f64),
    s2: (f64, f64),
    s3: (f64, f64),
    prev: (f64, f64),
    /// Smoothed narrow-band power.
    power: f64,
}

impl Demod {
    fn new(f: f64, rate: f64) -> Self {
        Self {
            inc: TAU * f / rate,
            phase: 0.0,
            s1: (0.0, 0.0),
            s2: (0.0, 0.0),
            s3: (0.0, 0.0),
            prev: (0.0, 0.0),
            power: 0.0,
        }
    }

    #[inline]
    fn push(&mut self, x: f64, k: f64) {
        let (s, c) = self.phase.sin_cos();
        self.phase += self.inc;
        if self.phase > TAU {
            self.phase -= TAU;
        }
        let (re, im) = (x * c, -x * s);
        self.s1.0 += k * (re - self.s1.0);
        self.s1.1 += k * (im - self.s1.1);
        self.s2.0 += k * (self.s1.0 - self.s2.0);
        self.s2.1 += k * (self.s1.1 - self.s2.1);
        self.s3.0 += k * (self.s2.0 - self.s3.0);
        self.s3.1 += k * (self.s2.1 - self.s3.1);
    }

    /// The phase step since the last reading (radians over `STEP`
    /// samples); the narrow band's smoothed power follows.
    fn read(&mut self, smooth: f64) -> f64 {
        let z = self.s3;
        let (pr, pi) = self.prev;
        // z · conj(prev)
        let re = z.0 * pr + z.1 * pi;
        let im = z.1 * pr - z.0 * pi;
        self.prev = z;
        let p = z.0 * z.0 + z.1 * z.1;
        self.power += smooth * (p - self.power);
        im.atan2(re)
    }
}

/// Hum removal for several channels (the notches the same on all; the
/// tracker listens to their sum).
pub struct Dehum {
    rate: f64,
    coefs: [Biquad; MAX_HARMONICS],
    active: usize,
    states: Vec<[[f64; 2]; MAX_HARMONICS]>,
    /// The fundamental the notches stand at, and what they were made for.
    freq: f64,
    made_for: (f64, usize, f64),
    nominal: f64,
    demods: [Demod; 2 * TRACKED],
    /// The family listened to for tracking: 0 = 50 Hz, 1 = 60 Hz (Auto),
    /// or the given fundamental's (index 0).
    family: usize,
    broadband: f64,
    offset: f64,
    step: usize,
    lp: f64,
    /// Settings the demodulators were made for.
    demod_for: (Option<f64>, bool),
}

impl Dehum {
    pub fn new(rate: f64, channels: usize) -> Self {
        let rate = rate.max(1.0);
        let mut d = Self {
            rate,
            coefs: [Biquad::default(); MAX_HARMONICS],
            active: 0,
            states: vec![[[0.0; 2]; MAX_HARMONICS]; channels],
            freq: 50.0,
            made_for: (0.0, 0, 0.0),
            nominal: 50.0,
            demods: [Demod::new(50.0, rate); 2 * TRACKED],
            family: 0,
            broadband: 0.0,
            offset: 0.0,
            step: 0,
            lp: 1.0 - (-TAU * TRACK_LP / rate).exp(),
            demod_for: (Some(-1.0), false),
        };
        d.make_demods(None);
        d
    }

    /// The fundamental the notches stand at now (Hz).
    pub fn frequency(&self) -> f64 {
        self.freq
    }

    /// The nominal mains it follows (50 or 60 in Auto).
    pub fn nominal(&self) -> f64 {
        self.nominal
    }

    fn make_demods(&mut self, fundamental: Option<f64>) {
        let families: [f64; 2] = match fundamental {
            Some(f) => [f, f],
            None => [50.0, 60.0],
        };
        for (fam, base) in families.iter().enumerate() {
            for h in 0..TRACKED {
                self.demods[fam * TRACKED + h] = Demod::new(base * (h + 1) as f64, self.rate);
            }
        }
        if let Some(f) = fundamental {
            self.nominal = f;
            self.family = 0;
        }
        self.offset = 0.0;
    }

    fn update_notches(&mut self, s: &HumSettings) {
        let key = (self.freq, s.harmonics, s.width_hz);
        if self.made_for == key {
            return;
        }
        self.made_for = key;
        let mut n = 0;
        for h in 1..=s.harmonics.min(MAX_HARMONICS) {
            let f = self.freq * h as f64;
            if f >= 0.45 * self.rate {
                break;
            }
            self.coefs[n] = Biquad::notch(f, s.width_hz, self.rate);
            n += 1;
        }
        self.active = n;
    }

    /// Take the trackers' readings: the fundamental and (Auto) the family.
    fn track(&mut self, s: &HumSettings) {
        let smooth = 1.0 - (-(STEP as f64) / (0.4 * self.rate)).exp();
        let mut weight = 0.0;
        let mut sum = 0.0;
        let mut family_power = [0.0f64; 2];
        let families = if s.fundamental.is_none() { 2 } else { 1 };
        let bw = (2.0 * TRACK_LP / self.rate).max(1e-9);
        for (fam, heard) in family_power.iter_mut().enumerate().take(families) {
            for h in 0..TRACKED {
                let d = &mut self.demods[fam * TRACKED + h];
                let dphi = d.read(smooth);
                *heard += d.power;
                if fam != self.family {
                    continue;
                }
                // How tonal: the narrow band against the level round it.
                // Weighed by the smoothed power: the reading's own power
                // ripples with the image the low pass leaves, as its phase
                // step does, and the two would bias the mean.
                let power = d.power;
                let tonal = power / (power + 4.0 * bw * self.broadband + 1e-20);
                let offset = dphi / (TAU * STEP as f64) * self.rate / (h + 1) as f64;
                if offset.abs() < TRACK_RANGE * 2.0 * self.nominal {
                    let w = tonal * tonal * power;
                    weight += w;
                    sum += w * offset;
                }
            }
        }
        if s.fundamental.is_none() {
            // 3 dB of hysteresis between the families.
            let (a, b) = (family_power[0], family_power[1]);
            let want = if self.family == 0 && b > 2.0 * a {
                1
            } else if self.family == 1 && a > 2.0 * b {
                0
            } else {
                self.family
            };
            if want != self.family {
                self.family = want;
                self.nominal = if want == 0 { 50.0 } else { 60.0 };
                self.offset = 0.0;
            }
        }
        if s.track && weight > 1e-24 {
            let k = 1.0 - (-(STEP as f64) / (0.5 * self.rate)).exp();
            let target = sum / weight;
            self.offset += k * (target - self.offset);
            let lim = TRACK_RANGE * self.nominal;
            self.offset = self.offset.clamp(-lim, lim);
        }
        if !s.track {
            self.offset = 0.0;
        }
        self.freq = self.nominal + self.offset;
    }

    /// Process one sample of every channel in place (`x`, as many as the
    /// channels made for).
    #[inline]
    pub fn process(&mut self, x: &mut [f64], s: &HumSettings) {
        if self.demod_for != (s.fundamental, s.track) {
            self.demod_for = (s.fundamental, s.track);
            self.make_demods(s.fundamental);
            self.freq = self.nominal;
        }
        if self.step == 0 {
            self.track(s);
            self.update_notches(s);
        }
        self.step = (self.step + 1) % STEP;
        // The trackers hear the channels' sum.
        let sum: f64 = x.iter().sum::<f64>() / x.len().max(1) as f64;
        self.broadband += (1.0 / (0.2 * self.rate)) * (sum * sum - self.broadband);
        let families = if s.fundamental.is_none() { 2 } else { 1 };
        let k = self.lp;
        for d in self.demods[..families * TRACKED].iter_mut() {
            d.push(sum, k);
        }
        let depth = s.depth.clamp(0.0, 1.0);
        for (c, v) in x.iter_mut().enumerate() {
            let Some(st) = self.states.get_mut(c) else {
                break;
            };
            let input = *v;
            let mut y = input;
            for (q, z) in self.coefs[..self.active].iter().zip(st.iter_mut()) {
                let out = q.b0 * y + z[0];
                z[0] = q.b1 * y - q.a1 * out + z[1];
                z[1] = q.b2 * y - q.a2 * out;
                y = out;
            }
            *v = input - depth * (input - y);
        }
    }

    /// Flush denormals (once a block).
    pub fn flush(&mut self) {
        for st in &mut self.states {
            for z in st.iter_mut() {
                for v in z.iter_mut() {
                    if v.abs() < 1e-25 {
                        *v = 0.0;
                    }
                }
            }
        }
    }

    /// Silence: the notches' state cleared, the tracker started again.
    pub fn reset(&mut self) {
        self.states.fill([[0.0; 2]; MAX_HARMONICS]);
        self.demod_for = (Some(-1.0), false);
        self.broadband = 0.0;
        self.step = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_db(x: &[f64], f: f64, rate: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        let n = x.len() as f64;
        for (k, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (TAU * k as f64 / n).cos();
            re += v * w * (TAU * f * k as f64 / rate).cos();
            im += v * w * (TAU * f * k as f64 / rate).sin();
        }
        20.0 * (4.0 * re.hypot(im) / n).max(1e-12).log10()
    }

    #[test]
    fn hum_that_drifts_is_followed_and_notched() {
        let rate = 48_000.0;
        // 60.6 Hz mains (1 % fast) with harmonics, under a 1 kHz tone.
        let f0 = 60.6;
        let len = (rate * 6.0) as usize;
        let mut x: Vec<f64> = (0..len)
            .map(|n| {
                let t = n as f64 / rate;
                let hum: f64 = (1..=6)
                    .map(|h| 0.05 / h as f64 * (TAU * f0 * h as f64 * t).sin())
                    .sum();
                hum + 0.2 * (TAU * 1_000.0 * t).sin()
            })
            .collect();
        let s = HumSettings {
            fundamental: None,
            harmonics: 8,
            width_hz: 2.0,
            depth: 1.0,
            track: true,
        };
        let mut d = Dehum::new(rate, 1);
        for v in &mut x {
            let mut one = [*v];
            d.process(&mut one, &s);
            *v = one[0];
        }
        assert_eq!(d.nominal(), 60.0);
        assert!((d.frequency() - f0).abs() < 0.005, "{}", d.frequency());
        let tail = &x[len - (rate as usize)..];
        for h in 1..=6 {
            let db = tone_db(tail, f0 * h as f64, rate);
            assert!(
                db < 20.0 * (0.05 / h as f64).log10() - 25.0,
                "harmonic {h}: {db:.1} dB"
            );
        }
        let keep = tone_db(tail, 1_000.0, rate);
        assert!((keep - 20.0 * 0.2f64.log10()).abs() < 0.2, "tone {keep:.2}");
    }
}
