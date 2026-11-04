//! FM: six sine operators in phase modulation.
//!
//! The operators are wired by one of the 32 classic algorithms
//! ([`algorithms`], written from their published diagrams) or a free
//! matrix (any operator into any other's phase, the diagonal its own
//! feedback, each heard by its own output level). Each has a ratio to the
//! key (coarse ½, 1…31 and a fine fraction) or a fixed frequency, a
//! detune, an output level on the classic 0–99 scale (¾ dB a step), its
//! own envelope (attack to a peak, a decay to a break level, a slope to
//! the sustain, a release — the decays in dB, at a rate set by the time
//! between their levels), velocity and keyboard level scaling either side
//! of a break key, keyboard rate scaling and LFO amplitude sensitivity.
//! A feedback loop as the algorithm draws it (the mean of the last two
//! outputs, as the original does), a pitch envelope, transpose, LFO 1 to
//! pitch and to the operators' amplitude, key-synced or free phases, and
//! the carriers spread across the stereo field if wanted. Then the shared
//! parts ([`super::poly`]). Voices from DX7 SysEx dumps load through
//! [`dx7`].

pub mod algorithms;
pub mod dx7;
#[cfg(test)]
mod tests;

use super::poly::{
    self, Block, Chassis, Common, Defaults, Head, NoteStart, Output, Step, Steps, Voiced,
};
use super::synth::{self, ChannelState, Env, Fx};
use super::{on_off, param, stepped};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use algorithms::ALGORITHMS;
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::{Arc, OnceLock};

/// Stable parameter ids (`id == position`): the shared block, the FM
/// globals, per operator [`OP_FIELDS`] fields at [`op`], then the matrix
/// ([`matrix`], [`out`]).
pub mod id {
    use super::poly::COMMON;
    pub use super::poly::id::*;

    /// 0–31: algorithms 1–32; 32: the free matrix.
    pub const ALGORITHM: u32 = COMMON;
    pub const FEEDBACK: u32 = COMMON + 1;
    pub const TRANSPOSE: u32 = COMMON + 2;
    /// LFO 1 to pitch (cents) and to the operators' amplitude (by each
    /// one's sensitivity).
    pub const LFO_PITCH: u32 = COMMON + 3;
    pub const LFO_AMP: u32 = COMMON + 4;
    /// The pitch envelope: from an offset at the note to the key's pitch
    /// in a time; after the key to an offset in a time.
    pub const PITCH_START: u32 = COMMON + 5;
    pub const PITCH_TIME: u32 = COMMON + 6;
    pub const PITCH_END: u32 = COMMON + 7;
    pub const PITCH_RELEASE: u32 = COMMON + 8;
    /// The operators start every note from zero phase (off: free).
    pub const KEY_SYNC: u32 = COMMON + 9;
    /// The carriers spread across the stereo field.
    pub const SPREAD: u32 = COMMON + 10;
    pub const OPS: u32 = COMMON + 11;

    /// An operator's fields.
    /// Coarse ratio: 0 = ½, 1…31.
    pub const RATIO: u32 = 0;
    /// The ratio times 1 + fine.
    pub const FINE: u32 = 1;
    pub const FIXED: u32 = 2;
    /// The fixed frequency (Hz).
    pub const FREQ: u32 = 3;
    /// Cents.
    pub const DETUNE: u32 = 4;
    /// Output level 0–99 (¾ dB a step).
    pub const LEVEL: u32 = 5;
    pub const VELOCITY: u32 = 6;
    pub const AMP_MOD: u32 = 7;
    pub const OP_ATTACK: u32 = 8;
    /// Levels 0–99: after the attack, after the decay, held.
    pub const PEAK: u32 = 9;
    pub const OP_DECAY: u32 = 10;
    pub const BREAK: u32 = 11;
    pub const SLOPE: u32 = 12;
    pub const OP_SUSTAIN: u32 = 13;
    pub const OP_RELEASE: u32 = 14;
    /// Envelopes faster up the keyboard.
    pub const RATE_SCALE: u32 = 15;
    /// Keyboard level scaling: the break key, and ±24 dB an octave at
    /// full either side of it.
    pub const KEY_BREAK: u32 = 16;
    pub const KEY_LEFT: u32 = 17;
    pub const KEY_RIGHT: u32 = 18;
    pub const OP_FIELDS: u32 = 19;

