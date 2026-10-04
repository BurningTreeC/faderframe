//! Character: a touch of analog after the EQ.
//!
//! * Clean: nothing at all.
//! * Subtle: a transformer's core. What saturates is the low end (a core's
//!   flux follows the signal's integral), softly and only as it gets loud,
//!   so the colour depends on the programme and on what the EQ did to the
//!   bass; mostly odd harmonics.
//! * Warm: a tube stage's asymmetric curve, which adds even harmonics. It
//!   is driven by the signal below the top octave, so it colours the body
//!   of the sound and the treble does not fold back above Nyquist.
//!
//! Both have unit gain for small signals and add nothing to quiet ones;
//! switching between them crossfades.

use super::State;
use super::design::{self, BandShape, BandType, Coefs, MAX_SECTIONS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Character {
    Clean,
    Subtle,
    Warm,
}

impl Character {
    pub const ALL: [Character; 3] = [Character::Clean, Character::Subtle, Character::Warm];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Character::Clean)
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Character::Clean => "Clean",
            Character::Subtle => "Subtle",
            Character::Warm => "Warm",
        }
    }
}

/// The transformer: where its low end begins (Hz), how hard it is driven
/// and how much of its saturation is heard.
const SUBTLE_CORNER: f64 = 150.0;
const SUBTLE_DRIVE: f64 = 0.8;
const SUBTLE_AMOUNT: f64 = 0.35;
/// The tube: its drive and bias, and where its drive rolls off (Hz).
const WARM_DRIVE: f64 = 1.0;
const WARM_BIAS: f64 = 0.1;
const WARM_TOP: f64 = 7_000.0;
/// The crossfade between characters (seconds).
const FADE: f64 = 0.02;

/// The tube's curve, unit slope at zero.
#[inline]
fn tube(v: f64) -> f64 {
    let tb = (WARM_DRIVE * WARM_BIAS).tanh();
    ((WARM_DRIVE * (v + WARM_BIAS)).tanh() - tb) / (WARM_DRIVE * (1.0 - tb * tb))
}

pub struct CharacterStage {
    target: Character,
    current: Character,
    from: Character,
    /// 0 → 1 while crossfading from `from` to `current`.
    fade: f64,
    fade_step: f64,
    lf: [f64; 2],
    a_lf: f64,
    top: Coefs,
    top_state: [State; 2],
    /// DC blocker of the tube's harmonics: last input, last output.
    dc: [(f64, f64); 2],
    dc_r: f64,
}

impl CharacterStage {
    pub fn new(sr: f64) -> Self {
        let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
        design::design(
            &BandShape {
                kind: BandType::HighCut,
                freq: WARM_TOP.min(0.4 * sr),
                gain: 0.0,
                q: std::f64::consts::FRAC_1_SQRT_2,
                slope: 12.0,
            },
            sr,
            &mut s,
        );
        Self {
            target: Character::Clean,
            current: Character::Clean,
            from: Character::Clean,
            fade: 1.0,
            fade_step: 1.0 / (FADE * sr),
            lf: [0.0; 2],
            a_lf: 1.0 - (-2.0 * std::f64::consts::PI * SUBTLE_CORNER / sr).exp(),
            top: s[0],
            top_state: [State::default(); 2],
            dc: [(0.0, 0.0); 2],
            dc_r: (-2.0 * std::f64::consts::PI * 5.0 / sr).exp(),
        }
    }

    /// The character to go to.
    pub fn set(&mut self, c: Character) {
        self.target = c;
        if self.fade >= 1.0 && c != self.current {
            self.from = self.current;
            self.current = c;
            self.fade = 0.0;
        }
    }

    #[inline]
    fn subtle(&mut self, ch: usize, x: f64) -> f64 {
        self.lf[ch] += self.a_lf * (x - self.lf[ch]);
        let d = SUBTLE_DRIVE * self.lf[ch];
        x - SUBTLE_AMOUNT * (d - d.tanh()) / SUBTLE_DRIVE
    }

