//! The heads and the equalisation round the tape: what the record
//! amplifier puts on, what the tape and the heads lose, what the
//! reproduce amplifier takes off, and the reproduce head's bump.
//!
//! **Standards.** The recorded short-circuit flux of the standard tape
//! follows `Φ(ω) = (1 + 1/(jωT1)) / (1 + jωT2)` (NAB; IEC/CCIR without
//! `T1`; AES at 30 ips): NAB 3180 + 50 µs at 7.5 and 15 ips, IEC 70 µs at
//! 7.5 and 35 µs at 15, 3180 + 90 µs at 3.75 for both, AES 17.5 µs at 30 ips
//! for both. The record equaliser shapes the signal by `Φ` and the
//! reproduce equaliser by `1/Φ`, so the standards cancel at small levels
//! and differ in where the tape is driven hard: what `Φ` takes off the
//! highs on the way to the tape (and the reproduce side puts back, with
//! the hiss) is headroom.
//!
//! **Losses** (Bertram, *Theory of Magnetic Recording*; Mallinson), for a
//! wavenumber `k = 2πf/v`:
//! * Thickness: only the top of a coating of thickness `δ` reaches the
//!   head at short wavelengths, `(1 − e^{−kδ})/(kδ)` — part of the
//!   recorded flux, so the record equaliser makes up for it.
//! * The recording zone: the bias erases and smears what the zone spans,
//!   `e^{−ka}` with `a` the zone's length ([`super::media::ZONE`]) — the
//!   record equaliser makes up for it at the aligned bias; over- or
//!   under-bias changes it.
//! * Reproduce gap `g` (`sinc(kg/2)`, 3 µm, 0.12 mil — the ATR sheet's
//!   reproduce head) and spacing `d` (`e^{−kd}`, 0.25 µm): the reproduce
//!   equaliser's treble trim, set on the calibration tape, makes up the
//!   gap's loss to three times its corner; the spacing's is left.
//!
//! Each loss is a first-order section at its −3 dB point (a minimum-phase
//! stand-in for the zero-phase physical loss). The record equaliser's two
//! boosts are the inverses of the thickness and zone losses, each held
//! back by a pole at the speed's bandwidth (12.5 kHz at 3.75 ips, scaling
//! with speed: 25 kHz at 7.5, 50 at 15, 100 at 30): the response is −3.2 dB
//! at 15 kHz at 7.5 ips, −1.3 dB at 18 kHz at 15 ips and −0.4 dB at 20 kHz
//! at 30 ips (the Studer A80 is specified 30 Hz–15 kHz and 30 Hz–18 kHz),
//! and the record current climbs far over the operating level at the top of
//! the band, which is where a slow tape saturates first.
//!
//! **Head bump.** A reproduce head of finite pole length picks up long
//! wavelengths unevenly (the contour effect): a rise where the wavelength
//! is about the contact length, a ripple above it and a loss below. It is
//! modelled as a bell at `v/λ_b`, a smaller dip above and a second-order
//! high-pass under it, from the shapes of published reproduce responses
//! (¼″ heads: λ_b ≈ 7 mm, +1.8 dB — 54 Hz at 15 ips, 109 Hz at 30; ½″
//! mastering heads: 10 mm, a gentler +1.0 dB).

use crate::dsp::filter::{Coefs, State};
use std::f64::consts::PI;

/// Tape speeds (ips) in [`super::id::SPEED`] order.
pub const SPEEDS: [f64; 4] = [3.75, 7.5, 15.0, 30.0];

/// m/s for ips.
pub fn metres_per_second(ips: f64) -> f64 {
    ips * 0.0254
}

/// The equalisation standards in [`super::id::EQ`] order.
pub const STANDARDS: [&str; 2] = ["NAB", "IEC"];

/// The time constants (s) `(T1, T2)` of a standard at a speed (`T1 = 0`:
/// none).
pub fn time_constants(speed: usize, iec: bool) -> (f64, f64) {
    match (speed.min(3), iec) {
        (0, _) => (3180e-6, 90e-6),
        (1, false) | (2, false) => (3180e-6, 50e-6),
        (1, true) => (0.0, 70e-6),
        (2, true) => (0.0, 35e-6),
        _ => (0.0, 17.5e-6),
    }
}

