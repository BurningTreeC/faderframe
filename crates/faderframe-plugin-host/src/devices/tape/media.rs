//! The tape: its formulations and the magnetics of recording on it.
//!
//! **Hysteresis.** The particles follow the Jiles–Atherton model (D. C.
//! Jiles, D. L. Atherton, "Theory of ferromagnetic hysteresis", J. Magn.
//! Magn. Mater. 61, 1986): an anhysteretic magnetisation `M_an`, an
//! irreversible part that domain-wall pinning `k` holds back,
//! `dM_irr/dH = (M_an − M_irr)/(δk)` (moving only while
//! `δ(M_an − M_irr) > 0`), and a reversible share `c`:
//! `M = (1 − c)M_irr + cM_an`.
//!
//! Two choices follow the physics of particulate tape rather than bulk
//! iron:
//!
//! * The anhysteretic is `M_s tanh(H_e/a)`, the form for uniaxial
//!   particles aligned with the field (Ramesh, Jiles, Roderick, "A model of
//!   anisotropic anhysteretic magnetization", IEEE Trans. Magn. 32, 1996):
//!   acicular oxide is oriented along the tape.
//! * The mean field `α` is negative and acts on the anhysteretic state:
//!   `M_an = M_s tanh((H + αM_an)/a)`, solved self-consistently. In an
//!   assembly of particles recorded anhysteretically the interaction
//!   (demagnetising) field of that equilibrium shears the steep intrinsic
//!   curve, which is what limits the anhysteretic susceptibility (Bertram,
//!   *Theory of Magnetic Recording*, on AC-bias recording). The shear makes
//!   the transfer straight for longer and its knee sharper than a bare
//!   `tanh` or Langevin: the measured third harmonic of 456 grows from 0.1 %
//!   to 3 % over 12.3 dB, steeper than the square law of a soft curve, and
//!   one shear width reproduces both of its published points (see
//!   [`STANDARD`]). The pinned particles chase that anhysteretic in the
//!   applied field, so the unbiased major loop keeps the coercivity `≈ k`
//!   and the squareness of oriented tape.
//!
//! **AC bias.** An ultrasonic bias field, decaying as the tape leaves the
//! gap, is the anhysteretic process itself: JA define `M_an` as the state an
//! AC field decaying in a DC field leaves (and Mallinson, *The Foundations
//! of Magnetic Recording*, describes biased recording the same way). The
//! particles whose switching fields the bias reaches at the recording zone
//! record anhysteretically; those it does not reach keep their pinning and
//! record on the hysteresis loop of the signal alone. The share reached,
//! [`Bias::switched`], comes from the spread of the switching fields
//! (across particles and through the coating's depth, where the bias field
//! falls off): under-bias leaves a part hysteretic (crossover-like
//! distortion, lost low-level sensitivity), the aligned bias reaches all
//! but a trace. Over-bias moves the recording point further from the gap,
//! where the signal field is weaker ([`Bias::drive`]), and lengthens the
//! recording zone, which erases short wavelengths ([`Bias::zone`]).
//!
//! **Solving it.** The irreversible equation is solved at the oversampled
//! rate with an exponential integrator ([`Magnet::step`]): over a step it is
//! linear in `M_irr` with a target moving along the field's path, so it is
//! integrated exactly for a target moving linearly — second order in the
//! step, never past the target, and exact in the limit where the bias has
//! taken the pinning away and the equation is infinitely stiff. Where the
//! target jumps within one step (a hot treble at a low rate) the step is
//! split. Explicit Runge–Kutta must keep `|ΔH|/k` under 2.8 to stay
//! stable, which the anhysteretically recorded population (`k → 0`) never
//! gives; the tests compare it with RK4.

/// The permeability of free space (H/m).
pub const MU0: f64 = 4.0e-7 * std::f64::consts::PI;

