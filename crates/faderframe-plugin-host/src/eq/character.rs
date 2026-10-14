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
//! * Console: a console's line amplifier — a symmetric soft limit on the
//!   whole band with a slight asymmetry from its bias, so loud peaks round
//!   off with mostly third harmonic and a touch of second; the treble is
//!   coloured like the bass.
//! * Tape: a tape machine's record path — the signal is pre-emphasised
//!   (+6 dB over 3 kHz, the record equalisation), saturated softly and
//!   de-emphasised again, so the treble saturates first and loud top end
//!   is tamed ("tape softening") while the small-signal response stays
//!   flat (the emphasis filters are exact inverses).
//!
//! All have unit gain for small signals and add nothing to quiet ones;
//! switching between them crossfades. The drive scales how hard the stage
//! is hit (the level is kept), and the stage can run oversampled (2× or
//! 4×, [`crate::dsp::oversample::Oversampler`]) so what a full band
//! character makes of the top octaves does not fold back into the audio
//! band; the oversampler's latency is part of the EQ's.

use super::State;
use super::design::{self, BandShape, BandType, Coefs, MAX_SECTIONS};
use crate::dsp::oversample::Oversampler;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Character {
    Clean,
    Subtle,
    Warm,
    Console,
    Tape,
}

impl Character {
    pub const ALL: [Character; 5] = [
        Character::Clean,
        Character::Subtle,
        Character::Warm,
        Character::Console,
        Character::Tape,
    ];

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
            Character::Console => "Console",
            Character::Tape => "Tape",
        }
    }

    /// What it is, for a menu's tooltip.
    pub fn describe(self) -> &'static str {
        match self {
            Character::Clean => "nothing added",
            Character::Subtle => "a transformer's low-end saturation",
            Character::Warm => "a tube stage's even harmonics",
            Character::Console => "a console line amplifier's soft limit",
            Character::Tape => "a tape machine's treble softening",
        }
    }
}

/// The oversampling choices of the character stage (factors).
pub const OVERSAMPLING: [usize; 3] = [1, 2, 4];
pub const OVERSAMPLING_NAMES: [&str; 3] = ["Off", "2×", "4×"];

/// The oversampling factor of a parameter value.
pub fn oversampling_factor(value: f64) -> usize {
    let i = (value.round().max(0.0) as usize).min(OVERSAMPLING.len() - 1);
    OVERSAMPLING[i]
}