    /// Operator `k`'s (0–5: operators 1–6) `field`.
    pub const fn op(k: usize, field: u32) -> u32 {
        OPS + OP_FIELDS * k as u32 + field
    }

    pub const MATRIX: u32 = OPS + 6 * OP_FIELDS;
    /// The free matrix: operator `from` into `to`'s phase.
    pub const fn matrix(from: usize, to: usize) -> u32 {
        MATRIX + 6 * from as u32 + to as u32
    }
    /// The free matrix: how loud operator `k` is heard.
    pub const fn out(k: usize) -> u32 {
        MATRIX + 36 + k as u32
    }
}

/// The free matrix's place on the Algorithm parameter.
pub const FREE_MATRIX: usize = 32;

/// Matrix and wheel targets: the shared ones, the operators' levels (±99
/// steps at full), the feedback (±100 %) and the modulation index (every
/// modulator ±18 dB).
pub const TARGETS: [&str; 18] = [
    "Off",
    "Vibrato",
    "Pitch",
    "Cutoff",
    "Resonance",
    "Filter Env",
    "Drive",
    "Volume",
    "Pan",
    "LFO 2 Rate",
    "Op 1 Level",
    "Op 2 Level",
    "Op 3 Level",
    "Op 4 Level",
    "Op 5 Level",
    "Op 6 Level",
    "Feedback",
    "Mod Index",
];

/// [`TARGETS`] by name.
pub mod target {
    pub use super::poly::target::*;
    pub const OP_LEVEL: [usize; 6] = [10, 11, 12, 13, 14, 15];
    pub const FEEDBACK: usize = 16;
    pub const MOD_INDEX: usize = 17;
}

/// Published: the shared values, then the newest voice's operator
/// envelopes (0…1).
pub mod value {
    pub use super::poly::value::*;
    pub const OP_ENV: [usize; 6] = [9, 10, 11, 12, 13, 14];
}
pub const TAP_VALUES: usize = 15;

/// dB a level step is.
const STEP_DB: f32 = 0.75;
/// What velocity takes off at full sensitivity and the softest touch.
const VELOCITY_DB: f32 = 36.0;
/// Keyboard level scaling at full: dB an octave.
const KEY_DB: f32 = 24.0;
/// Below this an envelope has ended.
const FLOOR_DB: f32 = -74.25;
/// LFO amplitude modulation at full.
const LFO_AMP_DEPTH: f32 = 0.9;

