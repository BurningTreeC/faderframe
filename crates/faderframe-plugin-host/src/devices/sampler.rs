//! Sampler: plays one sample across the keyboard, or an SFZ instrument.
//!
//! With a single sample its root key, loop (off, forward, while held:
//! start and end with a crossfade), start and end points, direction and
//! whether the pitch follows the keys are the device's settings; with an
//! SFZ the regions' opcodes apply (see [`super::sfz`]: conditions,
//! keyswitches, triggers, crossfades, two filters and an EQ, three
//! envelopes and three LFOs, controller modulation), the device's settings
//! on top. Every voice has an amplifier envelope (the device's ADSR, or a
//! region's `ampeg_*`), the device's filter (12 or 24 dB low pass, band,
//! high pass) the envelope can open, and reads its sample by cubic
//! interpolation at the ratio of the pitch and the sample's rate to the
//! host's. A region's modulation (pitch, level, pan, width, filters, EQ)
//! is evaluated every 16 samples, gains ramped between.

use super::keep_length::{KeepLength, Render};
use super::samples::{LoopMode, Sample, Shared, Zone};
use super::sfz::{
    CC_BEND, CC_CHANAFT, CC_KEY, CC_POLYAFT, CC_RANDOM_BI, CC_RANDOM_UNI, CC_VELOCITY, CONTROLLERS,
    CcMod, EgParam, EgSpec, FilterKind, Trigger, Xfade,
};
use super::{on_off, param, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, db_to_gain};
use faderframe_midi::MidiEvent;
use std::f32::consts::PI;
use std::sync::Arc;

pub mod id {
    pub const VOLUME: u32 = 0;
    pub const TRANSPOSE: u32 = 1;
    pub const TUNE: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const DECAY: u32 = 4;
    pub const SUSTAIN: u32 = 5;
    pub const RELEASE: u32 = 6;
    pub const CUTOFF: u32 = 7;
    pub const RESONANCE: u32 = 8;
    pub const FILTER_TYPE: u32 = 9;
    pub const FILTER_ENV: u32 = 10;
    pub const VELOCITY: u32 = 11;
    pub const ROOT: u32 = 12;
    pub const LOOP: u32 = 13;
    pub const LOOP_START: u32 = 14;
    pub const LOOP_END: u32 = 15;
    pub const START: u32 = 16;
    pub const CROSSFADE: u32 = 17;
    pub const POLYPHONY: u32 = 18;
    pub const PAN: u32 = 19;
    pub const REVERSE: u32 = 20;
    pub const KEY_TRACK: u32 = 21;
    /// 0: the pitch moves with the speed (repitch); 1: the length stays.
    pub const PITCH_MODE: u32 = 22;
    /// Where playing stops (0–1 of the sample; the start to here plays,
    /// backwards when reversed).
    pub const END: u32 = 23;
}

/// Published: voices sounding, the newest voice's key and where it plays
/// (0–1 of its sample) and which zone (−1 the single sample), the output's
/// peak (linear), and the keys held (see [`key_held`]).
pub mod value {
    pub const VOICES: usize = 0;
    pub const NOTE: usize = 1;
    pub const POSITION: usize = 2;
    pub const ZONE: usize = 3;
    pub const OUT_PEAK: usize = 4;
    /// The first of [`super::KEY_WORDS`] values of 24 key bits each.
    pub const KEYS: usize = 5;
}
/// Values that hold the held keys (24 keys each, exact in an `f32`).
pub const KEY_WORDS: usize = 6;
pub const TAP_VALUES: usize = value::KEYS + KEY_WORDS;

/// Whether `key` is held, from the published values (`value(i)` reads
/// value `i`).
pub fn key_held(value: impl Fn(usize) -> f32, key: u8) -> bool {
    let k = usize::from(key);
    let word = value(value::KEYS + k / 24) as u32;
    (word >> (k % 24)) & 1 == 1
}

pub const FILTERS: [&str; 4] = ["LP 12", "LP 24", "Band", "High"];
pub const LOOPS: [&str; 3] = ["Off", "Loop", "While Held"];
pub const PITCH_MODES: [&str; 2] = ["Repitch", "Keep Length"];

const MAX_VOICES: usize = 64;

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::VOLUME, "Volume", -48.0, 12.0, -6.0, Decibels),
        ranged(id::TRANSPOSE, "Transpose", -24.0, 24.0, 0.0),
        param(id::TUNE, "Tune", -100.0, 100.0, 0.0, None),
        param(id::ATTACK, "Attack", 0.5, 10_000.0, 1.0, Milliseconds),
        param(id::DECAY, "Decay", 1.0, 20_000.0, 1_000.0, Milliseconds),
        param(id::SUSTAIN, "Sustain", 0.0, 1.0, 1.0, Percent),
        param(id::RELEASE, "Release", 5.0, 20_000.0, 250.0, Milliseconds),
        param(id::CUTOFF, "Cutoff", 20.0, 20_000.0, 20_000.0, Hertz),
        param(id::RESONANCE, "Resonance", 0.0, 1.0, 0.1, Percent),
        stepped(id::FILTER_TYPE, "Filter Type", 3.0, 0.0),
        param(id::FILTER_ENV, "Filter Envelope", -1.0, 1.0, 0.0, Percent),
        param(id::VELOCITY, "Velocity", 0.0, 1.0, 0.8, Percent),
        ranged(id::ROOT, "Root Key", 0.0, 127.0, 60.0),
        stepped(id::LOOP, "Loop", 2.0, 0.0),
        param(id::LOOP_START, "Loop Start", 0.0, 1.0, 0.0, Percent),
        param(id::LOOP_END, "Loop End", 0.0, 1.0, 1.0, Percent),
        param(id::START, "Start", 0.0, 1.0, 0.0, Percent),
        param(
            id::CROSSFADE,
            "Loop Crossfade",
            0.0,
            500.0,
            10.0,
            Milliseconds,
        ),
        ranged(id::POLYPHONY, "Voices", 1.0, MAX_VOICES as f64, 32.0),
        param(id::PAN, "Pan", -1.0, 1.0, 0.0, None),
        stepped(id::REVERSE, "Reverse", 1.0, 0.0),
        stepped(id::KEY_TRACK, "Key Tracking", 1.0, 1.0),
        stepped(id::PITCH_MODE, "Pitch", 1.0, 0.0),
        param(id::END, "End", 0.0, 1.0, 1.0, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::FILTER_TYPE => pick(&FILTERS, v),
        id::LOOP => pick(&LOOPS, v),
        id::PITCH_MODE => pick(&PITCH_MODES, v),
        id::REVERSE | id::KEY_TRACK => on_off(v),
        id::ROOT => crate::devices::note_name(v.round() as i32),
        id::TRANSPOSE => format!("{:+.0} st", v.round()).replace("+0 st", "0 st"),
        id::TUNE => format!("{v:+.0} ¢").replace("+0 ¢", "0 ¢"),
        id::POLYPHONY => format!("{:.0}", v.round()),
        id::CUTOFF if v >= 19_500.0 => "Open".into(),
        id::PAN if v.abs() < 0.005 => "C".into(),
        id::PAN if v < 0.0 => format!("L {:.0}", -v * 100.0),
        id::PAN => format!("R {:.0}", v * 100.0),
        id::FILTER_ENV => format!("{:+.0} %", v * 100.0).replace("+0 %", "0 %"),
        _ => return None,
    })
}

/// Read channel `c` of a sample at a fractional frame (cubic Hermite).
#[inline]
pub(crate) fn read(s: &Sample, c: usize, pos: f64) -> f32 {
    let d = &s.data[c];
    let n = d.len();
    if n == 0 || pos < -1.0 || pos >= n as f64 {
        return 0.0;
    }
    let i = pos.floor() as isize;
    let t = (pos - i as f64) as f32;
    let at = |k: isize| -> f32 {
        if k < 0 || k as usize >= n {
            0.0
        } else {
            d[k as usize]
        }
    };
    let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// The voice's sample at its position (crossfaded into the loop start near
/// the loop's end), and on by `step`: the frame and whether that was the
/// end of the sample.
fn next_frame(v: &mut Voice, s: &Sample, step: f64) -> ([f32; 2], bool) {
    let stereo = s.stereo;
    let mut y = [
        read(s, 0, v.pos),
        if stereo { read(s, 1, v.pos) } else { 0.0 },
    ];
    if let Some((ls, le)) = v.looping
        && v.fade > 0.0
        && !v.reverse
        && v.pos > le - v.fade
    {
        let t = ((v.pos - (le - v.fade)) / v.fade) as f32;
        let back = v.pos - (le - ls);
        y[0] = y[0] * (1.0 - t) + read(s, 0, back) * t;
        if stereo {
            y[1] = y[1] * (1.0 - t) + read(s, 1, back) * t;
        }
    }
    if v.looping.is_none() {
        // Fade the last 3 ms before the end (an end point mid-waveform
        // would click).
        let left = if v.reverse {
            v.pos - v.end
        } else {
            v.end - v.pos
        };
        let fade = s.rate * 0.003;
        if left < fade {
            let g = (left / fade).max(0.0) as f32;
            y[0] *= g;
            y[1] *= g;
        }
    }
    if !stereo {
        y[1] = y[0];
    }
    let ended = if v.reverse {
        v.pos -= step;
        v.pos < v.end
    } else {
        v.pos += step;
        match v.looping {
            Some((ls, le)) if v.pos >= le => {
                v.pos -= le - ls;
                false
            }
            _ => v.pos >= v.end,
        }
    };
    (y, ended)
}

/// A Keep Length voice's sample at its own speed into `a`/`b`: the frames
/// it had (fewer once it ended).
fn feed_voice(v: &mut Voice, s: &Sample, step: f64, a: &mut [f32], b: &mut [f32]) -> usize {
    if v.fed_out {
        return 0;
    }
    for k in 0..a.len().min(b.len()) {
        let (y, ended) = next_frame(v, s, step);
        a[k] = y[0];
        b[k] = y[1];
        if ended {
            v.fed_out = true;
            return k + 1;
        }
    }
    a.len().min(b.len())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EgStage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Done,
}

/// An envelope: delay, attack (linear, from `start`), hold, decay and
/// release (exponential), sustain.
#[derive(Clone, Copy, Debug)]
struct Eg {
    stage: EgStage,
    level: f32,
    /// Samples left in the stage (delay, attack, hold).
    left: u32,
    start: f32,
    attack: u32,
    hold: u32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

/// A coefficient that falls by 60 dB in `samples`.
fn fall(samples: f32) -> f32 {
    (-6.9 / samples.max(1.0)).exp()
}

impl Eg {
    const OFF: Eg = Eg {
        stage: EgStage::Done,
        level: 0.0,
        left: 0,
        start: 0.0,
        attack: 0,
        hold: 0,
        decay_coef: 0.0,
        sustain: 1.0,
        release_coef: 0.0,
    };

    /// Times in samples.
    fn new(
        delay: f32,
        start: f32,
        attack: f32,
        hold: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    ) -> Self {
        Self {
            stage: EgStage::Delay,
            level: 0.0,
            left: delay.max(0.0) as u32,
            start: start.clamp(0.0, 1.0),
            attack: attack.max(0.0) as u32,
            hold: hold.max(0.0) as u32,
            decay_coef: if decay <= 0.0 { 0.0 } else { fall(decay) },
            sustain: sustain.clamp(0.0, 1.0),
            release_coef: fall(release),
        }
    }

    #[inline]
    fn tick(&mut self) -> f32 {
        match self.stage {
            EgStage::Delay => {
                if self.left == 0 {
                    self.stage = EgStage::Attack;
                    self.level = self.start;
                    self.left = self.attack;
                } else {
                    self.left -= 1;
                }
            }
            EgStage::Attack => {
                if self.left == 0 {
                    self.level = 1.0;
                    self.stage = EgStage::Hold;
                    self.left = self.hold;
                } else {
                    self.level += (1.0 - self.level) / self.left as f32;
                    self.left -= 1;
                }
            }
            EgStage::Hold => {
                if self.left == 0 {
                    self.stage = EgStage::Decay;
                } else {
                    self.left -= 1;
                }
            }
            EgStage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coef;
                if (self.level - self.sustain).abs() < 1e-4 {
                    self.level = self.sustain;
                    self.stage = EgStage::Sustain;
                }
            }
            EgStage::Sustain => self.level = self.sustain,
            EgStage::Release => {
                self.level *= self.release_coef;
                if self.level < 1e-4 {
                    self.level = 0.0;
                    self.stage = EgStage::Done;
                }
            }
            EgStage::Done => self.level = 0.0,
        }
        self.level
    }

    /// Into the release (a voice released before it sounded is done).
    fn release(&mut self) {
        self.stage = match self.stage {
            EgStage::Delay => EgStage::Done,
            EgStage::Done => EgStage::Done,
            _ => EgStage::Release,
        };
    }

    /// Into a quick release (`samples` long).
    fn cut(&mut self, samples: f32) {
        self.release_coef = fall(samples);
        self.release();
    }

    fn summary(&self) -> Stage {
        match self.stage {
            EgStage::Delay | EgStage::Attack => Stage::Attack,
            EgStage::Hold | EgStage::Decay => Stage::Decay,
            EgStage::Sustain => Stage::Sustain,
            EgStage::Release => Stage::Release,
            EgStage::Done => Stage::Idle,
        }
    }
}

/// A filter's state per channel: up to three SVF stages, or a biquad's
/// history.
#[derive(Clone, Copy, Debug, Default)]
struct FilterState {
    s: [(f32, f32); 3],
}

/// A filter's coefficients for a stretch of samples.
#[derive(Clone, Copy, Debug)]
enum Coefs {
    Off,
    /// One-pole TPT (`g / (1 + g)`).
    One(f32),
    /// SVF (`a1`, `a2`, `a3`, `k`).
    Svf(f32, f32, f32, f32),
    /// Biquad (b0, b1, b2, a1, a2).
    Biquad([f32; 5]),
}

impl Coefs {
    fn svf(fc: f32, q: f32, sr: f32) -> Self {
        let g = (PI * fc.clamp(10.0, sr * 0.45) / sr).tan();
        let k = 1.0 / q.max(0.05);
        let a1 = 1.0 / (1.0 + g * (g + k));
        Self::Svf(a1, g * a1, g * g * a1, k)
    }

    /// RBJ peaking, low and high shelf (`kind` 0, 1, 2).
    fn rbj(kind: u8, fc: f32, q: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q.max(0.05));
        let (b0, b1, b2, a0, a1, a2) = match kind {
            0 => (
                1.0 + alpha * a,
                -2.0 * cs,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cs,
                1.0 - alpha / a,
            ),
            1 => {
                let r = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + r),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - r),
                    (a + 1.0) + (a - 1.0) * cs + r,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - r,
                )
            }
            _ => {
                let r = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + r),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - r),
                    (a + 1.0) - (a - 1.0) * cs + r,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - r,
                )
            }
        };
        Self::Biquad([b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0])
    }
}

