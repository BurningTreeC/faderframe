//! Reverb: early reflections and a 16-line feedback delay network.
//!
//! The input waits out the pre-delay, then feeds
//! * the early reflections — a type's pattern of taps (time, level, side)
//!   scaled by the size, and
//! * the late reverb — through four allpass diffusers per side into 16
//!   delay lines (lengths spread geometrically over the type's range,
//!   scaled by the size, gliding when it moves) mixed every pass by a
//!   16 × 16 Hadamard matrix (orthogonal: the network itself neither adds
//!   nor loses energy). Each line ends in a three band decay filter — a
//!   one-pole split at 250 Hz and at the damping frequency, each band
//!   turned down by exactly what its decay time asks for the line's
//!   length (`10^(−3 L / (RT·rate))`): the bass decays over decay × bass,
//!   the middle over the decay, the highs over a third of it. The lines
//!   are read through slowly modulated taps (each its own rate and phase)
//!   so the tail does not ring at the lines' modes. Left and right are
//!   taken from the lines with orthogonal sign patterns: decorrelated.
//!
//! Freeze closes the input and makes every band lossless (and stops the
//! modulation, whose interpolation would dull the loop); ducking turns the
//! reverb down while the input plays; the wet signal has low and high cuts,
//! a low and a high shelf, and a width.
//!
//! Grown since the first version (everything new is neutral at its
//! default, so saved projects sound as they did):
//! * Spring: two spring lines (left and right, their lengths scaled by the
//!   size) looping through a chain of twelve second-order allpasses tuned
//!   between 1.8 and 3.8 kHz — the dispersion that makes a spring's chirp
//!   (`Chirp`) — and a 4.2 kHz low pass, their feedback set by the decay;
//!   the springs are the early part and feed the network.
//! * Cathedral: the longest lines, a 120 ms reflection pattern.
//! * Shimmer: the late output, pitch shifted (two crossfaded read heads
//!   over a 50 ms window, `Shifter`), band limited, softly limited and fed
//!   back in with the input: every pass climbs by the interval.
//! * The highs' decay multiplier (a third by default), the bass crossover,
//!   and a pre-delay synced to the song.

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::{Allpass, DelayLine};
use crate::dsp::filter::{BandShape, BandType, Filter, OnePole};
use crate::dsp::lfo::{DIVISIONS, division_name, division_seconds};
use crate::dsp::smooth::Smoothed;
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};
use std::sync::Arc;

pub mod id {
    pub const TYPE: u32 = 0;
    pub const SIZE: u32 = 1;
    pub const DECAY: u32 = 2;
    pub const PRE_DELAY: u32 = 3;
    pub const DAMPING: u32 = 4;
    pub const BASS: u32 = 5;
    pub const DIFFUSION: u32 = 6;
    pub const MODULATION: u32 = 7;
    pub const MOD_RATE: u32 = 8;
    pub const BALANCE: u32 = 9;
    pub const WIDTH: u32 = 10;
    pub const LOW_CUT: u32 = 11;
    pub const HIGH_CUT: u32 = 12;
    pub const MIX: u32 = 13;
    pub const FREEZE: u32 = 14;
    pub const DUCKING: u32 = 15;
    pub const PRE_SYNC: u32 = 16;
    pub const PRE_DIVISION: u32 = 17;
    /// The highs' decay as a multiple of the decay (a third by default).
    pub const HIGH_DECAY: u32 = 18;
    pub const BASS_FREQ: u32 = 19;
    pub const LOW_SHELF: u32 = 20;
    pub const HIGH_SHELF: u32 = 21;
    pub const SHIMMER: u32 = 22;
    pub const SHIMMER_PITCH: u32 = 23;
}

/// Published: input, output and wet peaks (linear), the ducking (dB).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const WET_PEAK: usize = 2;
    pub const DUCK: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const TYPES: [&str; 7] = [
    "Room",
    "Hall",
    "Plate",
    "Chamber",
    "Ambience",
    "Spring",
    "Cathedral",
];
pub const SPRING: usize = 5;
pub const CATHEDRAL: usize = 6;

/// The shimmer's intervals (name, semitones).
pub const SHIMMER_PITCHES: [(&str, f64); 7] = [
    ("−12", -12.0),
    ("−5", -5.0),
    ("+5", 5.0),
    ("+7", 7.0),
    ("+12", 12.0),
    ("+19", 19.0),
    ("+24", 24.0),
];
/// The default shimmer interval (an octave up).
const DEFAULT_SHIMMER_PITCH: f64 = 4.0;
/// The low shelf's and the high shelf's corners (Hz).
pub const LOW_SHELF_HZ: f64 = 250.0;
pub const HIGH_SHELF_HZ: f64 = 4_000.0;
/// The spring lines' lengths (ms, left and right, at size 1).
pub const SPRING_MS: [f64; 2] = [33.0, 41.0];
/// The spring chain's allpasses: tuned between these (Hz).
pub const CHIRP_HZ: (f64, f64) = (1_800.0, 3_800.0);
const CHIRP_STAGES: usize = 12;
/// The spring loop's low pass (Hz).
const SPRING_LP_HZ: f64 = 4_200.0;
/// The pitch shifter's window (seconds).
const SHIFT_WINDOW: f64 = 0.05;
/// The longest pre-delay (seconds; synced divisions clamp to it).
pub const MAX_PRE_SECONDS: f64 = 1.0;

/// A type's voicing.
pub struct Voicing {
    /// The delay lines' shortest and longest (ms, at size 1).
    pub lines: (f64, f64),
    /// The early reflections' span (ms) and level.
    pub early: (f64, f64),
    /// The diffusers' lengths (ms).
    pub diffusers: [f64; 4],
    /// The late reverb's level.
    pub late: f64,
}

