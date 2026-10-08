//! Console colour for every channel: what a channel picks up going through
//! the console's line amplifier, for all of a mix's channels at once. The
//! buses run the circuits themselves (`circuits::console_bus`, solved like
//! the preamps); a channel stage is each bus circuit measured and baked
//! ([`MODELS`], `tests/console_bake.rs`) — its small-signal response as a
//! high-pass, a low-pass, a bell and a high shelf, and its transfer curves
//! at levels from −24 to +12 dBFS, each relative to its level (the circuit
//! clean up to its rail and then clipping, or an AC-coupled class-A stage
//! whose operating point moves so it clips near the peak at any level, as
//! the circuit has them) — and held against the circuit by
//! `tests/console_match.rs`. It runs at the host rate with no latency, a
//! few dozen operations a sample.
//!
//! Levels are the buses': −18 dBFS is +4 dBu at the line, full scale +22
//! dBu. The drive raises the level into the stage and lowers it after, so
//! it changes the colour, not the level.

include!("console_models.rs");

/// A model measured on its circuit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConsoleModel {
    pub name: &'static str,
    /// The high-pass (coupling and transformer): corner and Q; 0 = none.
    pub hp_hz: f64,
    pub hp_q: f64,
    /// A first-order high-pass after it (a valve stage's further coupling);
    /// 0 = none.
    pub hp1_hz: f64,
    /// The low-pass (bandwidth); 0 = none.
    pub lp_hz: f64,
    /// Its Q when second order (an LC filter's); 0 = first order.
    pub lp_q: f64,
    /// A bell the response leaves after them: centre, gain (dB), Q.
    pub bell: (f64, f64, f64),
    /// A high shelf for the top octave: corner and gain (dB).
    pub shelf: (f64, f64),
    /// Where the emphasis bands turn (Hz): a low band below the first, a
    /// high band above the second (one pole each).
    pub emphasis: (f64, f64),
    /// For each of [`LEVELS`] (the level coming in): the low and the high
    /// band's gain into the curve (undone exactly after it) and a
    /// correction after (linear: low in, high in, low after, high after).
    /// A stage's distortion and compression change with the frequency
    /// differently at each level — the British 73's class-A stage, its
    /// transformer the collector load, clips later in the lows and
    /// compresses harder there when hot — so the bands are fitted to the
    /// circuit per level; at small levels they are 1.
    pub bands: &'static [[f32; 4]; LEVEL_COUNT],
    /// The transfer curves: for each of [`LEVELS`] (a signal's peak, full
    /// scale) [`TABLE_POINTS`] values of the output over the level (unity
    /// at small levels) for inputs over the level evenly from −1 to +1 — a
    /// stage whose operating point moves with the level (a class-A stage,
    /// AC-coupled) meets a different curve at each, and relative to the
    /// level neighbouring curves have their knees in nearly the same place,
    /// so mixing them moves the knee instead of making two.
    pub tables: &'static [[f32; TABLE_POINTS]; LEVEL_COUNT],
}

/// The peaks the curves are measured at (full scale): 0.06 to 4.2, about
/// 1.5 dB apart.
pub const LEVELS: [f64; LEVEL_COUNT] = [
    0.06, 0.07162, 0.08549, 0.102, 0.1218, 0.1454, 0.1736, 0.2072, 0.2473, 0.2952, 0.3523, 0.4206,
    0.502, 0.5992, 0.7153, 0.8538, 1.019, 1.216, 1.452, 1.733, 2.069, 2.47, 2.948, 3.519, 4.2,
];
pub const LEVEL_COUNT: usize = 25;

/// How long the level a stage follows takes to fall (s): about what the
/// circuits' coupling takes to move their operating point.
const LEVEL_RELEASE: f64 = 0.2;

/// How long a peak holds before the level falls (s): a period of 40 Hz, so
/// the level does not move inside a low note's period (which would add
/// distortion of its own).
const LEVEL_HOLD: f64 = 0.025;

