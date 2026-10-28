//! The T4 electro-optical attenuator: an electroluminescent panel lighting
//! a cadmium-sulphide photoresistor, sealed together in a can. It has no
//! circuit model (nothing in it is a junction or a valve), so it is built
//! here from the physics of its two halves, each constant fitted to what
//! is published about real cells (see [`super`]'s notes for the sources).
//!
//! * **The panel** ([`Panel`]) is a capacitor with a phosphor in its
//!   dielectric. Light comes from the charge that moves through the
//!   phosphor while the field across it is high: as the voltage sweeps
//!   from nothing to `u` it moves (and the phosphor turns into light)
//!   `G(u) = exp(−b/√u)` — the Alfrey–Taylor law, the measured brightness
//!   of powder EL against voltage. So a slow sine of amplitude `A` and
//!   frequency `f` emits `4 f G(A)` a second: brighter with level (steeply,
//!   past a threshold-like knee) and in proportion to frequency, one flash
//!   each half-cycle, as powder panels are at low frequencies. The charge
//!   cannot follow the field instantly: it relaxes toward `sgn(v)·G(|v|)`
//!   with two time constants (a share each), so above their corners less
//!   charge moves each half-cycle and the light rises more slowly than the
//!   frequency — the saturation measured on powder panels. The phosphor's
//!   afterglow smooths the flashes.
//! * **The photoresistor** ([`Photocell`]) conducts by its free electrons.
//!   Light makes them; they recombine through one level (a lifetime) and,
//!   the more there are, the faster, through the spread of recombination
//!   centres Rose describes (`n^(4/3)`: an exponential distribution with
//!   its characteristic temperature three times the room's, which makes
//!   the conductance go as the 0.75 power of the light — "if T1 = 1000 °K,
//!   the photocurrent increases as the 0.75 power", Rose 1951). That is why
//!   a lit cell answers faster than a dark one, and why its conductance
//!   goes as a power of the light under one. Trap levels in the crystal catch
//!   some of them and let them go again slowly: shallow traps fill in
//!   milliseconds and empty in a fraction of a second; deep ones fill only
//!   under long, bright light and take seconds to empty. While they empty
//!   they keep the cell conducting — the T4's famous memory: a short peak
//!   lets go quickly, minutes of hard levelling hold on for seconds.
//!
//! Everything here is allocation free and steps once a sample.

/// Entries of the panel's emission table (`G` over its voltage range).
const TABLE: usize = 512;

/// The panel's charge relaxations (see [`Panel`]).
pub const RELAXATIONS: usize = 2;

/// The panel's light law, its charge's relaxations and the phosphor's
/// afterglow.
#[derive(Clone, Copy, Debug)]
pub struct PanelSpec {
    /// The Alfrey–Taylor constant `b` (√volts, peak): `s(u) = exp(−b/√u)`.
    pub alfrey_taylor: f64,
    /// The highest panel voltage the table covers (peak volts); above it
    /// the law goes on linearly (a panel driven past it saturates anyway).
    pub top_volts: f64,
    /// The charge's relaxations: (corner, Hz; share of the charge).
    pub relaxations: [(f64, f64); RELAXATIONS],
    /// The phosphor's afterglow (s).
    pub afterglow: f64,
    /// The light that counts as 1 ([`Panel::light`]): a sine of this many
    /// peak volts at [`PanelSpec::reference_hz`].
    pub reference_volts: f64,
    pub reference_hz: f64,
}

/// An electroluminescent panel: its light from the voltage across it.
#[derive(Clone, Debug)]
pub struct Panel {
    /// `G(u) = exp(−b/√u)` at `u = i · step`.
    table: [f64; TABLE],
    step: f64,
    /// What makes the reference drive's light 1.
    scale: f64,
    /// The charge moved, per relaxation, its coefficient and share.
    moved: [f64; RELAXATIONS],
    follow: [f64; RELAXATIONS],
    share: [f64; RELAXATIONS],
    glow: f64,
    glow_k: f64,
    rate: f64,
}

impl Panel {
    pub fn new(spec: PanelSpec, rate: f64) -> Self {
        let rate = rate.max(1.0);
        let step = spec.top_volts / (TABLE - 1) as f64;
        let mut table = [0.0; TABLE];
        for (i, g) in table.iter_mut().enumerate().skip(1) {
            *g = (-spec.alfrey_taylor / (i as f64 * step).sqrt()).exp();
        }
        let mut p = Self {
            table,
            step,
            scale: 1.0,
            moved: [0.0; RELAXATIONS],
            follow: spec
                .relaxations
                .map(|(hz, _)| 1.0 - (-std::f64::consts::TAU * hz / rate).exp()),
            share: spec.relaxations.map(|(_, w)| w),
            glow: 0.0,
            glow_k: 1.0 - (-1.0 / (spec.afterglow.max(1e-6) * rate)).exp(),
            rate,
        };
        // The reference drive, played for 50 ms: the mean of its last 20 ms
        // is light 1.
        let n = (0.05 * rate) as usize;
        let tail = (0.02 * rate) as usize;
        let mut sum = 0.0;
        for i in 0..n {
            let w = std::f64::consts::TAU * spec.reference_hz * i as f64 / rate;
            let l = p.step(spec.reference_volts * w.sin());
            if i >= n - tail {
                sum += l;
            }
        }
        p.scale = tail as f64 / sum.max(1e-300);
        p.reset();
        p
    }