/// A tape formulation: its magnetic parameters (SI) and the published
/// figures it was fitted to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Formulation {
    pub name: &'static str,
    /// Saturation remanence `M_s` (A/m): the most the tape keeps, from its
    /// retentivity (`B_rs = μ0 M_s`; 1 G = 10⁻⁴ T).
    pub ms: f64,
    /// The anhysteretic's width `a` (A/m).
    pub a: f64,
    /// The interaction field's share `α` (negative: demagnetising).
    pub alpha: f64,
    /// Pinning `k` (A/m): set so the unbiased major loop's coercive field
    /// is the data sheet's ([`Self::coercivity`]); with this anhysteretic
    /// the loop's comes out at about 0.65 `k`.
    pub k: f64,
    /// The data sheet's coercivity (Oe).
    pub coercivity: f64,
    /// The reversible share `c`.
    pub c: f64,
    /// Coating thickness (m).
    pub coating: f64,
    /// The spread of the switching fields the bias meets (as a fraction of
    /// the aligned bias field at the recording zone): particles' switching
    /// field distribution and the coating's depth together.
    pub spread: f64,
    /// A-weighted (ASA) tape noise, nWb/m RMS, at 15 ips NAB on a 75 mil
    /// (¼″ two-track) track.
    pub noise: f64,
    /// The level (nWb/m RMS, 1 kHz) of 3 % third harmonic distortion.
    pub mol: f64,
    /// The specification's reference level (nWb/m) and its third harmonic
    /// distortion there.
    pub reference: (f64, f64),
}

/// A standard studio mastering tape in the manner of Ampex/Quantegy 456:
/// coercivity 330 Oe, retentivity 1460 G, 12.7 µm (0.5 mil) coating; 0.1 %
/// third harmonic at the 260 nWb/m reference and 3 % at +12.3 dB (1077
/// nWb/m, 15 ips), weighted S/N 76.6 dB at that level (Quantegy 456 data
/// sheet; the ATR Master Tape sheet's 330 Oe, 1400 G, 0.1 % at reference
/// and 3 % 12 dB above agree). The shear width `a/(|α| M_s)` = 0.477 fitted
/// to the 3 % point gives 0.108 % at 260 nWb/m: both points from one
/// parameter. `|α| M_s` puts the field of a 3 % recording at the
/// coercivity (an anhysteretic susceptibility of about 4).
pub const STANDARD: Formulation = Formulation {
    name: "Standard",
    ms: 116.2e3,
    a: 0.477 * 0.162 * 116.2e3,
    alpha: -0.162,
    k: 330.0 * 1.54 * OERSTED,
    coercivity: 330.0,
    c: 0.15,
    coating: 12.7e-6,
    spread: 0.30,
    noise: 1077.0 / 6_760.8, // 76.6 dB under the 3 % level
    mol: 1077.0,
    reference: (260.0, 0.001),
};

/// A high-output tape in the manner of GP9 (the "+9" class): its
/// saturation flux 3 dB over the standard's (the class's operating level
/// is 3 dB higher for the same headroom), a little more coercivity (350 Oe,
/// as the high-output 499's) and a sharper knee (shear width 0.40). Its
/// own data sheet was not at hand: the remanence and coating are the
/// standard's scaled for the +3 dB, the noise taken as the standard's.
pub const HIGH_OUTPUT: Formulation = Formulation {
    name: "High Output",
    ms: 138.0e3,
    a: 0.40 * 0.149 * 138.0e3,
    alpha: -0.149,
    k: 350.0 * 1.47 * OERSTED,
    coercivity: 350.0,
    c: 0.12,
    coating: 15.1e-6,
    spread: 0.28,
    noise: 1077.0 / 6_760.8,
    mol: 1567.0,
    reference: (320.0, 0.00072),
};

/// An older, thinner standard-output tape in the manner of Quantegy's 600
/// series (632): coercivity 310 Oe, retentivity 1100 G, 7.9 µm coating,
/// 0.75 % third harmonic on playback at its reference record level (taken
/// as 185 nWb/m; the reproduce equaliser lifts the 3 kHz harmonic 1.3 dB
/// over the fundamental, so 0.65 % on the tape) — a soft curve (shear
/// width 4.85) whose 3 % point falls near 380 nWb/m, +6 dB. Noise flux
/// scaled from the standard's by the thinner coating (√(7.9/12.7),
/// −2.1 dB).
pub const VINTAGE: Formulation = Formulation {
    name: "Vintage",
    ms: 87.5e3,
    a: 4.85 * 0.0713 * 87.5e3,
    alpha: -0.0713,
    k: 310.0 * 1.545 * OERSTED,
    coercivity: 310.0,
    c: 0.20,
    coating: 7.9e-6,
    spread: 0.38,
    noise: 1077.0 / 6_760.8 * 0.789,
    mol: 380.0,
    reference: (185.0, 0.0075),
};

