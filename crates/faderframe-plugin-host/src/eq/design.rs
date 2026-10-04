//! Filter design for the EQ: analog prototypes turned into digital biquads
//! whose magnitude follows the analog response all the way to Nyquist.
//!
//! Every band shape is a cascade of analog sections (first or second
//! order, each normalised to its own corner): Butterworth cuts of any
//! slope (whole orders, and fractional slopes as a ladder of first order
//! steps an octave apart), Butterworth shelving filters of any order
//! (Holters & Zölzer), bells, notches, band passes, all passes and the
//! flat tilt (a ladder of steps over the whole audio range). The same
//! cascade gives the analog magnitude and phase the linear and natural
//! phase modes are built from, and the digital sections the zero latency
//! mode runs.
//!
//! The usual bilinear transform squeezes the whole analog frequency axis
//! into the digital one, so a bell at 12 kHz on a 44.1 kHz session comes out
//! narrower and a high shelf stops short of its gain at the top
//! ("cramping"). Above a few hertz each section is designed instead after
//! Martin Vicanek, *Matched Second Order Digital Filters* (2016): the poles
//! by impulse invariance, the zeros so that the magnitude matches the
//! analog filter's where it is defined — a cut's level at its corner, a
//! band pass's and a notch's centre — and, for bells and shelves, exact at
//! DC and at the centre or both ends, with the remaining freedom fitted to
//! the analog magnitude up to Nyquist by least squares. All passes keep
//! the matched poles and mirror them into the zeros (exactly flat). Low
//! down, where the bilinear transform is exact to a hair and the matched
//! formulas lose precision, sections are bilinear with prewarping. The
//! editor draws its curves from these same coefficients, so what is shown
//! is what is heard.

use realfft::num_complex::Complex;
use std::f64::consts::PI;

/// A complex number (responses).
pub type C64 = Complex<f64>;

/// Below this digital frequency (radians per sample) sections are
/// bilinear; above it, matched.
const MATCHED_FROM: f64 = 0.001;
/// The highest centre frequency, as a fraction of the sample rate.
const MAX_FRACTION: f64 = 0.499;
/// First order corners above this (radians per sample) are fitted from DC
/// instead of pinned at the corner (which the digital filter cannot
/// reach).
const PIN_BELOW: f64 = 0.9 * PI;
/// The lowest and highest corners of the step ladders (Hz).
const LADDER_LO: f64 = 2.0;
const LADDER_HI: f64 = 50_000.0;
/// The octaves a flat tilt's gain is spread over (20 Hz to 20 kHz).
pub const FLAT_TILT_OCTAVES: f64 = 9.965_784_284_662_087;

/// The shapes a band can take. The order is the parameter's (saved)
/// value; [`BandType::MENU`] is the order menus show them in.
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
    /// A straight tilt over the whole spectrum, pivoting at the frequency:
    /// the gain is the change from 20 Hz to 20 kHz.
    FlatTilt,
    /// Shifts the phase round the frequency, leaving the level alone.
    AllPass,
}

impl BandType {
    pub const ALL: [BandType; 10] = [
        BandType::Bell,
        BandType::LowShelf,
        BandType::HighShelf,
        BandType::LowCut,
        BandType::HighCut,
        BandType::Notch,
        BandType::BandPass,
        BandType::TiltShelf,
        BandType::FlatTilt,
        BandType::AllPass,
    ];

    /// The order menus list the shapes in.
    pub const MENU: [BandType; 10] = [
        BandType::Bell,
        BandType::LowShelf,
        BandType::LowCut,
        BandType::HighShelf,
        BandType::HighCut,
        BandType::Notch,
        BandType::BandPass,
        BandType::TiltShelf,
        BandType::FlatTilt,
        BandType::AllPass,
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
            BandType::FlatTilt => "Flat Tilt",
            BandType::AllPass => "All Pass",
        }
    }

    /// Whether the gain control means anything (and the band can be
    /// dynamic).
    pub fn has_gain(self) -> bool {
        matches!(
            self,
            BandType::Bell
                | BandType::LowShelf
                | BandType::HighShelf
                | BandType::TiltShelf
                | BandType::FlatTilt
        )
    }

    /// Whether the slope control means anything.
    pub fn has_slope(self) -> bool {
        !matches!(self, BandType::Bell | BandType::FlatTilt)
    }

    /// Whether the Q control means anything (at slope `slope`).
    pub fn has_q(self, slope: f64) -> bool {
        match self {
            BandType::FlatTilt => false,
            // A first order cut, shelf or all pass has no resonance.
            BandType::LowCut
            | BandType::HighCut
            | BandType::LowShelf
            | BandType::HighShelf
            | BandType::TiltShelf
            | BandType::AllPass => slope >= 9.0,
            _ => true,
        }
    }

    pub fn is_cut(self) -> bool {
        matches!(self, BandType::LowCut | BandType::HighCut)
    }

    pub fn is_shelf(self) -> bool {
        matches!(
            self,
            BandType::LowShelf | BandType::HighShelf | BandType::TiltShelf
        )
    }

    /// The slopes the shape takes, in dB per octave (the lowest and the
    /// highest; [`BRICKWALL`] is above the highest).
    pub fn slope_range(self) -> (f64, f64) {
        match self {
            BandType::Bell | BandType::Notch => (12.0, 96.0),
            BandType::LowCut | BandType::HighCut => (0.0, BRICKWALL),
            BandType::BandPass => (0.0, 96.0),
            BandType::FlatTilt => (12.0, 12.0),
            _ => (6.0, 96.0),
        }
    }

    /// Whether any slope in its range works (fractional ones included),
    /// or it moves in steps (`step` dB/oct).
    pub fn slope_step(self) -> Option<f64> {
        match self {
            BandType::LowCut | BandType::HighCut => None,
            BandType::BandPass | BandType::Notch | BandType::Bell => Some(12.0),
            _ => Some(6.0),
        }
    }

    /// A slope this shape can take, nearest to `slope`.
    pub fn snap_slope(self, slope: f64) -> f64 {
        let (lo, hi) = self.slope_range();
        let s = slope.clamp(lo, hi);
        match self.slope_step() {
            Some(step) => ((s / step).round() * step).clamp(lo, hi.min(96.0)),
            None if s > 96.5 => BRICKWALL,
            None => s.min(96.0),
        }
    }
}

