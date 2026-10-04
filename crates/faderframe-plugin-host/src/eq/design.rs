//! Filter design for the EQ: analog prototypes turned into digital biquads
//! whose magnitude follows the analog response all the way to Nyquist.
//!
//! The usual bilinear transform squeezes the whole analog frequency axis
//! into the digital one, so a bell at 12 kHz on a 44.1 kHz session comes out
//! narrower and a high shelf stops short of its gain at the top
//! ("cramping"). Above a few hundred hertz each section is designed instead
//! after Martin Vicanek, *Matched Second Order Digital Filters* (2016): the
//! poles by impulse invariance, the zeros so that the magnitude matches the
//! analog filter's where it is defined — a cut's level at its corner, a
//! band pass's and a notch's centre — and, for bells and shelves, exact at
//! DC and at the centre or both ends, with the remaining freedom fitted to
//! the analog magnitude up to Nyquist by least squares. First order
//! sections place their pole so DC, the corner and Nyquist are all exact.
//! Low down, where the bilinear transform is exact to a hair and the
//! matched formulas lose precision, sections are bilinear with
//! prewarping. The editor draws its curves from these same
//! coefficients, so what is shown is what is heard.

use std::f64::consts::PI;

/// Below this digital frequency (radians per sample) sections are
/// bilinear; above it, matched.
const MATCHED_FROM: f64 = 0.001;
/// The highest centre frequency, as a fraction of the sample rate.
const MAX_FRACTION: f64 = 0.499;

/// The shapes a band can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandType {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    /// Tilts the spectrum round the frequency: half the gain above, the
    /// opposite below.
    TiltShelf,
}

impl BandType {
    pub const ALL: [BandType; 8] = [
        BandType::Bell,
        BandType::LowShelf,
        BandType::HighShelf,
        BandType::LowCut,
        BandType::HighCut,
        BandType::Notch,
        BandType::BandPass,
        BandType::TiltShelf,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(BandType::Bell)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            BandType::Bell => "Bell",
            BandType::LowShelf => "Low Shelf",
            BandType::HighShelf => "High Shelf",
            BandType::LowCut => "Low Cut",
            BandType::HighCut => "High Cut",
            BandType::Notch => "Notch",
            BandType::BandPass => "Band Pass",
            BandType::TiltShelf => "Tilt Shelf",
        }
    }

    /// Whether the gain control means anything.
    pub fn has_gain(self) -> bool {
        matches!(
            self,
            BandType::Bell | BandType::LowShelf | BandType::HighShelf | BandType::TiltShelf
        )
    }

    /// Whether the slope control means anything.
    pub fn has_slope(self) -> bool {
        matches!(
            self,
            BandType::LowShelf
                | BandType::HighShelf
                | BandType::LowCut
                | BandType::HighCut
                | BandType::TiltShelf
        )
    }

    pub fn is_cut(self) -> bool {
        matches!(self, BandType::LowCut | BandType::HighCut)
    }
}

/// Selectable slopes, in dB per octave.
pub const SLOPES: [u32; 8] = [6, 12, 18, 24, 36, 48, 72, 96];
/// Biquad sections a band may need (a 96 dB/oct cut).
pub const MAX_SECTIONS: usize = 8;

/// One second order section, `a0 = 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Squared magnitude at `w` radians per sample.
    pub fn magnitude2(&self, w: f64) -> f64 {
        let p = Phi::at(w);
        let b = p.form(
            (self.b0 + self.b1 + self.b2).powi(2),
            (self.b0 - self.b1 + self.b2).powi(2),
            -4.0 * self.b0 * self.b2,
        );
        let a = p.form(
            (1.0 + self.a1 + self.a2).powi(2),
            (1.0 - self.a1 + self.a2).powi(2),
            -4.0 * self.a2,
        );
        if a > 0.0 { (b / a).max(0.0) } else { 0.0 }
    }

    fn scaled(mut self, k: f64) -> Self {
        self.b0 *= k;
        self.b1 *= k;
        self.b2 *= k;
        self
    }
}