/// One SVF stage: (low, band, high).
#[inline]
fn svf_stage(st: &mut (f32, f32), x: f32, a1: f32, a2: f32, a3: f32, k: f32) -> (f32, f32, f32) {
    let (ic1, ic2) = st;
    let v3 = x - *ic2;
    let v1 = a1 * *ic1 + a2 * v3;
    let v2 = *ic2 + a2 * *ic1 + a3 * v3;
    *ic1 = 2.0 * v1 - *ic1;
    *ic2 = 2.0 * v2 - *ic2;
    (v2, v1, x - k * v1 - v2)
}

/// A region filter on one sample.
#[inline]
fn run_filter(kind: FilterKind, c: Coefs, st: &mut FilterState, x: f32) -> f32 {
    match c {
        Coefs::Off => x,
        Coefs::One(gg) => {
            let s = &mut st.s[0].0;
            let v = (x - *s) * gg;
            let lp = v + *s;
            *s = lp + v;
            match kind {
                FilterKind::Hp1 => x - lp,
                _ => lp,
            }
        }
        Coefs::Svf(a1, a2, a3, k) => {
            let stages = match kind {
                FilterKind::Lp4 | FilterKind::Hp4 => 2,
                FilterKind::Lp6 | FilterKind::Hp6 => 3,
                _ => 1,
            };
            let mut y = x;
            for st in st.s.iter_mut().take(stages) {
                let (lp, bp, hp) = svf_stage(st, y, a1, a2, a3, k);
                y = match kind {
                    FilterKind::Lp2 | FilterKind::Lp4 | FilterKind::Lp6 => lp,
                    FilterKind::Hp2 | FilterKind::Hp4 | FilterKind::Hp6 => hp,
                    FilterKind::Bp2 | FilterKind::Bp1 => bp * k,
                    FilterKind::Br2 | FilterKind::Br1 => lp + hp,
                    _ => lp,
                };
            }
            y
        }
        Coefs::Biquad([b0, b1, b2, a1, a2]) => {
            let (x1, x2) = st.s[0];
            let (y1, y2) = st.s[1];
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            st.s[0] = (x, x1);
            st.s[1] = (y, y1);
            y
        }
    }
}

/// A region filter's coefficients for a cutoff, resonance (dB) and gain.
fn filter_coefs(kind: FilterKind, fc: f32, res_db: f32, gain_db: f32, sr: f32) -> Coefs {
    // Resonance in dB over a Butterworth Q.
    let q = std::f32::consts::FRAC_1_SQRT_2 * 10f32.powf(res_db / 20.0);
    match kind {
        FilterKind::Lp1 | FilterKind::Hp1 => {
            let g = (PI * fc.clamp(10.0, sr * 0.45) / sr).tan();
            Coefs::One(g / (1.0 + g))
        }
        FilterKind::Bp1 | FilterKind::Br1 => Coefs::svf(fc, 0.5, sr),
        FilterKind::Peak => Coefs::rbj(0, fc, q, gain_db, sr),
        FilterKind::LowShelf => Coefs::rbj(1, fc, q, gain_db, sr),
        FilterKind::HighShelf => Coefs::rbj(2, fc, q, gain_db, sr),
        _ => Coefs::svf(fc, q, sr),
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    stage: Stage,
    /// The amplifier's envelope (and the filter's and pitch's for zones
    /// that have them).
    amp: Eg,
    fil_eg: Eg,
    pitch_eg: Eg,
    /// Samples before the voice starts (a zone's `delay`).
    delay: u32,
    /// The zone (`usize::MAX`: the single sample) and its sample.
    zone: usize,
    sample: usize,
    pos: f64,
    /// The step at the voice's own pitch (before bend and modulation).
    step: f64,
    /// Loop range (frames) when looping, and the crossfade before its end.
    looping: Option<(f64, f64)>,
    loop_mode: LoopMode,
    fade: f64,
    /// Plays to `end` (frames; going backwards: down to it), from `start`.
    end: f64,
    start: f64,
    reverse: bool,
    /// Plays this many more times after this one (`count`).
    repeats: u32,
    gain: [f32; 2],
    /// The gain the last block ended with (ramped to the next).
    last_gain: [f32; 2],
    key: u8,
    channel: u8,
    velocity: f32,
    held: bool,
    pedal_held: bool,
    group: u32,
    off_by: u32,
    svf: [[(f32, f32); 2]; 2],
    /// Region filters and EQ per channel.
    zf: [[FilterState; 2]; 2],
    eqs: [[FilterState; 3]; 2],
    /// Random draws (filter cents ×2) and LFO phases (amp, filter, pitch).
    rnd: [f32; 2],
    lfo: [f32; 3],
    /// Seconds since the voice started (LFO delays and fades).
    age_s: f32,
    age: u64,
    /// Keep Length: the stretcher it plays through and its pitch factor;
    /// its sample has all been fed.
    slot: Option<u8>,
    pitch: f32,
    fed_out: bool,
}

impl Voice {
    const IDLE: Voice = Voice {
        stage: Stage::Idle,
        amp: Eg::OFF,
        fil_eg: Eg::OFF,
        pitch_eg: Eg::OFF,
        delay: 0,
        zone: usize::MAX,
        sample: 0,
        pos: 0.0,
        step: 1.0,
        looping: None,
        loop_mode: LoopMode::NoLoop,
        fade: 0.0,
        end: 0.0,
        start: 0.0,
        reverse: false,
        repeats: 0,
        gain: [1.0; 2],
        last_gain: [0.0; 2],
        key: 0,
        channel: 0,
        velocity: 0.0,
        held: false,
        pedal_held: false,
        group: 0,
        off_by: 0,
        svf: [[(0.0, 0.0); 2]; 2],
        zf: [[FilterState { s: [(0.0, 0.0); 3] }; 2]; 2],
        eqs: [[FilterState { s: [(0.0, 0.0); 3] }; 3]; 2],
        rnd: [0.0; 2],
        lfo: [0.0; 3],
        age_s: 0.0,
        age: 0,
        slot: None,
        pitch: 1.0,
        fed_out: false,
    };

    fn release(&mut self) {
        self.held = false;
        if self.stage != Stage::Idle && self.loop_mode != LoopMode::OneShot {
            self.amp.release();
            self.fil_eg.release();
            self.pitch_eg.release();
            self.stage = self.amp.summary();
        }
        // Loop while held: play on out of the loop.
        if self.loop_mode == LoopMode::Sustain {
            self.looping = None;
        }
    }

    /// Stop quickly (`samples` long): chokes and polyphony limits.
    fn cut(&mut self, samples: f32) {
        self.held = false;
        self.amp.cut(samples);
        self.stage = self.amp.summary();
        self.loop_mode = LoopMode::NoLoop;
    }
}

/// What a note-on is: an attack, or a release trigger (with the
/// attenuation its held time gives).
#[derive(Clone, Copy, Debug, PartialEq)]
enum NoteKind {
    Attack,
    Release {
        held_s: f32,
    },
    ReleaseKey {
        held_s: f32,
    },
    /// A controller entered a zone's `on_cc` range.
    Controller,
}

/// MIDI state the zones' conditions and modulation read.
struct Controls {
    /// Controller values 0–1 per channel (the extended ones too).
    cc: Box<[[f32; CONTROLLERS]; 16]>,
    /// Polyphonic aftertouch per channel and key (0–1).
    polyaft: Box<[[f32; 128]; 16]>,
    bend_raw: [i32; 16],
    keys_down: [[bool; 128]; 16],
    key_velocity: [[u8; 128]; 16],
    /// When each key went down (samples since the processor started).
    key_at: Box<[[u64; 128]; 16]>,
    /// Release triggers held back by the pedal, per channel and key.
    pedal_releases: [[bool; 128]; 16],
    held: u32,
    last_keyswitch: Option<u8>,
    previous: Option<(u8, u8)>,
    tempo: f64,
}

impl Controls {
    fn new() -> Self {
        Self {
            cc: Box::new([[0.0; CONTROLLERS]; 16]),
            polyaft: Box::new([[0.0; 128]; 16]),
            bend_raw: [0; 16],
            keys_down: [[false; 128]; 16],
            key_velocity: [[0; 128]; 16],
            key_at: Box::new([[0; 128]; 16]),
            pedal_releases: [[false; 128]; 16],
            held: 0,
            last_keyswitch: None,
            previous: None,
            tempo: 120.0,
        }
    }

    /// A controller's value (0–1) through a curve.
    #[inline]
    fn value(&self, channel: u8, m: &CcMod, curves: &[[f32; 128]]) -> f32 {
        let v = self.cc[(channel & 15) as usize][m.cc.min(CONTROLLERS - 1)];
        match m.curve.and_then(|c| curves.get(c)) {
            Some(curve) => curve[(v * 127.0).round().clamp(0.0, 127.0) as usize],
            None => v,
        }
    }

    /// The sum of modulations.
    #[inline]
    fn sum(&self, channel: u8, mods: &[CcMod], curves: &[[f32; 128]]) -> f32 {
        mods.iter()
            .map(|m| m.amount * self.value(channel, m, curves))
            .sum()
    }
}

/// A crossfade's gain at `x` (power or gain curve).
fn xfade_gain(x: &Xfade, value: f32, power: bool) -> f32 {
    let t = if x.hi <= x.lo {
        if value >= x.hi { 1.0 } else { 0.0 }
    } else {
        ((value - x.lo) / (x.hi - x.lo)).clamp(0.0, 1.0)
    };
    let t = if x.fade_in { t } else { 1.0 - t };
    if power { t.sqrt() } else { t }
}

/// An envelope's depth (cents) for a note.
fn eg_depth(e: &EgSpec, c: &Controls, ch: u8, vel: f32, curves: &[[f32; 128]]) -> f32 {
    e.depth
        + e.vel2depth * vel
        + e.cc
            .iter()
            .filter(|(p, _)| *p == EgParam::Depth)
            .map(|(_, m)| m.amount * c.value(ch, m, curves))
            .sum::<f32>()
}

pub struct SamplerProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    watching: Watching,
    shared: Shared,
    sr: f64,
    voices: [Voice; MAX_VOICES],
    counter: u64,
    generation: u64,
    sustain: [bool; 16],
    bend: [f32; 16],
    bend_range: [f32; 16],
    rpn: [(u8, u8); 16],
    /// Round robin counters per key, and the random draw.
    rounds: [u32; 128],
    random: u32,
    /// Release triggers due (from note-offs and the pedal).
    pending_release: [(u8, u8, u8, NoteKind); 32],
    pending: usize,
    controls: Controls,
    /// Samples processed (for release triggers' decay).
    clock: u64,
    /// The block's two sides before they go out.
    mix: [Vec<f32>; 2],
    meters: [MeterTap; 2],
    /// Stretchers for Keep Length (made when the mode is on).
    keep: Option<KeepLength>,
}