/// The latency (samples) of a character stage oversampled by `factor`.
pub fn oversampling_latency(factor: usize) -> u32 {
    if factor <= 1 {
        0
    } else {
        Oversampler::new(factor).latency()
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
/// The console amplifier: its drive, how much of its limit is heard and
/// its bias (the asymmetry).
const CONSOLE_DRIVE: f64 = 0.7;
const CONSOLE_AMOUNT: f64 = 0.6;
const CONSOLE_BIAS: f64 = 0.05;
/// The tape: where the record emphasis starts (Hz), its boost (linear,
/// +6 dB) and the saturation's drive.
const TAPE_EMPHASIS: f64 = 3_000.0;
const TAPE_BOOST: f64 = 2.0;
const TAPE_DRIVE: f64 = 0.7;
/// The crossfade between characters (seconds).
const FADE: f64 = 0.02;
/// Time constant of the drive's glide (seconds).
const DRIVE_GLIDE: f64 = 0.01;

/// The tube's curve, unit slope at zero.
#[inline]
fn tube(v: f64) -> f64 {
    let tb = (WARM_DRIVE * WARM_BIAS).tanh();
    ((WARM_DRIVE * (v + WARM_BIAS)).tanh() - tb) / (WARM_DRIVE * (1.0 - tb * tb))
}

/// The console amplifier's curve, unit slope at zero: a biased soft
/// limit, of which only a part is heard.
#[inline]
fn console(v: f64) -> f64 {
    let tb = (CONSOLE_DRIVE * CONSOLE_BIAS).tanh();
    let limited =
        ((CONSOLE_DRIVE * (v + CONSOLE_BIAS)).tanh() - tb) / (CONSOLE_DRIVE * (1.0 - tb * tb));
    v + CONSOLE_AMOUNT * (limited - v)
}

/// A first order shelf and its exact inverse (bilinear), as `b0 + b1 z⁻¹
/// over 1 + a1 z⁻¹`.
#[derive(Clone, Copy)]
struct Shelf {
    b0: f64,
    b1: f64,
    a1: f64,
}

impl Shelf {
    /// A high shelf of gain `boost` (linear) from `corner` Hz at `sr`:
    /// `H(s) = (1 + boost · s/w) / (1 + s/w)`.
    fn high(corner: f64, boost: f64, sr: f64) -> Self {
        let k = (std::f64::consts::PI * corner.min(0.45 * sr) / sr).tan();
        // Bilinear: s/w → (1 − z⁻¹) / (k (1 + z⁻¹)).
        let n0 = k + boost;
        let n1 = k - boost;
        let d0 = k + 1.0;
        let d1 = k - 1.0;
        Self {
            b0: n0 / d0,
            b1: n1 / d0,
            a1: d1 / d0,
        }
    }

    fn inverse(self) -> Self {
        Self {
            b0: 1.0 / self.b0,
            b1: self.a1 / self.b0,
            a1: self.b1 / self.b0,
        }
    }

    #[inline]
    fn run(self, st: &mut (f64, f64), x: f64) -> f64 {
        let (x1, y1) = *st;
        let y = self.b0 * x + self.b1 * x1 - self.a1 * y1;
        *st = (x, y);
        y
    }
}

/// The colours themselves, built for the rate they run at (the
/// oversampled one).
struct Colour {
    lf: [f64; 2],
    a_lf: f64,
    top: Coefs,
    top_state: [State; 2],
    /// DC blockers of the tube's and the console's harmonics: last input,
    /// last output.
    dc: [(f64, f64); 2],
    console_dc: [(f64, f64); 2],
    dc_r: f64,
    emphasis: Shelf,
    de_emphasis: Shelf,
    tape_state: [[(f64, f64); 2]; 2],
}

impl Colour {
    fn new(sr: f64) -> Self {
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
        let emphasis = Shelf::high(TAPE_EMPHASIS, TAPE_BOOST, sr);
        Self {
            lf: [0.0; 2],
            a_lf: 1.0 - (-2.0 * std::f64::consts::PI * SUBTLE_CORNER / sr).exp(),
            top: s[0],
            top_state: [State::default(); 2],
            dc: [(0.0, 0.0); 2],
            console_dc: [(0.0, 0.0); 2],
            dc_r: (-2.0 * std::f64::consts::PI * 5.0 / sr).exp(),
            emphasis,
            de_emphasis: emphasis.inverse(),
            tape_state: [[(0.0, 0.0); 2]; 2],
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
    fn console(&mut self, ch: usize, x: f64) -> f64 {
        let e = console(x) - x;
        let (x1, y1) = self.console_dc[ch];
        let hp = e - x1 + self.dc_r * y1;
        self.console_dc[ch] = (e, hp);
        x + hp
    }

    #[inline]
    fn tape(&mut self, ch: usize, x: f64) -> f64 {
        let [pre, post] = &mut self.tape_state[ch];
        let v = self.emphasis.run(pre, x);
        let sat = (TAPE_DRIVE * v).tanh() / TAPE_DRIVE;
        self.de_emphasis.run(post, sat)
    }

    #[inline]
    fn one(&mut self, c: Character, ch: usize, x: f64) -> f64 {
        match c {
            Character::Clean => x,
            Character::Subtle => self.subtle(ch, x),
            Character::Warm => self.warm(ch, x),
            Character::Console => self.console(ch, x),
            Character::Tape => self.tape(ch, x),
        }
    }

    fn reset(&mut self) {
        self.lf = [0.0; 2];
        self.top_state = [State::default(); 2];
        self.dc = [(0.0, 0.0); 2];
        self.console_dc = [(0.0, 0.0); 2];
        self.tape_state = [[(0.0, 0.0); 2]; 2];
    }
}

pub struct CharacterStage {
    target: Character,
    current: Character,
    from: Character,
    /// 0 → 1 while crossfading from `from` to `current`.
    fade: f64,
    fade_step: f64,
    colour: Colour,
    /// The drive (1: as designed) and where it is going.
    drive: f64,
    drive_target: f64,
    drive_glide: f64,
    /// The oversamplers of the two channels (none at 1×).
    over: Option<Box<[Oversampler; 2]>>,
    factor: usize,
}

impl CharacterStage {
    /// A stage at `sr`, oversampled by `factor` (1, 2 or 4).
    pub fn new(sr: f64, factor: usize) -> Self {
        let factor = Oversampler::supported(factor.clamp(1, 4));
        let inner = sr * factor as f64;
        Self {
            target: Character::Clean,
            current: Character::Clean,
            from: Character::Clean,
            fade: 1.0,
            fade_step: 1.0 / (FADE * inner),
            colour: Colour::new(inner),
            drive: 1.0,
            drive_target: 1.0,
            drive_glide: (-1.0 / (DRIVE_GLIDE * sr)).exp(),
            over: (factor > 1)
                .then(|| Box::new([Oversampler::new(factor), Oversampler::new(factor)])),
            factor,
        }
    }

    /// The latency the oversampling costs (samples at the host rate).
    pub fn latency(&self) -> u32 {
        self.over.as_ref().map_or(0, |o| o[0].latency())
    }

    pub fn factor(&self) -> usize {
        self.factor
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

    /// How hard the stage is driven (1: as designed; 0: nothing).
    pub fn set_drive(&mut self, drive: f64) {
        self.drive_target = drive.clamp(0.0, 4.0);
    }

    /// Whether the stage changes the signal at all right now.
    fn colouring(&self) -> bool {
        self.current != Character::Clean || self.from != Character::Clean || self.fade < 1.0
    }

    /// One sample of one channel at the inner rate.
    #[inline]
    fn sample(&mut self, ch: usize, x: f64) -> f64 {
        let drive = self.drive;
        if drive < 1e-4 {
            return x;
        }
        let d = x * drive;
        let y = if self.fade >= 1.0 {
            self.colour.one(self.current, ch, d)
        } else {
            let t = self.fade;
            let a = self.colour.one(self.from, ch, d);
            let b = self.colour.one(self.current, ch, d);
            a + (b - a) * t
        };
        y / drive
    }

    #[inline]
    fn advance_fade(&mut self) {
        if self.fade < 1.0 {
            self.fade = (self.fade + self.fade_step).min(1.0);
        }
    }

    #[inline]
    pub fn process(&mut self, l: f64, r: f64) -> (f64, f64) {
        if self.fade >= 1.0 && self.target != self.current {
            self.set(self.target);
        }
        // The drive glides once per host sample.
        self.drive += (self.drive_target - self.drive) * (1.0 - self.drive_glide);
        if !self.colouring() {
            // Clean: the oversampler still runs for its latency.
            return match self.over.as_mut() {
                None => (l, r),
                Some(o) => (o[0].process(l, &mut |x| x), o[1].process(r, &mut |x| x)),
            };
        }
        match self.over.take() {
            None => {
                let y = (self.sample(0, l), self.sample(1, r));
                self.advance_fade();
                y
            }
            Some(mut o) => {
                // Both channels share the fade: it moves once per inner
                // sample pair.
                let fade_before = self.fade;
                let yl = o[0].process(l, &mut |x| {
                    let y = self.sample(0, x);
                    self.advance_fade();
                    y
                });
                let fade_after = self.fade;
                self.fade = fade_before;
                let yr = o[1].process(r, &mut |x| {
                    let y = self.sample(1, x);
                    self.advance_fade();
                    y
                });
                self.fade = fade_after;
                self.over = Some(o);
                (yl, yr)
            }
        }
    }

    pub fn reset(&mut self) {
        self.colour.reset();
        if let Some(o) = self.over.as_mut() {
            o[0].reset();
            o[1].reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output of a sine of `amp` at `f` Hz through a character
    /// (`drive`, oversampled by `factor`): the last second of two.
    fn render(c: Character, amp: f64, f: f64, drive: f64, factor: usize) -> Vec<f64> {
        let sr = 48_000.0;
        let mut st = CharacterStage::new(sr, factor);
        st.set(c);
        st.set_drive(drive);
        let n = 48_000;
        let mut y = Vec::with_capacity(n);
        for i in 0..2 * n {
            let x = amp * (std::f64::consts::TAU * f * i as f64 / sr).sin();
            let (l, _) = st.process(x, x);
            if i >= n {
                y.push(l);
            }
        }
        y
    }

    /// The amplitude of the `f` Hz component of `y` (at 48 kHz).
    fn bin(y: &[f64], f: f64) -> f64 {
        let w = std::f64::consts::TAU * f / 48_000.0;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in y.iter().enumerate() {
            re += v * (w * k as f64).cos();
            im += v * (w * k as f64).sin();
        }
        2.0 * re.hypot(im) / y.len() as f64
    }

    /// Total harmonic distortion of a sine of `amp` at `f` Hz through a
    /// character (percent).
    fn thd(c: Character, amp: f64, f: f64) -> f64 {
        thd_driven(c, amp, f, 1.0)
    }

    fn thd_driven(c: Character, amp: f64, f: f64, drive: f64) -> f64 {
        let y = render(c, amp, f, drive, 1);
        let fundamental = bin(&y, f);
        let harmonics: f64 = (2..8)
            .map(|h| bin(&y, f * h as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        100.0 * harmonics / fundamental
    }

    #[test]
    fn clean_is_untouched_and_the_others_colour_with_level() {
        assert!(thd(Character::Clean, 0.9, 100.0) < 1e-6);
        for c in Character::ALL.into_iter().skip(1) {
            let quiet = thd(c, 0.05, 100.0);
            let mid = thd(c, 0.25, 100.0);
            let loud = thd(c, 1.0, 100.0);
            assert!(quiet < 0.3, "{c:?} at −26 dBFS: {quiet:.3} %");
            assert!(mid > quiet && loud > mid, "{c:?}: {quiet} {mid} {loud}");
            assert!(loud > 0.5 && loud < 8.0, "{c:?} at 0 dBFS: {loud:.2} %");
        }
        // The transformer leaves the treble nearly alone; the console
        // colours it as much as the bass; the tape colours it more.
        assert!(thd(Character::Subtle, 1.0, 3_000.0) < 0.2 * thd(Character::Subtle, 1.0, 60.0));
        let (bass, treble) = (
            thd(Character::Console, 1.0, 60.0),
            thd(Character::Console, 1.0, 3_000.0),
        );
        assert!(
            (treble / bass - 1.0).abs() < 0.2,
            "console: {bass} vs {treble}"
        );
        assert!(
            thd(Character::Tape, 0.5, 5_000.0) > 1.5 * thd(Character::Tape, 0.5, 100.0),
            "tape softens the treble first"
        );
    }

    #[test]
    fn the_console_adds_odd_and_a_little_even() {
        let y = render(Character::Console, 1.0, 100.0, 1.0, 1);
        let (h2, h3) = (bin(&y, 200.0), bin(&y, 300.0));
        assert!(h3 > 3.0 * h2, "third {h3} over second {h2}");
        assert!(h2 > 1e-4, "a touch of second: {h2}");
    }

    #[test]
    fn small_signals_keep_their_level() {
        let sr = 48_000.0;
        for c in Character::ALL {
            for factor in [1, 2] {
                let mut st = CharacterStage::new(sr, factor);
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
                assert!(db.abs() < 0.05, "{c:?} ×{factor}: {db:.3} dB");
            }
        }
    }

    #[test]
    fn the_tape_emphasis_and_its_inverse_cancel() {
        let sr = 48_000.0;
        let pre = Shelf::high(TAPE_EMPHASIS, TAPE_BOOST, sr);
        let post = pre.inverse();
        let (mut a, mut b) = ((0.0, 0.0), (0.0, 0.0));
        let mut worst: f64 = 0.0;
        for i in 0..20_000 {
            let x = if i == 0 { 1.0 } else { 0.0 }
                + 0.5 * (std::f64::consts::TAU * 9_000.0 * i as f64 / sr).sin();
            let y = post.run(&mut b, pre.run(&mut a, x));
            worst = worst.max((y - x).abs());
        }
        assert!(worst < 1e-9, "{worst}");
    }

    #[test]
    fn the_drive_sets_how_much_colour_and_keeps_the_level() {
        let soft = thd_driven(Character::Warm, 0.5, 100.0, 0.5);
        let plain = thd_driven(Character::Warm, 0.5, 100.0, 1.0);
        let hard = thd_driven(Character::Warm, 0.5, 100.0, 2.0);
        assert!(soft < plain && plain < hard, "{soft} {plain} {hard}");
        assert!(thd_driven(Character::Console, 0.5, 100.0, 0.0) < 1e-9);
        for drive in [0.5, 2.0] {
            let y = render(Character::Console, 0.1, 1_000.0, drive, 1);
            let level = 20.0 * (bin(&y, 1_000.0) / 0.1).log10();
            assert!(level.abs() < 0.1, "drive {drive}: {level:.3} dB");
        }
    }

    #[test]
    fn oversampling_keeps_the_aliases_down_and_the_harmonics() {
        // A loud 15 kHz tone through the console: its third harmonic at
        // 45 kHz folds back to 3 kHz at 48 kHz — unless the stage runs at
        // 96 kHz, where the downsampler removes it.
        let plain = render(Character::Console, 0.9, 15_000.0, 1.0, 1);
        let over = render(Character::Console, 0.9, 15_000.0, 1.0, 2);
        let alias = |y: &[f64]| 20.0 * bin(y, 3_000.0).log10();
        let (a, b) = (alias(&plain), alias(&over));
        assert!(a > -60.0, "the alias is there at 1×: {a:.1} dB");
        assert!(b < a - 30.0, "and gone at 2×: {a:.1} → {b:.1} dB");
        // What is in the band stays: a 1 kHz tone's third harmonic.
        let plain = render(Character::Console, 0.9, 1_000.0, 1.0, 1);
        let over = render(Character::Console, 0.9, 1_000.0, 1.0, 4);
        let h3 = |y: &[f64]| 20.0 * bin(y, 3_000.0).log10();
        assert!(
            (h3(&plain) - h3(&over)).abs() < 0.5,
            "{} vs {}",
            h3(&plain),
            h3(&over)
        );
        // The latency is the oversampler's.
        let st = CharacterStage::new(48_000.0, 2);
        assert_eq!(st.latency(), oversampling_latency(2));
        assert!(st.latency() > 0);
        assert_eq!(CharacterStage::new(48_000.0, 1).latency(), 0);
    }

    #[test]
    fn an_oversampled_clean_stage_is_a_delay() {
        let sr = 48_000.0;
        let mut st = CharacterStage::new(sr, 2);
        let lat = st.latency() as usize;
        let mut out = Vec::new();
        for i in 0..4_000 {
            let x = 0.5 * (std::f64::consts::TAU * 1_000.0 * i as f64 / sr).sin();
            out.push(st.process(x, x).0);
        }
        for (i, y) in out.iter().enumerate().skip(2_000) {
            let x = 0.5 * (std::f64::consts::TAU * 1_000.0 * (i - lat) as f64 / sr).sin();
            assert!((y - x).abs() < 1e-3, "at {i}: {y} vs {x}");
        }
    }
}