/// The name of the equalisation at a speed ("NAB 50 µs", "AES 17.5 µs").
pub fn standard_name(speed: usize, iec: bool) -> String {
    let (t1, t2) = time_constants(speed, iec);
    let us = t2 * 1e6;
    let us = if (us - us.round()).abs() < 1e-6 {
        format!("{us:.0}")
    } else {
        format!("{us:.1}")
    };
    match (speed.min(3), iec, t1 > 0.0) {
        (3, ..) => format!("AES {us} µs"),
        (_, false, _) => format!("NAB {us} µs"),
        (_, true, true) => format!("IEC {us} µs + 3180"),
        (_, true, false) => format!("IEC {us} µs"),
    }
}

/// The NAB low-frequency pre-emphasis is held to +20 dB under 5 Hz.
const LF_FLOOR: f64 = 5.0;

/// Tape formats: track width and the track centres' distance (m), the
/// reproduce head's contact length for the bump (m), its bump (dB) and
/// the channels' crosstalk at 1 kHz (dB).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Format {
    pub name: &'static str,
    pub track: f64,
    pub pitch: f64,
    pub contact: f64,
    pub bump: f64,
    pub crosstalk: f64,
}

/// ¼″ two-track (NAB: 75 mil tracks, the guard band between them) and ½″
/// two-track (0.2″ tracks and guard band).
pub const FORMATS: [Format; 2] = [
    Format {
        name: "¼-inch",
        track: 0.075 * 0.0254,
        pitch: (0.075 + 0.096) * 0.0254,
        contact: 7.0e-3,
        bump: 1.8,
        crosstalk: -45.0,
    },
    Format {
        name: "½-inch",
        track: 0.2 * 0.0254,
        pitch: 0.3 * 0.0254,
        contact: 10.0e-3,
        bump: 1.0,
        crosstalk: -53.0,
    },
];

/// The reproduce gap and the head-to-tape spacing (m).
pub const GAP: f64 = 3.0e-6;
pub const SPACING: f64 = 0.25e-6;

/// The corner (Hz) of the thickness loss of a coating `delta` at `v`:
/// `(1 − e^{−x})/x = 1/√2` at x = 0.8533.
pub fn thickness_corner(v: f64, delta: f64) -> f64 {
    0.8533 * v / (2.0 * PI * delta)
}

/// The corner (Hz) of an exponential loss `e^{−kd}`: `kd = ln 2 / 2`.
pub fn exponential_corner(v: f64, d: f64) -> f64 {
    0.346_574 * v / (2.0 * PI * d.max(1e-12))
}

/// The corner (Hz) of the gap loss `sinc(kg/2)`: `kg/2 = 1.3916`.
pub fn gap_corner(v: f64, g: f64) -> f64 {
    1.3916 * v / (PI * g)
}

/// The record equaliser's bandwidth (Hz) at a speed (ips).
pub fn bandwidth(ips: f64) -> f64 {
    12.5e3 * ips / 3.75
}

/// A first-order section `y = b0 x + b1 x₁ − a1 y₁` (transposed).
#[derive(Clone, Copy, Debug)]
pub struct FirstOrder {
    b0: f64,
    b1: f64,
    a1: f64,
    z: f64,
}

impl Default for FirstOrder {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// The analog corner (rad/s) a bilinear section at `fs` is designed from
/// for a corner at `f` Hz: prewarped (the digital corner exactly at `f`)
/// up to a quarter of the rate; above, where a corner cannot be placed
/// (the record bandwidth, gap and spacing at 2×), scaled by the warp at
/// `fs/6`, so the section's attenuation across the audio band stays
/// within a fifth of a dB of the analog one's whatever the oversampling.
fn warp(f: f64, fs: f64) -> f64 {
    let f = f.max(1e-3);
    if f <= 0.25 * fs {
        2.0 * fs * (PI * f / fs).tan()
    } else {
        let sixth = PI / 6.0;
        2.0 * PI * f * sixth.tan() / sixth
    }
}

impl FirstOrder {
    pub const IDENTITY: FirstOrder = FirstOrder {
        b0: 1.0,
        b1: 0.0,
        a1: 0.0,
        z: 0.0,
    };

