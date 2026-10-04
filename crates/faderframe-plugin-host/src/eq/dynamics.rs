//! The trigger of a dynamic band: the input (or the sidechain) filtered to
//! the band's region — a band pass round a bell, the low or high side of a
//! shelf, everything for a tilt — or by the band's free low and high cuts,
//! then followed by a level detector.
//!
//! In auto mode the attack and release follow the band's frequency (a few
//! periods to attack, many more to release, so a bass band does not ripple
//! and a treble band is quick), and the threshold sits a little above the
//! trigger's own long-term level, so the band reacts to what stands out of
//! its region. In custom mode the times are scaled from the automatic ones
//! (attack and release at 50 % are automatic) and the threshold is the
//! band's own unless it is at its top ("auto").

use super::design::{self, BandShape, BandType, Coefs, MAX_SECTIONS};
use super::{BandParams, Placement, State};

/// How far above the trigger's long-term level the automatic threshold
/// sits (dB).
pub const AUTO_MARGIN: f64 = 2.0;
/// Time constant of the long-term level (seconds).
const AUTO_TIME: f64 = 1.5;
/// How long the long-term level follows the first sound quickly
/// (seconds).
const SETTLE: f64 = 0.2;
/// Levels under this are silence: the long-term level does not follow
/// them down.
const SILENT_DB: f64 = -100.0;

/// The automatic attack and release (ms) of a band at `freq`.
pub fn auto_times(freq: f64) -> (f64, f64) {
    let period = 1000.0 / freq.max(1.0);
    (
        (1.5 * period).clamp(0.5, 30.0),
        (15.0 * period).clamp(40.0, 400.0),
    )
}

/// The attack and release (ms) a band uses at `freq`.
pub fn times(p: &BandParams, freq: f64) -> (f64, f64) {
    let (a, r) = auto_times(freq);
    let (fa, fr) = p.time_factors();
    (a * fa, r * fr)
}

/// Sections a trigger's filters may take.
pub const TRIGGER_SECTIONS: usize = 4;

/// The filters a band's trigger passes through: its region, or its free
/// low and high cuts (24 dB/oct).
pub fn trigger_filters(
    p: &BandParams,
    freq: f64,
    q: f64,
    sr: f64,
) -> ([Coefs; TRIGGER_SECTIONS], usize) {
    let mut out = [Coefs::IDENTITY; TRIGGER_SECTIONS];
    let mut n = 0;
    let mut add = |kind: BandType, f: f64, q: f64, slope: f64| {
        let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
        let used = design::design(
            &BandShape {
                kind,
                freq: f,
                gain: 0.0,
                q,
                slope,
            },
            sr,
            &mut s,
        );
        for c in &s[..used] {
            if n < out.len() {
                out[n] = *c;
                n += 1;
            }
        }
    };
    let half = std::f64::consts::FRAC_1_SQRT_2;
    if p.free_trigger() {
        if p.trigger_low > 10.5 {
            add(BandType::LowCut, p.trigger_low, half, 24.0);
        }
        if p.trigger_high < 29_500.0 && p.trigger_high < 0.45 * sr {
            add(BandType::HighCut, p.trigger_high, half, 24.0);
        }
    } else {
        match p.kind {
            BandType::Bell => add(BandType::BandPass, freq, q.max(0.5), 12.0),
            BandType::LowShelf => add(BandType::HighCut, freq, half, 12.0),
            BandType::HighShelf => add(BandType::LowCut, freq, half, 12.0),
            // Tilts work on everything: the whole signal triggers them.
            _ => {}
        }
    }
    (out, n)
}

/// A band's level detector.
#[derive(Clone, Copy)]
pub struct Detector {
    filters: [Coefs; TRIGGER_SECTIONS],
    used: usize,
    state: [[State; TRIGGER_SECTIONS]; 2],
    /// Mean square of the filtered trigger.
    env: f64,
    attack: f64,
    release: f64,
    /// The long-term level (dB), and how long it has heard sound (the
    /// first sound sets it).
    slow_db: f64,
    heard: f64,
    /// Triggered by the sidechain.
    pub(crate) external: bool,
    /// What the filters and times were set for.
    set_for: Option<[f64; 8]>,
}