/// The slope value meaning "brickwall" (cuts).
pub const BRICKWALL: f64 = 100.0;
/// The slopes the wheel and menus step through, in dB per octave.
pub const SLOPES: [f64; 11] = [
    0.0, 6.0, 12.0, 18.0, 24.0, 30.0, 36.0, 48.0, 72.0, 96.0, BRICKWALL,
];
/// Biquad sections a band may need (a brickwall cut: order 32).
pub const MAX_SECTIONS: usize = 16;

/// A slope as menus show it.
pub fn slope_name(slope: f64) -> String {
    if slope > 96.5 {
        "Brickwall".into()
    } else if (slope - slope.round()).abs() < 0.05 {
        format!("{slope:.0} dB/oct")
    } else {
        format!("{slope:.1} dB/oct")
    }
}

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
        self.magnitude2_at(Phi::at(w))
    }

    /// Squared magnitude where `sin²(w/2)` is `s2` (precomputed per bin by
    /// callers that evaluate many sections at the same frequencies).
    pub fn magnitude2_sin2(&self, s2: f64) -> f64 {
        self.magnitude2_at(Phi::from_sin2(s2))
    }

    fn magnitude2_at(&self, p: Phi) -> f64 {
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

    /// The complex response at `w` radians per sample.
    pub fn response(&self, w: f64) -> C64 {
        let z1 = C64::from_polar(1.0, -w);
        let z2 = z1 * z1;
        let num = self.b0 + z1 * self.b1 + z2 * self.b2;
        let den = 1.0 + z1 * self.a1 + z2 * self.a2;
        num / den
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
        Self::from_sin2((w / 2.0).sin().powi(2))
    }

    fn from_sin2(p1: f64) -> Self {
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
        // Two real poles: e^(−w(ζ ± √(ζ²−1))), summed directly so a
        // heavily damped pair does not overflow the cosh.
        let r = (zeta * zeta - 1.0).sqrt();
        -((-w * (zeta - r)).exp() + (-w * (zeta + r)).exp())
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

    fn response(&self, omega: f64) -> C64 {
        let num = C64::new(self.n[0] - self.n[2] * omega * omega, self.n[1] * omega);
        let den = C64::new(self.d[0] - self.d[2] * omega * omega, self.d[1] * omega);
        if den.norm_sqr() > 0.0 {
            num / den
        } else {
            C64::new(0.0, 0.0)
        }
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
        dc_fit(self, w0, a1, a2)
    }
}

/// A numerator for poles `a1, a2`: exact at DC, `B1` and `B2` fitted to the
/// analog magnitude up to Nyquist.
fn dc_fit(proto: &Analog, w0: f64, a1: f64, a2: f64) -> Coefs {
    let aa = a_terms(a1, a2);
    let b0_ = proto.magnitude2(0.0) * aa.0;
    let (b1_, b2_) = fit_b1_b2((w0 / 16.0).min(PI / 64.0), b0_, |w| {
        proto.magnitude2(w / w0) * Phi::at(w).form(aa.0, aa.1, aa.2)
    });
    let (b0, b1, b2) = numerator(b0_, b1_, b2_);
    Coefs { b0, b1, b2, a1, a2 }
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
    let proto = Proto::Bell { g, q }.analog();
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let a = g.sqrt();
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

/// Second order low pass with resonance `q`: exact at DC and at the
/// corner, fitted above.
fn lowpass2(w0: f64, q: f64) -> Coefs {
    let proto = Proto::Lowpass2 { q }.analog();
    if w0 < MATCHED_FROM {
        return proto.bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * q));
    pinned_fit(&proto, w0, a1, a2)
}

/// Second order high pass with resonance `q`.
fn highpass2(w0: f64, q: f64) -> Coefs {
    let proto = Proto::Highpass2 { q }.analog();
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
    let proto = Proto::BandPass { q }.analog();
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
    let proto = Proto::Notch { q }.analog();
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

/// Second order all pass: matched poles, mirrored into the zeros.
fn allpass2(w0: f64, q: f64) -> Coefs {
    if w0 < MATCHED_FROM {
        return Proto::AllPass2 { q }.analog().bilinear(w0);
    }
    let (a1, a2) = matched_poles(w0, 1.0 / (2.0 * q));
    Coefs {
        b0: a2,
        b1: a1,
        b2: 1.0,
        a1,
        a2,
    }
}

/// First order all pass (unity at DC, −90° at the corner).
fn allpass1(w0: f64) -> Coefs {
    let a1 = if w0 < MATCHED_FROM {
        let k = (w0 / 2.0).tan();
        (k - 1.0) / (k + 1.0)
    } else {
        -(-w0).exp()
    };
    Coefs {
        b0: a1,
        b1: 1.0,
        b2: 0.0,
        a1,
        a2: 0.0,
    }
}

/// First order sections: `(n1 s + n0) / (s + p)` normalised to the corner.
/// Matched: the pole by impulse invariance, and a second order numerator
/// exact at DC and at the corner (or, for a corner past Nyquist, just at
/// DC) and fitted to the analog magnitude up to Nyquist (a single zero
/// cannot follow it there).
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
    let a1 = -(-p * w0).exp();
    if w0 * p.max(1.0) < PIN_BELOW {
        pinned_fit(&proto, w0, a1, 0.0)
    } else {
        dc_fit(&proto, w0, a1, 0.0)
    }
}

/// Butterworth quality factors of the second order sections of an order
/// `n` filter (the sharpest last), and whether it has a first order
/// section too.
fn butterworth(n: usize) -> ([f64; MAX_SECTIONS], usize, bool) {
    let mut qs = [0.0; MAX_SECTIONS];
    let pairs = (n / 2).min(MAX_SECTIONS);
    let odd = n % 2 == 1;
    for (k, q) in qs.iter_mut().enumerate().take(pairs) {
        let theta = if odd {
            (k + 1) as f64 * PI / n as f64
        } else {
            (2 * k + 1) as f64 * PI / (2 * n) as f64
        };
        // The poles nearest the axis come last.
        *q = 1.0 / (2.0 * theta.cos());
    }
    // The sharpest is the largest: sort ascending.
    qs[..pairs].sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (qs, pairs, odd)
}

/// What a band asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandShape {
    pub kind: BandType,
    pub freq: f64,
    /// dB (bells, shelves, tilts).
    pub gain: f64,
    pub q: f64,
    /// dB per octave ([`BRICKWALL`] for a brickwall cut).
    pub slope: f64,
}

