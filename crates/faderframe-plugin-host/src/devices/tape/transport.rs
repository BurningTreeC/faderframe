//! The transport: how the tape's speed wanders past the heads.
//!
//! The speed deviation is built from what a measured flutter spectrum of
//! a capstan machine shows: lines at the rotation rates of what touches the
//! tape — the capstan (and its second harmonic), the pinch roller, a guide
//! roller, the two reels (whose rate changes with the pack: taken at mid
//! pack) — over a floor of random flutter from tension and friction
//! (1–30 Hz), and scrape flutter: the tape's stick–slip on the heads
//! exciting its longitudinal resonances between the guides, a band round
//! 2–5 kHz that is too fast to hear as pitch and is heard instead as
//! modulation noise beside every tone (Bertram; the AES6 / IEC 386
//! definitions of wow, flutter and scrape flutter).
//!
//! Rates scale with the tape speed (the parts turn faster), the depth is
//! set so the weighted peak (the IEC 386 / DIN 45507 weighting, the "2σ"
//! value exceeded 5 % of the time) is that of a good studio machine —
//! 0.10 % at 3.75 ips, 0.06 % at 7.5, 0.04 % at 15 and 0.03 % at 30 (the
//! Studer A80's 0.06 % / 0.04 % at 7.5 / 15 ips, the A810's 0.04 % at 30) —
//! times the Wow & Flutter control.
//!
//! The transport hands back a displacement (host samples, zero-mean):
//! the oversampler reads the tape that much later (see
//! [`super::os::TapeOversampler`]). Its room is a third of a millisecond
//! at 48 kHz: it holds a good machine's wow in full from 7.5 ips up; where
//! the slowest lines (the reels, under 1 Hz — drift by AES6's definition,
//! and weighted down 15 dB and more) would not fit, at 3.75 ips or turned
//! up, they are held to it, and only past that the faster ones.

use crate::dsp::Noise;
use std::f64::consts::{PI, TAU};

/// The weighted peak (fraction) of a good machine at each speed.
pub const SPEC: [f64; 4] = [0.0010, 0.0006, 0.0004, 0.0003];

/// The parts' diameters (m) and their share of the flutter (unweighted
/// peak speed deviation, relative). The reels by their pack radius.
const CAPSTAN: f64 = 0.020;
const PINCH: f64 = 0.040;
const GUIDE: f64 = 0.030;
const TAKE_UP: f64 = 2.0 * 0.085;
const SUPPLY: f64 = 2.0 * 0.095;
/// (diameter, harmonic, share).
const LINES: [(f64, f64, f64); 6] = [
    (CAPSTAN, 1.0, 1.0),
    (CAPSTAN, 2.0, 0.3),
    (PINCH, 1.0, 0.4),
    (GUIDE, 1.0, 0.3),
    (TAKE_UP, 1.0, 0.8),
    (SUPPLY, 1.0, 0.6),
];
/// The random flutter's RMS share.
const RANDOM: f64 = 0.35;
/// Scrape flutter's RMS speed deviation per unit of the weighted peak.
const SCRAPE: f64 = 1.2;
/// Its band (Hz).
const SCRAPE_HZ: f64 = 3200.0;

/// The weighting's magnitude at a modulation frequency `f` (Hz): a
/// second-order high-pass at 1.5 Hz and a first-order low-pass at 10 Hz,
/// 0 dB at 4 Hz — within a dB or two of the IEC 386 curve from 0.5 to 50 Hz
/// (−19 dB at 0.5 Hz, −26 dB at 200 Hz).
pub fn weighting(f: f64) -> f64 {
    let raw = |f: f64| {
        let x = f / 1.5;
        let hp = x * x / ((1.0 - x * x).powi(2) + (x / 0.707).powi(2)).sqrt();
        let lp = 1.0 / (1.0 + (f / 10.0).powi(2)).sqrt();
        hp * lp
    };
    raw(f) / raw(4.0)
}

/// The rate (Hz) of a part of `diameter` turning under the tape at `v`
/// m/s, times `harmonic`.
fn rate(v: f64, diameter: f64, harmonic: f64) -> f64 {
    harmonic * v / (PI * diameter)
}