pub const VOICINGS: [Voicing; 7] = [
    Voicing {
        lines: (7.0, 33.0),
        early: (38.0, 0.9),
        diffusers: [3.1, 2.3, 7.3, 5.1],
        late: 0.9,
    },
    Voicing {
        lines: (27.0, 97.0),
        early: (85.0, 0.6),
        diffusers: [4.7, 3.6, 12.7, 9.3],
        late: 0.65,
    },
    Voicing {
        lines: (9.0, 45.0),
        early: (12.0, 0.0),
        diffusers: [1.9, 3.3, 9.1, 6.7],
        late: 0.8,
    },
    Voicing {
        lines: (14.0, 59.0),
        early: (55.0, 0.8),
        diffusers: [3.7, 2.9, 10.3, 7.9],
        late: 0.75,
    },
    Voicing {
        lines: (3.0, 16.0),
        early: (19.0, 1.0),
        diffusers: [1.3, 1.7, 3.9, 2.9],
        late: 1.0,
    },
    // Spring: the early level is the springs' own.
    Voicing {
        lines: (6.0, 29.0),
        early: (0.0, 0.7),
        diffusers: [1.1, 1.9, 4.3, 3.1],
        late: 0.7,
    },
    Voicing {
        lines: (35.0, 130.0),
        early: (120.0, 0.5),
        diffusers: [5.9, 4.3, 14.7, 11.1],
        late: 0.6,
    },
];

/// The longest of every voicing's lines, early span and diffuser (ms).
fn longest() -> (f64, f64, f64) {
    VOICINGS.iter().fold((0.0, 0.0, 0.0), |m, v| {
        (
            f64::max(m.0, v.lines.1),
            f64::max(m.1, v.early.0),
            v.diffusers.iter().copied().fold(m.2, f64::max),
        )
    })
}

/// The early reflections at size 1 of a span: (time as a fraction, level,
/// left or right).
const EARLY: [(f64, f64, bool); 12] = [
    (0.043, 0.84, true),
    (0.071, 0.79, false),
    (0.118, -0.72, true),
    (0.162, 0.66, false),
    (0.231, -0.58, false),
    (0.297, 0.55, true),
    (0.364, -0.47, true),
    (0.452, 0.43, false),
    (0.538, -0.37, true),
    (0.649, 0.31, false),
    (0.781, -0.26, true),
    (0.921, 0.21, false),
];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::TYPE, "Type", (TYPES.len() - 1) as f64, 1.0),
        param(id::SIZE, "Size", 0.0, 1.0, 0.5, Percent),
        param(id::DECAY, "Decay", 0.1, 20.0, 2.0, None),
        param(id::PRE_DELAY, "Pre-Delay", 0.0, 500.0, 12.0, Milliseconds),
        param(id::DAMPING, "Damping", 1_000.0, 20_000.0, 6_000.0, Hertz),
        param(id::BASS, "Bass Decay", 0.5, 2.0, 1.2, None),
        param(id::DIFFUSION, "Diffusion", 0.0, 1.0, 0.75, Percent),
        param(id::MODULATION, "Modulation", 0.0, 1.0, 0.3, Percent),
        param(id::MOD_RATE, "Modulation Rate", 0.05, 3.0, 0.6, Hertz),
        param(id::BALANCE, "Early/Late", 0.0, 1.0, 0.5, Percent),
        param(id::WIDTH, "Width", 0.0, 1.5, 1.0, Percent),
        param(id::LOW_CUT, "Low Cut", 20.0, 1_000.0, 20.0, Hertz),
        param(id::HIGH_CUT, "High Cut", 1_000.0, 20_000.0, 20_000.0, Hertz),
        param(id::MIX, "Mix", 0.0, 1.0, 0.3, Percent),
        stepped(id::FREEZE, "Freeze", 1.0, 0.0),
        param(id::DUCKING, "Ducking", 0.0, 1.0, 0.0, Percent),
        stepped(id::PRE_SYNC, "Pre-Delay Sync", 1.0, 0.0),
        stepped(
            id::PRE_DIVISION,
            "Pre-Delay Division",
            (DIVISIONS.len() - 1) as f64,
            5.0,
        ),
        param(id::HIGH_DECAY, "High Decay", 0.1, 2.0, 1.0 / 3.0, None),
        param(id::BASS_FREQ, "Bass Crossover", 50.0, 1_000.0, 250.0, Hertz),
        param(id::LOW_SHELF, "Low Shelf", -12.0, 12.0, 0.0, Decibels),
        param(id::HIGH_SHELF, "High Shelf", -12.0, 12.0, 0.0, Decibels),
        param(id::SHIMMER, "Shimmer", 0.0, 1.0, 0.0, Percent),
        stepped(
            id::SHIMMER_PITCH,
            "Shimmer Pitch",
            (SHIMMER_PITCHES.len() - 1) as f64,
            DEFAULT_SHIMMER_PITCH,
        ),
    ]
}

/// The shimmer's interval (semitones) for the parameter.
pub fn shimmer_semitones(v: f64) -> f64 {
    SHIMMER_PITCHES[(v.round().max(0.0) as usize).min(SHIMMER_PITCHES.len() - 1)].1
}