/// The formulations in [`super::id::TAPE`] order.
pub const FORMULATIONS: [Formulation; 3] = [STANDARD, HIGH_OUTPUT, VINTAGE];

/// One oersted in A/m.
pub const OERSTED: f64 = 1000.0 / (4.0 * std::f64::consts::PI);

impl Formulation {
    /// The short-circuit fluxivity (nWb/m) of a coating saturated to its
    /// remanence, `μ0 M_s δ` — the unit the model's magnetisation is
    /// counted in.
    pub fn saturation_flux(&self) -> f64 {
        MU0 * self.ms * self.coating * 1e9
    }

    /// The model's magnetisation (a share of saturation, peak) of a sine
    /// recorded at `nwb` nWb/m RMS.
    pub fn m_of_flux(&self, nwb: f64) -> f64 {
        nwb * std::f64::consts::SQRT_2 / self.saturation_flux()
    }
}

/// What the bias does at a current `beta` times the aligned one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bias {
    /// The share of the particles the bias switches (recorded
    /// anhysteretically).
    pub switched: f64,
    /// What still pins the rest, as a share of the tape's pinning: the
    /// bias carries them most of the way, and what holds them is what their
    /// switching fields lack of it.
    pub pinning: f64,
    /// The signal field at the recording point, relative to aligned.
    pub drive: f64,
    /// The recording zone's length, relative to aligned.
    pub zone: f64,
}

/// The bias field at the recording zone at alignment over the mean
/// switching field: 2.9 spreads above it, so the aligned bias reaches all
/// but 0.2 % of the particles (the bias at which the long-wavelength
/// output peaks lies just under it, as alignment practice has it).
fn reach(spread: f64) -> f64 {
    1.0 + 2.9 * spread
}

impl Bias {
    /// At `beta` times the aligned bias current on `f`.
    ///
    /// * The share switched is the normal distribution of the switching
    ///   fields (spread [`Formulation::spread`]) below the bias field; the
    ///   rest are pinned by the mean of what they lack (the distribution's
    ///   tail beyond the bias: `1 − t + σ φ(z)/(1 − Φ(z))`) — small just
    ///   under the alignment, so the signal itself switches them back and
    ///   forth across the threshold: the crossover-like distortion of an
    ///   under-biased machine at ordinary levels.
    /// * Over-bias moves the recording point away from the gap where both
    ///   fields fall off together; the deeper recording partly makes up
    ///   for it: the drive goes as `β^(−1/4)` (−0.75 dB at 3 dB over-bias,
    ///   the long-wavelength loss of published bias curves).
    /// * The zone grows as `β²`: with the 4 µm aligned zone the 10 kHz
    ///   sensitivity peaks under the aligned bias by about the over-bias
    ///   of alignment practice (the ATR Master Tape sheet's table: 4, 2–3
    ///   and 1–1.5 dB at 7.5, 15 and 30 ips; 456: 3 dB at 15 ips).
    pub fn at(beta: f64, f: &Formulation) -> Self {
        let beta = beta.clamp(0.05, 8.0);
        let s = f.spread.max(1e-3);
        let t = beta * reach(s);
        let z = (t - 1.0) / s;
        let tail = (1.0 - normal_cdf(z)).max(1e-300);
        let density = (-0.5 * z * z).exp() / (2.0 * std::f64::consts::PI).sqrt();
        Self {
            switched: normal_cdf(z),
            pinning: (1.0 - t + s * density / tail).clamp(1e-3, 1.0),
            drive: beta.powf(-0.25),
            zone: beta * beta,
        }
    }
}

/// The recording zone's length at the aligned bias (m): the bias's
/// erasure and the self-demagnetisation of short wavelengths together,
/// set so the 10 kHz saturation lies about 3, 5 and 14 dB under the 1 kHz
/// one at 30, 15 and 7.5 ips (NAB), as tape data sheets' SOL and MOL
/// figures part.
pub const ZONE: f64 = 4.0e-6;