/// The three basis functions of a squared biquad magnitude:
/// `|H|^2 = X0 φ0 + X1 φ1 + X2 φ2` with `φ1 = sin²(w/2)`, `φ0 = 1 − φ1`,
/// `φ2 = 4 φ0 φ1`.
#[derive(Clone, Copy)]
struct Phi {
    p0: f64,
    p1: f64,
    p2: f64,
}

impl Phi {
    fn at(w: f64) -> Self {
        let p1 = (w / 2.0).sin().powi(2);
        let p0 = 1.0 - p1;
        Self {
            p0,
            p1,
            p2: 4.0 * p0 * p1,
        }
    }

    fn form(&self, x0: f64, x1: f64, x2: f64) -> f64 {
        x0 * self.p0 + x1 * self.p1 + x2 * self.p2
    }
}

/// Matched poles: impulse invariance of a pole pair with natural frequency
/// `w` (radians per sample) and damping `zeta`.
fn matched_poles(w: f64, zeta: f64) -> (f64, f64) {
    let decay = (-zeta * w).exp();
    let a1 = if zeta <= 1.0 {
        -2.0 * decay * (w * (1.0 - zeta * zeta).sqrt()).cos()
    } else {
        -2.0 * decay * (w * (zeta * zeta - 1.0).sqrt()).cosh()
    };
    (a1, decay * decay)
}

/// `(A0, A1, A2)` of a denominator.
fn a_terms(a1: f64, a2: f64) -> (f64, f64, f64) {
    ((1.0 + a1 + a2).powi(2), (1.0 - a1 + a2).powi(2), -4.0 * a2)
}

/// A numerator from its `(B0, B1, B2)`.
fn numerator(b0_: f64, b1_: f64, b2_: f64) -> (f64, f64, f64) {
    let (s0, s1) = (b0_.max(0.0).sqrt(), b1_.max(0.0).sqrt());
    let w = 0.5 * (s0 + s1);
    let b0 = 0.5 * (w + (w * w + b2_).max(0.0).sqrt());
    let b1 = 0.5 * (s0 - s1);
    let b2 = if b0.abs() > 1e-300 {
        -b2_ / (4.0 * b0)
    } else {
        0.0
    };
    (b0, b1, b2)
}

/// An analog second order prototype normalised to a corner at 1 rad/s:
/// `(n2 s² + n1 s + n0) / (d2 s² + d1 s + d0)`.
#[derive(Clone, Copy, Debug)]
struct Analog {
    n: [f64; 3],
    d: [f64; 3],
}

impl Analog {
    fn magnitude2(&self, omega: f64) -> f64 {
        let o2 = omega * omega;
        let num = (self.n[0] - self.n[2] * o2).powi(2) + (self.n[1] * omega).powi(2);
        let den = (self.d[0] - self.d[2] * o2).powi(2) + (self.d[1] * omega).powi(2);
        if den > 0.0 { num / den } else { 0.0 }
    }