    /// `(n1 s + n0)/(d1 s + d0)` by the bilinear transform at `fs`.
    fn analog(n1: f64, n0: f64, d1: f64, d0: f64, fs: f64) -> Self {
        let k = 2.0 * fs;
        let a0 = d1 * k + d0;
        Self {
            b0: (n1 * k + n0) / a0,
            b1: (n0 - n1 * k) / a0,
            a1: (d0 - d1 * k) / a0,
            z: 0.0,
        }
    }

    /// `(1 + s/ωz)/(1 + s/ωp)` (zero and pole at `fz`, `fp` Hz; `None`
    /// for no zero).
    pub fn shelf(fz: Option<f64>, fp: f64, fs: f64) -> Self {
        let wp = warp(fp, fs);
        match fz {
            Some(fz) => Self::analog(1.0 / warp(fz, fs), 1.0, 1.0 / wp, 1.0, fs),
            None => Self::analog(0.0, 1.0, 1.0 / wp, 1.0, fs),
        }
    }

    /// `(s + ωa)/(s + ωb)`: a low shelf of `ωa/ωb` under the corners.
    pub fn low_shelf(fa: f64, fb: f64, fs: f64) -> Self {
        Self::analog(1.0, warp(fa, fs), 1.0, warp(fb, fs), fs)
    }

    /// Keep the state, take new coefficients.
    pub fn retune(&mut self, other: FirstOrder) {
        self.b0 = other.b0;
        self.b1 = other.b1;
        self.a1 = other.a1;
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z;
        self.z = self.b1 * x - self.a1 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    pub fn flush(&mut self) {
        self.z = crate::dsp::flush(self.z);
    }

    /// The magnitude at `f` Hz (rate `fs`).
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c, s) = (w.cos(), -w.sin());
        // (b0 + b1 z⁻¹)/(1 + a1 z⁻¹), z⁻¹ = c + js.
        let (nr, ni) = (self.b0 + self.b1 * c, self.b1 * s);
        let (dr, di) = (1.0 + self.a1 * c, self.a1 * s);
        (nr.hypot(ni) / dr.hypot(di).max(1e-300)).max(0.0)
    }
}

/// The record and reproduce sections at one speed, standard, formulation,
/// format, bias and azimuth, at the oversampled rate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Design {
    /// Record: NAB's low pre-emphasis, `Φ`'s high de-emphasis, the boosts
    /// for the thickness and the zone losses; then a gain that makes the
    /// whole of it 0 dB at 1 kHz.
    pub pre: [FirstOrder; 4],
    pub pre_gain: f64,
    /// Reproduce: `1/Φ`'s highs with the thickness loss, the zone's loss
    /// at the bias, `1/Φ`'s lows, the gap, the spacing and the azimuth
    /// losses; then a gain that makes record and reproduce together 0 dB at
    /// 1 kHz at the aligned bias.
    pub post: [FirstOrder; 6],
    pub post_gain: f64,
}

/// What the sections are designed for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setup {
    pub speed: usize,
    pub iec: bool,
    pub coating: f64,
    pub zone: f64,
    pub format: usize,
    /// Azimuth error (arc minutes).
    pub azimuth: f64,
    pub fs: f64,
}