/// A normalised analog section.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Proto {
    Bell {
        g: f64,
        q: f64,
    },
    /// General second order, matched from DC.
    Shelf(Analog),
    /// `(n1 s + n0) / (s + p)`.
    First {
        n1: f64,
        n0: f64,
        p: f64,
    },
    Lowpass2 {
        q: f64,
    },
    Highpass2 {
        q: f64,
    },
    BandPass {
        q: f64,
    },
    Notch {
        q: f64,
    },
    AllPass2 {
        q: f64,
    },
    AllPass1,
}

impl Proto {
    fn analog(&self) -> Analog {
        match *self {
            Proto::Bell { g, q } => {
                let a = g.sqrt();
                Analog {
                    n: [1.0, a / q, 1.0],
                    d: [1.0, 1.0 / (a * q), 1.0],
                }
            }
            Proto::Shelf(a) => a,
            Proto::First { n1, n0, p } => Analog {
                n: [n0, n1, 0.0],
                d: [p, 1.0, 0.0],
            },
            Proto::Lowpass2 { q } => Analog {
                n: [1.0, 0.0, 0.0],
                d: [1.0, 1.0 / q, 1.0],
            },
            Proto::Highpass2 { q } => Analog {
                n: [0.0, 0.0, 1.0],
                d: [1.0, 1.0 / q, 1.0],
            },
            Proto::BandPass { q } => Analog {
                n: [0.0, 1.0 / q, 0.0],
                d: [1.0, 1.0 / q, 1.0],
            },
            Proto::Notch { q } => Analog {
                n: [1.0, 0.0, 1.0],
                d: [1.0, 1.0 / q, 1.0],
            },
            Proto::AllPass2 { q } => Analog {
                n: [1.0, -1.0 / q, 1.0],
                d: [1.0, 1.0 / q, 1.0],
            },
            Proto::AllPass1 => Analog {
                n: [1.0, -1.0, 0.0],
                d: [1.0, 1.0, 0.0],
            },
        }
    }
}