    /// Bilinear transform with the corner prewarped to `w0`.
    fn bilinear(&self, w0: f64) -> Coefs {
        let k = (w0 / 2.0).tan();
        let k2 = k * k;
        let f = |c: &[f64; 3]| {
            (
                c[2] + c[1] * k + c[0] * k2,
                2.0 * (c[0] * k2 - c[2]),
                c[2] - c[1] * k + c[0] * k2,
            )
        };
        let (b0, b1, b2) = f(&self.n);
        let (a0, a1, a2) = f(&self.d);
        Coefs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Natural frequency (relative to the corner) and damping of the poles.
    fn poles(&self) -> (f64, f64) {
        let (d0, d1, d2) = (self.d[0], self.d[1], self.d[2]);
        let wn = (d0 / d2).sqrt();
        (wn, d1 / (2.0 * (d0 * d2).sqrt()))
    }

    /// Matched design for shelves: exact at DC, the rest of the numerator
    /// fitted to the analog magnitude up to Nyquist.
    fn matched_shelf(&self, w0: f64) -> Coefs {
        let (wn, zeta) = self.poles();
        let (a1, a2) = matched_poles(w0 * wn, zeta);
        let aa = a_terms(a1, a2);
        let b0_ = self.magnitude2(0.0) * aa.0;
        let (b1_, b2_) = fit_b1_b2(w0 / 16.0, b0_, |w| {
            self.magnitude2(w / w0) * Phi::at(w).form(aa.0, aa.1, aa.2)
        });
        let (b0, b1, b2) = numerator(b0_, b1_, b2_);
        Coefs { b0, b1, b2, a1, a2 }
    }
}

/// Least squares fit of the free `B2` of a numerator whose squared
/// magnitude at a probe is `c + B2 d` (`basis` gives `(c, d)`), to the
/// targets `target(w)`, in relative terms, over probes from `lo` to
/// Nyquist.
fn fit_b2(lo: f64, basis: impl Fn(&Phi) -> (f64, f64), target: impl Fn(f64) -> f64) -> f64 {
    const PROBES: usize = 32;
    let lo = lo.clamp(1e-4, PI * 0.5);
    let (mut num, mut den) = (0.0, 0.0);
    for k in 0..PROBES {
        let w = lo * (PI / lo).powf(k as f64 / (PROBES - 1) as f64);
        let p = Phi::at(w);
        let (c, d) = basis(&p);
        let r = target(w);
        let wt = 1.0 / r.max(1e-300);
        num += wt * wt * d * (r - c);
        den += wt * wt * d * d;
    }
    if den > 0.0 { num / den } else { 0.0 }
}

/// Least squares fit of `B1` and `B2` with `B0` given, as [`fit_b2`].
fn fit_b1_b2(lo: f64, b0_: f64, target: impl Fn(f64) -> f64) -> (f64, f64) {
    const PROBES: usize = 32;
    let lo = lo.clamp(1e-4, PI * 0.5);
    // Normal equations of min Σ w² (B0 φ0 + B1 φ1 + B2 φ2 − r)².
    let (mut s11, mut s12, mut s22, mut t1, mut t2) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for k in 0..PROBES {
        let w = lo * (PI / lo).powf(k as f64 / (PROBES - 1) as f64);
        let p = Phi::at(w);
        let r = target(w);
        let wt = (1.0 / r.max(1e-300)).powi(2);
        let e = r - b0_ * p.p0;
        s11 += wt * p.p1 * p.p1;
        s12 += wt * p.p1 * p.p2;
        s22 += wt * p.p2 * p.p2;
        t1 += wt * p.p1 * e;
        t2 += wt * p.p2 * e;
    }
    let det = s11 * s22 - s12 * s12;
    if det.abs() < 1e-300 {
        return (0.0, 0.0);
    }
    ((t1 * s22 - t2 * s12) / det, (s11 * t2 - s12 * t1) / det)
}

/// Peaking (bell) section: gain `g` (linear), quality `q`. Exact at DC and
/// at the centre; in between and up to Nyquist the analog magnitude is
/// fitted.
fn bell(w0: f64, g: f64, q: f64) -> Coefs {
    if (g - 1.0).abs() < 1e-12 {
        return Coefs::IDENTITY;
    }
    let a = g.sqrt();
    let proto = Analog {
        n: [1.0, a / q, 1.0],
        d: [1.0, 1.0 / (a * q), 1.0],
    };
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * a * q));
    let aa = a_terms(a1, a2);
    let c = Phi::at(w0);
    let b0_ = aa.0;
    let rc = g * g * c.form(aa.0, aa.1, aa.2);
    // The centre fixes B1 for any B2.
    let b2_ = fit_b2(
        w0 / 16.0,
        |p| {
            (
                b0_ * p.p0 + p.p1 * (rc - b0_ * c.p0) / c.p1,
                p.p2 - p.p1 * c.p2 / c.p1,
            )
        },
        |w| proto.magnitude2(w / w0) * Phi::at(w).form(aa.0, aa.1, aa.2),
    );
    let b1_ = (rc - b0_ * c.p0 - b2_ * c.p2) / c.p1;
    let (b0, b1, b2) = numerator(b0_, b1_, b2_);
    Coefs { b0, b1, b2, a1, a2 }
}

/// Second order low pass with resonance `q`: exact at DC and at the
/// corner, fitted above.
fn lowpass2(w0: f64, q: f64) -> Coefs {
    let proto = Analog {
        n: [1.0, 0.0, 0.0],
        d: [1.0, 1.0 / q, 1.0],
    };
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * q));
    pinned_fit(&proto, w0, a1, a2)
}