const DEFAULTS: Defaults = Defaults {
    volume: -8.0,
    filter: 0.0,
    cutoff: 12_000.0,
    attack: 0.5,
    decay: 10_000.0,
    sustain: 1.0,
    release: 10_000.0,
};

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    use id::*;
    let mut p = poly::parameters(TARGETS.len(), &DEFAULTS);
    p.extend([
        stepped(ALGORITHM, "Algorithm", FREE_MATRIX as f64, 0.0),
        param(FEEDBACK, "Feedback", 0.0, 1.0, 0.0, Percent),
        ranged(TRANSPOSE, "Transpose", -24.0, 24.0, 0.0),
        param(LFO_PITCH, "LFO to Pitch", 0.0, 300.0, 0.0, None),
        param(LFO_AMP, "LFO to Amplitude", 0.0, 1.0, 0.0, Percent),
        param(PITCH_START, "Pitch Env Start", -24.0, 24.0, 0.0, None),
        param(
            PITCH_TIME,
            "Pitch Env Time",
            1.0,
            10_000.0,
            100.0,
            Milliseconds,
        ),
        param(PITCH_END, "Pitch Env End", -24.0, 24.0, 0.0, None),
        param(
            PITCH_RELEASE,
            "Pitch Env Release",
            1.0,
            10_000.0,
            100.0,
            Milliseconds,
        ),
        stepped(KEY_SYNC, "Key Sync", 1.0, 1.0),
        param(SPREAD, "Stereo Spread", 0.0, 1.0, 0.0, Percent),
    ]);
    for k in 0..6 {
        let n = |f: &str| format!("Op {} {f}", k + 1);
        let first = k == 0;
        p.extend([
            ranged(op(k, RATIO), &n("Ratio"), 0.0, 31.0, 1.0),
            param(op(k, FINE), &n("Fine"), 0.0, 0.99, 0.0, None),
            stepped(op(k, FIXED), &n("Fixed"), 1.0, 0.0),
            param(op(k, FREQ), &n("Frequency"), 1.0, 9_772.0, 440.0, Hertz),
            param(op(k, DETUNE), &n("Detune"), -25.0, 25.0, 0.0, None),
            param(
                op(k, LEVEL),
                &n("Level"),
                0.0,
                99.0,
                if first { 99.0 } else { 0.0 },
                None,
            ),
            param(op(k, VELOCITY), &n("Velocity"), 0.0, 1.0, 0.3, Percent),
            param(op(k, AMP_MOD), &n("Amp Mod"), 0.0, 1.0, 0.0, Percent),
            param(
                op(k, OP_ATTACK),
                &n("Attack"),
                0.5,
                20_000.0,
                1.0,
                Milliseconds,
            ),
            param(op(k, PEAK), &n("Peak"), 0.0, 99.0, 99.0, None),
            param(
                op(k, OP_DECAY),
                &n("Decay"),
                1.0,
                30_000.0,
                600.0,
                Milliseconds,
            ),
            param(op(k, BREAK), &n("Break"), 0.0, 99.0, 90.0, None),
            param(
                op(k, SLOPE),
                &n("Slope"),
                1.0,
                30_000.0,
                2_000.0,
                Milliseconds,
            ),
            param(op(k, OP_SUSTAIN), &n("Sustain"), 0.0, 99.0, 85.0, None),
            param(
                op(k, OP_RELEASE),
                &n("Release"),
                5.0,
                30_000.0,
                400.0,
                Milliseconds,
            ),
            param(
                op(k, RATE_SCALE),
                &n("Rate Scaling"),
                0.0,
                1.0,
                0.2,
                Percent,
            ),
            ranged(op(k, KEY_BREAK), &n("Key Break"), 0.0, 127.0, 60.0),
            param(op(k, KEY_LEFT), &n("Key Left"), -1.0, 1.0, 0.0, Percent),
            param(op(k, KEY_RIGHT), &n("Key Right"), -1.0, 1.0, 0.0, Percent),
        ]);
    }
    for from in 0..6 {
        for to in 0..6 {
            let name = if from == to {
                format!("Matrix Op {} Feedback", from + 1)
            } else {
                format!("Matrix Op {} to Op {}", from + 1, to + 1)
            };
            p.push(param(matrix(from, to), &name, 0.0, 1.0, 0.0, Percent));
        }
    }
    for k in 0..6 {
        p.push(param(
            out(k),
            &format!("Matrix Op {} Out", k + 1),
            0.0,
            1.0,
            if k == 0 { 1.0 } else { 0.0 },
            Percent,
        ));
    }
    p
}