/// The standard normal distribution function (Abramowitz & Stegun
/// 7.1.26, error under 1.5·10⁻⁷).
pub fn normal_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// `m = tanh((h + αm)/a)` for `m` by Newton from `guess`, inside a
/// bracket (the residual falls with `m`: one root).
fn solve(inv_a: f64, alpha: f64, h: f64, guess: f64) -> f64 {
    let (mut lo, mut hi) = (-1.0f64, 1.0f64);
    let mut m = guess.clamp(-1.0, 1.0);
    for _ in 0..60 {
        let t = ((h + alpha * m) * inv_a).tanh();
        let r = t - m;
        if r.abs() < 1e-15 {
            break;
        }
        if r > 0.0 {
            lo = m;
        } else {
            hi = m;
        }
        let slope = -1.0 + alpha * inv_a * (1.0 - t * t);
        let next = m - r / slope;
        m = if next > lo && next < hi {
            next
        } else {
            0.5 * (lo + hi)
        };
        if hi - lo < 1e-15 {
            break;
        }
    }
    m
}

/// A formulation's anhysteretic, tabulated (it is odd: `h ≥ 0` only) with
/// its slope for cubic Hermite interpolation — within 10⁻⁷ of the solve,
/// for a few nanoseconds instead of a Newton search per sample.
pub struct Table {
    value: Vec<f64>,
    slope: Vec<f64>,
}

/// The table's step and its end (fields as `H/M_s`; beyond, the solve).
const TABLE_STEP: f64 = 1.0 / 256.0;
const TABLE_TOP: f64 = 8.0;

impl Table {
    fn new(f: &Formulation) -> Self {
        let (inv_a, alpha) = (f.ms / f.a, f.alpha);
        let n = (TABLE_TOP / TABLE_STEP) as usize + 2;
        let mut value = Vec::with_capacity(n);
        let mut slope = Vec::with_capacity(n);
        let mut m = 0.0;
        for i in 0..n {
            let h = i as f64 * TABLE_STEP;
            m = solve(inv_a, alpha, h, m);
            let t = inv_a * (1.0 - m * m);
            value.push(m);
            // dm/dh of the self-consistent curve, per table step.
            slope.push(t / (1.0 - alpha * t) * TABLE_STEP);
        }
        Self { value, slope }
    }

    #[inline]
    fn at(&self, h: f64) -> Option<f64> {
        let x = h.abs() / TABLE_STEP;
        let i = x as usize;
        if i + 1 >= self.value.len() {
            return None;
        }
        let t = x - i as f64;
        let (p0, p1) = (self.value[i], self.value[i + 1]);
        let (m0, m1) = (self.slope[i], self.slope[i + 1]);
        let t2 = t * t;
        let t3 = t2 * t;
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * p1
            + (t3 - t2) * m1;
        Some(if h < 0.0 { -y } else { y })
    }
}

/// The formulations' tables (built on first use: the processor's maker
/// builds them, never the audio thread).
pub fn tables() -> &'static [Table; 3] {
    static TABLES: std::sync::OnceLock<[Table; 3]> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| FORMULATIONS.map(|f| Table::new(&f)))
}

/// The model's constants for one formulation at one bias, normalised to
/// `M_s` (fields as `H/M_s`, magnetisation as `M/M_s`).
#[derive(Clone, Copy)]
pub struct Media {
    inv_a: f64,
    alpha: f64,
    k: f64,
    table: Option<&'static Table>,
    /// The weights of the anhysteretic (`w + (1 − w)c`) and of the pinned
    /// irreversible part (`(1 − w)(1 − c)`) in the magnetisation.
    g_an: f64,
    g_irr: f64,
}

impl Media {
    /// `f` with the share `switched` of it recorded anhysteretically and
    /// the rest pinned by `pinning` of its pinning.
    pub fn new(f: &Formulation, switched: f64, pinning: f64) -> Self {
        let w = switched.clamp(0.0, 1.0);
        // Under 0.3 % left pinned (the aligned bias leaves 0.2 %, some 55 dB
        // down): all of it anhysteretic, and no loop to follow.
        let w = if (1.0 - w) * (1.0 - f.c) < 3e-3 {
            1.0
        } else {
            w
        };
        Self {
            inv_a: f.ms / f.a,
            alpha: f.alpha,
            k: f.k / f.ms * pinning.clamp(1e-6, 1.0),
            table: FORMULATIONS
                .iter()
                .position(|x| x == f)
                .map(|i| &tables()[i]),
            g_an: w + (1.0 - w) * f.c,
            g_irr: (1.0 - w) * (1.0 - f.c),
        }
    }

    /// Without bias: the whole tape on its hysteresis loop.
    pub fn unbiased(f: &Formulation) -> Self {
        Self::new(f, 0.0, 1.0)
    }