/// A numerator for poles `a1, a2`, exact at DC and at the corner and
/// fitted to the analog magnitude elsewhere.
fn pinned_fit(proto: &Analog, w0: f64, a1: f64, a2: f64) -> Coefs {
    let aa = a_terms(a1, a2);
    let c = Phi::at(w0);
    let b0_ = proto.magnitude2(0.0) * aa.0;
    let rc = proto.magnitude2(1.0) * c.form(aa.0, aa.1, aa.2);
    let b2_ = fit_b2(
        w0 / 16.0,
        |p| {
            (
                b0_ * p.p0 + p.p1 * (rc - b0_ * c.p0) / c.p1,
                p.p2 - p.p1 * c.p2 / c.p1,
            )
        },
        |w| proto.magnitude2(w / w0) * Phi::at(w).form(aa.0, aa.1, aa.2),
    );
    let b1_ = (rc - b0_ * c.p0 - b2_ * c.p2) / c.p1;
    let (b0, b1, b2) = numerator(b0_, b1_, b2_);
    Coefs { b0, b1, b2, a1, a2 }
}

/// Second order high pass with resonance `q`.
fn highpass2(w0: f64, q: f64) -> Coefs {
    let proto = Analog {
        n: [0.0, 0.0, 1.0],
        d: [1.0, 1.0 / q, 1.0],
    };
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * q));
    let (aa0, aa1, aa2) = a_terms(a1, a2);
    let p = Phi::at(w0);
    let b0 = p.form(aa0, aa1, aa2).sqrt() * q / (4.0 * p.p1);
    Coefs {
        b0,
        b1: -2.0 * b0,
        b2: b0,
        a1,
        a2,
    }
}

/// Band pass, unity gain at its centre: a zero at DC, exact at the
/// centre, fitted elsewhere.
fn bandpass(w0: f64, q: f64) -> Coefs {
    let proto = Analog {
        n: [0.0, 1.0 / q, 0.0],
        d: [1.0, 1.0 / q, 1.0],
    };
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * q));
    pinned_fit(&proto, w0, a1, a2)
}

/// Notch: an exact zero at the centre and unity at DC; the poles' damping
/// is solved so that the upper −3 dB edge (or, past it, Nyquist) sits where
/// the analog notch has it, which keeps its width.
fn notch(w0: f64, q: f64) -> Coefs {
    let proto = Analog {
        n: [1.0, 0.0, 1.0],
        d: [1.0, 1.0 / q, 1.0],
    };
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let c = w0.cos();
    let make = |zeta: f64| {
        let (a1, a2) = matched_poles(w0, zeta);
        let k = (1.0 + a1 + a2) / (2.0 - 2.0 * c);
        Coefs {
            b0: k,
            b1: -2.0 * c * k,
            b2: k,
            a1,
            a2,
        }
    };
    let half = 1.0 / (2.0 * q);
    let edge = (w0 * ((1.0 + half * half).sqrt() + half)).min(PI);
    let want = proto.magnitude2(edge / w0);
    let miss = |zeta: f64| make(zeta).magnitude2(edge) - want;
    // Bisect in log damping for the level at the edge.
    let zeta0 = 1.0 / (2.0 * q);
    let (mut lo, mut hi) = ((zeta0 / 16.0).ln(), (zeta0 * 16.0).ln());
    let low_miss = miss(lo.exp());
    if low_miss * miss(hi.exp()) > 0.0 {
        return make(zeta0);
    }
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if miss(mid.exp()) * low_miss <= 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    make((0.5 * (lo + hi)).exp())
}

/// Second order shelves after the RBJ prototypes: gain `g` (linear) below
/// (`low`) or above the corner, `sqrt(g)` at it.
fn shelf2(w0: f64, g: f64, q: f64, low: bool) -> Coefs {
    if (g - 1.0).abs() < 1e-12 {
        return Coefs::IDENTITY;
    }
    let a = g.sqrt();
    let sa = a.sqrt();
    let proto = if low {
        Analog {
            n: [a * a, a * sa / q, a],
            d: [1.0, sa / q, a],
        }
    } else {
        Analog {
            n: [a, a * sa / q, a * a],
            d: [a, sa / q, 1.0],
        }
    };
    if w0 < MATCHED_FROM {
        proto.bilinear(w0)
    } else {
        proto.matched_shelf(w0)
    }
}