/// A section of a band: its prototype, the corner (Hz) it is normalised
/// to, and a gain factor.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Section {
    proto: Proto,
    corner: f64,
    scale: f64,
}

impl Section {
    fn new(proto: Proto, corner: f64) -> Self {
        Self {
            proto,
            corner,
            scale: 1.0,
        }
    }

    fn digital(&self, sample_rate: f64) -> Coefs {
        let w0 = 2.0 * PI * self.corner / sample_rate;
        let c = match self.proto {
            Proto::Bell { g, q } => bell(w0, g, q),
            Proto::Shelf(a) => {
                if w0 < MATCHED_FROM {
                    a.bilinear(w0)
                } else {
                    a.matched_shelf(w0)
                }
            }
            Proto::First { n1, n0, p } => first_order(w0, n1, n0, p),
            Proto::Lowpass2 { q } => lowpass2(w0, q),
            Proto::Highpass2 { q } => highpass2(w0, q),
            Proto::BandPass { q } => bandpass(w0, q),
            Proto::Notch { q } => notch(w0, q),
            Proto::AllPass2 { q } => allpass2(w0, q),
            Proto::AllPass1 => allpass1(w0),
        };
        c.scaled(self.scale)
    }

    fn response(&self, freq: f64) -> C64 {
        self.proto.analog().response(freq / self.corner) * self.scale
    }

    fn magnitude2(&self, freq: f64) -> f64 {
        self.proto.analog().magnitude2(freq / self.corner) * self.scale * self.scale
    }
}

/// The sections of a band.
struct Sections {
    list: [Section; MAX_SECTIONS],
    len: usize,
}

impl Sections {
    fn new() -> Self {
        Self {
            list: [Section::new(Proto::AllPass1, 1.0); MAX_SECTIONS],
            len: 0,
        }
    }

    fn room(&self) -> usize {
        MAX_SECTIONS - self.len
    }

    fn push(&mut self, s: Section) {
        if self.len < MAX_SECTIONS {
            self.list[self.len] = s;
            self.len += 1;
        }
    }

    fn scale_first(&mut self, k: f64) {
        if let Some(s) = self.list[..self.len].first_mut() {
            s.scale *= k;
        }
    }

    fn as_slice(&self) -> &[Section] {
        &self.list[..self.len]
    }

    /// First order steps `(s + z)/(s + p)` (ratios to their corners), two
    /// to a biquad.
    fn push_steps(&mut self, steps: &[(f64, f64, f64)]) {
        for pair in steps.chunks(2) {
            match *pair {
                [(c1, z1, p1), (c2, z2, p2)] => {
                    let (z1, p1, z2, p2) = (c1 * z1, c1 * p1, c2 * z2, c2 * p2);
                    let wc = (p1 * p2).sqrt();
                    self.push(Section::new(
                        Proto::Shelf(Analog {
                            n: [z1 * z2 / (wc * wc), (z1 + z2) / wc, 1.0],
                            d: [p1 * p2 / (wc * wc), (p1 + p2) / wc, 1.0],
                        }),
                        wc,
                    ));
                }
                [(c, z, p)] => self.push(Section::new(
                    Proto::First {
                        n1: 1.0,
                        n0: z / p,
                        p: 1.0,
                    },
                    c * p,
                )),
                _ => {}
            }
        }
    }
}

/// A ladder of first order steps of `step_db` each, an octave apart, from
/// `from` Hz while the corners stay between the ladder's limits (`up`:
/// towards higher frequencies), at most `max` of them. A step's level
/// changes by `step_db` from below its corner to above it.
fn ladder(from: f64, up: bool, step_db: f64, max: usize, out: &mut [(f64, f64, f64)]) -> usize {
    let x = step_db / (20.0 * 2f64.log10());
    let mut n = 0;
    let mut c = from;
    while n < max.min(out.len()) && (LADDER_LO..=LADDER_HI).contains(&c) {
        // (s + z)/(s + p) with z/p = 2^−x rises by x octaves' worth.
        out[n] = (c, 2f64.powf(-x / 2.0), 2f64.powf(x / 2.0));
        n += 1;
        c = if up { c * 2.0 } else { c * 0.5 };
    }
    n
}

/// Whole order and fraction of a cut's slope.
fn cut_order(slope: f64) -> (usize, f64) {
    if slope > 96.5 {
        return (32, 0.0);
    }
    let order = (slope / 6.0).clamp(0.0, 16.0);
    let mut whole = order.floor();
    let mut frac = order - whole;
    if frac < 0.03 {
        frac = 0.0;
    } else if frac > 0.97 {
        whole += 1.0;
        frac = 0.0;
    }
    (whole as usize, frac)
}