    /// At a bias.
    pub fn biased(f: &Formulation, b: &Bias) -> Self {
        Self::new(f, b.switched, b.pinning)
    }

    /// The anhysteretic at the field `h`: the `m` that solves
    /// `m = tanh((h + αm)/a)` — from the table, or by Newton from `guess`.
    #[inline]
    pub fn anhysteretic_from(&self, h: f64, guess: f64) -> f64 {
        match self.table.and_then(|t| t.at(h)) {
            Some(m) => m,
            None => solve(self.inv_a, self.alpha, h, guess),
        }
    }

    /// The static (fully biased) transfer.
    pub fn transfer(&self, h: f64) -> f64 {
        self.anhysteretic_from(h, (h * self.inv_a).tanh() * 0.5)
    }

    /// The same, solved (no table).
    pub fn transfer_solved(&self, h: f64) -> f64 {
        solve(self.inv_a, self.alpha, h, (h * self.inv_a).tanh() * 0.5)
    }

    /// The pinning (`k/M_s`).
    pub fn pinning(&self) -> f64 {
        self.k
    }
}

/// One channel's tape magnetisation: the Jiles–Atherton state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Magnet {
    /// Magnetisation (share of `M_s`).
    pub m: f64,
    /// The irreversible magnetisation of the population the bias does not
    /// reach.
    m_irr: f64,
    /// The field and the anhysteretic at the last step.
    h: f64,
    man: f64,
}