/// Where `l` falls among [`LEVELS`]: the upper index (1…) and the weight of
/// that level against the one below (in log).
#[inline]
fn level_at(l: f64) -> (usize, f64) {
    // The levels are even in log (to four places): the position is a
    // logarithm away.
    let x =
        ((l.max(LEVELS[0]) / LEVELS[0]).ln() * LEVEL_STEPS_PER_LN).min((LEVEL_COUNT - 1) as f64);
    let i = (x as usize + 1).min(LEVEL_COUNT - 1);
    (i, (x - (i - 1) as f64).clamp(0.0, 1.0))
}

/// How often (samples) the bands' gains follow the level coming in.
const BAND_EVERY: u32 = 16;

/// Level steps per unit of log: 24 steps from 0.06 to 4.2.
const LEVEL_STEPS_PER_LN: f64 = 24.0 / 4.248_495_242_049_359; // ln(70)

/// A level follower: the peak, held a while, then falling.
#[derive(Clone, Copy, Debug, Default)]
struct Follower {
    level: f64,
    hold: u32,
    hold_for: u32,
    fall: f64,
}

impl Follower {
    fn new(rate: f64) -> Self {
        Self {
            level: 0.0,
            hold: 0,
            hold_for: (LEVEL_HOLD * rate) as u32,
            fall: (-1.0 / (LEVEL_RELEASE * rate)).exp(),
        }
    }

    #[inline]
    fn follow(&mut self, peak: f64) -> f64 {
        if peak >= self.level {
            self.level = peak;
            self.hold = self.hold_for;
        } else if self.hold > 0 {
            self.hold -= 1;
        } else {
            self.level = peak + (self.level - peak) * self.fall;
        }
        self.level
    }
}