/// The ratio an operator's coarse and fine values make.
pub fn ratio(coarse: f64, fine: f64) -> f64 {
    let c = coarse.round().clamp(0.0, 31.0);
    (if c == 0.0 { 0.5 } else { c }) * (1.0 + fine.clamp(0.0, 0.99))
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::format(pid, v, &TARGETS);
    }
    Some(match pid.0 {
        ALGORITHM if v.round() as usize >= FREE_MATRIX => "Matrix".into(),
        ALGORITHM => format!("{:.0}", v.round() + 1.0),
        TRANSPOSE => format!("{:+.0} st", v.round()).replace("+0 st", "0 st"),
        LFO_PITCH => format!("{v:.0} ¢"),
        PITCH_START | PITCH_END => format!("{v:+.1} st").replace("+0.0 st", "0 st"),
        KEY_SYNC => on_off(v),
        x if (OPS..MATRIX).contains(&x) => match (x - OPS) % OP_FIELDS {
            RATIO => {
                let c = v.round();
                if c == 0.0 {
                    "½".into()
                } else {
                    format!("{c:.0}")
                }
            }
            FINE => format!("+{:.0} %", v * 100.0),
            FIXED => if v >= 0.5 { "Fixed" } else { "Ratio" }.into(),
            DETUNE => format!("{v:+.1} ¢"),
            LEVEL | PEAK | BREAK | OP_SUSTAIN => format!("{v:.0}"),
            KEY_BREAK => super::note_name(v.round() as i32),
            KEY_LEFT | KEY_RIGHT => format!("{:+.1} dB/oct", v * f64::from(KEY_DB)),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether a parameter takes modulation per note (a voice's own).
pub fn per_voice(pid: ParameterId) -> bool {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::per_voice(pid);
    }
    !matches!(pid.0, ALGORITHM | KEY_SYNC | SPREAD)
        && !((OPS..MATRIX).contains(&pid.0)
            && matches!((pid.0 - OPS) % OP_FIELDS, RATIO | FIXED | KEY_BREAK))
}

/// A level step's amplitude (0 is silence).
#[inline]
pub fn step_gain(level: f32) -> f32 {
    if level <= 0.0 {
        0.0
    } else {
        10f32.powf((level.min(99.0) - 99.0) * STEP_DB / 20.0)
    }
}

/// A level step in dB (silence at the floor).
fn step_db(level: f32) -> f32 {
    if level <= 0.0 {
        FLOOR_DB - 1.0
    } else {
        (level.min(99.0) - 99.0) * STEP_DB
    }
}

/// The feedback amount (of the last two outputs' sum, in cycles) at
/// `f` (0…1): a quarter cycle at full, halving every seventh.
fn feedback_cycles(f: f32) -> f32 {
    if f <= 0.0 {
        0.0
    } else {
        0.25 * 2f32.powf(7.0 * (f.min(1.0) - 1.0))
    }
}

/// A sine over a cycle (4096 points and a guard), made once off the
/// audio thread.
fn sine_table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=4096)
            .map(|i| (std::f64::consts::TAU * i as f64 / 4096.0).sin() as f32)
            .collect()
    })
}

#[inline]
fn sine(table: &[f32], phase: f32) -> f32 {
    let p = phase - phase.floor();
    let x = p * 4096.0;
    let i = (x as usize).min(4095);
    let t = x - i as f32;
    table[i] + (table[i + 1] - table[i]) * t
}

#[derive(Clone, Copy, Debug)]
struct OpSettings {
    ratio: f32,
    fixed: Option<f32>,
    /// The detune as a factor.
    detune: f32,
    level: f32,
    velocity: f32,
    ams: f32,
    attack: f32,
    peak: f32,
    decay: f32,
    brk: f32,
    slope: f32,
    sustain: f32,
    release: f32,
    rate_scale: f32,
    key_break: f32,
    key_left: f32,
    key_right: f32,
}

#[derive(Clone, Copy)]
struct Settings {
    c: Common,
    algorithm: usize,
    feedback: f32,
    transpose: f32,
    /// Semitones LFO 1 swings the pitch.
    lfo_pitch: f32,
    lfo_amp: f32,
    pitch_start: f32,
    pitch_time: f32,
    pitch_end: f32,
    pitch_release: f32,
    key_sync: bool,
    spread: f32,
    ops: [OpSettings; 6],
    matrix: [[f32; 6]; 6],
    out: [f32; 6],
}