/// The pre-delay (seconds) for the parameters at a tempo.
pub fn pre_delay_seconds(params: &ParamValues, tempo: f64) -> f64 {
    let s = if get(params, id::PRE_SYNC) >= 0.5 {
        division_seconds(
            get(params, id::PRE_DIVISION).round().max(0.0) as usize,
            tempo,
        )
    } else {
        get(params, id::PRE_DELAY) * 0.001
    };
    s.clamp(0.0, MAX_PRE_SECONDS)
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::TYPE => pick(&TYPES, v),
        id::FREEZE | id::PRE_SYNC => on_off(v),
        id::PRE_DIVISION => division_name(v.round().max(0.0) as usize).into(),
        id::HIGH_DECAY => format!("{v:.2}×"),
        id::LOW_SHELF | id::HIGH_SHELF if v.abs() < 0.05 => "Flat".into(),
        id::LOW_SHELF | id::HIGH_SHELF => format!("{v:+.1} dB"),
        id::SHIMMER if v < 0.005 => "Off".into(),
        id::SHIMMER_PITCH => {
            let i = (v.round().max(0.0) as usize).min(SHIMMER_PITCHES.len() - 1);
            SHIMMER_PITCHES[i].0.into()
        }
        id::DECAY if v < 1.0 => format!("{:.0} ms", v * 1000.0),
        id::DECAY => format!("{v:.2} s"),
        id::BASS => format!("{v:.2}×"),
        id::DAMPING if v >= 19_500.0 => "Off".into(),
        id::LOW_CUT if v <= 20.5 => "Off".into(),
        id::HIGH_CUT if v >= 19_500.0 => "Off".into(),
        id::BALANCE if (v - 0.5).abs() < 0.01 => "Both".into(),
        id::BALANCE if v < 0.5 => format!("Early {:.0}", (0.5 - v) * 200.0),
        id::BALANCE => format!("Late {:.0}", (v - 0.5) * 200.0),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// How much the size scales a type's lengths.
pub fn size_scale(size: f64) -> f64 {
    0.6 + 0.8 * size.clamp(0.0, 1.0)
}

/// The decay time (s) of the bass, middle and highs.
pub fn decay_times(params: &ParamValues) -> [f64; 3] {
    let rt = get(params, id::DECAY).max(0.05);
    let high = if get(params, id::DAMPING) >= 19_500.0 {
        rt
    } else {
        rt * get(params, id::HIGH_DECAY).clamp(0.1, 2.0)
    };
    [rt * get(params, id::BASS), rt, high]
}

const LINES: usize = 16;
const STEP: usize = 32;

/// The 16 point Hadamard transform, normalised (orthogonal).
#[inline]
fn hadamard(v: &mut [f64; LINES]) {
    let mut h = 1;
    while h < LINES {
        let mut i = 0;
        while i < LINES {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += 2 * h;
        }
        h *= 2;
    }
    for x in v.iter_mut() {
        *x *= 0.25;
    }
}

/// The output sign patterns (two rows of a Hadamard matrix).
const OUT_L: [f64; LINES] = [
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0,
];
const OUT_R: [f64; LINES] = [
    1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0,
];

/// A line's three band decay.
#[derive(Clone, Copy, Default)]
struct Decay {
    low: OnePole,
    high: OnePole,
    gains: [f64; 3],
}

impl Decay {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let low = self.low.low(x);
        let rest = x - low;
        let mid = self.high.low(rest);
        let high = rest - mid;
        self.gains[0] * low + self.gains[1] * mid + self.gains[2] * high
    }
}