    /// `G(u)` for `u ≥ 0`.
    #[inline]
    pub fn g(&self, u: f64) -> f64 {
        let x = u / self.step;
        let i = x as usize;
        if i + 1 < TABLE {
            let t = x - i as f64;
            self.table[i] + (self.table[i + 1] - self.table[i]) * t
        } else {
            // Past the table: the law's slope at its top, on.
            let top = self.table[TABLE - 1];
            let slope = (top - self.table[TABLE - 2]) / self.step;
            top + slope * (u - (TABLE - 1) as f64 * self.step)
        }
    }

    /// The panel at `volts` now (one sample on): its light (1 = the
    /// reference drive's).
    #[inline]
    pub fn step(&mut self, volts: f64) -> f64 {
        let v = if volts.is_finite() { volts } else { 0.0 };
        // Where the field would have put the charge (signed), and how far
        // each share of it moves toward that this sample.
        let target = self.g(v.abs()).copysign(v);
        let mut swept = 0.0;
        for k in 0..RELAXATIONS {
            let d = (target - self.moved[k]) * self.follow[k];
            self.moved[k] += d;
            swept += self.share[k] * d.abs();
        }
        let emitted = swept * self.rate * self.scale;
        self.glow += (emitted - self.glow) * self.glow_k;
        if self.glow < 1e-30 {
            self.glow = 0.0;
        }
        self.glow
    }

    /// The light now (1 = the reference drive's).
    pub fn light(&self) -> f64 {
        self.glow
    }

    pub fn reset(&mut self) {
        self.moved = [0.0; RELAXATIONS];
        self.glow = 0.0;
    }
}

/// Trap levels in the photoresistor.
pub const TRAPS: usize = 3;

/// A trap level: how many there are (relative to the free electrons at the
/// reference light), how readily they catch one (per second at that
/// density) and how long one stays caught (s).
#[derive(Clone, Copy, Debug)]
pub struct Trap {
    pub density: f64,
    pub capture: f64,
    pub release: f64,
}

/// The photoresistor's constants.
#[derive(Clone, Copy, Debug)]
pub struct CellSpec {
    /// Free electrons made a second by light 1 (in units of the electrons
    /// light 1 holds steady without traps, so its steady density is 1).
    pub generation: f64,
    /// The free electrons' own lifetime (s): recombination through one
    /// level.
    pub lifetime: f64,
    /// Recombination through the centres' spread, at density 1 (s):
    /// `dn/dt = −n^(4/3) / τ`.
    pub pairs: f64,
    pub traps: [Trap; TRAPS],
    /// Conductance in the dark, and per unit density (siemens).
    pub dark: f64,
    pub per_density: f64,
}

/// A cadmium-sulphide photoresistor: its conductance from the light on it.
#[derive(Clone, Copy, Debug)]
pub struct Photocell {
    spec: CellSpec,
    dt: f64,
    /// Free electrons (1 = steady at light 1, without traps).
    n: f64,
    /// Each trap level's occupancy (0…1).
    held: [f64; TRAPS],
}

impl Photocell {
    pub fn new(spec: CellSpec, rate: f64) -> Self {
        Self {
            spec,
            dt: 1.0 / rate.max(1.0),
            n: 0.0,
            held: [0.0; TRAPS],
        }
    }

    /// One sample in `light`: the rate equations, each solved implicitly
    /// for its own unknown (stable at any rate, however fast the lit cell
    /// answers).
    #[inline]
    pub fn step(&mut self, light: f64) {
        let s = &self.spec;
        let dt = self.dt;
        let mut released = 0.0;
        let mut catching = 0.0;
        for (t, m) in s.traps.iter().zip(&self.held) {
            released += t.density * m / t.release;
            catching += t.capture * (1.0 - m);
        }
        let made = s.generation * light.max(0.0);
        let n = (self.n + dt * (made + released))
            / (1.0 + dt * (1.0 / s.lifetime + self.n.cbrt() / s.pairs + catching));
        self.n = if n < 1e-30 { 0.0 } else { n };
        for (t, m) in s.traps.iter().zip(&mut self.held) {
            // Caught at `capture · n` per empty place (per unit of the
            // level's density), let go at `1 / release`.
            let caught = t.capture * self.n / t.density.max(1e-12);
            *m = (*m + dt * caught) / (1.0 + dt * (caught + 1.0 / t.release));
            if *m < 1e-30 {
                *m = 0.0;
            }
        }
    }

    /// The free electrons now (1 = steady at light 1 without traps).
    pub fn density(&self) -> f64 {
        self.n
    }

    /// How full each trap level is (0…1).
    pub fn held(&self) -> [f64; TRAPS] {
        self.held
    }

    /// Its conductance now (siemens).
    #[inline]
    pub fn conductance(&self) -> f64 {
        self.spec.dark + self.spec.per_density * self.n
    }

    /// Its resistance now (ohms).
    #[inline]
    pub fn ohms(&self) -> f64 {
        1.0 / self.conductance()
    }

    pub fn reset(&mut self) {
        self.n = 0.0;
        self.held = [0.0; TRAPS];
    }
}