impl SamplerProcessor {
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        shared: Shared,
    ) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        Self {
            tap,
            watching: Watching::new(sr as f32),
            shared,
            sr,
            voices: [Voice::IDLE; MAX_VOICES],
            counter: 0,
            generation: 0,
            sustain: [false; 16],
            bend: [0.0; 16],
            bend_range: [2.0; 16],
            rpn: [(127, 127); 16],
            rounds: [0; 128],
            random: 0x2545_f491,
            pending_release: [(0, 0, 0, NoteKind::Attack); 32],
            pending: 0,
            controls: Controls::new(),
            clock: 0,
            mix: [vec![0.0; block], vec![0.0; block]],
            meters: [MeterTap::new(sr as f32); 2],
            keep: (params.get(id::PITCH_MODE as usize) >= 0.5)
                .then(|| KeepLength::new(sr, block))
                .flatten(),
            params,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(pid as usize))
    }

    fn rand(&mut self) -> f32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        self.random as f32 / u32::MAX as f32
    }

    fn free_voice(&self) -> usize {
        let poly = (self.get(id::POLYPHONY).round() as usize).clamp(1, MAX_VOICES);
        let voices = &self.voices[..poly];
        voices
            .iter()
            .position(|v| v.stage == Stage::Idle)
            .or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.stage == Stage::Release)
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            })
    }

    /// New samples: controllers start where the instrument says, the
    /// keyswitch at its default.
    fn adopt(&mut self, set: &super::samples::SampleSet) {
        if let Some(inst) = &set.instrument {
            for c in self.controls.cc.iter_mut() {
                c.copy_from_slice(&inst.initial_cc[..]);
            }
        }
        self.controls.last_keyswitch = set.zones.iter().find_map(|z| z.sw_default);
    }

    /// Start a voice on `zone` (or the single sample) of `set`.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        set: &super::samples::SampleSet,
        zone: Option<&Zone>,
        zone_index: usize,
        sample: usize,
        channel: u8,
        key: u8,
        velocity: u8,
        kind: NoteKind,
    ) {
        let Some(s) = set.samples.get(sample) else {
            return;
        };
        let sr = self.sr;
        let srf = sr as f32;
        let curves: &[[f32; 128]] = set.instrument.as_ref().map_or(&[], |i| &i.curves[..]);
        let ch = channel;
        let vel = f64::from(velocity) / 127.0;
        let velf = vel as f32;
        let ms = |v: f64| (v * 0.001 * sr).max(1.0) as f32;
        // The amplifier's envelope: the zone's (SFZ's defaults for what it
        // leaves out) or the device's ADSR.
        let amp = match zone.filter(|z| z.ampeg.set) {
            Some(z) => self.eg_of(&z.ampeg, ch, velf, curves, true),
            None => Eg::new(
                0.0,
                0.0,
                ms(self.get(id::ATTACK)),
                0.0,
                ms(self.get(id::DECAY)),
                self.get(id::SUSTAIN) as f32,
                ms(self.get(id::RELEASE)),
            ),
        };
        let (fil_eg, fil_depth) = match zone.filter(|z| z.fileg.set) {
            Some(z) => (self.eg_of(&z.fileg, ch, velf, curves, false), 0.0),
            None => (Eg::OFF, 0.0f32),
        };
        let _ = fil_depth;
        let pitch_eg = match zone.filter(|z| z.pitcheg.set) {
            Some(z) => self.eg_of(&z.pitcheg, ch, velf, curves, false),
            None => Eg::OFF,
        };
        let frames = s.frames as f64;
        let single = zone.is_none();
        let root = zone.map_or(self.get(id::ROOT).round(), |z| z.root);
        let keytrack = zone.map_or(
            if self.get(id::KEY_TRACK) >= 0.5 {
                100.0
            } else {
                0.0
            },
            |z| z.keytrack,
        );
        let pitch_rand = zone.map_or(0.0, |z| f64::from(z.pitch_random));
        let pitch_rand = if pitch_rand != 0.0 {
            f64::from(self.rand()) * pitch_rand
        } else {
            0.0
        };
        let tune = zone.map_or(0.0, |z| z.tune + f64::from(z.pitch_veltrack) * vel)
            + pitch_rand
            + self.get(id::TUNE)
            + 100.0 * self.get(id::TRANSPOSE).round();
        let semis = (f64::from(key) - root) * keytrack / 100.0 + tune / 100.0;
        // A wavetable's one cycle sounds at the root key's pitch.
        let rate = match zone {
            Some(z) if z.oscillator => 440.0 * 2f64.powf((root - 69.0) / 12.0) * frames,
            _ => s.rate,
        };
        let ratio = 2f64.powf(semis / 12.0) * rate / sr;
        let reverse = if single {
            self.get(id::REVERSE) >= 0.5
        } else {
            zone.is_some_and(|z| z.reverse)
        };
        let loop_mode = match zone.and_then(|z| z.loop_mode) {
            Some(m) => m,
            None if single => match self.get(id::LOOP).round() as i64 {
                1 => LoopMode::Continuous,
                2 => LoopMode::Sustain,
                _ => LoopMode::NoLoop,
            },
            None => {
                if zone.is_some_and(|z| z.loop_start.is_some() && z.loop_end.is_some()) {
                    LoopMode::Continuous
                } else {
                    LoopMode::NoLoop
                }
            }
        };
        let mut end = zone
            .and_then(|z| z.end)
            .map_or(frames, |e| ((e + 1) as f64).min(frames));
        let start = if single {
            // The region between the start and end markers.
            let a = self.get(id::START).clamp(0.0, 0.99) * frames;
            end = (self.get(id::END).clamp(0.0, 1.0) * frames)
                .max(a + 1.0)
                .min(frames);
            a
        } else {
            let z = zone.map_or(0.0, |z| {
                let random = if z.offset_random > 0 {
                    f64::from(self.rand()) * z.offset_random as f64
                } else {
                    0.0
                };
                z.offset as f64 + random + f64::from(self.controls.sum(ch, &z.offset_cc, curves))
            });
            z.clamp(0.0, (end - 1.0).max(0.0))
        };
        let looping = match loop_mode {
            LoopMode::Continuous | LoopMode::Sustain => {
                let (ls, le) = match zone {
                    Some(z) => (
                        z.loop_start.map_or(0.0, |v| v as f64),
                        z.loop_end.map_or(end, |v| (v as f64 + 1.0).min(end)),
                    ),
                    None => (
                        self.get(id::LOOP_START).clamp(0.0, 1.0) * frames,
                        (self.get(id::LOOP_END).clamp(0.0, 1.0) * frames).min(end),
                    ),
                };
                (le - ls > 16.0).then_some((ls, le))
            }
            _ => None,
        };
        let fade = looping.map_or(0.0, |(ls, le)| {
            let seconds = zone
                .and_then(|z| z.loop_crossfade)
                .map_or(self.get(id::CROSSFADE) * 0.001, f64::from);
            (seconds * s.rate).min((le - ls) * 0.5).min(ls)
        });
        // The level: the device's and the zone's volume, velocity, key
        // tracking, randomness, crossfades by key and velocity, release
        // triggers' decay.
        let device_vt = self.get(id::VELOCITY);
        let vel_gain = match zone.and_then(|z| z.velcurve.as_deref()) {
            Some(curve) => f64::from(curve[usize::from(velocity.min(127))]),
            None => {
                let vt = device_vt * zone.map_or(1.0, |z| f64::from(z.veltrack));
                if vt >= 0.0 {
                    1.0 - vt + vt * vel * vel
                } else {
                    // Negative tracking: louder when softer.
                    1.0 + vt * vel * vel
                }
            }
        };
        let mut db = self.get(id::VOLUME);
        let mut lin = vel_gain;
        if let Some(z) = zone {
            db += f64::from(z.volume)
                + f64::from(z.amp_keytrack) * (f64::from(key) - f64::from(z.amp_keycenter));
            if z.amp_random != 0.0 {
                db += f64::from(self.rand() * z.amp_random);
            }
            if let NoteKind::Release { held_s } | NoteKind::ReleaseKey { held_s } = kind {
                db -= f64::from(z.rt_decay * held_s);
            }
            lin *= f64::from(z.amplitude);
            for x in &z.xfades {
                let (value, power) = match x.cc {
                    CC_KEY => (f32::from(key), !z.xf_key_gain),
                    CC_VELOCITY => (f32::from(velocity), !z.xf_vel_gain),
                    _ => continue,
                };
                lin *= f64::from(xfade_gain(x, value, power));
            }
        }
        let level = db_to_gain(db as f32) * lin as f32;
        let pan =
            (self.get(id::PAN) + zone.map_or(0.0, |z| f64::from(z.pan))).clamp(-1.0, 1.0) as f32;
        let gain = [level * (1.0 - pan).min(1.0), level * (1.0 + pan).min(1.0)];
        let (group, off_by) = zone.map_or((0, 0), |z| (z.group, z.off_by));
        // Choke: this group turns off the voices it names.
        if group > 0 {
            for v in &mut self.voices {
                if v.stage != Stage::Idle && v.off_by == group {
                    let z = set.zones.get(v.zone);
                    match z {
                        Some(z) if z.off_normal => v.release(),
                        _ => {
                            let t = z.and_then(|z| z.off_time).unwrap_or(0.006);
                            v.cut(t * srf);
                        }
                    }
                }
            }
        }
        // Polyphony limits of the zone's group and of the note.
        if let Some(z) = zone {
            for (limit, same_key) in [(z.polyphony, false), (z.note_polyphony, true)] {
                let Some(limit) = limit else { continue };
                let sounding = |v: &Voice| {
                    v.stage != Stage::Idle
                        && v.stage != Stage::Release
                        && v.group == z.group
                        && (!same_key || v.key == key)
                };
                while self.voices.iter().filter(|v| sounding(v)).count() >= limit.max(1) as usize {
                    let Some(oldest) = self
                        .voices
                        .iter_mut()
                        .filter(|v| sounding(v))
                        .min_by_key(|v| v.age)
                    else {
                        break;
                    };
                    oldest.cut(0.006 * srf);
                }
            }
        }
        // Random draws for the filters' cutoffs.
        let mut rnd = [0.0f32; 2];
        if let Some(z) = zone {
            for (i, f) in z.filters.iter().enumerate() {
                if let Some(f) = f
                    && f.random != 0.0
                {
                    rnd[i] = self.rand() * f.random;
                }
            }
        }
        let delay = zone.map_or(0.0, |z| {
            let random = if z.delay_random > 0.0 {
                self.rand() * z.delay_random
            } else {
                0.0
            };
            (z.delay + random + self.controls.sum(ch, &z.delay_cc, curves)).max(0.0)
        });
        self.counter += 1;
        let idx = self.free_voice();
        // Keep Length: read at the sample's own speed, pitched by a
        // stretcher.
        let keep = self.get(id::PITCH_MODE) >= 0.5;
        let (step, pitch, slot) = match self.keep.as_mut().filter(|_| keep) {
            Some(pool) => {
                let ages: [u64; MAX_VOICES] = std::array::from_fn(|i| self.voices[i].age);
                let (slot, evicted) = pool.claim(idx, |i| ages[i]);
                if let Some(e) = evicted {
                    self.voices[e].stage = Stage::Idle;
                    self.voices[e].slot = None;
                }
                (rate / sr, 2f64.powf(semis / 12.0) as f32, Some(slot))
            }
            None => (ratio, 1.0, None),
        };
        self.voices[idx] = Voice {
            stage: Stage::Attack,
            amp,
            fil_eg,
            pitch_eg,
            delay: (f64::from(delay) * sr) as u32,
            zone: zone_index,
            sample,
            pos: if reverse { end - 1.0 } else { start },
            step,
            looping,
            loop_mode,
            fade,
            end: if reverse { start } else { end },
            start,
            reverse,
            repeats: zone.map_or(0, |z| z.count.saturating_sub(1)),
            gain,
            last_gain: gain,
            key,
            channel,
            velocity: velf,
            held: kind == NoteKind::Attack,
            pedal_held: false,
            group,
            off_by,
            svf: [[(0.0, 0.0); 2]; 2],
            zf: [[FilterState::default(); 2]; 2],
            eqs: [[FilterState::default(); 3]; 2],
            rnd,
            lfo: [0.0; 3],
            age_s: 0.0,
            age: self.counter,
            slot,
            pitch,
            fed_out: false,
        };
        let _ = fil_depth;
    }

    /// A zone's envelope for a note (`amp`: times clamped so it cannot
    /// click).
    fn eg_of(&self, e: &EgSpec, ch: u8, vel: f32, curves: &[[f32; 128]], amp: bool) -> Eg {
        let sr = self.sr as f32;
        let mut t = [
            e.delay + e.vel2delay * vel,
            e.start,
            e.attack + e.vel2attack * vel,
            e.hold + e.vel2hold * vel,
            e.decay + e.vel2decay * vel,
            e.sustain + e.vel2sustain * vel,
            e.release + e.vel2release * vel,
        ];
        for (p, m) in &e.cc {
            let v = m.amount * self.controls.value(ch, m, curves);
            let i = match p {
                EgParam::Delay => 0,
                EgParam::Start => 1,
                EgParam::Attack => 2,
                EgParam::Hold => 3,
                EgParam::Decay => 4,
                EgParam::Sustain => 5,
                EgParam::Release => 6,
                EgParam::Depth => continue,
            };
            t[i] += v;
        }
        let min = if amp { 0.001 } else { 0.0 };
        Eg::new(
            t[0].max(0.0) * sr,
            t[1],
            t[2].max(0.0) * sr,
            t[3].max(0.0) * sr,
            t[4].max(0.0) * sr,
            t[5],
            t[6].max(min) * sr,
        )
    }

    /// Whether `z` sounds for this note now.
    fn zone_takes(&self, z: &Zone, channel: u8, key: u8, velocity: u8, rand: f64) -> bool {
        let c = &self.controls;
        let ch = (channel & 15) as usize;
        if !(z.lochan..=z.hichan).contains(&channel) || !z.takes(key, velocity) {
            return false;
        }
        if rand < z.lorand || rand >= z.hirand.max(z.lorand + 1e-9) {
            return false;
        }
        for &(cc, lo, hi) in &z.cc_ranges {
            let v = (c.cc[ch][cc.min(CONTROLLERS - 1)] * 127.0).round() as u8;
            if !(lo..=hi).contains(&v) {
                return false;
            }
        }
        if !(z.lobend..=z.hibend).contains(&c.bend_raw[ch]) {
            return false;
        }
        let chanaft = (c.cc[ch][CC_CHANAFT] * 127.0).round() as u8;
        let polyaft = (c.polyaft[ch][usize::from(key & 127)] * 127.0).round() as u8;
        if !(z.lochanaft..=z.hichanaft).contains(&chanaft)
            || !(z.lopolyaft..=z.hipolyaft).contains(&polyaft)
        {
            return false;
        }
        let bpm = c.tempo as f32;
        if bpm < z.lobpm || bpm >= z.hibpm {
            return false;
        }
        if let Some(sw) = z.sw_last
            && c.last_keyswitch != Some(sw)
        {
            return false;
        }
        if let Some(k) = z.sw_down
            && !c.keys_down.iter().any(|keys| keys[usize::from(k & 127)])
        {
            return false;
        }
        if let Some(k) = z.sw_up
            && c.keys_down.iter().any(|keys| keys[usize::from(k & 127)])
        {
            return false;
        }
        if let Some(k) = z.sw_previous
            && c.previous.map(|(p, _)| p) != Some(k)
        {
            return false;
        }
        true
    }

    fn note_on(
        &mut self,
        set: &super::samples::SampleSet,
        channel: u8,
        key: u8,
        velocity: u8,
        kind: NoteKind,
    ) {
        if set.zones.is_empty() {
            if kind == NoteKind::Attack
                && let Some(Some(sample)) = set.slots.first()
            {
                self.start(set, None, usize::MAX, *sample, channel, key, velocity, kind);
            }
            return;
        }
        let attack = kind == NoteKind::Attack;
        // A keyswitch: remembered, and it plays nothing itself unless a
        // zone covers the key.
        if attack && set.zones.iter().any(|z| z.is_keyswitch(key)) {
            self.controls.last_keyswitch = Some(key);
        }
        let k = key as usize & 127;
        let round = if attack {
            self.rounds[k]
        } else {
            self.rounds[k].saturating_sub(1)
        };
        if attack {
            self.rounds[k] = self.rounds[k].wrapping_add(1);
        }
        let rand = f64::from(self.rand());
        // Other keys held (first and legato triggers).
        let others = self.controls.held.saturating_sub(u32::from(attack));
        let previous_velocity = self.controls.previous.map_or(velocity, |(_, v)| v);
        for (i, z) in set.zones.iter().enumerate() {
            let fits = match (z.trigger, kind) {
                (Trigger::Attack, NoteKind::Attack) => true,
                (Trigger::First, NoteKind::Attack) => others == 0,
                (Trigger::Legato, NoteKind::Attack) => others > 0,
                (Trigger::Release, NoteKind::Release { .. }) => true,
                (Trigger::ReleaseKey, NoteKind::ReleaseKey { .. }) => true,
                _ => false,
            };
            if !fits {
                continue;
            }
            let vel = if z.sw_vel_previous {
                previous_velocity
            } else {
                velocity
            };
            if !self.zone_takes(z, channel, key, vel, rand) {
                continue;
            }
            if z.seq_length > 1 && round % z.seq_length + 1 != z.seq_position {
                continue;
            }
            self.start(set, Some(z), i, z.sample, channel, key, vel, kind);
        }
    }

    /// Zones a controller's move into their `on_cc` range triggers.
    fn controller_triggers(
        &mut self,
        set: &super::samples::SampleSet,
        channel: u8,
        cc: usize,
        old: f32,
        new: f32,
    ) {
        let (old, new) = ((old * 127.0).round() as u8, (new * 127.0).round() as u8);
        for (i, z) in set.zones.iter().enumerate() {
            let fires = z.on_cc.iter().any(|&(c, lo, hi)| {
                c == cc && (lo..=hi).contains(&new) && !(lo..=hi).contains(&old)
            });
            if fires && (z.lochan..=z.hichan).contains(&channel) {
                let key = if z.lokey == z.hikey {
                    z.lokey
                } else {
                    z.root.round() as u8
                };
                self.start(
                    set,
                    Some(z),
                    i,
                    z.sample,
                    channel,
                    key,
                    100,
                    NoteKind::Controller,
                );
            }
        }
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        let ch = (channel & 15) as usize;
        let k = usize::from(key & 127);
        let pedal = self.sustain[ch];
        for v in &mut self.voices {
            if v.key == key && v.channel == channel && v.held && v.stage != Stage::Idle {
                if pedal {
                    v.pedal_held = true;
                    v.held = false;
                } else {
                    v.release();
                }
            }
        }
        if self.controls.keys_down[ch][k] {
            self.controls.keys_down[ch][k] = false;
            self.controls.held = self.controls.held.saturating_sub(1);
        }
        let held_s =
            (self.clock.saturating_sub(self.controls.key_at[ch][k])) as f32 / self.sr as f32;
        let velocity = self.controls.key_velocity[ch][k].max(1);
        // `release_key` at once; `release` once the pedal is up.
        self.queue_release(channel, key, velocity, NoteKind::ReleaseKey { held_s });
        if pedal {
            self.controls.pedal_releases[ch][k] = true;
        } else {
            self.queue_release(channel, key, velocity, NoteKind::Release { held_s });
        }
    }

    fn queue_release(&mut self, channel: u8, key: u8, velocity: u8, kind: NoteKind) {
        if self.pending < self.pending_release.len() {
            self.pending_release[self.pending] = (channel, key, velocity, kind);
            self.pending += 1;
        }
    }

    fn handle(&mut self, set: Option<&super::samples::SampleSet>, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => {
                let ch = (channel & 15) as usize;
                let k = usize::from(key & 127);
                if !self.controls.keys_down[ch][k] {
                    self.controls.held += 1;
                }
                self.controls.keys_down[ch][k] = true;
                self.controls.key_velocity[ch][k] = velocity;
                self.controls.key_at[ch][k] = self.clock;
                self.controls.cc[ch][CC_VELOCITY] = f32::from(velocity) / 127.0;
                self.controls.cc[ch][CC_KEY] = f32::from(key) / 127.0;
                let r = self.rand();
                self.controls.cc[ch][CC_RANDOM_UNI] = r;
                self.controls.cc[ch][CC_RANDOM_BI] = r;
                if let Some(set) = set {
                    self.note_on(set, channel, key, velocity, NoteKind::Attack);
                }
                self.controls.previous = Some((key, velocity));
            }
            MidiEvent::NoteOff { channel, key, .. } => self.note_off(channel, key),
            MidiEvent::ControlChange { controller, .. }
                if controller == MidiEvent::CC_ALL_NOTES_OFF
                    || controller == MidiEvent::CC_ALL_SOUND_OFF =>
            {
                self.sustain = [false; 16];
                self.controls.keys_down = [[false; 128]; 16];
                self.controls.pedal_releases = [[false; 128]; 16];
                self.controls.held = 0;
                for v in &mut self.voices {
                    v.release();
                }
            }
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => {
                let ch = (channel & 15) as usize;
                let cc = usize::from(controller & 127);
                let old = self.controls.cc[ch][cc];
                let new = f32::from(value) / 127.0;
                self.controls.cc[ch][cc] = new;
                if let Some(set) = set {
                    self.controller_triggers(set, channel, cc, old, new);
                }
                match controller {
                    64 => {
                        self.sustain[ch] = value >= 64;
                        if value < 64 {
                            for v in &mut self.voices {
                                if v.channel == channel && v.pedal_held {
                                    v.pedal_held = false;
                                    v.release();
                                }
                            }
                            // Release triggers held back by the pedal.
                            for k in 0..128u8 {
                                if std::mem::take(
                                    &mut self.controls.pedal_releases[ch][usize::from(k)],
                                ) {
                                    let held_s = (self
                                        .clock
                                        .saturating_sub(self.controls.key_at[ch][usize::from(k)]))
                                        as f32
                                        / self.sr as f32;
                                    let velocity =
                                        self.controls.key_velocity[ch][usize::from(k)].max(1);
                                    self.queue_release(
                                        channel,
                                        k,
                                        velocity,
                                        NoteKind::Release { held_s },
                                    );
                                }
                            }
                        }
                    }
                    101 => self.rpn[ch].0 = value,
                    100 => self.rpn[ch].1 = value,
                    6 if self.rpn[ch] == (0, 0) => {
                        self.bend_range[ch] = f32::from(value.max(1));
                    }
                    _ => {}
                }
            }
            MidiEvent::PitchBend { channel, value } => {
                let c = (channel & 15) as usize;
                let raw = i32::from(value) - 8192;
                self.controls.bend_raw[c] = raw;
                self.controls.cc[c][CC_BEND] = f32::from(value) / 16383.0;
                self.bend[c] = raw as f32 / 8192.0 * self.bend_range[c];
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                self.controls.cc[(channel & 15) as usize][CC_CHANAFT] = f32::from(pressure) / 127.0;
            }
            MidiEvent::PolyPressure {
                channel,
                key,
                pressure,
            } => {
                let c = (channel & 15) as usize;
                let p = f32::from(pressure) / 127.0;
                self.controls.polyaft[c][usize::from(key & 127)] = p;
                self.controls.cc[c][CC_POLYAFT] = p;
            }
            _ => {}
        }
    }

    // Each frame writes both output sides at its index.
    #[allow(clippy::needless_range_loop)]
    fn render(
        &mut self,
        set: Option<&super::samples::SampleSet>,
        out: &mut [&mut [f32]; 2],
        start: usize,
        end: usize,
    ) {
        let Some(set) = set else { return };
        let sr = self.sr as f32;
        let kind = self.get(id::FILTER_TYPE).round() as i64;
        let cutoff = self.get(id::CUTOFF) as f32;
        let env_amt = self.get(id::FILTER_ENV) as f32 * 6.0;
        let filtering = cutoff < 19_500.0 || env_amt.abs() > 0.01 || kind != 0;
        let k = 2.0 - 1.9 * self.get(id::RESONANCE).clamp(0.0, 1.0) as f32;
        let bends = self.bend;
        let curves: &[[f32; 128]] = set.instrument.as_ref().map_or(&[], |i| &i.curves[..]);
        let Self {
            voices,
            keep,
            controls,
            ..
        } = self;
        for v in voices.iter_mut().filter(|v| v.stage != Stage::Idle) {
            let Some(s) = set.samples.get(v.sample) else {
                v.stage = Stage::Idle;
                continue;
            };
            let zone = set.zones.get(v.zone);
            // A zone's delay: silent until it starts.
            let mut from = start;
            if v.delay > 0 {
                let d = (v.delay as usize).min(end - start);
                v.delay -= d as u32;
                from += d;
                if from >= end {
                    continue;
                }
            }
            let bend_st = bends[(v.channel & 15) as usize];
            let ch = v.channel;
            // Keep Length: the segment from the voice's stretcher (the bend
            // moves its pitch, not its speed).
            let mut kept: Option<(&[f32], &[f32])> = None;
            if let (Some(slot), Some(pool)) = (v.slot, keep.as_mut()) {
                let bend = 2f64.powf(f64::from(bend_st) / 12.0);
                let (step, transpose) = (v.step, v.pitch * bend as f32);
                let mut feed = |a: &mut [f32], b: &mut [f32]| feed_voice(v, s, step, a, b);
                match pool.render(slot, transpose, end - from, &mut feed) {
                    Render::Wait => continue,
                    Render::Done => {
                        v.stage = Stage::Idle;
                        continue;
                    }
                    Render::Out(l, r) => kept = Some((l, r)),
                }
            }
            let mut i = from;
            while i < end && v.stage != Stage::Idle {
                let n = (end - i).min(16);
                let dt = n as f32 / sr;
                // This stretch's modulation.
                let mut cents = 0.0f32;
                let mut gain_db = 0.0f32;
                let mut gain_lin = 1.0f32;
                let mut pan_add = 0.0f32;
                let mut width = 1.0f32;
                let mut position = 0.0f32;
                let mut zc: [Coefs; 2] = [Coefs::Off; 2];
                let mut zkind = [FilterKind::Lp2; 2];
                let mut eqc: [Coefs; 3] = [Coefs::Off; 3];
                let lfo = |phase: &mut f32,
                           spec: &crate::devices::sfz::LfoSpec,
                           age: f32,
                           c: &Controls|
                 -> f32 {
                    if !spec.active() || age < spec.delay {
                        return 0.0;
                    }
                    let freq = (spec.freq + c.sum(ch, &spec.freq_cc, curves)).max(0.0);
                    *phase = (*phase + freq * dt).fract();
                    let fade = if spec.fade > 0.0 {
                        ((age - spec.delay) / spec.fade).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    let depth = spec.depth + c.sum(ch, &spec.depth_cc, curves);
                    (std::f32::consts::TAU * *phase).sin() * depth * fade
                };
                let c = &*controls;
                match zone {
                    Some(z) => {
                        // Pitch: the zone's bend range, envelope, LFO,
                        // controllers.
                        let b = c.bend_raw[(ch & 15) as usize] as f32 / 8192.0;
                        let mut bend_cents = if b >= 0.0 {
                            b * z.bend_up
                        } else {
                            -b * z.bend_down
                        };
                        if z.bend_step > 1.0 {
                            bend_cents = (bend_cents / z.bend_step).round() * z.bend_step;
                        }
                        cents += bend_cents + c.sum(ch, &z.pitch_cc, curves);
                        if z.pitcheg.set {
                            cents +=
                                v.pitch_eg.level * eg_depth(&z.pitcheg, c, ch, v.velocity, curves);
                        }
                        cents += lfo(&mut v.lfo[2], &z.pitchlfo, v.age_s, c);
                        // Level and image.
                        gain_db += c.sum(ch, &z.volume_cc, curves)
                            + lfo(&mut v.lfo[0], &z.amplfo, v.age_s, c);
                        gain_lin *= (1.0 + c.sum(ch, &z.amplitude_cc, curves)).max(0.0);
                        for x in &z.xfades {
                            if x.cc != CC_KEY && x.cc != CC_VELOCITY {
                                let value =
                                    c.cc[(ch & 15) as usize][x.cc.min(CONTROLLERS - 1)] * 127.0;
                                gain_lin *= xfade_gain(x, value, !z.xf_cc_gain);
                            }
                        }
                        pan_add = c.sum(ch, &z.pan_cc, curves);
                        width = (z.width + c.sum(ch, &z.width_cc, curves)).clamp(-1.0, 1.0);
                        position =
                            (z.position + c.sum(ch, &z.position_cc, curves)).clamp(-1.0, 1.0);
                        // Filters: key, velocity, random, envelope (the
                        // first), LFO (the first), controllers.
                        let fil_lfo = lfo(&mut v.lfo[1], &z.fillfo, v.age_s, c);
                        let fil_env = if z.fileg.set {
                            v.fil_eg.level * eg_depth(&z.fileg, c, ch, v.velocity, curves)
                        } else {
                            0.0
                        };
                        for (fi, f) in z.filters.iter().enumerate() {
                            let Some(f) = f else { continue };
                            let mut fc_cents = f.keytrack
                                * (f32::from(v.key) - f32::from(f.keycenter))
                                + f.veltrack * v.velocity
                                + v.rnd[fi]
                                + c.sum(ch, &f.cutoff_cc, curves);
                            if fi == 0 {
                                fc_cents += fil_env + fil_lfo;
                            }
                            let fc = f.cutoff * 2f32.powf(fc_cents / 1200.0);
                            let res = f.resonance + c.sum(ch, &f.resonance_cc, curves);
                            let gain = f.gain + c.sum(ch, &f.gain_cc, curves);
                            zc[fi] = filter_coefs(f.kind, fc, res, gain, sr);
                            zkind[fi] = f.kind;
                        }
                        if z.eq_active() {
                            for (bi, band) in z.eq.iter().enumerate() {
                                if !band.active() {
                                    continue;
                                }
                                let freq = band.freq
                                    + band.vel2freq * v.velocity
                                    + c.sum(ch, &band.freq_cc, curves);
                                let bw = (band.bw + c.sum(ch, &band.bw_cc, curves)).max(0.01);
                                let gain = band.gain
                                    + band.vel2gain * v.velocity
                                    + c.sum(ch, &band.gain_cc, curves);
                                // Bandwidth in octaves to Q.
                                let two = 2f32.powf(bw);
                                let q = two.sqrt() / (two - 1.0);
                                eqc[bi] = Coefs::rbj(0, freq, q, gain, sr);
                            }
                        }
                    }
                    None => {
                        cents += bend_st * 100.0;
                    }
                }
                let step = v.step * 2f64.powf(f64::from(cents) / 1200.0);
                let level = db_to_gain(gain_db) * gain_lin;
                let pan = pan_add.clamp(-1.0, 1.0);
                let target = [
                    v.gain[0] * level * (1.0 - pan).min(1.0),
                    v.gain[1] * level * (1.0 + pan).min(1.0),
                ];
                let from_gain = v.last_gain;
                v.last_gain = target;
                // The device's filter for this stretch.
                let velocity = v.velocity;
                let coefs = |env: f32| {
                    let fc = (cutoff * 2f32.powf(env_amt * env * velocity)).clamp(20.0, sr * 0.45);
                    let g = (PI * fc / sr).tan();
                    let a1 = 1.0 / (1.0 + g * (g + k));
                    (a1, g * a1, g * g * a1)
                };
                let dc = coefs(v.amp.level);
                let stereo_sample = s.stereo;
                for j in 0..n {
                    let idx = i + j;
                    let env = v.amp.tick();
                    v.fil_eg.tick();
                    v.pitch_eg.tick();
                    if v.amp.stage == EgStage::Done {
                        v.stage = Stage::Idle;
                        break;
                    }
                    let mut y = match kept {
                        Some((l, r)) => [l[idx - from], r[idx - from]],
                        None => {
                            let (y, ended) = next_frame(v, s, step);
                            if ended {
                                if v.repeats > 0 {
                                    // `count`: from its start again.
                                    v.repeats -= 1;
                                    v.pos = if v.reverse {
                                        v.start.max(v.end)
                                    } else {
                                        v.start
                                    };
                                } else {
                                    v.stage = Stage::Idle;
                                }
                            }
                            y
                        }
                    };
                    // Width and position (stereo samples).
                    if zone.is_some() && (width != 1.0 || position != 0.0) {
                        let (l, r) = (y[0], y[1]);
                        let mid = 0.5 * (l + r);
                        let side = 0.5 * (l - r) * if stereo_sample { width } else { 0.0 };
                        let (l, r) = (mid + side, mid - side);
                        y = [l * (1.0 - position).min(1.0), r * (1.0 + position).min(1.0)];
                    }
                    let t = (j as f32 + 1.0) / n as f32;
                    for side in 0..2 {
                        let mut x = y[side];
                        for fi in 0..2 {
                            if let Coefs::Off = zc[fi] {
                                continue;
                            }
                            x = run_filter(zkind[fi], zc[fi], &mut v.zf[side][fi], x);
                        }
                        for bi in 0..3 {
                            if let Coefs::Off = eqc[bi] {
                                continue;
                            }
                            x = run_filter(FilterKind::Peak, eqc[bi], &mut v.eqs[side][bi], x);
                        }
                        if filtering {
                            let (a1, a2, a3) = dc;
                            let (ic1, ic2) = &mut v.svf[side][0];
                            let v3 = x - *ic2;
                            let v1 = a1 * *ic1 + a2 * v3;
                            let v2 = *ic2 + a2 * *ic1 + a3 * v3;
                            *ic1 = 2.0 * v1 - *ic1;
                            *ic2 = 2.0 * v2 - *ic2;
                            x = match kind {
                                1 => {
                                    let (jc1, jc2) = &mut v.svf[side][1];
                                    let w3 = v2 - *jc2;
                                    let w1 = a1 * *jc1 + a2 * w3;
                                    let w2 = *jc2 + a2 * *jc1 + a3 * w3;
                                    *jc1 = 2.0 * w1 - *jc1;
                                    *jc2 = 2.0 * w2 - *jc2;
                                    w2
                                }
                                2 => v1 * k,
                                3 => x - k * v1 - v2,
                                _ => v2,
                            };
                        }
                        let g = from_gain[side] + (target[side] - from_gain[side]) * t;
                        out[side][idx] += x * env * g;
                    }
                    if v.stage == Stage::Idle {
                        break;
                    }
                }
                if v.stage != Stage::Idle {
                    v.stage = v.amp.summary();
                }
                v.age_s += dt;
                i += n;
            }
            for st in v.svf.iter_mut().flatten() {
                if st.0.abs() < 1e-20 {
                    st.0 = 0.0;
                }
                if st.1.abs() < 1e-20 {
                    st.1 = 0.0;
                }
            }
            for st in v.zf.iter_mut().flatten().chain(v.eqs.iter_mut().flatten()) {
                for p in &mut st.s {
                    if p.0.abs() < 1e-20 {
                        p.0 = 0.0;
                    }
                    if p.1.abs() < 1e-20 {
                        p.1 = 0.0;
                    }
                }
            }
        }
    }
}