impl Design {
    pub fn new(s: &Setup) -> Self {
        let ips = SPEEDS[s.speed.min(3)];
        let v = metres_per_second(ips);
        let fs = s.fs;
        let (t1, t2) = time_constants(s.speed, s.iec);
        let f2 = 1.0 / (2.0 * PI * t2);
        let ft = thickness_corner(v, s.coating);
        let fz1 = exponential_corner(v, super::media::ZONE);
        let fz = exponential_corner(v, super::media::ZONE * s.zone.max(1e-3));
        let bw = bandwidth(ips);
        let lf = |inverse: bool| {
            if t1 > 0.0 {
                let f1 = 1.0 / (2.0 * PI * t1);
                if inverse {
                    FirstOrder::low_shelf(LF_FLOOR, f1, fs)
                } else {
                    FirstOrder::low_shelf(f1, LF_FLOOR, fs)
                }
            } else {
                FirstOrder::IDENTITY
            }
        };
        let pre = [
            lf(false),
            FirstOrder::shelf(None, f2, fs),
            FirstOrder::shelf(Some(ft), bw, fs),
            FirstOrder::shelf(Some(fz1), bw, fs),
        ];
        let fmt = FORMATS[s.format.min(FORMATS.len() - 1)];
        let tan = (s.azimuth.abs() / 60.0).to_radians().tan();
        // The azimuth loss sinc(πw tanθ/λ) at −3 dB.
        let az = if tan > 1e-9 {
            FirstOrder::shelf(None, 1.3916 * v / (PI * fmt.track * tan), fs)
        } else {
            FirstOrder::IDENTITY
        };
        let post = [
            FirstOrder::shelf(Some(f2), ft, fs),
            FirstOrder::shelf(None, fz, fs),
            lf(true),
            // The gap's loss, made up to three times its corner.
            FirstOrder::shelf(None, 3.0 * gap_corner(v, GAP), fs),
            FirstOrder::shelf(None, exponential_corner(v, SPACING), fs),
            az,
        ];
        let at = |sections: &[FirstOrder], f: f64| -> f64 {
            sections.iter().map(|x| x.magnitude(f, fs)).product()
        };
        let pre_gain = 1.0 / at(&pre, 1000.0).max(1e-12);
        // Normalised with the zone's loss at the aligned bias: bias changes
        // move the level as on the machine.
        let mut aligned = post;
        aligned[1] = FirstOrder::shelf(None, fz1, fs);
        aligned[5] = FirstOrder::IDENTITY;
        let post_gain = 1.0 / (at(&aligned, 1000.0) * at(&pre, 1000.0) * pre_gain).max(1e-12);
        Self {
            pre,
            pre_gain,
            post,
            post_gain,
        }
    }

    /// Take `other`'s coefficients, keeping the filters' states.
    pub fn retune(&mut self, other: &Design) {
        for (a, b) in self.pre.iter_mut().zip(&other.pre) {
            a.retune(*b);
        }
        for (a, b) in self.post.iter_mut().zip(&other.post) {
            a.retune(*b);
        }
        self.pre_gain = other.pre_gain;
        self.post_gain = other.post_gain;
    }

    #[inline]
    pub fn record(&mut self, x: f64) -> f64 {
        let mut y = x * self.pre_gain;
        for s in &mut self.pre {
            y = s.process(y);
        }
        y
    }

    #[inline]
    pub fn reproduce(&mut self, x: f64) -> f64 {
        let mut y = x * self.post_gain;
        for s in &mut self.post {
            y = s.process(y);
        }
        y
    }

    pub fn reset(&mut self) {
        for s in self.pre.iter_mut().chain(&mut self.post) {
            s.reset();
        }
    }

    pub fn flush(&mut self) {
        for s in self.pre.iter_mut().chain(&mut self.post) {
            s.flush();
        }
    }

    /// The small-signal magnitude of record and reproduce at `f`.
    pub fn response(&self, f: f64, fs: f64) -> f64 {
        self.pre_gain
            * self.post_gain
            * self
                .pre
                .iter()
                .chain(&self.post)
                .map(|s| s.magnitude(f, fs))
                .product::<f64>()
    }

    /// The reproduce side's magnitude at `f` (what the hiss goes through).
    pub fn reproduce_response(&self, f: f64, fs: f64) -> f64 {
        self.post_gain
            * self
                .post
                .iter()
                .map(|s| s.magnitude(f, fs))
                .product::<f64>()
    }

    /// The record side's magnitude at `f` (how hard the tape is driven).
    #[cfg(test)]
    pub fn record_response(&self, f: f64, fs: f64) -> f64 {
        self.pre_gain * self.pre.iter().map(|s| s.magnitude(f, fs)).product::<f64>()
    }
}