/// The unweighted depth per unit of the weighted peak: the lines and the
/// random flutter weighted, their RMS taken, and a crest factor of 1.9 (the
/// 2σ value of the sum).
fn scale(v: f64) -> f64 {
    let mut ms = 0.0;
    for (d, h, share) in LINES {
        let w = weighting(rate(v, d, h)) * share;
        ms += w * w / 2.0;
    }
    // The random flutter: flat 1–30 Hz, its weighted power share ~0.55.
    ms += 0.55 * RANDOM * RANDOM;
    1.0 / (1.9 * ms.sqrt())
}

/// A slowly wandering sinusoidal line.
#[derive(Clone, Copy, Debug, Default)]
struct Line {
    phase: f64,
    freq: f64,
    /// Peak speed deviation (fraction).
    amp: f64,
    /// A slow wander of its phase (radians) and depth.
    wander: f64,
    depth: f64,
}

/// The lines (all under 30 Hz) are worked out every this many samples
/// and joined linearly in between.
const CONTROL: usize = 32;

/// The transport of one machine (shared by its channels).
#[derive(Clone, Debug)]
pub struct Transport {
    sr: f64,
    lines: [Line; 6],
    /// The lines' speed and displacement (s) at the last and the next
    /// control point, and the samples left to the next.
    from: (f64, f64),
    to: (f64, f64),
    left: usize,
    noise: Noise,
    /// The random flutter: white speed noise through two poles at 30 Hz
    /// and a zero-mean (leaky) integrator.
    lp: [f64; 2],
    lp_k: f64,
    hp_k: f64,
    random_amp: f64,
    random_d: f64,
    /// Scrape flutter: band-passed speed noise, integrated over its band.
    bp: [f64; 2],
    bp_c: [f64; 3],
    scrape_amp: f64,
    /// The last speed deviation (fraction).
    pub speed: f64,
    displacement: f64,
    /// The last parameters: speed index and depth.
    set_for: (usize, f64),
    /// The most the displacement may swing (host samples).
    room: f64,
}

impl Transport {
    /// A transport at `sr` whose displacement stays within `room` host
    /// samples.
    pub fn new(sr: f64, room: f64) -> Self {
        let mut t = Self {
            sr,
            lines: [Line::default(); 6],
            from: (0.0, 0.0),
            to: (0.0, 0.0),
            left: 0,
            noise: Noise::new(0x7a9e_5eed),
            lp: [0.0; 2],
            lp_k: 1.0 - (-TAU * 30.0 / sr).exp(),
            hp_k: (-TAU * 0.4 / sr).exp(),
            random_amp: 0.0,
            random_d: 0.0,
            bp: [0.0; 2],
            bp_c: [0.0; 3],
            scrape_amp: 0.0,
            speed: 0.0,
            displacement: 0.0,
            set_for: (usize::MAX, -1.0),
            room: room.max(0.0),
        };
        // Start the lines at different places.
        for (i, l) in t.lines.iter_mut().enumerate() {
            l.phase = 1.3 + 2.1 * i as f64;
        }
        t.set(2, 1.0);
        t
    }

    /// Speed `speed` (index into [`super::heads::SPEEDS`]) and the Wow &
    /// Flutter control `depth` (1 = the machine's specification).
    pub fn set(&mut self, speed: usize, depth: f64) {
        let depth = depth.max(0.0);
        if self.set_for == (speed, depth) {
            return;
        }
        self.set_for = (speed, depth);
        let speed = speed.min(3);
        let v = super::heads::metres_per_second(super::heads::SPEEDS[speed]);
        let peak = SPEC[speed] * depth * scale(v);
        for (l, (d, h, share)) in self.lines.iter_mut().zip(LINES) {
            l.freq = rate(v, d, h);
            l.amp = peak * share;
        }
        self.random_amp = peak * RANDOM;
        // Fit the swing into the room: the random flutter's (its RMS
        // displacement through the 0.4 Hz leak, three times) and the
        // lines', the slow ones held first.
        // (The lines wander in depth by a few per cent: some margin.)
        let room = self.room / self.sr / 1.15;
        let random = 3.0 * self.random_amp * 0.0457;
        let swing = |l: &Line| l.amp / (TAU * l.freq.max(1e-3));
        let (mut slow, mut fast) = (0.0, 0.0);
        for l in &self.lines {
            if l.freq < 1.0 {
                slow += swing(l);
            } else {
                fast += swing(l);
            }
        }
        let free = room - random - fast;
        if slow + fast + random > room {
            if free > 0.0 {
                let k = (free / slow.max(1e-12)).min(1.0);
                for l in self.lines.iter_mut().filter(|l| l.freq < 1.0) {
                    l.amp *= k;
                }
            } else {
                let k = ((room - random).max(0.0) / (fast + slow).max(1e-12)).min(1.0);
                for l in &mut self.lines {
                    l.amp *= k;
                }
            }
        }
        // Scrape flutter by the machine's specification (it is the heads'
        // and the tape's, not the speed's).
        self.scrape_amp = SPEC[2] * depth * SCRAPE;
        // A band-pass (Q 1) round the scrape band.
        let w = TAU * SCRAPE_HZ.min(0.4 * self.sr) / self.sr;
        let alpha = w.sin() / 2.0;
        let a0 = 1.0 + alpha;
        self.bp_c = [alpha / a0, -2.0 * w.cos() / a0, (1.0 - alpha) / a0];
    }