/// The analog sections of a band at its frequency `freq`.
fn sections_of(shape: &BandShape, freq: f64) -> Sections {
    let mut out = Sections::new();
    let q = shape.q.clamp(0.01, 100.0);
    let g = 10f64.powf(shape.gain / 20.0);
    match shape.kind {
        BandType::Bell => {
            if (g - 1.0).abs() > 1e-12 {
                out.push(Section::new(Proto::Bell { g, q }, freq));
            }
        }
        BandType::Notch => {
            let k = (shape.slope / 12.0).round().clamp(1.0, 8.0) as usize;
            // Each of k notches as wide as keeps the whole one's −3 dB
            // edges.
            let t = 2f64.powf(-1.0 / k as f64);
            let qk = q * (t / (1.0 - t)).sqrt();
            for _ in 0..k {
                out.push(Section::new(Proto::Notch { q: qk }, freq));
            }
        }
        BandType::BandPass => {
            let k = (shape.slope / 12.0).round().clamp(0.0, 8.0) as usize;
            if k > 0 {
                let qk = q * (2f64.powf(1.0 / k as f64) - 1.0).sqrt();
                for _ in 0..k {
                    out.push(Section::new(Proto::BandPass { q: qk }, freq));
                }
            }
        }
        BandType::AllPass => {
            let k = (shape.slope / 6.0).round().clamp(1.0, 16.0) as usize;
            if k % 2 == 1 {
                out.push(Section::new(Proto::AllPass1, freq));
            }
            for _ in 0..k / 2 {
                out.push(Section::new(Proto::AllPass2 { q }, freq));
            }
        }
        BandType::LowShelf | BandType::HighShelf | BandType::TiltShelf => {
            let low = shape.kind != BandType::HighShelf;
            // A tilt is a low shelf by minus the gain, lifted by half of it.
            let (g, lift) = if shape.kind == BandType::TiltShelf {
                (1.0 / g, g.sqrt())
            } else {
                (g, 1.0)
            };
            if (g - 1.0).abs() > 1e-12 {
                let m = (shape.slope / 6.0).round().clamp(1.0, 16.0) as usize;
                shelf(&mut out, low, g, q, m, freq);
                out.scale_first(lift);
            }
        }
        BandType::FlatTilt => {
            if (g - 1.0).abs() > 1e-12 {
                let mut steps = [(0.0, 0.0, 0.0); 2 * MAX_SECTIONS];
                let per_octave = shape.gain / FLAT_TILT_OCTAVES;
                let n = ladder(LADDER_LO, true, per_octave, 2 * MAX_SECTIONS, &mut steps);
                out.push_steps(&steps[..n]);
                // Level at the pivot: none.
                let at = out
                    .as_slice()
                    .iter()
                    .map(|s| s.magnitude2(freq))
                    .product::<f64>()
                    .sqrt();
                if at > 0.0 {
                    out.scale_first(1.0 / at);
                }
            }
        }
        BandType::LowCut | BandType::HighCut => {
            let low = shape.kind == BandType::LowCut;
            let (order, frac) = cut_order(shape.slope);
            let (qs, pairs, odd) = butterworth(order);
            if odd {
                out.push(Section::new(
                    if low {
                        Proto::First {
                            n1: 1.0,
                            n0: 0.0,
                            p: 1.0,
                        }
                    } else {
                        Proto::First {
                            n1: 0.0,
                            n0: 1.0,
                            p: 1.0,
                        }
                    },
                    freq,
                ));
            }
            // The resonance control lifts the sharpest section.
            let resonance = q / std::f64::consts::FRAC_1_SQRT_2;
            for (k, &qk) in qs.iter().enumerate().take(pairs) {
                let qk = if k + 1 == pairs { qk * resonance } else { qk };
                out.push(Section::new(
                    if low {
                        Proto::Highpass2 { q: qk }
                    } else {
                        Proto::Lowpass2 { q: qk }
                    },
                    freq,
                ));
            }
            if frac > 0.0 {
                // The rest of the slope: steps of 6·frac dB an octave apart
                // away from the corner, starting half an octave out.
                let mut steps = [(0.0, 0.0, 0.0); 2 * MAX_SECTIONS];
                let step = 6.0 * frac;
                let room = 2 * out.room();
                let n = if low {
                    ladder(freq / 2f64.sqrt(), false, step, room, &mut steps)
                } else {
                    ladder(freq * 2f64.sqrt(), true, -step, room, &mut steps)
                };
                out.push_steps(&steps[..n]);
                if !low {
                    // (s + z)/(s + p) with z > p has DC gain z/p: bring the
                    // pass band back to unity.
                    let dc: f64 = steps[..n].iter().map(|(_, z, p)| z / p).product();
                    if dc > 0.0 {
                        out.scale_first(1.0 / dc);
                    }
                }
            }
        }
    }
    out
}