impl PluginProcessor for SamplerProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames.min(self.mix[0].len());
        let watched = self
            .tap
            .as_ref()
            .is_some_and(|t| self.watching.check(t, frames));
        // The samples, if the control side is not swapping them right now.
        let shared = Arc::clone(&self.shared);
        let guard = shared.try_lock();
        let set = guard.as_ref().and_then(|g| {
            if g.generation != self.generation {
                // New samples: what plays from the old ones stops.
                self.generation = g.generation;
                for v in &mut self.voices {
                    v.stage = Stage::Idle;
                }
                if let Some(set) = g.set.as_deref() {
                    self.adopt(set);
                }
            }
            g.set.as_deref()
        });
        if ctx.transport.tempo > 0.0 {
            self.controls.tempo = ctx.transport.tempo;
        }
        // Stretchers of voices that stopped go back; the block's priming.
        if let Some(pool) = &mut self.keep {
            let voices = &self.voices;
            pool.begin_block(frames, self.sr, |o, slot| {
                voices[o].stage != Stage::Idle && voices[o].slot == Some(slot)
            });
        }
        // Render both sides into the preallocated mix (taken out and put
        // back: no allocation), split at the events.
        let mut mix = std::mem::take(&mut self.mix);
        {
            let [l, r] = &mut mix;
            l[..frames].fill(0.0);
            r[..frames].fill(0.0);
            let mut sides: [&mut [f32]; 2] = [&mut l[..frames], &mut r[..frames]];
            let mut pos = 0usize;
            let block_start = self.clock;
            if let Some(events) = io.events_in.first() {
                for e in events.iter() {
                    let at = (e.sample_offset as usize).min(frames);
                    if at > pos {
                        self.render(set, &mut sides, pos, at);
                        pos = at;
                    }
                    self.clock = block_start + at as u64;
                    self.handle(set, e.event);
                    if self.pending > 0 {
                        let due = std::mem::take(&mut self.pending);
                        if let Some(set) = set {
                            for j in 0..due {
                                let (c, k, v, kind) = self.pending_release[j];
                                self.note_on(set, c, k, v, kind);
                            }
                        }
                    }
                }
            }
            if frames > pos {
                self.render(set, &mut sides, pos, frames);
            }
            self.clock = block_start + frames as u64;
        }
        let newest = self
            .voices
            .iter()
            .filter(|v| v.stage != Stage::Idle)
            .max_by_key(|v| v.age)
            .copied();
        let position = newest.and_then(|v| {
            let len = set?.samples.get(v.sample)?.frames as f64;
            (len > 0.0).then(|| (v.pos / len) as f32)
        });
        drop(guard);
        let Some(out) = io.audio_out.first_mut() else {
            self.mix = mix;
            return ProcessStatus::Continue;
        };
        out.clear();
        let channels = out.num_channels();
        if channels >= 2 {
            out.channel_mut(0)[..frames].copy_from_slice(&mix[0][..frames]);
            out.channel_mut(1)[..frames].copy_from_slice(&mix[1][..frames]);
        } else if channels == 1 {
            for (o, (a, b)) in out
                .channel_mut(0)
                .iter_mut()
                .zip(mix[0].iter().zip(&mix[1]))
                .take(frames)
            {
                *o = 0.5 * (a + b);
            }
        }
        self.mix = mix;
        if let Some(tap) = &self.tap {
            let sounding = self
                .voices
                .iter()
                .filter(|v| v.stage != Stage::Idle)
                .count();
            tap.set_value(value::VOICES, sounding as f32);
            let mut held = [0u32; KEY_WORDS];
            for v in &self.voices {
                if matches!(v.stage, Stage::Attack | Stage::Decay | Stage::Sustain) {
                    let k = usize::from(v.key);
                    if let Some(w) = held.get_mut(k / 24) {
                        *w |= 1 << (k % 24);
                    }
                }
            }
            for (i, w) in held.iter().enumerate() {
                tap.set_value(value::KEYS + i, *w as f32);
            }
            if let Some(v) = newest {
                tap.set_value(value::NOTE, f32::from(v.key));
                tap.set_value(
                    value::ZONE,
                    if v.zone == usize::MAX {
                        -1.0
                    } else {
                        v.zone as f32
                    },
                );
                tap.set_value(value::POSITION, position.unwrap_or(0.0));
            }
            let mut peak = 0.0f32;
            for c in 0..2.min(channels) {
                for s in out.channel(c).iter().take(frames) {
                    self.meters[c].add(*s);
                    peak = peak.max(s.abs());
                }
                self.meters[c].publish(&tap.meter_out, c, frames);
            }
            tap.raise_value(value::OUT_PEAK, peak);
            if watched && channels > 0 {
                let r = out.channel(1.min(channels - 1));
                tap.output.push(&out.channel(0)[..frames], &r[..frames]);
            }
        }
        if self.voices.iter().all(|v| v.stage == Stage::Idle) {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; MAX_VOICES];
        self.sustain = [false; 16];
        self.pending = 0;
        self.controls.keys_down = [[false; 128]; 16];
        self.controls.pedal_releases = [[false; 128]; 16];
        self.controls.held = 0;
        if let Some(pool) = &mut self.keep {
            pool.release_all();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{SR, bin_db};
    use crate::devices::samples::{SampleDoc, SampleHost, fixtures};
    use faderframe_audio_graph::AudioBuffer;
    use faderframe_core::ChannelLayout;
    use faderframe_midi::{MidiBuffer, TimedMidiEvent};
    use faderframe_transport::TransportInfo;

    pub(crate) struct Play<P> {
        pub p: P,
        out: Vec<AudioBuffer>,
        events: Vec<MidiBuffer>,
        transport: TransportInfo,
    }

    impl<P: PluginProcessor> Play<P> {
        pub fn new(p: P) -> Self {
            let mut out = vec![AudioBuffer::new(ChannelLayout::Stereo, 256)];
            out[0].set_len(256);
            Self {
                p,
                out,
                events: vec![MidiBuffer::with_capacity(64)],
                transport: TransportInfo::default(),
            }
        }

        pub fn send(&mut self, e: MidiEvent) {
            self.events[0].push(TimedMidiEvent::new(0, e)).unwrap();
        }

        pub fn run(&mut self, seconds: f64) -> (Vec<f32>, Vec<f32>, ProcessStatus) {
            let (mut l, mut r) = (Vec::new(), Vec::new());
            let mut status = ProcessStatus::Continue;
            for _ in 0..(seconds * SR / 256.0).ceil() as usize {
                let ctx = PluginProcessContext {
                    transport: &self.transport,
                    param_events: &[],
                    harmony: &crate::NO_HARMONY,
                    param_mods: &[],
                    note_mods: &[],
                };
                let mut io = NodeIo {
                    frames: 256,
                    audio_in: &[],
                    audio_out: &mut self.out,
                    events_in: &self.events,
                    events_out: &mut [],
                };
                status = self.p.process(&ctx, &mut io);
                self.events[0].clear();
                l.extend_from_slice(self.out[0].channel(0));
                r.extend_from_slice(self.out[0].channel(1));
            }
            (l, r, status)
        }
    }

    pub(crate) fn on(key: u8, velocity: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity,
        }
    }

    pub(crate) fn off(key: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            channel: 0,
            key,
            velocity: 0,
        }
    }

    fn sampler(doc: SampleDoc, set: &[(u32, f64)]) -> (Play<SamplerProcessor>, SampleHost) {
        let params = ParamValues::new(parameters());
        for (id, v) in set {
            params.set_by_id(ParameterId(*id), *v).unwrap();
        }
        let mut host = SampleHost::default();
        host.set_doc(doc, None);
        let p = SamplerProcessor::new(
            params,
            None,
            &crate::devices::rig::config(),
            Arc::clone(&host.shared),
        );
        (Play::new(p), host)
    }

    #[test]
    fn a_sample_plays_at_the_pitch_of_the_key_and_loops() {
        let d = fixtures::dir("sampler");
        let mut doc = SampleDoc::default();
        // A at 44.1 kHz, its root A4: an octave up plays 880 Hz.
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 0.5)),
        );
        let (mut s, _host) = sampler(doc.clone(), &[(id::ROOT, 69.0), (id::VOLUME, 0.0)]);
        s.send(on(81, 127));
        let (l, _, _) = s.run(0.3);
        assert!(bin_db(&l, 880.0) > -12.0, "{}", bin_db(&l, 880.0));
        assert!(bin_db(&l, 440.0) < -60.0);
        // Without a loop it stops at its end (0.25 s at twice the speed)…
        let (l, _, _) = s.run(0.2);
        assert!(l.iter().all(|v| v.abs() < 1e-6));
        // …with one it goes on until released, then sleeps.
        let (mut s, _host) = sampler(
            doc,
            &[
                (id::ROOT, 69.0),
                (id::LOOP, 1.0),
                (id::LOOP_START, 0.2),
                (id::LOOP_END, 0.8),
            ],
        );
        s.send(on(69, 100));
        s.run(1.0);
        let (l, _, _) = s.run(0.2);
        assert!(
            bin_db(&l, 440.0) > -20.0,
            "still looping: {}",
            bin_db(&l, 440.0)
        );
        s.send(off(69));
        let (_, _, status) = s.run(2.0);
        assert_eq!(status, ProcessStatus::Sleep);
    }

    #[test]
    fn the_start_and_end_points_bound_what_plays() {
        // One second: 440 Hz, then 880 Hz.
        let d = fixtures::dir("sampler-region");
        let path = d.join("two.wav");
        let x: Vec<f32> = (0..48_000)
            .map(|i| {
                let f = if i < 24_000 { 440.0 } else { 880.0 };
                (0.5 * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin()) as f32
            })
            .collect();
        faderframe_audio_files::write_wav(
            &path,
            &[x],
            48_000,
            faderframe_audio_files::WavFormat::Float32,
            false,
        )
        .unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(path.to_string_lossy().into_owned()));
        let play = |set: &[(u32, f64)]| {
            let mut all = vec![(id::ROOT, 69.0), (id::RELEASE, 5.0), (id::VOLUME, 0.0)];
            all.extend_from_slice(set);
            let (mut s, _host) = sampler(doc.clone(), &all);
            s.send(on(69, 127));
            let (l, _, _) = s.run(1.2);
            let last = l.iter().rposition(|v| v.abs() > 1e-4).unwrap_or(0);
            (l, last as f64 / SR)
        };
        // The first half only.
        let (l, length) = play(&[(id::END, 0.5)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        assert!(
            bin_db(&l[..20_000], 440.0) > -12.0,
            "{}",
            bin_db(&l[..20_000], 440.0)
        );
        assert!(bin_db(&l, 880.0) < -40.0, "{}", bin_db(&l, 880.0));
        // The middle.
        let (_, length) = play(&[(id::START, 0.25), (id::END, 0.75)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        // Reversed: the region backwards (the 440 half, not the 880 one).
        let (l, length) = play(&[(id::END, 0.5), (id::REVERSE, 1.0)]);
        assert!((length - 0.5).abs() < 0.01, "{length}");
        assert!(
            bin_db(&l[..20_000], 440.0) > -12.0,
            "{}",
            bin_db(&l[..20_000], 440.0)
        );
        assert!(bin_db(&l, 880.0) < -40.0, "{}", bin_db(&l, 880.0));
        // The end does not click: the last milliseconds fade out.
        let (l, length) = play(&[(id::END, 0.3)]);
        let end = (length * SR) as usize;
        let peak =
            |r: std::ops::RangeInclusive<usize>| l[r].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let (before, last) = (peak(end - 400..=end - 300), peak(end - 20..=end));
        assert!(before > 0.4 && last < 0.1, "{before} → {last}");
    }

    #[test]
    fn every_held_key_is_published() {
        let d = fixtures::dir("sampler-keys");
        let mut doc = SampleDoc::default();
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 2.0)),
        );
        let params = ParamValues::new(parameters());
        params.set_by_id(ParameterId(id::LOOP), 2.0).unwrap();
        let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
        let mut host = SampleHost::default();
        host.set_doc(doc, None);
        let mut s = Play::new(SamplerProcessor::new(
            params,
            Some(Arc::clone(&tap)),
            &crate::devices::rig::config(),
            Arc::clone(&host.shared),
        ));
        let held = |k: u8| key_held(|i| tap.value(i), k);
        // A chord, its notes far apart (in different words of bits).
        for k in [21, 60, 64, 108] {
            s.send(on(k, 100));
        }
        s.run(0.1);
        assert!([21, 60, 64, 108].into_iter().all(held), "all four lit");
        assert!(!held(62) && !held(0) && !held(127));
        // A released key goes dark while it fades out.
        s.send(off(64));
        s.run(0.02);
        assert!(!held(64));
        assert!(held(60) && held(108));
    }

    #[test]
    fn keep_length_moves_the_pitch_not_the_length() {
        let d = fixtures::dir("sampler-keep");
        let mut doc = SampleDoc::default();
        // Half a second of A4 at 44.1 kHz, played an octave up.
        doc.set(
            0,
            Some(fixtures::tone(&d.join("a.wav"), 440.0, 44_100, 0.5)),
        );
        // How long it sounds (above -40 dBFS), and its pitch.
        let play = |mode: f64| {
            let (mut s, _host) = sampler(
                doc.clone(),
                &[(id::ROOT, 69.0), (id::VOLUME, 0.0), (id::PITCH_MODE, mode)],
            );
            s.send(on(81, 127));
            let (l, _, _) = s.run(1.0);
            let sounding = l.iter().rposition(|v| v.abs() > 0.01).unwrap_or(0);
            let head = &l[..(0.2 * SR) as usize];
            (
                sounding as f64 / SR,
                bin_db(head, 880.0),
                bin_db(head, 440.0),
            )
        };
        let (repitch, hi, lo) = play(0.0);
        assert!((repitch - 0.25).abs() < 0.03, "{repitch}");
        assert!(hi > -12.0 && lo < -40.0, "{hi} {lo}");
        let (kept, hi, lo) = play(1.0);
        assert!((kept - 0.5).abs() < 0.05, "the length stays: {kept}");
        assert!(hi > -14.0, "an octave up: {hi}");
        assert!(lo < hi - 20.0, "not the original pitch: {lo} {hi}");
        // It starts on time: the first 20 ms already sound.
        let (mut s, _host) = sampler(
            doc,
            &[(id::ROOT, 69.0), (id::VOLUME, 0.0), (id::PITCH_MODE, 1.0)],
        );
        s.send(on(81, 127));
        let (l, _, _) = s.run(0.02);
        let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1, "no latency: {peak}");
    }

    /// A sampler playing `sfz` (tones written as `name.wav` first).
    fn sfz(
        name: &str,
        tones: &[(&str, f64, f64)],
        sfz: &str,
    ) -> (Play<SamplerProcessor>, SampleHost) {
        let d = fixtures::dir(name);
        for (file, f, seconds) in tones {
            fixtures::tone(&d.join(format!("{file}.wav")), *f, 48_000, *seconds);
        }
        std::fs::write(d.join("i.sfz"), sfz).unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(d.join("i.sfz").to_string_lossy().into_owned()));
        sampler(doc, &[(id::VOLUME, 0.0)])
    }

    fn cc(controller: u8, value: u8) -> MidiEvent {
        MidiEvent::ControlChange {
            channel: 0,
            controller,
            value,
        }
    }

    fn rms_db(x: &[f32]) -> f64 {
        let e: f64 = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
        10.0 * e.max(1e-30).log10()
    }

    /// Zero crossings upwards per second.
    fn freq(x: &[f32]) -> f64 {
        let ups = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f64 * SR / x.len() as f64
    }

    #[test]
    fn sfz_generators_play_at_the_keys_pitch() {
        let (mut s, _h) = sfz("sfz-gen", &[], "<region>sample=*sine ampeg_release=0.01");
        s.send(on(69, 127));
        let (l, _, _) = s.run(0.5);
        let f = freq(&l[l.len() / 2..]);
        assert!((f - 440.0).abs() < 3.0, "{f}");
    }

    #[test]
    fn sfz_filters_and_their_envelope_shape_the_sound() {
        let (mut s, _h) = sfz(
            "sfz-fil",
            &[],
            "<region>sample=*saw key=60 ampeg_release=0.01\n<region>sample=*saw key=62 ampeg_release=0.01 fil_type=lpf_2p cutoff=400\n<region>sample=*saw key=64 ampeg_release=0.01 fil_type=lpf_4p cutoff=200 fileg_depth=3600 fileg_decay=1 fileg_sustain=0",
        );
        let h8 = 261.6256 * 8.0;
        let play = |s: &mut Play<SamplerProcessor>, key: u8, secs: f64| {
            s.send(on(key, 127));
            let (l, _, _) = s.run(secs);
            s.send(off(key));
            s.run(0.2);
            l
        };
        let open = play(&mut s, 60, 0.4);
        let low = play(&mut s, 62, 0.4);
        let first = |x: &[f32]| bin_db(x, 261.6256);
        assert!((first(&open) - first(&low)).abs() < 3.0);
        assert!(
            bin_db(&open, h8) > bin_db(&low, h8) + 20.0,
            "{} vs {}",
            bin_db(&open, h8),
            bin_db(&low, h8)
        );
        // The filter envelope opens the 4-pole filter, then closes it.
        let swept = play(&mut s, 64, 1.0);
        let h4 = 261.6256 * 4.0;
        let (early, late) = (&swept[..2_400], &swept[38_400..48_000]);
        assert!(
            bin_db(early, h4) > bin_db(late, h4) + 30.0,
            "{} vs {}",
            bin_db(early, h4),
            bin_db(late, h4)
        );
    }

    #[test]
    fn sfz_controllers_modulate_and_choose_regions() {
        let (mut s, _h) = sfz(
            "sfz-cc",
            &[("a", 300.0, 1.0), ("b", 500.0, 1.0)],
            "<control>set_cc1=127\n<global>ampeg_release=0.01\n<region>sample=a.wav key=60 volume_oncc20=-20 hicc30=63\n<region>sample=b.wav key=60 pitch_keycenter=60 locc30=64\n<region>sample=*sine key=69 pitch_oncc1=1200 bend_up=700\n<region>sample=a.wav key=72 pitch_keycenter=72 on_locc31=64 on_hicc31=127",
        );
        let play = |s: &mut Play<SamplerProcessor>, key: u8| {
            s.send(on(key, 127));
            let (l, _, _) = s.run(0.3);
            s.send(off(key));
            s.run(0.1);
            l
        };
        let full = play(&mut s, 60);
        s.send(cc(20, 127));
        s.run(0.01);
        let down = play(&mut s, 60);
        let drop = bin_db(&full, 300.0) - bin_db(&down, 300.0);
        assert!((drop - 20.0).abs() < 1.0, "{drop}");
        // CC 30 high: the other region.
        s.send(cc(30, 100));
        s.run(0.01);
        let other = play(&mut s, 60);
        assert!(bin_db(&other, 500.0) > bin_db(&other, 300.0) + 30.0);
        // `key` sets the root too (middle C for a generator); the control
        // header's CC 1 (127) raises it an octave, a full bend up a fifth.
        let up = play(&mut s, 69);
        assert!((freq(&up[up.len() / 2..]) - 523.25).abs() < 6.0);
        s.send(MidiEvent::PitchBend {
            channel: 0,
            value: 16_383,
        });
        let bent = play(&mut s, 69);
        let fifth = 523.25 * 2f64.powf(7.0 / 12.0);
        assert!((freq(&bent[bent.len() / 2..]) - fifth).abs() < 8.0);
        // A controller moving into a region's on_cc range plays it.
        s.send(MidiEvent::PitchBend {
            channel: 0,
            value: 8_192,
        });
        s.send(cc(31, 100));
        let (l, _, _) = s.run(0.3);
        assert!(bin_db(&l, 300.0) > -20.0, "{}", bin_db(&l, 300.0));
    }

    #[test]
    fn sfz_keyswitches_pick_the_articulation() {
        let (mut s, _h) = sfz(
            "sfz-sw",
            &[("a", 300.0, 1.0), ("b", 500.0, 1.0)],
            "<global>sw_lokey=24 sw_hikey=25 sw_default=24 key=60 ampeg_release=0.01\n<region>sample=a.wav sw_last=24\n<region>sample=b.wav sw_last=25",
        );
        let play = |s: &mut Play<SamplerProcessor>| {
            s.send(on(60, 100));
            let (l, _, _) = s.run(0.3);
            s.send(off(60));
            s.run(0.1);
            l
        };
        let a = play(&mut s);
        assert!(bin_db(&a, 300.0) > bin_db(&a, 500.0) + 30.0);
        s.send(on(25, 100));
        s.send(off(25));
        let (silent, _, _) = s.run(0.1);
        assert!(rms_db(&silent) < -90.0, "a keyswitch plays nothing");
        let b = play(&mut s);
        assert!(bin_db(&b, 500.0) > bin_db(&b, 300.0) + 30.0);
    }

    #[test]
    fn sfz_release_triggers_first_and_legato() {
        let (mut s, _h) = sfz(
            "sfz-rel",
            &[("a", 300.0, 1.0), ("b", 500.0, 1.0), ("r", 700.0, 0.3)],
            "<global>ampeg_release=0.01\n<region>sample=a.wav lokey=60 hikey=62 pitch_keycenter=60 trigger=first\n<region>sample=b.wav lokey=60 hikey=62 pitch_keycenter=60 trigger=legato\n<region>sample=r.wav key=64 trigger=release rt_decay=20",
        );
        // Alone: the first region; with a key held: the legato one.
        s.send(on(60, 100));
        let (alone, _, _) = s.run(0.2);
        assert!(bin_db(&alone, 300.0) > bin_db(&alone, 500.0) + 30.0);
        s.send(on(60 + 2, 100));
        let (both, _, _) = s.run(0.2);
        let b = 500.0 * 2f64.powf(2.0 / 12.0);
        assert!(bin_db(&both, b) > -20.0, "{}", bin_db(&both, b));
        s.send(off(60));
        s.send(off(62));
        s.run(0.2);
        // A release trigger loses 20 dB per second held.
        let release = |s: &mut Play<SamplerProcessor>, held: f64| {
            s.send(on(64, 100));
            let (pressed, _, _) = s.run(held);
            assert!(rms_db(&pressed) < -90.0, "nothing until the release");
            s.send(off(64));
            let (l, _, _) = s.run(0.4);
            rms_db(&l[..4_800])
        };
        let short = release(&mut s, 0.1);
        let long = release(&mut s, 1.1);
        assert!(((short - long) - 20.0).abs() < 2.0, "{short} {long}");
    }

    #[test]
    fn sfz_velocity_crossfades_lfos_delays_and_counts() {
        let (mut s, _h) = sfz(
            "sfz-misc",
            &[("a", 300.0, 1.0), ("b", 500.0, 1.0), ("c", 700.0, 0.1)],
            "<global>ampeg_release=0.01\n<region>sample=a.wav key=60 xfin_lovel=1 xfin_hivel=127\n<region>sample=b.wav key=60 pitch_keycenter=60 xfout_lovel=1 xfout_hivel=127\n<region>sample=*sine key=62 amplfo_freq=4 amplfo_depth=6\n<region>sample=a.wav key=64 pitch_keycenter=64 delay=0.1\n<region>sample=c.wav key=65 pitch_keycenter=65 count=2\n<region>sample=c.wav key=67 pitch_keycenter=67 loop_mode=one_shot\n<region>sample=*sine key=69 note_polyphony=1",
        );
        let play = |s: &mut Play<SamplerProcessor>, key: u8, vel: u8, secs: f64| {
            s.send(on(key, vel));
            let (l, _, _) = s.run(secs);
            s.send(off(key));
            s.run(0.1);
            l
        };
        let hard = play(&mut s, 60, 127, 0.3);
        assert!(bin_db(&hard, 300.0) > bin_db(&hard, 500.0) + 40.0);
        let mid = play(&mut s, 60, 64, 0.3);
        assert!((bin_db(&mid, 300.0) - bin_db(&mid, 500.0)).abs() < 1.0);
        // ±6 dB of tremolo.
        let trem = play(&mut s, 62, 127, 1.0);
        let windows: Vec<f64> = trem[9_600..].chunks(960).map(rms_db).collect();
        let (lo, hi) = windows
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), w| (a.min(*w), b.max(*w)));
        assert!(hi - lo > 9.0 && hi - lo < 13.0, "{lo} {hi}");
        // Silent for its delay.
        let delayed = play(&mut s, 64, 127, 0.3);
        assert!(rms_db(&delayed[..4_000]) < -90.0);
        assert!(rms_db(&delayed[6_000..]) > -20.0);
        // Twice through, then done (a one-shot ignores the note-off).
        let counted = play(&mut s, 65, 127, 0.3);
        assert!(rms_db(&counted[5_000..9_000]) > -20.0, "the second time");
        assert!(rms_db(&counted[10_500..]) < -90.0);
        let once = play(&mut s, 67, 127, 0.3);
        assert!(rms_db(&once[5_000..9_000]) < -90.0);
        // One voice per key.
        s.send(on(69, 127));
        s.run(0.05);
        s.send(on(69, 127));
        s.run(0.05);
        let sounding =
            s.p.voices
                .iter()
                .filter(|v| matches!(v.stage, Stage::Attack | Stage::Decay | Stage::Sustain))
                .count();
        assert_eq!(sounding, 1);
    }

    #[test]
    fn sfz_layers_round_robins_and_chokes() {
        let d = fixtures::dir("sfz-play");
        for (name, f) in [
            ("soft", 300.0),
            ("loud", 500.0),
            ("rr1", 700.0),
            ("rr2", 900.0),
        ] {
            fixtures::tone(&d.join(format!("{name}.wav")), f, 48_000, 1.0);
        }
        let sfz = "<group>pitch_keycenter=60 key=60 ampeg_release=0.01\n<region>sample=soft.wav hivel=63\n<region>sample=loud.wav lovel=64\n<group>key=62 pitch_keycenter=62 seq_length=2 group=1\n<region>sample=rr1.wav seq_position=1\n<region>sample=rr2.wav seq_position=2\n<group>key=64 pitch_keycenter=64 off_by=1\n<region>sample=soft.wav";
        std::fs::write(d.join("i.sfz"), sfz).unwrap();
        let mut doc = SampleDoc::default();
        doc.set(0, Some(d.join("i.sfz").to_string_lossy().into_owned()));
        let (mut s, host) = sampler(doc, &[(id::VOLUME, 0.0)]);
        assert_eq!(host.doc.files.len(), 1);
        let play = |s: &mut Play<SamplerProcessor>, key: u8, vel: u8| {
            s.send(on(key, vel));
            let (l, _, _) = s.run(0.2);
            s.send(off(key));
            s.run(0.3);
            l
        };
        let soft = play(&mut s, 60, 40);
        assert!(bin_db(&soft, 300.0) > bin_db(&soft, 500.0) + 30.0);
        let loud = play(&mut s, 60, 120);
        assert!(bin_db(&loud, 500.0) > bin_db(&loud, 300.0) + 30.0);
        let first = play(&mut s, 62, 100);
        let second = play(&mut s, 62, 100);
        assert!(bin_db(&first, 700.0) > bin_db(&first, 900.0) + 30.0);
        assert!(bin_db(&second, 900.0) > bin_db(&second, 700.0) + 30.0);
        // Key 64 (group 1's off_by) is cut by a hit on key 62 (group 1).
        s.send(on(64, 100));
        s.run(0.1);
        s.send(on(62, 100));
        let (l, _, _) = s.run(0.3);
        let tail = &l[l.len() / 2..];
        assert!(
            bin_db(tail, 300.0) < -60.0,
            "choked: {}",
            bin_db(tail, 300.0)
        );
    }
}