/// RBJ biquads (bilinear) for the head bump at the host rate.
fn peaking(f: f64, q: f64, db: f64, fs: f64) -> Coefs {
    let a = 10f64.powf(db / 40.0);
    let w = 2.0 * PI * f.clamp(1.0, 0.45 * fs) / fs;
    let alpha = w.sin() / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    Coefs {
        b0: (1.0 + alpha * a) / a0,
        b1: -2.0 * w.cos() / a0,
        b2: (1.0 - alpha * a) / a0,
        a1: -2.0 * w.cos() / a0,
        a2: (1.0 - alpha / a) / a0,
    }
}

fn high_pass(f: f64, q: f64, fs: f64) -> Coefs {
    let w = 2.0 * PI * f.clamp(1.0, 0.45 * fs) / fs;
    let alpha = w.sin() / (2.0 * q);
    let c = w.cos();
    let a0 = 1.0 + alpha;
    Coefs {
        b0: (1.0 + c) / 2.0 / a0,
        b1: -(1.0 + c) / a0,
        b2: (1.0 + c) / 2.0 / a0,
        a1: -2.0 * c / a0,
        a2: (1.0 - alpha) / a0,
    }
}

/// The bump's centre (Hz) for a format at a speed.
pub fn bump_frequency(speed: usize, format: usize) -> f64 {
    let v = metres_per_second(SPEEDS[speed.min(3)]);
    v / FORMATS[format.min(FORMATS.len() - 1)].contact
}

/// The reproduce head's low end: the bump, the dip above it and the loss
/// under it.
#[derive(Clone, Copy, Debug)]
pub struct Bump {
    coefs: [Coefs; 3],
    state: [State; 3],
    /// Makes it 0 dB at 1 kHz.
    gain: f64,
}

impl Bump {
    pub fn new(speed: usize, format: usize, fs: f64) -> Self {
        let mut b = Self {
            coefs: [Coefs::IDENTITY; 3],
            state: [State::default(); 3],
            gain: 1.0,
        };
        b.set(speed, format, fs);
        b
    }

    pub fn set(&mut self, speed: usize, format: usize, fs: f64) {
        let fb = bump_frequency(speed, format);
        let fmt = FORMATS[format.min(FORMATS.len() - 1)];
        self.coefs = [
            peaking(fb, 1.1, fmt.bump, fs),
            peaking(2.1 * fb, 1.6, -0.4 * fmt.bump, fs),
            high_pass(0.3 * fb, 0.75, fs),
        ];
        self.gain = 1.0;
        self.gain = 1.0 / self.magnitude(1000.0, fs).max(1e-9);
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let mut y = x * self.gain;
        for (s, c) in self.state.iter_mut().zip(&self.coefs) {
            y = s.run(c, y);
        }
        y
    }

    pub fn reset(&mut self) {
        self.state = [State::default(); 3];
    }

    pub fn flush(&mut self) {
        for s in &mut self.state {
            s.flush();
        }
    }

    /// The magnitude at `f` Hz.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        self.gain
            * self
                .coefs
                .iter()
                .map(|c| {
                    let (c1, s1) = (w.cos(), -w.sin());
                    let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
                    let nr = c.b0 + c.b1 * c1 + c.b2 * c2;
                    let ni = c.b1 * s1 + c.b2 * s2;
                    let dr = 1.0 + c.a1 * c1 + c.a2 * c2;
                    let di = c.a1 * s1 + c.a2 * s2;
                    nr.hypot(ni) / dr.hypot(di).max(1e-300)
                })
                .product::<f64>()
    }
}

/// The A-weighting (IEC 61672) magnitude at `f`.
pub fn a_weighting(f: f64) -> f64 {
    let f2 = f * f;
    let num = 12194.0f64.powi(2) * f2 * f2;
    let den = (f2 + 20.6f64.powi(2))
        * ((f2 + 107.7f64.powi(2)) * (f2 + 737.9f64.powi(2))).sqrt()
        * (f2 + 12194.0f64.powi(2));
    num / den * 10f64.powf(2.0 / 20.0)
}