/// Butterworth shelving sections of order `m` (Holters & Zölzer): `g`
/// (linear) below (`low`) or above the corner, its square root at it; the
/// resonance `q` lifts the sharpest section as in a second order shelf.
fn shelf(out: &mut Sections, low: bool, g: f64, q: f64, m: usize, freq: f64) {
    let (qs, pairs, odd) = butterworth(m);
    let r = g.powf(1.0 / (2.0 * m as f64));
    let (z, p) = if low { (r, 1.0 / r) } else { (1.0 / r, r) };
    let start = out.len;
    if odd {
        out.push(Section::new(Proto::First { n1: 1.0, n0: z, p }, freq));
    }
    let resonance = q / std::f64::consts::FRAC_1_SQRT_2;
    for (k, &qk) in qs.iter().enumerate().take(pairs) {
        let qk = if k + 1 == pairs { qk * resonance } else { qk };
        out.push(Section::new(
            Proto::Shelf(Analog {
                n: [z * z, z / qk, 1.0],
                d: [p * p, p / qk, 1.0],
            }),
            freq,
        ));
    }
    if !low && let Some(s) = out.list.get_mut(start) {
        s.scale *= g;
    }
}

/// The sections of a band at `sample_rate`; returns how many are used.
pub fn design(shape: &BandShape, sample_rate: f64, out: &mut [Coefs; MAX_SECTIONS]) -> usize {
    let freq = shape.freq.clamp(1.0, MAX_FRACTION * sample_rate);
    let sections = sections_of(shape, freq);
    for (o, s) in out.iter_mut().zip(sections.as_slice()) {
        *o = s.digital(sample_rate);
    }
    sections.len
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

/// The complex response of a cascade at `freq`.
pub fn sections_response(sections: &[Coefs], sample_rate: f64, freq: f64) -> C64 {
    let w = 2.0 * PI * freq.clamp(0.0, 0.5 * sample_rate) / sample_rate;
    sections
        .iter()
        .fold(C64::new(1.0, 0.0), |acc, c| acc * c.response(w))
}

/// A band's analog prototype, built once to be evaluated at many
/// frequencies (allocation free).
pub struct AnalogBand {
    sections: Sections,
}

impl AnalogBand {
    pub fn new(shape: &BandShape) -> Self {
        Self {
            sections: sections_of(shape, shape.freq),
        }
    }

    pub fn magnitude2(&self, freq: f64) -> f64 {
        self.sections
            .as_slice()
            .iter()
            .map(|s| s.magnitude2(freq))
            .product()
    }

    pub fn db(&self, freq: f64) -> f64 {
        10.0 * self.magnitude2(freq).max(1e-30).log10()
    }

    pub fn response(&self, freq: f64) -> C64 {
        self.sections
            .as_slice()
            .iter()
            .fold(C64::new(1.0, 0.0), |acc, s| acc * s.response(freq))
    }
}

/// The analog response of a band in dB at `freq` (what the digital one is
/// matched to; the linear phase mode plays it exactly).
pub fn analog_db(shape: &BandShape, freq: f64) -> f64 {
    AnalogBand::new(shape).db(freq)
}

/// The analog response of a band (magnitude and phase) at `freq`.
pub fn analog_response(shape: &BandShape, freq: f64) -> C64 {
    AnalogBand::new(shape).response(freq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: BandType, freq: f64, gain: f64, q: f64, slope: f64) -> BandShape {
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
                let s = shape(BandType::Bell, f, g, q, 12.0);
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
        let s = shape(BandType::Bell, 12_000.0, 12.0, 1.0, 12.0);
        let sr = 44_100.0;
        let w0 = 2.0 * PI * 12_000.0 / sr;
        let bil = Proto::Bell {
            g: 10f64.powf(12.0 / 20.0),
            q: 1.0,
        }
        .analog()
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
                for slope in [6.0, 12.0] {
                    for &(f, g) in &[(80.0, 6.0), (1_000.0, -9.0), (10_000.0, 8.0)] {
                        let s = shape(kind, f, g, std::f64::consts::FRAC_1_SQRT_2, slope);
                        let e = worst(&s, sr);
                        assert!(e < 0.4, "{kind:?}/{slope} {f} Hz {g} dB at {sr}: {e:.3}");
                    }
                }
            }
        }
        let s = shape(BandType::TiltShelf, 1_000.0, 6.0, 0.707, 12.0);
        assert!((band_db(&s, 48_000.0, 20.0) + 3.0).abs() < 0.05);
        assert!((band_db(&s, 48_000.0, 20_000.0) - 3.0).abs() < 0.1);
        let s = shape(BandType::LowShelf, 100.0, 10.0, 0.707, 12.0);
        assert!((band_db(&s, 48_000.0, 10.0) - 10.0).abs() < 0.05);
        assert!((band_db(&s, 48_000.0, 100.0) - 5.0).abs() < 0.05);
    }

    #[test]
    fn steeper_shelves_keep_their_ends_and_midpoint() {
        let sr = 48_000.0;
        for slope in [18.0, 24.0, 48.0, 96.0] {
            for kind in [BandType::LowShelf, BandType::HighShelf] {
                let s = shape(kind, 1_000.0, 9.0, 0.707, slope);
                let (lo, hi) = if kind == BandType::LowShelf {
                    (9.0, 0.0)
                } else {
                    (0.0, 9.0)
                };
                assert!(
                    (band_db(&s, sr, 20.0) - lo).abs() < 0.05,
                    "{kind:?} {slope}"
                );
                assert!(
                    (band_db(&s, sr, 18_000.0) - hi).abs() < 0.15,
                    "{kind:?} {slope}"
                );
                assert!(
                    (band_db(&s, sr, 1_000.0) - 4.5).abs() < 0.1,
                    "{kind:?} {slope}"
                );
                assert!(worst(&s, sr) < 0.5, "{kind:?} {slope}: {}", worst(&s, sr));
            }
            // Steeper: an octave below the corner the 48 dB/oct shelf is
            // nearly there, the 12 dB/oct one is not.
            let steep = shape(BandType::LowShelf, 1_000.0, 9.0, 0.707, 48.0);
            let gentle = shape(BandType::LowShelf, 1_000.0, 9.0, 0.707, 12.0);
            assert!(band_db(&steep, sr, 707.0) > band_db(&gentle, sr, 707.0) + 1.0);
        }
    }

    #[test]
    fn cuts_fall_at_their_slopes() {
        let sr = 48_000.0;
        for slope in [6.0, 12.0, 18.0, 24.0, 30.0, 36.0, 48.0, 72.0, 96.0] {
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
            assert!((drop - slope).abs() < 0.6, "{slope} dB/oct fell {drop:.2}");
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
        let res = shape(BandType::LowCut, 1_000.0, 0.0, 2.0, 12.0);
        assert!(band_db(&res, sr, 1_000.0) > 5.5);
        // A high cut near Nyquist still reaches its corner level.
        let top = shape(BandType::HighCut, 18_000.0, 0.0, 0.707, 24.0);
        assert!((band_db(&top, 44_100.0, 18_000.0) + 3.0).abs() < 0.3);
        // No slope: no cut.
        let flat = shape(BandType::LowCut, 1_000.0, 0.0, 0.707, 0.0);
        assert!(band_db(&flat, sr, 100.0).abs() < 1e-9);
    }

    #[test]
    fn fractional_slopes_fall_at_their_fraction() {
        let sr = 48_000.0;
        for slope in [3.5, 9.0, 15.0, 27.0, 45.0] {
            for kind in [BandType::LowCut, BandType::HighCut] {
                let s = shape(
                    kind,
                    if kind == BandType::LowCut {
                        2_000.0
                    } else {
                        300.0
                    },
                    0.0,
                    0.707,
                    slope,
                );
                // Two to three octaves into the stop band.
                let (a, b) = if kind == BandType::LowCut {
                    (250.0, 125.0)
                } else {
                    (2_400.0, 4_800.0)
                };
                let drop = band_db(&s, sr, a) - band_db(&s, sr, b);
                assert!(
                    (drop - slope).abs() < 0.9,
                    "{kind:?} {slope} dB/oct fell {drop:.2}"
                );
                // The pass band stays flat.
                let pass = if kind == BandType::LowCut {
                    16_000.0
                } else {
                    30.0
                };
                assert!(
                    band_db(&s, sr, pass).abs() < 0.3,
                    "{kind:?} {slope} pass band"
                );
                assert!(worst(&s, sr) < 0.5, "{kind:?} {slope}: {}", worst(&s, sr));
            }
        }
    }

    #[test]
    fn a_brickwall_is_steep() {
        let sr = 48_000.0;
        let s = shape(BandType::HighCut, 1_000.0, 0.0, 0.707, BRICKWALL);
        assert!((band_db(&s, sr, 1_000.0) + 3.01).abs() < 0.2);
        assert!(band_db(&s, sr, 1_200.0) < -40.0);
        assert!(band_db(&s, sr, 850.0).abs() < 0.3);
        let s = shape(BandType::LowCut, 100.0, 0.0, 0.707, BRICKWALL);
        assert!(band_db(&s, sr, 83.0) < -40.0);
        assert!(band_db(&s, sr, 120.0).abs() < 0.3);
    }

    #[test]
    fn notches_and_band_passes_hit_their_centres() {
        let sr = 48_000.0;
        for f in [50.0, 1_000.0, 15_000.0] {
            for slope in [12.0, 24.0, 48.0] {
                let n = shape(BandType::Notch, f, 0.0, 4.0, slope);
                assert!(band_db(&n, sr, f) < -60.0, "notch at {f}");
                // Far from the centre it leaves the signal alone.
                let away = if f > 1_000.0 { f / 8.0 } else { f * 8.0 };
                assert!(band_db(&n, sr, away).abs() < 0.2, "notch {f} at {away}");
                let b = shape(BandType::BandPass, f, 0.0, 2.0, slope);
                assert!(band_db(&b, sr, f).abs() < 0.05, "band pass at {f}");
                // Each cascaded section adds its own small error near
                // Nyquist.
                let tolerance = 0.15 + 0.45 * (slope / 12.0);
                assert!(
                    worst(&b, sr) < tolerance,
                    "band pass {f}: {}",
                    worst(&b, sr)
                );
                assert!(worst(&n, sr) < 1.0, "notch {f}: {}", worst(&n, sr));
            }
        }
        // Steeper band passes keep their −3 dB edges.
        for slope in [12.0, 24.0, 48.0] {
            let b = shape(BandType::BandPass, 1_000.0, 0.0, 2.0, slope);
            let edge = 1_000.0 * ((1.0 + 1.0 / 16.0f64).sqrt() + 0.25);
            assert!((analog_db(&b, edge) + 3.01).abs() < 0.05, "{slope}");
        }
    }

    #[test]
    fn all_passes_are_flat_and_turn_the_phase() {
        let sr = 48_000.0;
        for slope in [6.0, 12.0, 24.0] {
            for f in [100.0, 2_000.0, 15_000.0] {
                let s = shape(BandType::AllPass, f, 0.0, 1.0, slope);
                let mut c = [Coefs::IDENTITY; MAX_SECTIONS];
                let n = design(&s, sr, &mut c);
                for probe in [20.0, f, 19_000.0] {
                    assert!(sections_db(&c[..n], sr, probe).abs() < 1e-6);
                    assert!((analog_db(&s, probe)).abs() < 1e-9);
                }
                // At the corner: −90° per first order, −180° per second.
                let want = -(slope / 6.0) * 90.0;
                let phase = analog_response(&s, f).arg().to_degrees();
                let wrap = (phase - want).rem_euclid(360.0);
                assert!(wrap < 1e-6 || wrap > 360.0 - 1e-6, "{slope} {f}: {phase}");
            }
        }
    }

    #[test]
    fn a_flat_tilt_is_straight() {
        let sr = 48_000.0;
        let s = shape(BandType::FlatTilt, 1_000.0, 10.0, 1.0, 12.0);
        // No change at the pivot, the gain spread over 20 Hz to 20 kHz.
        assert!(band_db(&s, sr, 1_000.0).abs() < 0.1);
        let per_octave = 10.0 / FLAT_TILT_OCTAVES;
        for f in [40.0, 160.0, 640.0, 2_560.0, 10_240.0] {
            let want = per_octave * (f / 1_000.0f64).log2();
            let got = band_db(&s, sr, f);
            assert!((got - want).abs() < 0.25, "{f}: {got:.2} vs {want:.2}");
        }
        assert!(worst(&s, sr) < 0.3, "{}", worst(&s, sr));
    }

    #[test]
    fn digital_sections_follow_the_analog_phase_low_down() {
        // The natural phase mode corrects what is left near Nyquist.
        let sr = 48_000.0;
        for kind in [BandType::Bell, BandType::LowShelf, BandType::HighCut] {
            let s = shape(kind, 1_000.0, 6.0, 1.0, 12.0);
            let mut c = [Coefs::IDENTITY; MAX_SECTIONS];
            let n = design(&s, sr, &mut c);
            for f in [50.0, 200.0] {
                let d = sections_response(&c[..n], sr, f);
                let a = analog_response(&s, f);
                let diff = (d / a).arg().to_degrees().abs();
                assert!(diff < 3.0, "{kind:?} at {f}: {diff:.2}°");
                assert!(((d.norm() / a.norm()).log10() * 20.0).abs() < 0.2);
            }
        }
    }

    #[test]
    fn the_bilinear_and_matched_ranges_meet() {
        // Just either side of the switch-over the two designs agree.
        let sr = 48_000.0;
        let edge = MATCHED_FROM * sr / (2.0 * PI);
        for kind in BandType::ALL {
            if kind == BandType::FlatTilt {
                continue;
            }
            let below = shape(kind, edge * 0.999, 6.0, 1.0, 24.0);
            let above = shape(kind, edge * 1.001, 6.0, 1.0, 24.0);
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

    #[test]
    fn slopes_snap_to_what_a_shape_takes() {
        assert_eq!(BandType::Bell.snap_slope(28.0), 24.0);
        assert_eq!(BandType::LowCut.snap_slope(3.5), 3.5);
        assert_eq!(BandType::LowCut.snap_slope(99.0), BRICKWALL);
        assert_eq!(BandType::LowShelf.snap_slope(0.0), 6.0);
        assert_eq!(BandType::LowShelf.snap_slope(100.0), 96.0);
        assert_eq!(slope_name(BRICKWALL), "Brickwall");
        assert_eq!(slope_name(3.5), "3.5 dB/oct");
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
                    for &(g, q, slope) in &[(6.0, 0.7, 6.0), (-9.0, 2.0, 12.0), (12.0, 0.5, 48.0)] {
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
