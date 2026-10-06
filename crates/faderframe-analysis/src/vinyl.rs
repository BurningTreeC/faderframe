//! What a record's groove makes of a song: measurements for a vinyl
//! premaster.
//!
//! * Bass out of phase — below 150 Hz a lathe cuts the difference of the
//!   channels vertically; much side signal there, or left and right
//!   pulling against each other, lifts the stylus out of the groove (the
//!   cutting engineer would have to make the bass mono or turn it down).
//! * Esses and harsh highs — the groove's curvature limits how loud high
//!   frequencies can be (worst at the inner groove, where the groove runs
//!   slowest); the loudest 10 ms of the sibilance band shows the peaks.
//! * Brightness — the highs' share of the whole: where a song belongs on a
//!   side (bright songs away from the inner groove).
//!
//! The levels are of the audio as measured; a premaster's gain moves the
//! sibilance level by as much.

/// A song's vinyl measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VinylReport {
    /// Below 150 Hz: the side's energy relative to the mid's (dB; −∞ for
    /// mono).
    pub low_side_db: f64,
    /// Below 150 Hz: the lowest correlation of left and right over 400 ms
    /// windows with bass in them (−1 … 1; 1 without bass).
    pub low_correlation: f64,
    /// The loudest 10 ms of the sibilance band, 4.5–10 kHz (dB, a full
    /// scale sine reading 0).
    pub sibilance_db: f64,
    /// The energy above 8 kHz relative to the whole signal's (dB).
    pub highs_db: f64,
}

/// The lowest frequency a lathe's vertical (side) motion takes freely.
pub const LOW_SPLIT_HZ: f64 = 150.0;