impl Magnet {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The irreversible magnetisation after a path of `ds` (direction
    /// `dir`) on which the anhysteretic target moves linearly from `a0` to
    /// `a1`, pinning `k`: `dM_irr/ds = (target − M_irr)/k` while the target
    /// is on the side the field moves to, integrated exactly; the target may
    /// cross `M_irr` within the step (a reversal's pinned stretch ends
    /// there).
    #[inline]
    fn irreversible(mi: f64, a0: f64, a1: f64, ds: f64, dir: f64, k: f64) -> f64 {
        if ds <= 1e-300 {
            return mi;
        }
        let g0 = dir * (a0 - mi);
        let g1 = dir * (a1 - mi);
        if g1 <= 0.0 && g0 <= 0.0 {
            return mi;
        }
        let b = (a1 - a0) / ds;
        let (s0, start) = if g0 > 0.0 {
            (0.0, a0)
        } else {
            // Pinned until the target reaches M_irr.
            let s = if b != 0.0 { (mi - a0) / b } else { 0.0 };
            (s.clamp(0.0, ds), mi)
        };
        let x = (ds - s0) / k.max(1e-300);
        let e = if x > 40.0 { 0.0 } else { (-x).exp() };
        a1 - b * k + (mi - start + b * k) * e
    }

    /// Advance to the field `h` (share of `M_s`): the new magnetisation.
    #[inline]
    pub fn step(&mut self, h: f64, p: &Media) -> f64 {
        let man = p.anhysteretic_from(h, self.man);
        if p.g_irr > 0.0 {
            // Split the step where the target moves far within it, so it is
            // followed along its curve and not along a chord.
            let jump = (man - self.man).abs();
            let parts = if jump > 0.1 {
                ((jump / 0.1).ceil() as usize).min(16)
            } else {
                1
            };
            let (h0, mut a0) = (self.h, self.man);
            let mut mi = self.m_irr;
            let mut hp = h0;
            for i in 1..=parts {
                let (hi, ai) = if i == parts {
                    (h, man)
                } else {
                    let hi = h0 + (h - h0) * i as f64 / parts as f64;
                    (hi, p.anhysteretic_from(hi, a0))
                };
                let d = hi - hp;
                mi = Self::irreversible(mi, a0, ai, d.abs(), d.signum(), p.k);
                a0 = ai;
                hp = hi;
            }
            self.m_irr = mi;
        } else {
            self.m_irr = man;
        }
        self.h = h;
        self.man = man;
        self.m = p.g_an * man + p.g_irr * self.m_irr;
        self.m
    }
}

/// The fundamental (peak) of the static transfer driven by a sine of
/// amplitude `amp` (share of `M_s`).
pub fn fundamental(media: &Media, amp: f64) -> f64 {
    const N: usize = 64;
    let mut b1 = 0.0;
    for n in 0..N {
        let s = (std::f64::consts::TAU * (n as f64 + 0.5) / N as f64).sin();
        b1 += media.transfer(amp * s) * s;
    }
    2.0 * b1 / N as f64
}

/// The drive amplitude (share of `M_s`) whose recording has a fundamental
/// of `m` (fully biased).
pub fn amplitude_for(media: &Media, m: f64) -> f64 {
    let (mut lo, mut hi) = (1e-9f64, 1e3f64);
    for _ in 0..60 {
        let mid = (lo * hi).sqrt();
        if fundamental(media, mid) < m {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo * hi).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dM/dH` of the Jiles–Atherton equations as usually integrated
    /// (normalised; one population, the pinned one, `δ_M` switching).
    fn slope(p: &Media, c: f64, m: f64, h: f64, dir: f64) -> f64 {
        let man = p.transfer(h);
        // dM_an/dH of the sheared anhysteretic.
        let t = (1.0 - man * man) * p.inv_a;
        let dman = t / (1.0 - p.alpha * t);
        let mirr = (m - c * man) / (1.0 - c);
        let x = man - mirr;
        let irr = if dir * x > 0.0 { x / (dir * p.k) } else { 0.0 };
        (1.0 - c) * irr + c * dman
    }

    /// One RK4 step from `h0` to `h1` (the field linear in between).
    fn rk4(p: &Media, c: f64, m: f64, h0: f64, h1: f64) -> f64 {
        let dh = h1 - h0;
        let dir = dh.signum();
        let k1 = slope(p, c, m, h0, dir);
        let k2 = slope(p, c, m + 0.5 * dh * k1, h0 + 0.5 * dh, dir);
        let k3 = slope(p, c, m + 0.5 * dh * k2, h0 + 0.5 * dh, dir);
        let k4 = slope(p, c, m + dh * k3, h1, dir);
        m + dh / 6.0 * (k1 + 2.0 * k2 + 2.0 * k3 + k4)
    }

    /// A sine of `amp` (share of M_s), `steps` per period, three periods:
    /// the last period by each solver, and the reference (the exponential
    /// integrator at 256 sub-steps).
    fn runs(p: &Media, c: f64, amp: f64, steps: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let h = |i: f64| amp * (std::f64::consts::TAU * i / steps as f64).sin();
        let (mut exact, mut ours, mut rk) = (Magnet::default(), Magnet::default(), 0.0f64);
        let (mut a, mut b, mut r) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..3 * steps {
            let (h0, h1) = (h(i as f64), h(i as f64 + 1.0));
            for s in 1..=256 {
                exact.step(h(i as f64 + s as f64 / 256.0), p);
            }
            ours.step(h1, p);
            rk = rk4(p, c, rk, h0, h1);
            if i >= 2 * steps {
                a.push(exact.m);
                b.push(ours.m);
                r.push(rk);
            }
        }
        (a, b, r)
    }

    fn worst(a: &[f64], b: &[f64]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| {
                if y.is_finite() {
                    (x - y).abs()
                } else {
                    f64::INFINITY
                }
            })
            .fold(0.0, f64::max)
    }

    /// Why the exponential integrator: on the pinned population (the loop a
    /// signal draws without bias) both it and RK4 converge at a fine step;
    /// at a coarse step — a hot 10 kHz at 2× — RK4's error grows to a
    /// sizeable share of saturation while ours stays near the reference;
    /// and once the bias has taken the pinning away (`k → 0`, the equation
    /// infinitely stiff) RK4 is unstable at any step this rate gives,
    /// while ours lands on the anhysteretic exactly.
    #[test]
    fn the_exponential_integrator_is_exact_where_rk4_is_unstable() {
        let f = STANDARD;
        let pinned = Media::unbiased(&f);
        // 1 kHz at 192 kHz (192 steps a period), 2.5 coercivities: both fine.
        let (exact, ours, rk) = runs(&pinned, f.c, 0.6, 192);
        let (e_ours, e_rk) = (worst(&exact, &ours), worst(&exact, &rk));
        println!("fine: ours {e_ours:.2e}, rk4 {e_rk:.2e}");
        assert!(e_ours < 2e-3, "ours {e_ours}");
        assert!(e_rk < 2e-2, "rk4 {e_rk}");
        // 10 kHz at 96 kHz (9.6 steps a period), hot.
        let (exact, ours, rk) = runs(&pinned, f.c, 1.5, 10);
        let (e_ours, e_rk) = (worst(&exact, &ours), worst(&exact, &rk));
        println!("coarse: ours {e_ours:.2e}, rk4 {e_rk:.2e}");
        assert!(e_ours < 0.02, "ours {e_ours}");
        assert!(e_rk > 2.0 * e_ours, "rk4 {e_rk} vs ours {e_ours}");
        // The bias takes the pinning away: k a thousandth of the tape's.
        let mut free = pinned;
        free.k = pinned.k * 1e-3;
        let (exact, ours, rk) = runs(&free, f.c, 0.6, 40);
        let (e_ours, e_rk) = (worst(&exact, &ours), worst(&exact, &rk));
        println!("stiff: ours {e_ours:.2e}, rk4 {e_rk:.2e}");
        assert!(e_ours < 2e-3, "ours {e_ours}");
        assert!(e_rk.is_nan() || e_rk > 0.1, "rk4 should fail here: {e_rk}");
        // And it is the anhysteretic.
        let mut m = Magnet::default();
        for i in 0..400 {
            let h = 0.6 * (i as f64 * 0.05).sin();
            let got = m.step(h, &free);
            assert!((got - free.transfer(h)).abs() < 0.01, "{i}: {got}");
        }
    }

    /// Without bias the tape draws its hysteresis loop: saturation near
    /// `M_s`, a coercivity near the formulation's, remanence between them.
    #[test]
    fn each_tape_draws_its_loop() {
        for f in FORMULATIONS {
            let p = Media::unbiased(&f);
            let k = f.k / f.ms;
            let top = 6.0 * k;
            let mut m = Magnet::default();
            let n = 4000;
            let (mut coercive, mut remanent, mut most) = (f64::NAN, f64::NAN, 0.0f64);
            let mut last = (0.0, 0.0);
            for i in 0..3 * n {
                // A triangle ±top.
                let t = (i % n) as f64 / n as f64;
                let h = top
                    * if t < 0.25 {
                        4.0 * t
                    } else if t < 0.75 {
                        2.0 - 4.0 * t
                    } else {
                        4.0 * t - 4.0
                    };
                let y = m.step(h, &p);
                if i >= 2 * n && (0.25..0.75).contains(&t) {
                    // The descending branch.
                    if last.1 > 0.0 && y <= 0.0 {
                        coercive = -(last.0 + (h - last.0) * last.1 / (last.1 - y));
                    }
                    if last.0 > 0.0 && h <= 0.0 {
                        remanent = y;
                    }
                }
                most = most.max(y);
                last = (h, y);
            }
            let hc = coercive * f.ms / OERSTED;
            let hc_spec = f.coercivity;
            println!(
                "{}: saturation {most:.3} M_s, coercivity {hc:.0} Oe (spec {hc_spec:.0}), squareness {remanent:.2}",
                f.name
            );
            assert!(most > 0.9, "{}: {most}", f.name);
            assert!(
                (hc / hc_spec - 1.0).abs() < 0.3,
                "{}: {hc} Oe for {hc_spec}",
                f.name
            );
            assert!((0.4..0.98).contains(&remanent), "{}: {remanent}", f.name);
        }
    }

    #[test]
    fn the_bias_switches_everything_at_alignment_and_less_under_it() {
        for f in FORMULATIONS {
            let aligned = Bias::at(1.0, &f);
            assert!(aligned.switched > 0.995, "{}", f.name);
            assert!(Bias::at(2.0, &f).switched > aligned.switched);
            let under = Bias::at(0.5, &f);
            assert!(under.switched < 0.6, "{}: {}", f.name, under.switched);
            assert!(Bias::at(0.25, &f).switched < 0.15, "{}", f.name);
            assert!(under.drive > 1.0 && Bias::at(2.0, &f).drive < 1.0);
            assert!(Bias::at(1.41, &f).zone > 1.9);
            // Just under the alignment what still pins the rest is little.
            assert!(Bias::at(0.7, &f).pinning < 0.4, "{}", f.name);
            assert!(Bias::at(0.05, &f).pinning > 0.85, "{}", f.name);
        }
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        // The tables hold the solve.
        for f in FORMULATIONS {
            let p = Media::new(&f, 1.0, 1.0);
            let mut worst = 0.0f64;
            for i in -4000..4000 {
                let h = i as f64 * 0.002 + 0.000_37;
                worst = worst.max((p.transfer(h) - p.transfer_solved(h)).abs());
            }
            assert!(worst < 1e-7, "{}: {worst}", f.name);
        }
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-4);
    }
}