    #[inline]
    fn warm(&mut self, ch: usize, x: f64) -> f64 {
        let v = self.top_state[ch].run(&self.top, x);
        let e = tube(v) - v;
        let (x1, y1) = self.dc[ch];
        let hp = e - x1 + self.dc_r * y1;
        self.dc[ch] = (e, hp);
        x + hp
    }

    #[inline]
    fn one(&mut self, c: Character, ch: usize, x: f64) -> f64 {
        match c {
            Character::Clean => x,
            Character::Subtle => self.subtle(ch, x),
            Character::Warm => self.warm(ch, x),
        }
    }

    #[inline]
    pub fn process(&mut self, l: f64, r: f64) -> (f64, f64) {
        if self.fade >= 1.0 {
            if self.target != self.current {
                self.set(self.target);
            }
            if self.current == Character::Clean {
                return (l, r);
            }
            return (self.one(self.current, 0, l), self.one(self.current, 1, r));
        }
        let t = self.fade;
        self.fade = (self.fade + self.fade_step).min(1.0);
        let (from, to) = (self.from, self.current);
        let a = (self.one(from, 0, l), self.one(from, 1, r));
        let b = (self.one(to, 0, l), self.one(to, 1, r));
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    pub fn reset(&mut self) {
        self.lf = [0.0; 2];
        self.top_state = [State::default(); 2];
        self.dc = [(0.0, 0.0); 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Total harmonic distortion of a sine of `amp` at `f` Hz through a
    /// character (percent).
    fn thd(c: Character, amp: f64, f: f64) -> f64 {
        let sr = 48_000.0;
        let mut st = CharacterStage::new(sr);
        st.set(c);
        let n = 48_000;
        let mut y = Vec::with_capacity(n);
        for i in 0..2 * n {
            let x = amp * (std::f64::consts::TAU * f * i as f64 / sr).sin();
            let (l, _) = st.process(x, x);
            if i >= n {
                y.push(l);
            }
        }
        let bin = |h: f64| {
            let w = std::f64::consts::TAU * f * h / sr;
            let (mut re, mut im) = (0.0, 0.0);
            for (k, v) in y.iter().enumerate() {
                re += v * (w * k as f64).cos();
                im += v * (w * k as f64).sin();
            }
            re.hypot(im)
        };
        let fundamental = bin(1.0);
        let harmonics: f64 = (2..8).map(|h| bin(h as f64).powi(2)).sum::<f64>().sqrt();
        100.0 * harmonics / fundamental
    }

    #[test]
    fn clean_is_untouched_and_the_others_colour_with_level() {
        assert!(thd(Character::Clean, 0.9, 100.0) < 1e-6);
        for c in [Character::Subtle, Character::Warm] {
            let quiet = thd(c, 0.05, 100.0);
            let mid = thd(c, 0.25, 100.0);
            let loud = thd(c, 1.0, 100.0);
            assert!(quiet < 0.3, "{c:?} at −26 dBFS: {quiet:.3} %");
            assert!(mid > quiet && loud > mid, "{c:?}: {quiet} {mid} {loud}");
            assert!(loud > 0.5 && loud < 8.0, "{c:?} at 0 dBFS: {loud:.2} %");
        }
        // The transformer leaves the treble nearly alone.
        assert!(thd(Character::Subtle, 1.0, 3_000.0) < 0.2 * thd(Character::Subtle, 1.0, 60.0));
    }

    #[test]
    fn small_signals_keep_their_level() {
        let sr = 48_000.0;
        for c in Character::ALL {
            let mut st = CharacterStage::new(sr);
            st.set(c);
            let (mut sum_in, mut sum_out) = (0.0, 0.0);
            for i in 0..48_000 {
                let x = 0.01 * (std::f64::consts::TAU * 1_000.0 * i as f64 / sr).sin();
                let (y, _) = st.process(x, x);
                if i > 4_800 {
                    sum_in += x * x;
                    sum_out += y * y;
                }
            }
            let db = 10.0 * (sum_out / sum_in).log10();
            assert!(db.abs() < 0.05, "{c:?}: {db:.3} dB");
        }
    }
}