/// A biquad section (direct form I).
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    /// RBJ low or high pass at `f` with quality `q`.
    fn pass(high: bool, f: f64, q: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * f.min(0.45 * rate) / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + cos) / 2.0, -(1.0 + cos))
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos)
        };
        Self {
            b: [b0 / a0, b1 / a0, b0 / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// A fourth order Butterworth (two sections).
#[derive(Clone, Copy, Debug)]
struct Butter4([Biquad; 2]);

impl Butter4 {
    fn new(high: bool, f: f64, rate: f64) -> Self {
        Self([
            Biquad::pass(high, f, 0.541_196_100_146_197, rate),
            Biquad::pass(high, f, 1.306_562_964_876_376_7, rate),
        ])
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.0[0].run(x);
        self.0[1].run(y)
    }
}

fn db(power: f64) -> f64 {
    if power > 0.0 {
        10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Measure a song (one or two channels; more: the first two).
pub fn measure(audio: &[Vec<f32>], sample_rate: u32) -> VinylReport {
    let rate = f64::from(sample_rate.max(8_000));
    let frames = audio.first().map_or(0, Vec::len);
    let left = audio.first().map(Vec::as_slice).unwrap_or(&[]);
    let right = audio.get(1).map_or(left, Vec::as_slice);
    let mut low = [
        Butter4::new(false, LOW_SPLIT_HZ, rate),
        Butter4::new(false, LOW_SPLIT_HZ, rate),
    ];
    let mut sib = (
        Butter4::new(true, 4_500.0, rate),
        Biquad::pass(false, 10_000.0, std::f64::consts::FRAC_1_SQRT_2, rate),
    );
    let mut highs = Butter4::new(true, 8_000.0, rate);
    let (mut mid_e, mut side_e, mut all_e, mut high_e) = (0.0, 0.0, 0.0, 0.0);
    // 400 ms windows of the bass: (Σ l·r, Σ l², Σ r²).
    let window = (0.4 * rate) as usize;
    let mut windows: Vec<(f64, f64, f64)> = Vec::with_capacity(frames / window.max(1) + 1);
    let mut acc = (0.0, 0.0, 0.0);
    // 10 ms slices of the sibilance band.
    let slice = (0.01 * rate) as usize;
    let (mut sib_acc, mut sib_n, mut sib_max) = (0.0, 0, 0.0f64);
    for i in 0..frames {
        let (l, r) = (f64::from(left[i]), f64::from(right[i]));
        let (ll, lr) = (low[0].run(l), low[1].run(r));
        let (m, s) = (0.5 * (ll + lr), 0.5 * (ll - lr));
        mid_e += m * m;
        side_e += s * s;
        acc.0 += ll * lr;
        acc.1 += ll * ll;
        acc.2 += lr * lr;
        if (i + 1) % window == 0 {
            windows.push(acc);
            acc = (0.0, 0.0, 0.0);
        }
        let mono = 0.5 * (l + r);
        all_e += 0.5 * (l * l + r * r);
        let h = highs.run(mono);
        high_e += h * h;
        let b = sib.1.run(sib.0.run(mono));
        sib_acc += b * b;
        sib_n += 1;
        if sib_n == slice {
            sib_max = sib_max.max(sib_acc / slice as f64);
            sib_acc = 0.0;
            sib_n = 0;
        }
    }
    // Windows with bass worth hearing: within 30 dB of the loudest and
    // over −50 dBFS RMS (a filter's onset ripple is not bass).
    let energy = |w: &(f64, f64, f64)| w.1 + w.2;
    let loudest = windows.iter().map(energy).fold(0.0, f64::max);
    let floor = (loudest * 1e-3).max(2.0 * window as f64 * 1e-5);
    let low_correlation = windows
        .iter()
        .filter(|w| energy(w) > floor)
        .map(|w| w.0 / (w.1 * w.2).sqrt().max(1e-30))
        .fold(1.0f64, f64::min)
        .clamp(-1.0, 1.0);
    VinylReport {
        low_side_db: if mid_e > 0.0 {
            db(side_e / mid_e)
        } else if side_e > 0.0 {
            // Exactly opposite channels.
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        },
        low_correlation,
        // Twice the mean square: a sine's amplitude.
        sibilance_db: db(2.0 * sib_max),
        highs_db: if all_e > 0.0 {
            db(high_e / all_e)
        } else {
            f64::NEG_INFINITY
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(f: f64, amp: f64, seconds: f64, phase: f64) -> Vec<f32> {
        (0..(seconds * f64::from(SR)) as usize)
            .map(|i| {
                (amp * (std::f64::consts::TAU * f * i as f64 / f64::from(SR) + phase).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn bass_out_of_phase_is_found() {
        // The same bass on both sides: no side, fully correlated.
        let bass = sine(60.0, 0.5, 3.0, 0.0);
        let r = measure(&[bass.clone(), bass.clone()], SR);
        assert!(r.low_side_db < -60.0, "{r:?}");
        assert!(r.low_correlation > 0.99, "{r:?}");
        // One side inverted: all side, correlation −1.
        let inverted: Vec<f32> = bass.iter().map(|v| -v).collect();
        let r = measure(&[bass.clone(), inverted], SR);
        assert!(r.low_side_db > 40.0, "{r:?}");
        assert!(r.low_correlation < -0.99, "{r:?}");
        // A quarter period apart: as much side as mid, uncorrelated.
        let shifted = sine(60.0, 0.5, 3.0, std::f64::consts::FRAC_PI_2);
        let r = measure(&[bass, shifted], SR);
        assert!(r.low_side_db.abs() < 1.0, "{r:?}");
        assert!(r.low_correlation.abs() < 0.05, "{r:?}");
        // Highs out of phase do not count.
        let hiss = sine(3_000.0, 0.5, 3.0, 0.0);
        let inverted: Vec<f32> = hiss.iter().map(|v| -v).collect();
        let r = measure(&[hiss, inverted], SR);
        assert!(r.low_correlation > 0.99, "{r:?}");
    }

    #[test]
    fn esses_and_brightness_are_measured() {
        // A 6 kHz burst at −6 dBFS in a second of a quiet 200 Hz tone.
        let mut x = sine(200.0, 0.1, 1.0, 0.0);
        for (i, v) in sine(6_000.0, 0.5, 0.05, 0.0).into_iter().enumerate() {
            x[20_000 + i] += v;
        }
        let r = measure(&[x.clone(), x], SR);
        assert!((r.sibilance_db + 6.0).abs() < 1.0, "{r:?}");
        // A dark tone: no esses, hardly any highs.
        let dark = sine(200.0, 0.5, 1.0, 0.0);
        let d = measure(&[dark.clone(), dark], SR);
        assert!(d.sibilance_db < -60.0 && d.highs_db < -60.0, "{d:?}");
        // Brightness: a 10 kHz tone is all highs.
        let air = sine(10_000.0, 0.5, 1.0, 0.0);
        let a = measure(&[air.clone(), air], SR);
        assert!(a.highs_db > -1.0, "{a:?}");
        // Silence measures as nothing.
        let s = measure(&[vec![0.0; 1000], vec![0.0; 1000]], SR);
        assert_eq!(s.low_correlation, 1.0);
        assert_eq!(s.sibilance_db, f64::NEG_INFINITY);
    }
}