/// A second-order allpass (transposed direct form II): flat in level,
/// its group delay peaks at its frequency — a chain of them disperses.
#[derive(Clone, Copy, Default)]
struct Allpass2 {
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Allpass2 {
    /// Poles at `freq` with radius `r` (closer to 1: sharper, more delay).
    fn set(&mut self, freq: f64, r: f64, rate: f64) {
        let theta = TAU * freq.clamp(10.0, 0.45 * rate) / rate;
        self.a1 = -2.0 * r * theta.cos();
        self.a2 = r * r;
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.a2 * x + self.s1;
        self.s1 = self.a1 * x - self.a1 * y + self.s2;
        self.s2 = x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    fn flush(&mut self) {
        self.s1 = flush(self.s1);
        self.s2 = flush(self.s2);
    }
}

/// One spring: a line looping through the chirp chain and a low pass.
struct Spring {
    line: DelayLine,
    chirp: [Allpass2; CHIRP_STAGES],
    lp: OnePole,
    /// Its length now (samples) and the loop's gain.
    len: f64,
    gain: f64,
}

impl Spring {
    fn new(cap: usize, rate: f64) -> Self {
        let mut chirp = [Allpass2::default(); CHIRP_STAGES];
        for (i, ap) in chirp.iter_mut().enumerate() {
            let t = i as f64 / (CHIRP_STAGES - 1) as f64;
            ap.set(CHIRP_HZ.0 + (CHIRP_HZ.1 - CHIRP_HZ.0) * t, 0.9, rate);
        }
        Self {
            line: DelayLine::new(cap),
            chirp,
            lp: OnePole::new(SPRING_LP_HZ, rate),
            len: 1.0,
            gain: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let mut y = self.line.read(self.len);
        for ap in &mut self.chirp {
            y = ap.process(y);
        }
        let y = self.lp.low(y);
        self.line.push(flush(x + self.gain * y));
        y
    }

    fn reset(&mut self) {
        self.line.reset();
        self.chirp.iter_mut().for_each(Allpass2::reset);
        self.lp.reset();
    }

    fn flush(&mut self) {
        self.chirp.iter_mut().for_each(Allpass2::flush);
        self.lp.flush();
    }
}

/// A pitch shifter: two read heads crossfaded over a window.
struct Shifter {
    line: DelayLine,
    phase: f64,
    window: f64,
}

impl Shifter {
    fn new(rate: f64) -> Self {
        let window = (SHIFT_WINDOW * rate).round();
        Self {
            line: DelayLine::new(window as usize + 8),
            phase: 0.0,
            window,
        }
    }

    /// `x` shifted by `ratio` (2: an octave up).
    #[inline]
    fn process(&mut self, x: f64, ratio: f64) -> f64 {
        self.line.push(x);
        let p1 = self.phase;
        let p2 = (p1 + 0.5).fract();
        let y = (PI * p1).sin() * self.line.read(1.0 + p1 * self.window)
            + (PI * p2).sin() * self.line.read(1.0 + p2 * self.window);
        self.phase = (self.phase + (1.0 - ratio) / self.window).rem_euclid(1.0);
        y
    }

    fn reset(&mut self) {
        self.line.reset();
    }
}

pub struct ReverbProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    pre: [DelayLine; 2],
    diffusers: [[Allpass; 4]; 2],
    lines: Vec<DelayLine>,
    /// Each line's length now (samples) and where it glides to.
    length: [f64; LINES],
    target: [f64; LINES],
    decay: [Decay; LINES],
    /// Each line's modulation phase and rate factor.
    phase: [f64; LINES],
    rate: [f64; LINES],
    low_cut: Filter<2>,
    high_cut: Filter<2>,
    low_shelf: Filter<2>,
    high_shelf: Filter<2>,
    springs: [Spring; 2],
    shifters: [Shifter; 2],
    /// The shimmer feed's band limit.
    shimmer_lp: [OnePole; 2],
    shimmer_hp: [OnePole; 2],
    shimmer: Smoothed,
    /// The longest pre-delay (samples) the lines hold.
    pre_max: usize,
    freeze: Smoothed,
    mix: Smoothed,
    modulation: Smoothed,
    duck_env: f64,
    duck: f64,
    /// The late output of the last sample (what the shimmer shifts).
    late_out: [f64; 2],
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl ReverbProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        // The longest line (Cathedral at the largest size) with room for
        // the modulation.
        let (lines_ms, early_ms, ap_ms) = longest();
        let line_cap = (lines_ms * 0.001 * size_scale(1.0) * sr) as usize + 128;
        let ap_cap = (ap_ms * 1.07 * 0.001 * size_scale(1.0) * sr) as usize + 8;
        let early_cap = (early_ms * 0.001 * size_scale(1.0) * sr) as usize + 8;
        let pre_max = (MAX_PRE_SECONDS * sr) as usize;
        let pre_cap = pre_max + early_cap + 16;
        let spring_cap = (SPRING_MS[1] * 0.001 * size_scale(1.0) * sr) as usize + 8;
        let mk_ap = || Allpass::new(ap_cap);
        let mt = MeterTap::new(sr as f32);
        let mut s = Self {
            watching: Watching::new(sr as f32),
            sr,
            pre: [DelayLine::new(pre_cap), DelayLine::new(pre_cap)],
            diffusers: [
                [mk_ap(), mk_ap(), mk_ap(), mk_ap()],
                [mk_ap(), mk_ap(), mk_ap(), mk_ap()],
            ],
            lines: (0..LINES).map(|_| DelayLine::new(line_cap)).collect(),
            length: [0.0; LINES],
            target: [0.0; LINES],
            decay: [Decay::default(); LINES],
            phase: std::array::from_fn(|i| (i as f64 * 0.618_034).fract()),
            rate: std::array::from_fn(|i| 0.7 + 0.6 * i as f64 / (LINES - 1) as f64),
            low_cut: Filter::default(),
            high_cut: Filter::default(),
            low_shelf: Filter::default(),
            high_shelf: Filter::default(),
            springs: [Spring::new(spring_cap, sr), Spring::new(spring_cap, sr)],
            shifters: [Shifter::new(sr), Shifter::new(sr)],
            shimmer_lp: [OnePole::new(6_000.0, sr); 2],
            shimmer_hp: [OnePole::new(150.0, sr); 2],
            shimmer: Smoothed::new(0.0, 30.0, sr),
            pre_max,
            freeze: Smoothed::new(0.0, 20.0, sr),
            mix: Smoothed::new(0.3, 20.0, sr),
            modulation: Smoothed::new(0.0, 50.0, sr),
            duck_env: 0.0,
            duck: 1.0,
            late_out: [0.0; 2],
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        s.control();
        s.length = s.target;
        s.decay_gains();
        s.freeze.snap();
        s.mix.snap();
        s.modulation.snap();
        s.shimmer.snap();
        s
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn kind(&self) -> usize {
        (self.get(id::TYPE).round().max(0.0) as usize).min(TYPES.len() - 1)
    }

    fn voicing(&self) -> &'static Voicing {
        &VOICINGS[self.kind()]
    }

    fn control(&mut self) {
        let sr = self.sr;
        let v = self.voicing();
        let scale = size_scale(self.get(id::SIZE));
        let (lo, hi) = (v.lines.0 * scale, v.lines.1 * scale);
        for i in 0..LINES {
            let ms = lo * (hi / lo).powf(i as f64 / (LINES - 1) as f64);
            // Odd whole numbers of samples, kept apart.
            let n = (ms * 0.001 * sr).round() as usize | 1;
            self.target[i] = n as f64 + 2.0 * i as f64;
        }
        for c in 0..2 {
            for (k, ap) in self.diffusers[c].iter_mut().enumerate() {
                // The right side a little longer: decorrelated from the start.
                let ms = v.diffusers[k] * scale * if c == 1 { 1.07 } else { 1.0 };
                ap.set_len(((ms * 0.001 * sr).round() as usize).max(1));
            }
        }
        for (spring, ms) in self.springs.iter_mut().zip(SPRING_MS) {
            spring.len = (ms * 0.001 * scale * sr).max(1.0);
        }
        let damping = self.get(id::DAMPING).min(0.45 * sr);
        let bass = self.get(id::BASS_FREQ).clamp(20.0, 2_000.0);
        for d in &mut self.decay {
            d.low.set(bass, sr);
            d.high.set(damping, sr);
        }
        self.shimmer.set(self.get(id::SHIMMER).clamp(0.0, 1.0));
        let shelf = |kind: BandType, freq: f64, gain: f64| BandShape {
            kind,
            freq,
            gain,
            q: FRAC_1_SQRT_2,
            slope: 12.0,
        };
        let low = self.get(id::LOW_SHELF);
        if low.abs() >= 0.05 {
            self.low_shelf
                .set(shelf(BandType::LowShelf, LOW_SHELF_HZ, low), sr);
        } else {
            self.low_shelf.clear();
        }
        let high = self.get(id::HIGH_SHELF);
        if high.abs() >= 0.05 {
            self.high_shelf
                .set(shelf(BandType::HighShelf, HIGH_SHELF_HZ, high), sr);
        } else {
            self.high_shelf.clear();
        }
        let frozen = self.get(id::FREEZE) >= 0.5;
        self.freeze.set(if frozen { 1.0 } else { 0.0 });
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        self.modulation.set(if frozen {
            0.0
        } else {
            self.get(id::MODULATION)
        });
        let low = self.get(id::LOW_CUT);
        if low > 20.5 {
            self.low_cut.set(cut(BandType::LowCut, low), sr);
        } else {
            self.low_cut.clear();
        }
        let high = self.get(id::HIGH_CUT);
        if high < 19_500.0 && high < 0.45 * sr {
            self.high_cut.set(cut(BandType::HighCut, high), sr);
        } else {
            self.high_cut.clear();
        }
    }

    /// The lines' band gains for their lengths now (lossless when frozen),
    /// and the springs' loop gain for the decay.
    fn decay_gains(&mut self) {
        let rts = decay_times(&self.params);
        let frozen = self.freeze.value;
        for (decay, length) in self.decay.iter_mut().zip(&self.length) {
            for (gain, rt) in decay.gains.iter_mut().zip(rts) {
                let g = 10f64.powf(-3.0 * length / (rt * self.sr));
                *gain = g + frozen * (1.0 - g);
            }
        }
        for s in &mut self.springs {
            let g = 10f64.powf(-3.0 * s.len / (rts[1] * self.sr));
            s.gain = (g + frozen * (1.0 - g)).min(0.995);
        }
    }
}

fn cut(kind: BandType, freq: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain: 0.0,
        q: FRAC_1_SQRT_2,
        slope: 12.0,
    }
}

impl PluginProcessor for ReverbProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let sr = self.sr;
        let tempo = if ctx.transport.tempo > 0.0 {
            ctx.transport.tempo
        } else {
            120.0
        };
        let glide = 1.0 - (-(STEP as f64) / (0.15 * sr)).exp();
        let duck_attack = 1.0 - (-1.0 / (0.01 * sr)).exp();
        let duck_release = (-1.0 / (0.3 * sr)).exp();
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak, mut wet_peak) = (0.0f64, 0.0f64, 0.0f64);
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            let mut moved = at == 0;
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                    moved = true;
                } else {
                    break;
                }
            }
            if moved {
                self.control();
            }
            for i in 0..LINES {
                self.length[i] += (self.target[i] - self.length[i]) * glide;
            }
            self.decay_gains();
            let v = self.voicing();
            let spring = self.kind() == SPRING;
            let scale = size_scale(self.get(id::SIZE));
            let pre =
                ((pre_delay_seconds(&self.params, tempo) * sr).round() as usize).min(self.pre_max);
            let early_span = v.early.0 * 0.001 * scale * sr;
            let ratio = 2f64.powf(shimmer_semitones(self.get(id::SHIMMER_PITCH)) / 12.0);
            let balance = self.get(id::BALANCE);
            let early_gain = v.early.1 * (2.0 * (1.0 - balance)).min(1.0) * 0.5;
            let late_gain = v.late * (2.0 * balance).min(1.0);
            let g_ap = 0.75 * self.get(id::DIFFUSION);
            let depth = 0.0015 * sr;
            let step = self.get(id::MOD_RATE) / sr;
            let width = self.get(id::WIDTH);
            let ducking = self.get(id::DUCKING);
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let frozen = self.freeze.tick();
                let mix = self.mix.tick();
                let modulation = self.modulation.tick() * depth;
                let shimmer = self.shimmer.tick();
                let live = 1.0 - frozen;
                // Pre-delay, then the early reflections from it.
                let mut early = [0.0; 2];
                let mut fed = [0.0; 2];
                for c in 0..2 {
                    self.pre[c].push(x[c] * live);
                    fed[c] = self.pre[c].tap(pre);
                }
                if spring {
                    // The springs ring on what comes in; they are the
                    // early part and feed the network.
                    for c in 0..2 {
                        early[c] = self.springs[c].process(fed[c]);
                        fed[c] += 0.5 * early[c];
                    }
                } else if early_gain > 0.0 {
                    for (t, g, left) in EARLY {
                        let d = pre + (t * early_span) as usize;
                        let (l, r) = (self.pre[0].tap(d), self.pre[1].tap(d + 7));
                        let (gl, gr) = if left { (1.0, 0.35) } else { (0.35, 1.0) };
                        early[0] += g * gl * l;
                        early[1] += g * gr * r;
                    }
                }
                // The shimmer: the last late output, shifted, band limited
                // and softly limited, in with the input.
                if shimmer > 0.0 {
                    for (c, f) in fed.iter_mut().enumerate() {
                        let y = self.shifters[c].process(self.late_out[c], ratio);
                        let y = self.shimmer_hp[c].high(self.shimmer_lp[c].low(y));
                        *f += shimmer * live * 0.5 * (y * 2.0).tanh();
                    }
                }
                // Diffused into the network: left into even lines, right
                // into odd ones.
                let mut inject = [0.0; 2];
                for c in 0..2 {
                    let mut s = fed[c];
                    for ap in &mut self.diffusers[c] {
                        s = ap.process(s, g_ap);
                    }
                    inject[c] = s * 0.6;
                }
                let mut y = [0.0; LINES];
                for (k, yk) in y.iter_mut().enumerate() {
                    let m = if modulation > 0.0 {
                        self.phase[k] = (self.phase[k] + step * self.rate[k]).fract();
                        modulation * (1.0 + (TAU * self.phase[k]).sin())
                    } else {
                        0.0
                    };
                    *yk = self.lines[k].read(self.length[k] + m - 1.0);
                }
                let (mut late_l, mut late_r) = (0.0, 0.0);
                let mut f = [0.0; LINES];
                for k in 0..LINES {
                    late_l += OUT_L[k] * y[k];
                    late_r += OUT_R[k] * y[k];
                    f[k] = self.decay[k].process(y[k]);
                }
                hadamard(&mut f);
                for k in 0..LINES {
                    let s = if k % 4 < 2 { 1.0 } else { -1.0 };
                    self.lines[k].push(flush(f[k] + s * inject[k % 2]));
                }
                self.late_out = [late_l * 0.25, late_r * 0.25];
                let late = [late_l * 0.25 * late_gain, late_r * 0.25 * late_gain];
                let mut wet = [0.0; 2];
                for c in 0..2 {
                    let w = early[c] * early_gain + late[c];
                    let w = self.high_cut.process(c, self.low_cut.process(c, w));
                    wet[c] = self.high_shelf.process(c, self.low_shelf.process(c, w));
                }
                // Width, ducking.
                let mid = 0.5 * (wet[0] + wet[1]);
                let side = 0.5 * (wet[0] - wet[1]) * width;
                let lvl = x[0].abs().max(x[1].abs());
                self.duck_env = if lvl > self.duck_env {
                    self.duck_env + (lvl - self.duck_env) * duck_attack
                } else {
                    self.duck_env * duck_release
                };
                let want = if ducking > 0.0 && frozen < 0.5 {
                    gain(-ducking * 0.6 * (db(self.duck_env) + 40.0).clamp(0.0, 40.0))
                } else {
                    1.0
                };
                self.duck += (want - self.duck) * duck_attack;
                let w = [(mid + side) * self.duck, (mid - side) * self.duck];
                wet_peak = wet_peak.max(w[0].abs()).max(w[1].abs());
                let o = [
                    x[0] * (1.0 - mix) + w[0] * mix,
                    x[1] * (1.0 - mix) + w[1] * mix,
                ];
                out_peak = out_peak.max(o[0].abs()).max(o[1].abs());
                if channels == 1 {
                    out.channel_mut(0)[i] = (0.5 * (o[0] + o[1])) as f32;
                } else {
                    out.channel_mut(0)[i] = o[0] as f32;
                    out.channel_mut(1)[i] = o[1] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for d in &mut self.decay {
            d.low.flush();
            d.high.flush();
        }
        self.low_cut.flush();
        self.high_cut.flush();
        self.low_shelf.flush();
        self.high_shelf.flush();
        for c in 0..2 {
            self.springs[c].flush();
            self.shimmer_lp[c].flush();
            self.shimmer_hp[c].flush();
            self.late_out[c] = flush(self.late_out[c]);
        }
        self.duck_env = flush(self.duck_env);
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.raise_value(value::WET_PEAK, wet_peak as f32);
        self.tap
            .set_value(value::DUCK, (-db(self.duck.max(1e-6))) as f32);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        if watched {
            self.tap
                .input
                .push(&self.scratch[0][..n], &self.scratch[1][..n]);
            let r = out.channel(1.min(channels - 1));
            self.tap.output.push(&out.channel(0)[..n], &r[..n]);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.reset();
        }
        for c in 0..2 {
            self.pre[c].reset();
            for ap in &mut self.diffusers[c] {
                ap.reset();
            }
            self.springs[c].reset();
            self.shifters[c].reset();
            self.shimmer_lp[c].reset();
            self.shimmer_hp[c].reset();
        }
        self.late_out = [0.0; 2];
        for d in &mut self.decay {
            d.low.reset();
            d.high.reset();
        }
        self.low_cut.reset();
        self.high_cut.reset();
        self.low_shelf.reset();
        self.high_shelf.reset();
        self.duck_env = 0.0;
        self.duck = 1.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<ReverbProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, ReverbProcessor::new)
    }

    fn click(n: usize) -> (f32, f32) {
        if n == 0 { (1.0, 1.0) } else { (0.0, 0.0) }
    }

    /// The reverberation time of an impulse response (s): Schroeder's
    /// backward integration, the fall from −5 to −25 dB, times three.
    fn rt60(ir: &[f32]) -> f64 {
        let mut energy: Vec<f64> = ir.iter().map(|v| f64::from(*v).powi(2)).collect();
        for i in (0..energy.len() - 1).rev() {
            energy[i] += energy[i + 1];
        }
        let total = energy[0];
        let at = |d: f64| {
            energy
                .iter()
                .position(|e| 10.0 * (e / total).log10() < d)
                .unwrap() as f64
                / SR
        };
        3.0 * (at(-25.0) - at(-5.0))
    }

    #[test]
    fn the_decay_is_as_set() {
        for (kind, rt) in [
            (1.0, 2.0),
            (0.0, 0.8),
            (2.0, 4.0),
            (3.0, 1.5),
            (4.0, 0.5),
            (5.0, 1.2),
            (6.0, 3.0),
        ] {
            let mut r = rig(&[
                (id::TYPE, kind),
                (id::DECAY, rt),
                (id::BASS, 1.0),
                (id::DAMPING, 20_000.0),
                (id::MIX, 1.0),
                (id::BALANCE, 1.0),
                (id::MODULATION, 0.0),
                (id::PRE_DELAY, 0.0),
            ]);
            let (l, _) = r.run(rt * 1.6 + 0.3, click, silence);
            let measured = rt60(&l);
            assert!(
                (measured / rt - 1.0).abs() < 0.12,
                "{}: {measured:.2} s for {rt} s",
                TYPES[kind as usize]
            );
            // No DC, nothing out of hand.
            let mean = l.iter().map(|v| f64::from(*v)).sum::<f64>() / l.len() as f64;
            assert!(mean.abs() < 1e-3, "{}: DC {mean}", TYPES[kind as usize]);
            assert!(l.iter().all(|v| v.is_finite() && v.abs() < 2.0));
        }
    }

    #[test]
    fn the_highs_decay_over_their_own_multiple() {
        let fall = |mult: f64| {
            let mut r = rig(&[
                (id::DECAY, 3.0),
                (id::DAMPING, 3_000.0),
                (id::HIGH_DECAY, mult),
                (id::BASS, 1.0),
                (id::MIX, 1.0),
                (id::BALANCE, 1.0),
                (id::MODULATION, 0.0),
            ]);
            r.run(0.4, tone(8_000.0, 0.3), silence);
            let (a, _) = r.run(0.05, silence, silence);
            r.run(0.5, silence, silence);
            let (b, _) = r.run(0.05, silence, silence);
            bin_db(&a, 8_000.0) - bin_db(&b, 8_000.0)
        };
        // Over 0.55 s: 60 dB per 0.6 s at a fifth, per 3 s at one.
        let (short, long) = (fall(0.2), fall(1.0));
        assert!(short > 30.0 && long < 16.0, "−{short:.1} vs −{long:.1} dB");
        // A third is the default: the same as the first version's rt / 3.
        let rts = decay_times(&ParamValues::new(parameters()));
        assert!((rts[2] - rts[1] / 3.0).abs() < 1e-6);
    }

    #[test]
    fn the_pre_delay_follows_the_tempo_when_synced() {
        let mut r = rig(&[(id::PRE_SYNC, 1.0), (id::PRE_DIVISION, 5.0), (id::MIX, 1.0)]);
        // 1/16 at 120 BPM: 125 ms.
        r.transport.tempo = 120.0;
        let (l, _) = r.run(0.4, click, silence);
        assert!(l[..6_000].iter().all(|v| v.abs() < 1e-9));
        assert!(l[6_000..9_000].iter().any(|v| v.abs() > 1e-3));
        // Too long a division clamps to the longest pre-delay.
        let mut r = rig(&[
            (id::PRE_SYNC, 1.0),
            (id::PRE_DIVISION, 20.0),
            (id::MIX, 1.0),
        ]);
        r.transport.tempo = 120.0;
        let (l, _) = r.run(1.3, click, silence);
        assert!(l[..48_000].iter().all(|v| v.abs() < 1e-9));
        assert!(l[48_000..52_000].iter().any(|v| v.abs() > 1e-3));
    }

    #[test]
    fn the_shelves_shape_the_tail() {
        let at = |set: &[(u32, f64)], f: f64| {
            let mut all = vec![(id::MIX, 1.0), (id::BALANCE, 1.0), (id::DECAY, 2.0)];
            all.extend_from_slice(set);
            let mut r = rig(&all);
            let (l, _) = r.run(1.5, tone(f, 0.3), silence);
            bin_db(&l, f)
        };
        let (low, high) = (at(&[], 100.0), at(&[], 8_000.0));
        assert!(at(&[(id::LOW_SHELF, -12.0)], 100.0) < low - 8.0);
        assert!(at(&[(id::LOW_SHELF, 6.0)], 100.0) > low + 4.0);
        assert!(at(&[(id::HIGH_SHELF, -12.0)], 8_000.0) < high - 8.0);
        assert!(at(&[(id::HIGH_SHELF, 6.0)], 8_000.0) > high + 4.0);
        // The bass crossover moves where the bass multiplier takes over:
        // a 400 Hz tone rings longer with the crossover above it.
        let fall = |bass_hz: f64| {
            let mut r = rig(&[
                (id::DECAY, 2.0),
                (id::BASS, 2.0),
                (id::BASS_FREQ, bass_hz),
                (id::DAMPING, 20_000.0),
                (id::MIX, 1.0),
                (id::BALANCE, 1.0),
            ]);
            r.run(0.4, tone(400.0, 0.3), silence);
            let (a, _) = r.run(0.05, silence, silence);
            r.run(0.5, silence, silence);
            let (b, _) = r.run(0.05, silence, silence);
            bin_db(&a, 400.0) - bin_db(&b, 400.0)
        };
        assert!(fall(800.0) < fall(100.0) - 5.0);
    }

    #[test]
    fn shimmer_climbs_an_octave_and_stays_bounded() {
        let octave = |shimmer: f64| {
            let mut r = rig(&[
                (id::TYPE, 1.0),
                (id::DECAY, 4.0),
                (id::SHIMMER, shimmer),
                (id::SHIMMER_PITCH, 4.0),
                (id::DAMPING, 20_000.0),
                (id::MIX, 1.0),
                (id::BALANCE, 1.0),
            ]);
            r.run(0.5, tone(440.0, 0.3), silence);
            let (l, _) = r.run(1.5, silence, silence);
            bin_db(&l, 880.0) - bin_db(&l, 440.0)
        };
        let (plain, shimmered) = (octave(0.0), octave(1.0));
        assert!(shimmered > plain + 12.0, "{plain:.1} → {shimmered:.1} dB");
        // At the longest decay with full shimmer it swells, never explodes.
        let mut r = rig(&[
            (id::DECAY, 20.0),
            (id::SHIMMER, 1.0),
            (id::MIX, 1.0),
            (id::BALANCE, 1.0),
        ]);
        r.run(1.0, tone(440.0, 0.5), silence);
        let (l, _) = r.run(6.0, silence, silence);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() < 4.0));
        // Off, the shifters do not run: the plain hall, untouched.
        let mut a = rig(&[(id::MIX, 1.0), (id::SHIMMER_PITCH, 0.0)]);
        let mut b = rig(&[(id::MIX, 1.0)]);
        assert_eq!(a.run(0.5, click, silence), b.run(0.5, click, silence));
    }

    #[test]
    fn the_spring_chirps_where_a_plate_does_not() {
        // A click through the springs: the band round 2.8 kHz arrives
        // spread out (the chain's group delay), a plate's does not.
        let spread = |kind: f64| {
            let mut r = rig(&[
                (id::TYPE, kind),
                (id::DECAY, 1.0),
                (id::MIX, 1.0),
                (id::BALANCE, 0.0),
                (id::PRE_DELAY, 0.0),
                (id::DAMPING, 20_000.0),
            ]);
            let (l, _) = r.run(0.3, click, silence);
            // The energy's centre of time over the first 60 ms.
            let (mut sum, mut moment) = (0.0, 0.0);
            for (i, v) in l[..2_880].iter().enumerate() {
                let e = f64::from(*v).powi(2);
                sum += e;
                moment += e * i as f64;
            }
            (sum, moment / sum.max(1e-12) / SR * 1000.0)
        };
        let (spring_energy, spring_centre) = spread(SPRING as f64);
        assert!(spring_energy > 1e-4, "the springs sound ({spring_energy})");
        assert!(
            spring_centre > 10.0,
            "the springs' first bounce is spread late: {spring_centre:.1} ms"
        );
        let (_, ambience_centre) = spread(4.0);
        assert!(ambience_centre < spring_centre);
    }

    #[test]
    fn highs_die_sooner_than_the_bass_and_the_sides_differ() {
        let mut r = rig(&[
            (id::DECAY, 3.0),
            (id::DAMPING, 3_000.0),
            (id::BASS, 1.5),
            (id::MIX, 1.0),
        ]);
        r.run(0.4, tone(150.0, 0.3), silence);
        let (l, _) = r.run(0.05, silence, silence);
        let (low0, high0) = (bin_db(&l, 150.0), 0.0);
        let _ = high0;
        r.run(1.0, silence, silence);
        let (l2, _) = r.run(0.05, silence, silence);
        let low_fall = low0 - bin_db(&l2, 150.0);
        let mut r = rig(&[
            (id::DECAY, 3.0),
            (id::DAMPING, 3_000.0),
            (id::BASS, 1.5),
            (id::MIX, 1.0),
        ]);
        r.run(0.4, tone(8_000.0, 0.3), silence);
        let (h, _) = r.run(0.05, silence, silence);
        let h0 = bin_db(&h, 8_000.0);
        r.run(1.0, silence, silence);
        let (h2, _) = r.run(0.05, silence, silence);
        let high_fall = h0 - bin_db(&h2, 8_000.0);
        // Over 1.05 s: the bass falls 60/4.5 dB a second, the highs 60/1.
        assert!(
            high_fall > 2.5 * low_fall,
            "low −{low_fall:.1}, high −{high_fall:.1}"
        );
        // Left and right are decorrelated.
        let mut r = rig(&[(id::MIX, 1.0), (id::BALANCE, 1.0)]);
        let (l, rr) = r.run(1.0, click, silence);
        let tail = 6_000..l.len();
        let (mut lr, mut ll, mut rr2) = (0.0, 0.0, 0.0);
        for i in tail {
            lr += f64::from(l[i]) * f64::from(rr[i]);
            ll += f64::from(l[i]).powi(2);
            rr2 += f64::from(rr[i]).powi(2);
        }
        let corr = lr / (ll * rr2).sqrt();
        assert!(corr.abs() < 0.3, "{corr:.2}");
    }

    #[test]
    fn freeze_holds_and_pre_delay_waits() {
        let mut r = rig(&[(id::DECAY, 1.0), (id::MIX, 1.0)]);
        r.run(0.3, tone(500.0, 0.3), silence);
        r.set(id::FREEZE, 1.0);
        let (a, _) = r.run(0.5, silence, silence);
        let (b, _) = r.run(2.0, tone(1_234.0, 0.5), silence);
        let level = |x: &[f32]| {
            10.0 * (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).log10()
        };
        let (la, lb) = (level(&a[a.len() / 2..]), level(&b[b.len() / 2..]));
        assert!((la - lb).abs() < 1.5, "{la:.1} → {lb:.1}");
        assert!(bin_db(&b, 1_234.0) < lb - 30.0, "the input stays out");
        // Pre-delay: nothing before it.
        let mut r = rig(&[(id::PRE_DELAY, 100.0), (id::MIX, 1.0)]);
        let (l, _) = r.run(0.3, click, silence);
        assert!(l[..4_800].iter().all(|v| v.abs() < 1e-9));
        assert!(l[4_800..9_600].iter().any(|v| v.abs() > 1e-3));
    }

    #[test]
    fn the_level_is_sensible_and_dry_passes_at_zero_mix() {
        let mut r = rig(&[(id::MIX, 1.0)]);
        let (l, _) = r.run(3.0, click, silence);
        let energy: f64 = l.iter().map(|v| f64::from(*v).powi(2)).sum();
        let e = 10.0 * energy.log10();
        assert!(e > -12.0 && e < 3.0, "{e:.1} dB");
        let mut r = rig(&[(id::MIX, 0.0)]);
        let input = tone(700.0, 0.4);
        let (l, _) = r.run(0.2, &input, silence);
        for (i, v) in l.iter().enumerate().take(4_000) {
            assert!((v - input(i).0).abs() < 1e-6);
        }
    }
}