impl Default for Detector {
    fn default() -> Self {
        Self {
            filters: [Coefs::IDENTITY; TRIGGER_SECTIONS],
            used: 0,
            state: [[State::default(); TRIGGER_SECTIONS]; 2],
            env: 0.0,
            attack: 1.0,
            release: 1.0,
            slow_db: -60.0,
            heard: 0.0,
            external: false,
            set_for: None,
        }
    }
}

impl Detector {
    /// Follow the band's settings (filters and times change only when they
    /// move; the state is kept).
    pub fn configure(&mut self, p: &BandParams, freq: f64, q: f64, sr: f64) {
        let (attack, release) = times(p, freq);
        let key = [
            freq,
            q,
            f64::from(u8::from(p.free_trigger())),
            p.trigger_low,
            p.trigger_high,
            p.kind.index() as f64,
            attack,
            release,
        ];
        self.external = p.keyed_externally();
        let moved = self.set_for.is_none_or(|k| {
            k.iter()
                .zip(&key)
                .any(|(a, b)| (a - b).abs() > 1e-6 * b.abs().max(1.0))
        });
        if !moved {
            return;
        }
        self.set_for = Some(key);
        (self.filters, self.used) = trigger_filters(p, freq, q, sr);
        let coeff = |ms: f64| 1.0 - (-1.0 / (ms.max(0.05) * 0.001 * sr)).exp();
        self.attack = coeff(attack);
        self.release = coeff(release);
    }

    #[inline]
    fn filter(&mut self, ch: usize, x: f64) -> f64 {
        let mut y = x;
        for (s, c) in self.state[ch]
            .iter_mut()
            .zip(self.filters.iter())
            .take(self.used)
        {
            y = s.run(c, y);
        }
        y
    }

    /// One frame of the trigger.
    #[inline]
    pub fn feed(&mut self, l: f64, r: f64, placement: Placement) {
        let x2 = match placement {
            Placement::Stereo => {
                let a = self.filter(0, l);
                let b = self.filter(1, r);
                (a * a).max(b * b)
            }
            Placement::Left => self.filter(0, l).powi(2),
            Placement::Right => self.filter(0, r).powi(2),
            Placement::Mid => self.filter(0, 0.5 * (l + r)).powi(2),
            Placement::Side => self.filter(0, 0.5 * (l - r)).powi(2),
        };
        let k = if x2 > self.env {
            self.attack
        } else {
            self.release
        };
        self.env += (x2 - self.env) * k;
    }

    /// The trigger's level, dB relative to a full scale sine's peak.
    pub fn level_db(&self) -> f64 {
        10.0 * (2.0 * self.env).max(1e-14).log10()
    }

    /// Move the long-term level on by `dt` seconds.
    pub fn control(&mut self, dt: f64) {
        let level = self.level_db();
        if level > SILENT_DB {
            // The first sound sets it, and it follows quickly while that
            // settles; then slowly.
            if self.heard == 0.0 {
                self.slow_db = level;
            }
            let time = if self.heard < SETTLE { 0.05 } else { AUTO_TIME };
            self.heard += dt;
            self.slow_db += (level - self.slow_db) * (1.0 - (-dt / time).exp());
        }
    }

    /// The automatic threshold.
    pub fn auto_threshold(&self) -> f64 {
        self.slow_db + AUTO_MARGIN
    }

    pub fn reset(&mut self) {
        self.state = [[State::default(); TRIGGER_SECTIONS]; 2];
        self.env = 0.0;
        self.heard = 0.0;
    }

    pub fn flush(&mut self) {
        for s in self.state.iter_mut().flatten() {
            s.flush();
        }
        if self.env < 1e-20 {
            self.env = 0.0;
        }
    }
}