impl Settings {
    fn read(get: &dyn Fn(u32) -> f32, sr: f32) -> Self {
        use id::*;
        let op_settings = |k: usize| {
            let f = |field| get(op(k, field));
            let fine_ratio = ratio(f64::from(f(RATIO)), f64::from(f(FINE))) as f32;
            OpSettings {
                ratio: fine_ratio,
                fixed: (f(FIXED) >= 0.5).then(|| f(FREQ).max(0.0)),
                detune: 2f32.powf(f(DETUNE) / 1200.0),
                level: f(LEVEL),
                velocity: f(VELOCITY).clamp(0.0, 1.0),
                ams: f(AMP_MOD).clamp(0.0, 1.0),
                attack: f(OP_ATTACK).max(0.1),
                peak: f(PEAK),
                decay: f(OP_DECAY).max(0.1),
                brk: f(BREAK),
                slope: f(SLOPE).max(0.1),
                sustain: f(OP_SUSTAIN),
                release: f(OP_RELEASE).max(0.1),
                rate_scale: f(RATE_SCALE).clamp(0.0, 1.0),
                key_break: f(KEY_BREAK),
                key_left: f(KEY_LEFT).clamp(-1.0, 1.0),
                key_right: f(KEY_RIGHT).clamp(-1.0, 1.0),
            }
        };
        Settings {
            c: Common::read(get, sr, TARGETS.len()),
            algorithm: (get(ALGORITHM).round().max(0.0) as usize).min(FREE_MATRIX),
            feedback: get(FEEDBACK).clamp(0.0, 1.0),
            transpose: get(TRANSPOSE).round(),
            lfo_pitch: get(LFO_PITCH) / 100.0,
            lfo_amp: get(LFO_AMP).clamp(0.0, 1.0),
            pitch_start: get(PITCH_START),
            pitch_time: get(PITCH_TIME).max(1.0),
            pitch_end: get(PITCH_END),
            pitch_release: get(PITCH_RELEASE).max(1.0),
            key_sync: get(KEY_SYNC) >= 0.5,
            spread: get(SPREAD).clamp(0.0, 1.0),
            ops: std::array::from_fn(op_settings),
            matrix: std::array::from_fn(|from| {
                std::array::from_fn(|to| get(matrix(from, to)).clamp(0.0, 1.0))
            }),
            out: std::array::from_fn(|k| get(out(k)).clamp(0.0, 1.0)),
        }
    }