    /// The lines a control step `step` (s) on: their speed and
    /// displacement (s).
    fn lines_on(&mut self, step: f64) -> (f64, f64) {
        let (mut speed, mut d) = (0.0, 0.0);
        for l in &mut self.lines {
            l.phase += TAU * l.freq * step;
            if l.phase > TAU {
                l.phase -= TAU;
            }
            // Eccentricities are not perfect: phase and depth wander a
            // little (about 0.014 rad and 2.5 %), slowly.
            l.wander += self.noise.tick() * 2e-4 - l.wander * 0.05 * step;
            l.depth += self.noise.tick() * 5e-4 - l.depth * 0.1 * step;
            let a = l.amp * (1.0 + l.depth.clamp(-0.5, 0.5));
            let (sin, cos) = (l.phase + l.wander).sin_cos();
            speed += a * sin;
            // The displacement is the speed's integral.
            if l.freq > 0.0 {
                d -= a * cos / (TAU * l.freq);
            }
        }
        (speed, d)
    }

    /// The next displacement (host samples; positive: the tape is late).
    #[inline]
    pub fn tick(&mut self) -> f64 {
        let dt = 1.0 / self.sr;
        if self.left == 0 {
            self.from = self.to;
            self.to = self.lines_on(CONTROL as f64 * dt);
            self.left = CONTROL;
        }
        let t = 1.0 - self.left as f64 / CONTROL as f64;
        self.left -= 1;
        let mut speed = self.from.0 + (self.to.0 - self.from.0) * t;
        let mut d = self.from.1 + (self.to.1 - self.from.1) * t;
        // Random flutter: white speed noise, two poles at 30 Hz.
        let n = self.noise.tick() * 3f64.sqrt();
        self.lp[0] += self.lp_k * (n - self.lp[0]);
        self.lp[1] += self.lp_k * (self.lp[0] - self.lp[1]);
        // Two one-poles at 30 Hz pass 1/√(2·…) of white noise's RMS:
        // normalised so the band's RMS is `random_amp`.
        let r = self.lp[1] * self.random_amp * (2.0 * self.sr / (TAU * 30.0) * 2.0).sqrt();
        speed += r;
        self.random_d = (self.random_d + r * dt) * self.hp_k;
        d += self.random_d;
        // Scrape flutter.
        let x = self.noise.tick() * 3f64.sqrt();
        let [b0, a1, a2] = self.bp_c;
        let y = b0 * x + self.bp[0];
        self.bp[0] = -a1 * y + self.bp[1];
        self.bp[1] = -b0 * x - a2 * y;
        // Unit-variance white through a Q-1 band-pass of bandwidth B has an
        // RMS of √(πB/sr)·…: normalised by the band's share.
        let bw = SCRAPE_HZ.min(0.4 * self.sr);
        let s = y * self.scrape_amp * (self.sr / (PI * bw)).sqrt();
        speed += s;
        d -= s / (TAU * SCRAPE_HZ);
        self.speed = speed;
        self.displacement = d * self.sr;
        self.displacement
    }

    pub fn reset(&mut self) {
        self.lp = [0.0; 2];
        self.bp = [0.0; 2];
        self.random_d = 0.0;
    }
}