/// First order sections: `(n1 s + n0) / (s + p)` normalised to the corner.
/// Matched: the pole by impulse invariance, and a second order numerator
/// exact at DC and at the corner and fitted to the analog magnitude up to
/// Nyquist (a single zero cannot follow it there).
fn first_order(w0: f64, n1: f64, n0: f64, p: f64) -> Coefs {
    if w0 < MATCHED_FROM {
        let k = (w0 / 2.0).tan();
        let (b0, b1) = (n1 + n0 * k, n0 * k - n1);
        let (a0, a1) = (1.0 + p * k, p * k - 1.0);
        return Coefs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: 0.0,
            a1: a1 / a0,
            a2: 0.0,
        };
    }
    // As a second order prototype with a pole pair (s + p)(s + c) and the
    // matching zero, so the shared fit applies; `c` far above Nyquist.
    let c = 1e6;
    let proto = Analog {
        n: [n0 * c, n0 + n1 * c, n1],
        d: [p * c, p + c, 1.0],
    };
    pinned_fit(&proto, w0, -(-p * w0).exp(), 0.0)
}

/// Butterworth quality factors of the second order sections of an order
/// `n` filter, and whether it has a first order section too.
fn butterworth(n: usize) -> ([f64; MAX_SECTIONS], usize, bool) {
    let mut qs = [0.0; MAX_SECTIONS];
    let pairs = n / 2;
    let odd = n % 2 == 1;
    for (k, q) in qs.iter_mut().enumerate().take(pairs) {
        let theta = if odd {
            (k + 1) as f64 * PI / n as f64
        } else {
            (2 * k + 1) as f64 * PI / (2 * n) as f64
        };
        *q = 1.0 / (2.0 * theta.cos());
    }
    (qs, pairs, odd)
}

/// What a band asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandShape {
    pub kind: BandType,
    pub freq: f64,
    /// dB (bells, shelves, tilt).
    pub gain: f64,
    pub q: f64,
    /// dB per octave (cuts, shelves).
    pub slope: u32,
}

/// The sections of a band at `sample_rate`; returns how many are used.
pub fn design(shape: &BandShape, sample_rate: f64, out: &mut [Coefs; MAX_SECTIONS]) -> usize {
    let freq = shape.freq.clamp(1.0, MAX_FRACTION * sample_rate);
    let w0 = 2.0 * PI * freq / sample_rate;
    let q = shape.q.clamp(0.01, 100.0);
    let g = 10f64.powf(shape.gain / 20.0);
    let order2 = shape.slope >= 12;
    match shape.kind {
        BandType::Bell => {
            out[0] = bell(w0, g, q);
            1
        }
        BandType::Notch => {
            out[0] = notch(w0, q);
            1
        }
        BandType::BandPass => {
            out[0] = bandpass(w0, q);
            1
        }
        BandType::LowShelf | BandType::HighShelf | BandType::TiltShelf => {
            let low = shape.kind != BandType::HighShelf;
            // A tilt is a low shelf by minus the gain, lifted by half of it.
            let (g, lift) = if shape.kind == BandType::TiltShelf {
                (1.0 / g, g.sqrt())
            } else {
                (g, 1.0)
            };
            out[0] = if order2 {
                shelf2(w0, g, q, low)
            } else if low {
                // (s + √g) / (s + 1/√g): g below, 1 above, √g at the corner.
                let r = g.sqrt();
                first_order(w0, 1.0, g / r, 1.0 / r)
            } else {
                // (√g s + 1) / (s/√g + 1) = g (s + 1/√g) / (s + √g).
                let r = g.sqrt();
                first_order(w0, g, g / r, r)
            };
            out[0] = out[0].scaled(lift);
            1
        }
        BandType::LowCut | BandType::HighCut => {
            let order = (shape.slope / 6).clamp(1, 16) as usize;
            let (qs, pairs, odd) = butterworth(order);
            let low = shape.kind == BandType::LowCut;
            let mut n = 0;
            if odd {
                out[n] = if low {
                    first_order(w0, 1.0, 0.0, 1.0)
                } else {
                    first_order(w0, 0.0, 1.0, 1.0)
                };
                n += 1;
            }
            // The resonance control lifts the sharpest section.
            let resonance = q / std::f64::consts::FRAC_1_SQRT_2;
            for (k, &qk) in qs.iter().enumerate().take(pairs) {
                let qk = if k + 1 == pairs { qk * resonance } else { qk };
                out[n] = if low {
                    highpass2(w0, qk)
                } else {
                    lowpass2(w0, qk)
                };
                n += 1;
            }
            n
        }
    }
}