/// The points of a curve, from −1 to +1 times its level.
pub const TABLE_POINTS: usize = 257;

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn identity() -> Self {
        Self {
            b: [1.0, 0.0, 0.0],
            ..Self::default()
        }
    }

    /// RBJ's cookbook: a high-pass at `hz`, `q`.
    fn high_pass(hz: f64, q: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * hz / rate;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }

    /// A high shelf (RBJ, slope 1).
    fn high_shelf(hz: f64, db: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * hz / rate;
        let (s, c) = w.sin_cos();
        let a = 10f64.powf(db / 40.0);
        let alpha = s / 2.0 * std::f64::consts::SQRT_2;
        let k = 2.0 * a.sqrt() * alpha;
        let a0 = (a + 1.0) - (a - 1.0) * c + k;
        Self {
            b: [
                a * ((a + 1.0) + (a - 1.0) * c + k) / a0,
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c) / a0,
                a * ((a + 1.0) + (a - 1.0) * c - k) / a0,
            ],
            a: [
                2.0 * ((a - 1.0) - (a + 1.0) * c) / a0,
                ((a + 1.0) - (a - 1.0) * c - k) / a0,
            ],
            z: [0.0; 2],
        }
    }

    /// A peaking bell.
    fn bell(hz: f64, db: f64, q: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * hz / rate;
        let (s, c) = w.sin_cos();
        let a = 10f64.powf(db / 40.0);
        let alpha = s / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha / a;
        Self {
            b: [
                (1.0 + alpha * a) / a0,
                -2.0 * c / a0,
                (1.0 - alpha * a) / a0,
            ],
            a: [-2.0 * c / a0, (1.0 - alpha / a) / a0],
            z: [0.0; 2],
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    fn reset(&mut self) {
        self.z = [0.0; 2];
    }

    fn flush(&mut self) {
        for z in &mut self.z {
            if z.abs() < 1e-20 {
                *z = 0.0;
            }
        }
    }
}

/// A one-pole low-pass.
#[derive(Clone, Copy, Debug, Default)]
struct OnePole {
    b: [f64; 2],
    p: f64,
    z: f64,
}

impl OnePole {
    /// An analog one-pole low-pass at `hz`, matched: the pole where the
    /// analog one maps, the zero placed so the magnitude is the analog
    /// one's at DC and at 0.4 of the rate (19.2 kHz at 48 kHz) — right up
    /// to the top of the band, even for a corner above Nyquist (a valve
    /// stage's roll-off near 30 kHz still takes a dB at 20 kHz).
    fn new(hz: f64, rate: f64) -> Self {
        let w = std::f64::consts::TAU * hz / rate;
        let p = (-w).exp();
        let wm = 0.8 * std::f64::consts::PI;
        // |H|² the analog has there, times the pole's |1 − p e^{−jw}|².
        let target = 1.0 / (1.0 + (wm / w).powi(2)) * (1.0 - 2.0 * p * wm.cos() + p * p);
        // b0 + b1 = 1 − p (unity at DC); b0² + b1² + 2 b0 b1 cos wm = target.
        let sum = 1.0 - p;
        let product = ((sum * sum - target) / (2.0 * (1.0 - wm.cos()))).min(sum * sum / 4.0);
        let root = (sum * sum / 4.0 - product).max(0.0).sqrt();
        Self {
            b: [0.5 * sum + root, 0.5 * sum - root],
            p,
            z: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        // Transposed direct form: y = b0·x + z, z = b1·x + p·y.
        let y = self.b[0] * x + self.z;
        self.z = self.b[1] * x + self.p * y;
        y
    }
}

/// An analog second-order low-pass (`f0`, `q`), matched: the poles where
/// the analog ones map (impulse invariance), the zeros so the magnitude is
/// the analog one's at DC, at the corner (a quarter of the rate when the
/// corner is near or above Nyquist) and at Nyquist (after
/// Vicanek, "Matched Second Order Digital Filters", 2016) — right to the
/// top of the band, corners above Nyquist included, where the bilinear
/// transform would bend it.
fn matched_low_pass(hz: f64, q: f64, rate: f64) -> Biquad {
    let w0 = std::f64::consts::TAU * hz / rate;
    let zeta = 0.5 / q.max(0.05);
    let decay = (-zeta * w0).exp();
    let a1 = if zeta <= 1.0 {
        -2.0 * decay * ((1.0 - zeta * zeta).sqrt() * w0).cos()
    } else {
        -2.0 * decay * ((zeta * zeta - 1.0).sqrt() * w0).cosh()
    };
    let a2 = decay * decay;
    // The analog magnitude squared at a frequency (Hz).
    let analog = |f: f64| {
        let x = f / hz;
        1.0 / ((1.0 - x * x).powi(2) + (x / q).powi(2))
    };
    // What the numerator's magnitude squared must be there: the analog's
    // times the denominator's.
    let denominator = |w: f64| {
        let (c, c2) = (w.cos(), (2.0 * w).cos());
        let (sn, s2) = (w.sin(), (2.0 * w).sin());
        (1.0 + a1 * c + a2 * c2).powi(2) + (a1 * sn + a2 * s2).powi(2)
    };
    let dc = analog(0.0) * denominator(0.0);
    let nyquist = analog(rate / 2.0) * denominator(std::f64::consts::PI);
    let wm = if w0 < 0.9 * std::f64::consts::PI {
        w0
    } else {
        0.5 * std::f64::consts::PI
    };
    let middle = analog(wm * rate / std::f64::consts::TAU) * denominator(wm);
    // B(z) = b0 + b1 z⁻¹ + b2 z⁻²: |B(1)| = p + b1, |B(−1)| = p − b1 with
    // p = b0 + b2; with m = b0 − b2, |B|² at w is
    // b1² + p² (1 + cos 2w) / 2 + 2 b1 p cos w + m² (1 − cos 2w) / 2.
    let (r0, r1) = (dc.sqrt(), nyquist.sqrt());
    let p = 0.5 * (r0 + r1);
    let b1 = 0.5 * (r0 - r1);
    let (c, c2) = (wm.cos(), (2.0 * wm).cos());
    let m = (2.0 * (middle - b1 * b1 - 2.0 * b1 * p * c - 0.5 * p * p * (1.0 + c2)) / (1.0 - c2))
        .max(0.0)
        .sqrt();
    Biquad {
        b: [0.5 * (p + m), b1, 0.5 * (p - m)],
        a: [a1, a2],
        z: [0.0; 2],
    }
}

/// An emphasis band: `x + a·H(x)` with `H` a one-pole low-pass (or the
/// high-pass it leaves), and its exact inverse (solved for the delay-free
/// part), so a gain into the curve comes off exactly after it.
#[derive(Clone, Copy, Debug, Default)]
struct Band {
    k: f64,
    high: bool,
    s: f64,
}

impl Band {
    fn new(hz: f64, rate: f64, high: bool) -> Self {
        Self {
            k: 1.0 - (-std::f64::consts::TAU * hz / rate).exp(),
            high,
            s: 0.0,
        }
    }

    /// `x` with the band at `g` (linear).
    #[inline]
    fn apply(&mut self, x: f64, g: f64) -> f64 {
        self.s += self.k * (x - self.s);
        let part = if self.high { x - self.s } else { self.s };
        x + (g - 1.0) * part
    }

    /// The `z` that [`Self::apply`] at `g` turns into `y` (this band's own
    /// state following `z`).
    #[inline]
    fn undo(&mut self, y: f64, g: f64) -> f64 {
        let a = g - 1.0;
        let prev = self.s;
        // The band's part of z is d·z + e (prev: the low-pass's state).
        let (d, e) = if self.high {
            (1.0 - self.k, -(1.0 - self.k) * prev)
        } else {
            (self.k, (1.0 - self.k) * prev)
        };
        let z = (y - a * e) / (1.0 + a * d);
        self.s = prev + self.k * (z - prev);
        z
    }
}

/// One channel's console stage.
#[derive(Clone, Debug)]
pub struct ConsoleStage {
    model: ConsoleModel,
    drive: f64,
    hp: Biquad,
    bell: Biquad,
    shelf: Biquad,
    lp: Option<OnePole>,
    lp2: Option<Biquad>,
    /// The further high-pass, as the low-pass it takes away.
    hp1: Option<OnePole>,
    /// The emphasis bands' gains per level (see [`ConsoleModel::bands`]).
    bands: [[f64; 4]; LEVEL_COUNT],
    /// The bands into the curve, undone after it, and the corrections
    /// after (low, high each).
    pre: [Band; 2],
    undo: [Band; 2],
    after: [Band; 2],
    /// The level coming in (for the bands) and into the curve.
    input: Follower,
    level: Follower,
    /// The bands' gains now, and the samples until they are looked up
    /// again (they follow the level coming in, which moves slowly).
    gains: [f64; 4],
    gains_in: u32,
    dc: (f64, f64),
    dc_r: f64,
}

impl ConsoleStage {
    /// The stage for `model` (an index into [`MODELS`]) at `drive_db`.
    pub fn new(model: usize, drive_db: f64, rate: f64) -> Self {
        Self::with_model(MODELS[model.min(MODELS.len() - 1)], drive_db, rate)
    }

    /// The stage for a model of its own (fitting one).
    pub fn with_model(m: ConsoleModel, drive_db: f64, rate: f64) -> Self {
        let rate = rate.max(1.0);
        let nyquist = rate * 0.45;
        let hp = if m.hp_hz > 0.0 {
            Biquad::high_pass(m.hp_hz.min(nyquist), m.hp_q, rate)
        } else {
            Biquad::identity()
        };
        let bell = if m.bell.1.abs() > 0.05 && m.bell.0 < nyquist {
            Biquad::bell(m.bell.0, m.bell.1, m.bell.2, rate)
        } else {
            Biquad::identity()
        };
        let shelf = if m.shelf.1.abs() > 0.05 && m.shelf.0 < nyquist {
            Biquad::high_shelf(m.shelf.0, m.shelf.1, rate)
        } else {
            Biquad::identity()
        };
        let second = m.lp_q > 0.0;
        let lp = (m.lp_hz > 0.0 && m.lp_hz < 100.0 * rate && !second)
            .then(|| OnePole::new(m.lp_hz, rate));
        let lp2 = (m.lp_hz > 0.0 && m.lp_hz < 100.0 * rate && second)
            .then(|| matched_low_pass(m.lp_hz, m.lp_q, rate));
        let hp1 = (m.hp1_hz > 0.0).then(|| OnePole::new(m.hp1_hz, rate));
        let (fl, fh) = m.emphasis;
        let band = || {
            [
                Band::new(fl.clamp(1.0, nyquist), rate, false),
                Band::new(fh.clamp(1.0, nyquist), rate, true),
            ]
        };
        Self {
            model: m,
            drive: 10f64.powf(drive_db.clamp(-24.0, 24.0) / 20.0),
            hp,
            bell,
            shelf,
            lp,
            lp2,
            hp1,
            bands: std::array::from_fn(|i| std::array::from_fn(|k| f64::from(m.bands[i][k]))),
            pre: band(),
            undo: band(),
            after: band(),
            input: Follower::new(rate),
            level: Follower::new(rate),
            gains: [1.0; 4],
            gains_in: 0,
            dc: (0.0, 0.0),
            dc_r: (-std::f64::consts::TAU * 5.0 / rate).exp(),
        }
    }

    /// The drive (linear; cheap enough to ramp sample by sample).
    #[inline]
    pub fn set_drive(&mut self, drive: f64) {
        self.drive = drive.clamp(0.063, 16.0);
    }

    /// Other band gains (fitting a model).
    pub fn set_bands(&mut self, bands: &[[f64; 4]; LEVEL_COUNT]) {
        self.bands = *bands;
    }

    /// The curve at `u` for a signal peaking at `level` (never below
    /// `|u|`): the two curves either side, mixed in log, at `u` over the
    /// level. Past the top level the top curve stays where it is (its ends
    /// are the rails).
    #[inline]
    fn curve_at(&self, u: f64, level: f64) -> f64 {
        let l = level.min(LEVELS[LEVEL_COUNT - 1]);
        if l < 1e-9 {
            return u;
        }
        let v = u / l;
        let (i, w) = level_at(l);
        let t = &self.model.tables;
        let lo = Self::lookup(&t[i - 1], v);
        let y = if w <= 0.0 {
            lo
        } else {
            lo + (Self::lookup(&t[i], v) - lo) * w
        };
        y * l
    }

    /// The bands' gains at `level`.
    #[inline]
    fn bands_at(&self, level: f64) -> [f64; 4] {
        let (i, w) = level_at(level);
        let (a, b) = (&self.bands[i - 1], &self.bands[i]);
        std::array::from_fn(|k| a[k] + (b[k] - a[k]) * w)
    }

    /// A curve at `v` (−1…+1; Catmull-Rom between its points, flat past
    /// them).
    #[inline]
    fn lookup(t: &[f32; TABLE_POINTS], v: f64) -> f64 {
        let last = (TABLE_POINTS - 1) as f64;
        let x = ((v + 1.0) / 2.0 * last).clamp(0.0, last);
        let i = (x as usize).min(TABLE_POINTS - 2);
        let f = x - i as f64;
        let at = |k: isize| f64::from(t[(i as isize + k).clamp(0, last as isize) as usize]);
        let (p0, p1, p2, p3) = (at(-1), at(0), at(1), at(2));
        p1 + 0.5
            * f
            * (p2 - p0
                + f * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + f * (3.0 * (p1 - p2) + p3 - p0)))
    }

    pub fn name(&self) -> &'static str {
        self.model.name
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let x = if x.is_finite() { x } else { 0.0 };
        let x = self.hp.process(x);
        let x = self.bell.process(x);
        let x = self.shelf.process(x);
        let x = match &mut self.lp {
            Some(lp) => lp.process(x),
            None => x,
        };
        let x = match &mut self.lp2 {
            Some(lp) => lp.process(x),
            None => x,
        };
        let x = match &mut self.hp1 {
            Some(hp) => x - hp.process(x),
            None => x,
        };
        let x = x * self.drive;
        let coming = self.input.follow(x.abs());
        if self.gains_in == 0 {
            self.gains = self.bands_at(coming);
            self.gains_in = BAND_EVERY;
        }
        self.gains_in -= 1;
        let [low_in, high_in, low_after, high_after] = self.gains;
        let u = self.pre[0].apply(x, low_in);
        let u = self.pre[1].apply(u, high_in);
        let level = self.level.follow(u.abs());
        let y = self.curve_at(u, level);
        let y = self.undo[1].undo(y, high_in);
        let y = self.undo[0].undo(y, low_in);
        let y = self.after[0].apply(y, low_after);
        let y = self.after[1].apply(y, high_after) / self.drive;
        // No DC from the even harmonics.
        let out = y - self.dc.0 + self.dc_r * self.dc.1;
        self.dc = (y, out);
        out
    }

    pub fn reset(&mut self) {
        self.hp.reset();
        self.bell.reset();
        self.shelf.reset();
        for b in self
            .pre
            .iter_mut()
            .chain(&mut self.undo)
            .chain(&mut self.after)
        {
            b.s = 0.0;
        }
        for p in [&mut self.lp, &mut self.hp1].into_iter().flatten() {
            p.z = 0.0;
        }
        if let Some(b) = &mut self.lp2 {
            b.reset();
        }
        self.input.level = 0.0;
        self.level.level = 0.0;
        self.gains_in = 0;
        self.dc = (0.0, 0.0);
    }

    /// Denormals off the filters' state (once a block).
    pub fn flush(&mut self) {
        self.hp.flush();
        self.bell.flush();
        self.shelf.flush();
        if let Some(b) = &mut self.lp2 {
            b.flush();
        }
        for b in self
            .pre
            .iter_mut()
            .chain(&mut self.undo)
            .chain(&mut self.after)
        {
            if b.s.abs() < 1e-20 {
                b.s = 0.0;
            }
        }
        if self.dc.1.abs() < 1e-20 {
            self.dc.1 = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matched low-pass keeps the analog magnitude up to near Nyquist
    /// (where the bilinear transform would fall to nothing), corners above
    /// Nyquist included.
    #[test]
    fn the_matched_low_pass_follows_the_analog_one() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            for (f0, q) in [
                (15_000.0, 0.8),
                (22_000.0, 0.707),
                (30_000.0, 1.0),
                (8_000.0, 0.6),
            ] {
                let b = matched_low_pass(f0, q, rate);
                for f in [100.0, 1_000.0, 5_000.0, 10_000.0, 16_000.0, 20_000.0] {
                    let w = std::f64::consts::TAU * f / rate;
                    let z = |k: f64| (k * w).cos();
                    let s = |k: f64| (k * w).sin();
                    let num = ((b.b[0] + b.b[1] * z(1.0) + b.b[2] * z(2.0)).powi(2)
                        + (b.b[1] * s(1.0) + b.b[2] * s(2.0)).powi(2))
                    .sqrt();
                    let den = ((1.0 + b.a[0] * z(1.0) + b.a[1] * z(2.0)).powi(2)
                        + (b.a[0] * s(1.0) + b.a[1] * s(2.0)).powi(2))
                    .sqrt();
                    let digital = 20.0 * (num / den).log10();
                    let x = f / f0;
                    let analog = -10.0 * ((1.0 - x * x).powi(2) + (x / q).powi(2)).log10();
                    // Exact at DC, the corner and Nyquist; within 0.4 dB
                    // to 0.8 of Nyquist, a dB or so in the last of it.
                    // 0.4 dB or 8 % of the attenuation (0.6 for a corner
                    // at Nyquist), 1.5 dB in the band's last fifth.
                    let near = if f0 < 0.45 * rate { 0.4 } else { 0.6 };
                    let limit = if f < 0.4 * rate {
                        f64::max(near, 0.08 * analog.abs())
                    } else {
                        1.5
                    };
                    assert!(
                        (digital - analog).abs() < limit,
                        "{f0} Hz q {q} at {rate}: {f} Hz {digital:.2} vs {analog:.2} dB"
                    );
                }
            }
        }
    }
}