    /// Whether operator `k` is heard.
    fn carrier(&self, k: usize) -> bool {
        match ALGORITHMS.get(self.algorithm) {
            Some(a) => a.is_carrier(k),
            None => self.out[k] > 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Slope,
    Sustain,
    Release,
}

/// An operator's envelope, run every control tick.
#[derive(Clone, Copy, Debug)]
struct OpEnv {
    stage: Stage,
    /// Its level in dB (decays and release) and as an amplitude.
    db: f32,
    amp: f32,
}

impl OpEnv {
    const IDLE: OpEnv = OpEnv {
        stage: Stage::Idle,
        db: FLOOR_DB - 1.0,
        amp: 0.0,
    };

    fn release(&mut self) {
        if !matches!(self.stage, Stage::Idle | Stage::Release) {
            if self.stage == Stage::Attack {
                self.db = 20.0 * self.amp.max(1e-6).log10();
            }
            self.stage = Stage::Release;
        }
    }

    /// Move on a tick (`tick_ms` long, times scaled by `scale`).
    fn tick(&mut self, s: &OpSettings, tick_ms: f32, scale: f32) {
        let ticks = |ms: f32| (ms * scale / tick_ms).max(1.0);
        // Towards `to` at `per_tick` dB; whether it is there.
        let toward = |db: &mut f32, to: f32, per_tick: f32| {
            if (*db - to).abs() <= per_tick {
                *db = to;
                true
            } else {
                *db += per_tick.copysign(to - *db);
                false
            }
        };
        let (peak, brk, sus) = (step_db(s.peak), step_db(s.brk), step_db(s.sustain));
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                let target = step_gain(s.peak);
                self.amp += target.max(1e-3) / ticks(s.attack);
                if self.amp >= target {
                    self.amp = target;
                    self.db = 20.0 * target.max(1e-6).log10();
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let rate = ((peak - brk).abs() / ticks(s.decay)).max(1e-4);
                if (peak - brk).abs() < 1e-3 || toward(&mut self.db, brk, rate) {
                    self.db = brk;
                    self.stage = Stage::Slope;
                }
            }
            Stage::Slope => {
                let rate = ((brk - sus).abs() / ticks(s.slope)).max(1e-4);
                if (brk - sus).abs() < 1e-3 || toward(&mut self.db, sus, rate) {
                    self.db = sus;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.db = sus,
            Stage::Release => {
                self.db -= -FLOOR_DB / ticks(s.release);
                if self.db <= FLOOR_DB {
                    self.stage = Stage::Idle;
                }
            }
        }
        if self.stage == Stage::Idle {
            self.amp = 0.0;
        } else if self.stage != Stage::Attack {
            self.amp = if self.db <= FLOOR_DB {
                0.0
            } else {
                10f32.powf(self.db / 20.0)
            };
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Op {
    phase: f32,
    inc: f32,
    /// The amplitude now and its step per sample to the next tick's.
    amp: f32,
    step: f32,
    /// The last two outputs (feedback).
    y1: f32,
    y2: f32,
    env: OpEnv,
}

impl Op {
    const IDLE: Op = Op {
        phase: 0.0,
        inc: 0.0,
        amp: 0.0,
        step: 0.0,
        y1: 0.0,
        y2: 0.0,
        env: OpEnv::IDLE,
    };
}

#[derive(Clone, Copy)]
struct Voice {
    head: Head,
    ops: [Op; 6],
    /// The pitch envelope's offset now (semitones).
    pitch: f32,
    gain: f32,
    pans: (f32, f32),
    drive: f32,
    /// The feedback amount now.
    feedback: f32,
    own: Option<Settings>,
}

impl Voiced for Voice {
    fn head(&self) -> &Head {
        &self.head
    }
    fn head_mut(&mut self) -> &mut Head {
        &mut self.head
    }
}

impl Voice {
    const IDLE: Voice = Voice {
        head: Head::IDLE,
        ops: [Op::IDLE; 6],
        pitch: 0.0,
        gain: 0.0,
        pans: (1.0, 1.0),
        drive: 0.0,
        feedback: 0.0,
        own: None,
    };

    fn start(&mut self, ns: NoteStart, s: &Settings) {
        if !ns.retrigger {
            return;
        }
        self.own = None;
        self.pitch = s.pitch_start;
        for op in &mut self.ops {
            // From where a stolen voice was (no click), else from silence.
            let amp = if ns.kept { op.env.amp } else { 0.0 };
            op.env = OpEnv {
                stage: Stage::Attack,
                db: FLOOR_DB - 1.0,
                amp,
            };
            if s.key_sync {
                op.phase = 0.0;
            }
            if !ns.kept {
                op.amp = 0.0;
                op.y1 = 0.0;
                op.y2 = 0.0;
            }
        }
    }
}

/// A voice's control tick.
#[allow(clippy::too_many_arguments)]
fn control(v: &mut Voice, s: &Settings, ch: &ChannelState, b: &Block, sr: f32, vibrato: f32) {
    let m = poly::control_tick(&mut v.head, &s.c, ch, b, sr);
    let tick_ms = synth::CONTROL_INTERVAL as f32 * 1000.0 / sr;
    // The pitch envelope.
    let released = v.head.releasing();
    let (to, time) = if released {
        (s.pitch_end, s.pitch_release)
    } else {
        (0.0, s.pitch_time)
    };
    let span = if released {
        (s.pitch_end).abs().max(1e-3)
    } else {
        s.pitch_start.abs().max(1e-3)
    };
    let per = span * tick_ms / time;
    if (v.pitch - to).abs() <= per {
        v.pitch = to;
    } else {
        v.pitch += per.copysign(to - v.pitch);
    }
    let semis = poly::semitones(&v.head, &s.c, ch, &m, vibrato)
        + s.transpose
        + v.pitch
        + s.lfo_pitch * b.lfo;
    let key_hz = 440.0 * 2f32.powf(semis / 12.0);
    let key = f32::from(v.head.key);
    let index = 2f32.powf(3.0 * m.get(target::MOD_INDEX));
    let lfo_am = 1.0 - LFO_AMP_DEPTH * s.lfo_amp * 0.5 * (1.0 - b.lfo);
    let mut carriers_done = true;
    for (k, (op, os)) in v.ops.iter_mut().zip(&s.ops).enumerate() {
        if released {
            op.env.release();
        }
        let scale = 2f32.powf(-os.rate_scale * (key - 60.0) / 12.0);
        op.env.tick(os, tick_ms, scale);
        let carrier = s.carrier(k);
        let level = os.level + 99.0 * m.get(target::OP_LEVEL[k]);
        // A carrier set silent is done whatever its envelope.
        if carrier && op.env.stage != Stage::Idle && level > 0.0 {
            carriers_done = false;
        }
        let hz = os.fixed.unwrap_or(key_hz * os.ratio) * os.detune;
        op.inc = (hz / sr).min(0.5);
        let level = step_gain(level);
        let velocity = 10f32.powf(-VELOCITY_DB * os.velocity * (1.0 - v.head.velocity) / 20.0);
        let away = key - os.key_break;
        let key_db = if away < 0.0 {
            os.key_left * KEY_DB * -away / 12.0
        } else {
            os.key_right * KEY_DB * away / 12.0
        };
        let key_gain = 10f32.powf(key_db.min(12.0) / 20.0);
        let am = 1.0 - os.ams * (1.0 - lfo_am);
        let mut amp = op.env.amp * level * velocity * key_gain * am;
        if !carrier {
            amp *= index;
        }
        op.step = (amp - op.amp) / synth::CONTROL_INTERVAL as f32;
    }
    // Released and every carrier silent: the voice is done.
    if released && carriers_done {
        v.head.amp = Env::IDLE;
    }
    v.feedback = feedback_cycles((s.feedback + m.get(target::FEEDBACK)).clamp(0.0, 1.0));
    v.gain = poly::level(&s.c, &v.head, ch, &m, 1.0);
    v.pans = poly::pan_gains(&v.head, &m);
    v.drive = (s.c.drive + m.get(target::DRIVE)).clamp(0.0, 1.0);
}

/// Carriers' output: a quarter, so six at full keep their headroom.
const OUT: f32 = 0.25;

pub struct FmProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    out: Output,
    sr: f32,
    chassis: Chassis<Voice>,
    fx: Fx,
    sine: &'static [f32],
}

impl FmProcessor {
    pub fn new(params: ParamValues, tap: Option<Arc<AnalysisTap>>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate as f32;
        Self {
            params,
            tap,
            out: Output::new(sr),
            sr,
            chassis: Chassis::new(Voice::IDLE),
            fx: Fx::new(config.sample_rate),
            sine: sine_table(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        s: &Settings,
        b: &Block,
        out_l: &mut [f32],
        mut out_r: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        let sr = self.sr;
        let sine_tbl = self.sine;
        let vibrato = self.chassis.vibrato(end - start, sr);
        let channels = self.chassis.channels;
        let params = &self.params;
        for v in self.chassis.voices.iter_mut().filter(|v| v.head.sounding()) {
            if v.head.notes.dirty {
                let notes = v.head.notes;
                v.own = Some(Settings::read(
                    &|p| params.shifted(p as usize, notes.of(p)),
                    sr,
                ));
                v.head.notes.dirty = false;
            }
            let own = v.own;
            let s = own.as_ref().unwrap_or(s);
            let ch = channels[(v.head.channel & 15) as usize];
            let alg = ALGORITHMS.get(s.algorithm);
            // Each operator's place across the field (alternately left
            // and right).
            let pan: [f32; 6] =
                std::array::from_fn(|k| if k % 2 == 0 { -s.spread } else { s.spread });
            v.head.begin_segment(b);
            for i in start..end {
                let (amp_env, _) = v.head.tick(&s.c);
                if !v.head.sounding() {
                    break;
                }
                if v.head.control == 0 {
                    control(v, s, &ch, b, sr, vibrato);
                    if !v.head.sounding() {
                        break;
                    }
                }
                v.head.control = (v.head.control + 1) % synth::CONTROL_INTERVAL;
                let mut y = [0.0f32; 6];
                for j in (0..6).rev() {
                    let mut m = 0.0f32;
                    match alg {
                        Some(a) => {
                            let bits = a.mods[j];
                            for (i, yi) in y.iter().enumerate().skip(j + 1) {
                                if bits >> i & 1 == 1 {
                                    m += *yi;
                                }
                            }
                            if j == a.feedback.1 {
                                let f = &v.ops[a.feedback.0];
                                m += v.feedback * (f.y1 + f.y2);
                            }
                        }
                        None => {
                            for (i, (row, op)) in s.matrix.iter().zip(&v.ops).enumerate() {
                                let depth = row[j];
                                if depth <= 0.0 {
                                    continue;
                                }
                                m += if i == j {
                                    feedback_cycles(depth) * (op.y1 + op.y2)
                                } else if i > j {
                                    depth * y[i]
                                } else {
                                    depth * op.y1
                                };
                            }
                        }
                    }
                    let op = &mut v.ops[j];
                    op.amp = (op.amp + op.step).max(0.0);
                    y[j] = if op.amp > 0.0 {
                        sine(sine_tbl, op.phase + m) * op.amp
                    } else {
                        0.0
                    };
                    op.phase += op.inc;
                    if op.phase >= 1.0 {
                        op.phase -= 1.0;
                    }
                }
                let mut side = [0.0f32; 2];
                for (k, op) in v.ops.iter_mut().enumerate() {
                    op.y2 = op.y1;
                    op.y1 = y[k];
                    let heard = match alg {
                        Some(a) => {
                            if a.is_carrier(k) {
                                y[k]
                            } else {
                                0.0
                            }
                        }
                        None => y[k] * s.out[k],
                    };
                    if heard != 0.0 {
                        side[0] += heard * (1.0 - pan[k]);
                        side[1] += heard * (1.0 + pan[k]);
                    }
                }
                side[0] *= OUT;
                side[1] *= OUT;
                let (gain, pans, drive) = (v.gain * amp_env, v.pans, v.drive);
                poly::finish(
                    &mut v.head,
                    &s.c,
                    side,
                    gain,
                    pans,
                    drive,
                    out_l,
                    out_r.as_deref_mut(),
                    i,
                );
            }
            v.head.filter.settle();
        }
    }
}

impl PluginProcessor for FmProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let frames = io.frames;
        let sr = self.sr;
        let params = &self.params;
        let get = |p: u32| params.get(p as usize);
        let settings = Settings::read(&get, sr);
        let block = self.chassis.begin_block(&get, ctx.transport, frames, sr);
        let tempo = if ctx.transport.tempo > 0.0 {
            ctx.transport.tempo
        } else {
            120.0
        };
        let fx = poly::fx_settings(&get, f64::from(sr), tempo);
        let watched = self
            .tap
            .as_ref()
            .is_some_and(|t| self.out.watching.check(t, frames));
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        let stereo = out.num_channels() >= 2;
        let mut steps = Steps::new(io.events_in.first(), ctx.note_mods, frames);
        while let Some(step) = steps.next_step() {
            match step {
                Step::Render(a, b) => {
                    if stereo {
                        let (l, r) = out.channel_pair_mut(0, 1);
                        self.render(&settings, &block, l, Some(r), a, b);
                    } else {
                        self.render(&settings, &block, out.channel_mut(0), None, a, b);
                    }
                }
                Step::Midi(e) => {
                    let s = &settings;
                    self.chassis
                        .handle(e, &s.c, &mut |v: &mut Voice, ns| v.start(ns, s));
                }
                Step::NoteMod(m) => {
                    if let Some(i) = self.params.index(m.parameter)
                        && per_voice(m.parameter)
                    {
                        let share = poly::share_of(&self.params, i, m.amount);
                        self.chassis.note_mod(m, i, share);
                    }
                }
            }
        }
        poly::effects(&mut self.fx, &fx, out, frames);
        if let Some(tap) = &self.tap {
            let newest = self.chassis.newest();
            self.out.publish(
                tap,
                newest.map(|v| &v.head),
                self.chassis.sounding(),
                block.lfo,
                settings.c.lfo2_shape,
                sr,
                out,
                frames,
                watched,
            );
            for k in 0..6 {
                tap.set_value(value::OP_ENV[k], newest.map_or(0.0, |v| v.ops[k].env.amp));
            }
        }
        if self.chassis.sounding() == 0 && !self.fx.ringing() {
            self.fx.reset();
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.chassis.reset();
        self.fx.reset();
    }

    fn set_tuning(&mut self, tuning: &faderframe_midi::tuning::TuningTable) {
        self.chassis.tuning = *tuning;
    }
}