/// A band's magnitude in dB at `freq`.
pub fn band_db(shape: &BandShape, sample_rate: f64, freq: f64) -> f64 {
    let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
    let n = design(shape, sample_rate, &mut s);
    sections_db(&s[..n], sample_rate, freq)
}

/// The magnitude of a cascade in dB at `freq`.
pub fn sections_db(sections: &[Coefs], sample_rate: f64, freq: f64) -> f64 {
    let w = 2.0 * PI * freq.clamp(0.0, 0.5 * sample_rate) / sample_rate;
    let m2: f64 = sections.iter().map(|c| c.magnitude2(w)).product();
    10.0 * m2.max(1e-30).log10()
}

/// The analog response of a band in dB at `freq` (what the digital one is
/// matched to; tests compare the two).
pub fn analog_db(shape: &BandShape, freq: f64) -> f64 {
    let omega = freq / shape.freq;
    let g = 10f64.powf(shape.gain / 20.0);
    let q = shape.q;
    let order2 = shape.slope >= 12;
    let mag2 = match shape.kind {
        BandType::Bell => {
            let a = g.sqrt();
            Analog {
                n: [1.0, a / q, 1.0],
                d: [1.0, 1.0 / (a * q), 1.0],
            }
            .magnitude2(omega)
        }
        BandType::Notch => Analog {
            n: [1.0, 0.0, 1.0],
            d: [1.0, 1.0 / q, 1.0],
        }
        .magnitude2(omega),
        BandType::BandPass => Analog {
            n: [0.0, 1.0 / q, 0.0],
            d: [1.0, 1.0 / q, 1.0],
        }
        .magnitude2(omega),
        BandType::LowShelf | BandType::HighShelf | BandType::TiltShelf => {
            let low = shape.kind != BandType::HighShelf;
            let (g, lift2) = if shape.kind == BandType::TiltShelf {
                (1.0 / g, g)
            } else {
                (g, 1.0)
            };
            let a = g.sqrt();
            let sa = a.sqrt();
            let m = if order2 {
                let proto = if low {
                    Analog {
                        n: [a * a, a * sa / q, a],
                        d: [1.0, sa / q, a],
                    }
                } else {
                    Analog {
                        n: [a, a * sa / q, a * a],
                        d: [a, sa / q, 1.0],
                    }
                };
                proto.magnitude2(omega)
            } else if low {
                (g + omega * omega) / (1.0 / g + omega * omega)
            } else {
                (1.0 + g * omega * omega) / (1.0 + omega * omega / g)
            };
            m * lift2
        }
        BandType::LowCut | BandType::HighCut => {
            let order = (shape.slope / 6).clamp(1, 16) as i32;
            let (qs, pairs, odd) = butterworth(order as usize);
            let low = shape.kind == BandType::LowCut;
            let resonance = q / std::f64::consts::FRAC_1_SQRT_2;
            let mut m = if odd {
                if low {
                    omega * omega / (1.0 + omega * omega)
                } else {
                    1.0 / (1.0 + omega * omega)
                }
            } else {
                1.0
            };
            for (k, &qk) in qs.iter().enumerate().take(pairs) {
                let qk = if k + 1 == pairs { qk * resonance } else { qk };
                let proto = Analog {
                    n: if low {
                        [0.0, 0.0, 1.0]
                    } else {
                        [1.0, 0.0, 0.0]
                    },
                    d: [1.0, 1.0 / qk, 1.0],
                };
                m *= proto.magnitude2(omega);
            }
            m
        }
    };
    10.0 * mag2.max(1e-30).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: BandType, freq: f64, gain: f64, q: f64, slope: u32) -> BandShape {
        BandShape {
            kind,
            freq,
            gain,
            q,
            slope,
        }
    }

    /// The worst difference to the analog response over the audible range
    /// below 0.45 × the sample rate, in dB (ignoring depths under −40 dB,
    /// where cuts and notches leave the audible picture).
    fn worst(s: &BandShape, sr: f64) -> f64 {
        let mut worst = 0.0f64;
        let mut f = 20.0;
        while f < (0.45 * sr).min(20_000.0) {
            let d = band_db(s, sr, f);
            let a = analog_db(s, f);
            if a > -40.0 {
                worst = worst.max((d - a).abs());
            }
            f *= 1.02;
        }
        worst
    }

    #[test]
    fn bells_keep_their_shape_up_to_nyquist() {
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            for &(f, g, q) in &[
                (100.0, 6.0, 1.0),
                (1_000.0, -12.0, 0.7),
                (8_000.0, 9.0, 2.0),
                (12_000.0, 6.0, 1.0),
                (16_000.0, -6.0, 0.5),
                (30.0, 12.0, 4.0),
            ] {
                let s = shape(BandType::Bell, f, g, q, 12);
                let e = worst(&s, sr);
                assert!(e < 0.35, "bell {f} Hz {g} dB Q {q} at {sr}: {e:.3} dB");
                // Exactly the gain at the centre.
                let at = band_db(&s, sr, f);
                assert!((at - g).abs() < 0.01, "{at} at the centre of {f}");
            }
        }
    }

    #[test]
    fn a_bilinear_bell_would_cramp_where_the_matched_one_does_not() {
        // The reason for the matched design: a 12 kHz bell at 44.1 kHz.
        let s = shape(BandType::Bell, 12_000.0, 12.0, 1.0, 12);
        let sr = 44_100.0;
        let w0 = 2.0 * PI * 12_000.0 / sr;
        let a = 10f64.powf(12.0 / 40.0);
        let bil = Analog {
            n: [1.0, a, 1.0],
            d: [1.0, 1.0 / a, 1.0],
        }
        .bilinear(w0);
        let at_18k = |c: &Coefs| sections_db(&[*c], sr, 18_000.0);
        let analog = analog_db(&s, 18_000.0);
        let matched = band_db(&s, sr, 18_000.0);
        assert!((matched - analog).abs() < 0.4);
        assert!(
            (at_18k(&bil) - analog).abs() > 2.0,
            "the bilinear bell falls away early"
        );
    }

    #[test]
    fn shelves_and_tilts_reach_their_gains() {
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            for kind in [BandType::LowShelf, BandType::HighShelf, BandType::TiltShelf] {
                for slope in [6, 12] {
                    for &(f, g) in &[(80.0, 6.0), (1_000.0, -9.0), (10_000.0, 8.0)] {
                        let s = shape(kind, f, g, std::f64::consts::FRAC_1_SQRT_2, slope);
                        let e = worst(&s, sr);
                        assert!(e < 0.4, "{kind:?}/{slope} {f} Hz {g} dB at {sr}: {e:.3}");
                    }
                }
            }
        }
        let s = shape(BandType::TiltShelf, 1_000.0, 6.0, 0.707, 12);
        assert!((band_db(&s, 48_000.0, 20.0) + 3.0).abs() < 0.05);
        assert!((band_db(&s, 48_000.0, 20_000.0) - 3.0).abs() < 0.1);
        let s = shape(BandType::LowShelf, 100.0, 10.0, 0.707, 12);
        assert!((band_db(&s, 48_000.0, 10.0) - 10.0).abs() < 0.05);
        assert!((band_db(&s, 48_000.0, 100.0) - 5.0).abs() < 0.05);
    }

    #[test]
    fn cuts_fall_at_their_slopes() {
        let sr = 48_000.0;
        for (i, &slope) in SLOPES.iter().enumerate() {
            let low = shape(BandType::LowCut, 1_000.0, 0.0, 0.707, slope);
            let high = shape(BandType::HighCut, 1_000.0, 0.0, 0.707, slope);
            // Butterworth: −3 dB at the corner.
            assert!(
                (band_db(&low, sr, 1_000.0) + 3.01).abs() < 0.05,
                "low cut {slope}"
            );
            assert!(
                (band_db(&high, sr, 1_000.0) + 3.01).abs() < 0.1,
                "high cut {slope}"
            );
            // An octave further the slope is nearly reached.
            let drop = band_db(&low, sr, 250.0) - band_db(&low, sr, 125.0);
            assert!(
                (drop - slope as f64).abs() < 0.6,
                "{slope} dB/oct fell {drop:.2} (index {i})"
            );
            // Flat in the pass band.
            assert!(band_db(&low, sr, 10_000.0).abs() < 0.05);
            assert!(band_db(&high, sr, 100.0).abs() < 0.05);
            assert!(
                worst(&low, sr) < 0.5,
                "low cut {slope}: {}",
                worst(&low, sr)
            );
            assert!(
                worst(&high, sr) < 0.5,
                "high cut {slope}: {}",
                worst(&high, sr)
            );
        }
        // Resonance lifts the corner.
        let res = shape(BandType::LowCut, 1_000.0, 0.0, 2.0, 12);
        assert!(band_db(&res, sr, 1_000.0) > 5.5);
        // A high cut near Nyquist still reaches its corner level.
        let top = shape(BandType::HighCut, 18_000.0, 0.0, 0.707, 24);
        assert!((band_db(&top, 44_100.0, 18_000.0) + 3.0).abs() < 0.3);
    }

    #[test]
    fn notches_and_band_passes_hit_their_centres() {
        let sr = 48_000.0;
        for f in [50.0, 1_000.0, 15_000.0] {
            let n = shape(BandType::Notch, f, 0.0, 4.0, 12);
            assert!(band_db(&n, sr, f) < -60.0, "notch at {f}");
            // Far from the centre it leaves the signal alone.
            let away = if f > 1_000.0 { f / 8.0 } else { f * 8.0 };
            assert!(band_db(&n, sr, away).abs() < 0.2, "notch {f} at {away}");
            let b = shape(BandType::BandPass, f, 0.0, 2.0, 12);
            assert!(band_db(&b, sr, f).abs() < 0.05, "band pass at {f}");
            assert!(worst(&b, sr) < 0.6, "band pass {f}: {}", worst(&b, sr));
            assert!(worst(&n, sr) < 1.0, "notch {f}: {}", worst(&n, sr));
        }
    }

    #[test]
    fn the_bilinear_and_matched_ranges_meet() {
        // Just either side of the switch-over the two designs agree.
        let sr = 48_000.0;
        let edge = MATCHED_FROM * sr / (2.0 * PI);
        for kind in BandType::ALL {
            let below = shape(kind, edge * 0.999, 6.0, 1.0, 24);
            let above = shape(kind, edge * 1.001, 6.0, 1.0, 24);
            for f in [edge / 4.0, edge, edge * 4.0] {
                let (a, b) = (band_db(&below, sr, f), band_db(&above, sr, f));
                if a > -30.0 {
                    assert!(
                        (a - b).abs() < 0.1,
                        "{kind:?} jumps {:.3} dB at {f:.0} Hz",
                        a - b
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod accuracy {
    use super::*;

    /// Prints the worst deviation from the analog response per type (run
    /// with `--nocapture` to see the table).
    #[test]
    fn report() {
        for sr in [44_100.0f64, 48_000.0, 96_000.0] {
            for kind in BandType::ALL {
                let mut worst = (0.0f64, 0.0, 0.0);
                for &f in &[30.0, 200.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0] {
                    for &(g, q, slope) in &[(6.0, 0.7, 6), (-9.0, 2.0, 12), (12.0, 0.5, 48)] {
                        let s = BandShape {
                            kind,
                            freq: f,
                            gain: g,
                            q,
                            slope,
                        };
                        let mut x = 20.0;
                        while x < (0.45 * sr).min(20_000.0) {
                            let a = analog_db(&s, x);
                            if a > -40.0 {
                                let e = (band_db(&s, sr, x) - a).abs();
                                if e > worst.0 {
                                    worst = (e, f, x);
                                }
                            }
                            x *= 1.03;
                        }
                    }
                }
                println!(
                    "{sr:>6} {:>11}: {:.3} dB (band at {} Hz, at {:.0} Hz)",
                    kind.name(),
                    worst.0,
                    worst.1,
                    worst.2
                );
            }
        }
    }
}
