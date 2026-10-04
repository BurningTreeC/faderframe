//! Synth: a polyphonic virtual analogue.
//!
//! Per voice: two oscillators (saw, square with pulse width, triangle,
//! sine; octave, semitone, level), up to seven unison copies spread in
//! pitch and across the stereo field, a sub oscillator an octave down and
//! noise; drive into a state-variable filter (12 or 24 dB low pass, band
//! pass, high pass) with key tracking, velocity and an envelope (its own,
//! or the amplifier's as the first Synth had it); the amplifier envelope.
//! One LFO (free or synced, six shapes) moves pitch, cutoff, level and
//! pulse width. Poly, mono or legato with glide.
//!
//! The first nine parameters are the old Synth's and keep their meaning,
//! as do its MIDI handling (sustain pedal, pitch bend ±2 with RPN 0, MPE
//! zones at ±48, channel pressure louder, CC 74 brighter, mod wheel
//! vibrato) and per-note expressions (tuning, volume, pan, vibrato,
//! expression, brightness, pressure).

use super::{on_off, param, pick, stepped};
use crate::dsp::lfo::{DIVISIONS, Lfo, Shape, division_name};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, db_to_gain};
use faderframe_midi::{MidiEvent, NoteExpressionKind};
use std::f32::consts::PI;
use std::sync::Arc;

pub mod id {
    pub const VOLUME: u32 = 0;
    pub const CUTOFF: u32 = 1;
    pub const RESONANCE: u32 = 2;
    pub const ENV_AMOUNT: u32 = 3;
    pub const ATTACK: u32 = 4;
    pub const DECAY: u32 = 5;
    pub const SUSTAIN: u32 = 6;
    pub const RELEASE: u32 = 7;
    pub const DETUNE: u32 = 8;
    pub const OSC1_WAVE: u32 = 9;
    pub const OSC1_OCTAVE: u32 = 10;
    pub const OSC1_PW: u32 = 11;
    pub const OSC1_LEVEL: u32 = 12;
    pub const OSC2_WAVE: u32 = 13;
    pub const OSC2_OCTAVE: u32 = 14;
    pub const OSC2_SEMI: u32 = 15;
    pub const OSC2_LEVEL: u32 = 16;
    pub const SUB: u32 = 17;
    pub const NOISE: u32 = 18;
    pub const UNISON: u32 = 19;
    pub const UNISON_SPREAD: u32 = 20;
    pub const WIDTH: u32 = 21;
    pub const FILTER_TYPE: u32 = 22;
    pub const DRIVE: u32 = 23;
    pub const KEY_TRACK: u32 = 24;
    pub const VELOCITY_CUTOFF: u32 = 25;
    pub const FILTER_ENV: u32 = 26;
    pub const F_ATTACK: u32 = 27;
    pub const F_DECAY: u32 = 28;
    pub const F_SUSTAIN: u32 = 29;
    pub const F_RELEASE: u32 = 30;
    pub const LFO_RATE: u32 = 31;
    pub const LFO_SYNC: u32 = 32;
    pub const LFO_DIVISION: u32 = 33;
    pub const LFO_SHAPE: u32 = 34;
    pub const LFO_PITCH: u32 = 35;
    pub const LFO_CUTOFF: u32 = 36;
    pub const LFO_AMP: u32 = 37;
    pub const LFO_PW: u32 = 38;
    pub const VOICE_MODE: u32 = 39;
    pub const GLIDE: u32 = 40;
    pub const VELOCITY_AMP: u32 = 41;
    pub const POLYPHONY: u32 = 42;
}

/// Published: voices sounding, the newest voice's key, amplifier and
/// filter envelopes (0…1) and cutoff (Hz), the output's peak (linear),
/// the LFO (−1…1).
pub mod value {
    pub const VOICES: usize = 0;
    pub const NOTE: usize = 1;
    pub const AMP_ENV: usize = 2;
    pub const FILTER_ENV: usize = 3;
    pub const CUTOFF: usize = 4;
    pub const OUT_PEAK: usize = 5;
    pub const LFO: usize = 6;
}
pub const TAP_VALUES: usize = 7;

pub const WAVES: [&str; 4] = ["Saw", "Square", "Triangle", "Sine"];
pub const FILTERS: [&str; 4] = ["LP 12", "LP 24", "Band", "High"];
pub const MODES: [&str; 3] = ["Poly", "Mono", "Legato"];
pub const SHAPES: [&str; 6] = ["Sine", "Triangle", "Saw", "Square", "S&H", "Drift"];
pub const FILTER_ENVS: [&str; 2] = ["Amp", "Own"];

const MAX_VOICES: usize = 32;
const MAX_UNISON: usize = 7;
/// Filter coefficients and pitches are recomputed every this many samples.
const CONTROL_INTERVAL: u32 = 16;

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::VOLUME, "Volume", -48.0, 6.0, -6.0, Decibels),
        param(id::CUTOFF, "Cutoff", 20.0, 20_000.0, 2_400.0, Hertz),
        param(id::RESONANCE, "Resonance", 0.0, 1.0, 0.25, Percent),
        param(id::ENV_AMOUNT, "Env Amount", -1.0, 1.0, 0.5, Percent),
        param(id::ATTACK, "Attack", 0.5, 10_000.0, 5.0, Milliseconds),
        param(id::DECAY, "Decay", 1.0, 10_000.0, 300.0, Milliseconds),
        param(id::SUSTAIN, "Sustain", 0.0, 1.0, 0.6, Percent),
        param(id::RELEASE, "Release", 5.0, 10_000.0, 350.0, Milliseconds),
        param(id::DETUNE, "Detune", 0.0, 50.0, 9.0, None),
        stepped(id::OSC1_WAVE, "Osc 1 Wave", 3.0, 0.0),
        ranged(id::OSC1_OCTAVE, "Osc 1 Octave", -2.0, 2.0, 0.0),
        param(id::OSC1_PW, "Osc 1 Pulse Width", 0.05, 0.95, 0.5, Percent),
        param(id::OSC1_LEVEL, "Osc 1 Level", 0.0, 1.0, 1.0, Percent),
        stepped(id::OSC2_WAVE, "Osc 2 Wave", 3.0, 0.0),
        ranged(id::OSC2_OCTAVE, "Osc 2 Octave", -2.0, 2.0, 0.0),
        ranged(id::OSC2_SEMI, "Osc 2 Semitone", -12.0, 12.0, 0.0),
        param(id::OSC2_LEVEL, "Osc 2 Level", 0.0, 1.0, 1.0, Percent),
        param(id::SUB, "Sub", 0.0, 1.0, 0.0, Percent),
        param(id::NOISE, "Noise", 0.0, 1.0, 0.0, Percent),
        ranged(id::UNISON, "Unison", 1.0, 7.0, 1.0),
        param(id::UNISON_SPREAD, "Unison Spread", 0.0, 100.0, 20.0, None),
        param(id::WIDTH, "Width", 0.0, 1.0, 0.6, Percent),
        stepped(id::FILTER_TYPE, "Filter Type", 3.0, 0.0),
        param(id::DRIVE, "Drive", 0.0, 1.0, 0.0, Percent),
        param(id::KEY_TRACK, "Key Tracking", 0.0, 1.0, 0.0, Percent),
        param(
            id::VELOCITY_CUTOFF,
            "Velocity to Cutoff",
            0.0,
            1.0,
            1.0,
            Percent,
        ),
        stepped(id::FILTER_ENV, "Filter Envelope", 1.0, 0.0),
        param(
            id::F_ATTACK,
            "Filter Attack",
            0.5,
            10_000.0,
            5.0,
            Milliseconds,
        ),
        param(
            id::F_DECAY,
            "Filter Decay",
            1.0,
            10_000.0,
            300.0,
            Milliseconds,
        ),
        param(id::F_SUSTAIN, "Filter Sustain", 0.0, 1.0, 0.3, Percent),
        param(
            id::F_RELEASE,
            "Filter Release",
            5.0,
            10_000.0,
            350.0,
            Milliseconds,
        ),
        param(id::LFO_RATE, "LFO Rate", 0.05, 20.0, 4.0, Hertz),
        stepped(id::LFO_SYNC, "LFO Sync", 1.0, 0.0),
        stepped(
            id::LFO_DIVISION,
            "LFO Division",
            (DIVISIONS.len() - 1) as f64,
            11.0,
        ),
        stepped(id::LFO_SHAPE, "LFO Shape", 5.0, 0.0),
        param(id::LFO_PITCH, "LFO to Pitch", 0.0, 100.0, 0.0, None),
        param(id::LFO_CUTOFF, "LFO to Cutoff", 0.0, 4.0, 0.0, None),
        param(id::LFO_AMP, "LFO to Level", 0.0, 1.0, 0.0, Percent),
        param(id::LFO_PW, "LFO to Pulse Width", 0.0, 0.45, 0.0, None),
        stepped(id::VOICE_MODE, "Voice Mode", 2.0, 0.0),
        param(id::GLIDE, "Glide", 0.0, 2_000.0, 0.0, Milliseconds),
        param(
            id::VELOCITY_AMP,
            "Velocity to Level",
            0.0,
            1.0,
            1.0,
            Percent,
        ),
        ranged(id::POLYPHONY, "Voices", 1.0, MAX_VOICES as f64, 16.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::OSC1_WAVE | id::OSC2_WAVE => pick(&WAVES, v),
        id::FILTER_TYPE => pick(&FILTERS, v),
        id::VOICE_MODE => pick(&MODES, v),
        id::LFO_SHAPE => pick(&SHAPES, v),
        id::FILTER_ENV => pick(&FILTER_ENVS, v),
        id::LFO_SYNC => on_off(v),
        id::LFO_DIVISION => division_name(v.round().max(0.0) as usize).into(),
        id::OSC1_OCTAVE | id::OSC2_OCTAVE | id::OSC2_SEMI => {
            format!("{:+.0}", v.round()).replace("+0", "0")
        }
        id::UNISON | id::POLYPHONY => format!("{:.0}", v.round()),
        id::DETUNE | id::UNISON_SPREAD | id::LFO_PITCH => format!("{v:.0} ¢"),
        id::LFO_CUTOFF => format!("{v:.1} oct"),
        id::LFO_PW => format!("{:.0} %", v * 100.0),
        id::ENV_AMOUNT => format!("{:+.0} %", v * 100.0).replace("+0 %", "0 %"),
        id::GLIDE if v < 0.5 => "Off".into(),
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// An ADSR's per-sample settings.
#[derive(Clone, Copy, Default)]
struct EnvSettings {
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

#[derive(Clone, Copy, Debug)]
struct Env {
    stage: Stage,
    value: f32,
}

impl Env {
    const IDLE: Env = Env {
        stage: Stage::Idle,
        value: 0.0,
    };

    #[inline]
    fn tick(&mut self, s: &EnvSettings) -> f32 {
        match self.stage {
            Stage::Attack => {
                self.value += s.attack_step;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.value = s.sustain + (self.value - s.sustain) * s.decay_coef;
                if (self.value - s.sustain).abs() < 1e-4 {
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.value = s.sustain,
            Stage::Release => {
                self.value *= s.release_coef;
                if self.value < 1e-4 {
                    self.stage = Stage::Idle;
                    self.value = 0.0;
                }
            }
            Stage::Idle => {}
        }
        self.value
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }
}

/// A TPT state-variable filter's state.
#[derive(Clone, Copy, Debug, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// Low, band and high for coefficients `(a1, a2, a3, k)`.
    #[inline]
    fn run(&mut self, x: f32, c: (f32, f32, f32, f32)) -> (f32, f32, f32) {
        let (a1, a2, a3, k) = c;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1, x - k * v1 - v2)
    }
}

fn svf_coefs(g: f32, k: f32) -> (f32, f32, f32, f32) {
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    (a1, a2, g * a2, k)
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    amp: Env,
    filt: Env,
    key: u8,
    channel: u8,
    velocity: f32,
    /// Oscillator phases: per unison copy, oscillator 1 and 2.
    phase: [[f32; 2]; MAX_UNISON],
    sub: f32,
    noise: u32,
    /// Per side, two filter stages.
    svf: [[Svf; 2]; 2],
    coefs: [(f32, f32, f32, f32); 2],
    /// Phase increments (per unison copy, oscillator) and the sub's.
    inc: [[f32; 2]; MAX_UNISON],
    sub_inc: f32,
    control_counter: u32,
    age: u64,
    pedal_held: bool,
    /// Glide: semitones still to go (falls to zero).
    glide: f32,
    tuning: f32,
    volume: f32,
    expression: f32,
    pan: f32,
    vibrato: f32,
    brightness: f32,
    pressure: f32,
}

impl Voice {
    const IDLE: Voice = Voice {
        amp: Env::IDLE,
        filt: Env::IDLE,
        key: 0,
        channel: 0,
        velocity: 0.0,
        phase: [[0.0; 2]; MAX_UNISON],
        sub: 0.0,
        noise: 0x1234_5678,
        svf: [[Svf { ic1: 0.0, ic2: 0.0 }; 2]; 2],
        coefs: [(0.0, 0.0, 0.0, 1.4); 2],
        inc: [[0.0; 2]; MAX_UNISON],
        sub_inc: 0.0,
        control_counter: 0,
        age: 0,
        pedal_held: false,
        glide: 0.0,
        tuning: 0.0,
        volume: 1.0,
        expression: 1.0,
        pan: 0.0,
        vibrato: 0.0,
        brightness: 0.5,
        pressure: 0.0,
    };

    fn sounding(&self) -> bool {
        self.amp.stage != Stage::Idle
    }

    fn express(&mut self, kind: NoteExpressionKind, v: f32) {
        match kind {
            NoteExpressionKind::Volume => self.volume = 10f32.powf(v.min(12.0) / 20.0),
            NoteExpressionKind::Pan => self.pan = v.clamp(-1.0, 1.0),
            NoteExpressionKind::Tuning => self.tuning = v.clamp(-120.0, 120.0),
            NoteExpressionKind::Vibrato => self.vibrato = v.clamp(0.0, 1.0),
            NoteExpressionKind::Expression => self.expression = v.clamp(0.0, 1.0),
            NoteExpressionKind::Brightness => self.brightness = v.clamp(0.0, 1.0),
            NoteExpressionKind::Pressure => self.pressure = v.clamp(0.0, 1.0),
        }
    }
}

/// Default pitch-bend range in semitones (RPN 0 changes it per channel).
const BEND_RANGE: f32 = 2.0;
/// MPE member channels' default bend range.
const MPE_BEND_RANGE: f32 = 48.0;
/// Full mod wheel: vibrato depth in semitones, at this rate.
const VIBRATO_DEPTH: f32 = 0.5;
const VIBRATO_HZ: f32 = 5.5;

/// Per-channel controller state.
#[derive(Clone, Copy, Debug)]
struct ChannelState {
    sustain: bool,
    bend: f32,
    bend_range: f32,
    modulation: f32,
    pressure: f32,
    timbre: f32,
    rpn: (u8, u8),
}

impl ChannelState {
    const REST: ChannelState = ChannelState {
        sustain: false,
        bend: 0.0,
        bend_range: BEND_RANGE,
        modulation: 0.0,
        pressure: 0.0,
        timbre: 0.5,
        rpn: (127, 127),
    };
}

/// Block-rate snapshot of the parameters, in per-sample units.
#[derive(Clone, Copy)]
struct Settings {
    gain: f32,
    cutoff: f32,
    k: f32,
    env_octaves: f32,
    amp: EnvSettings,
    filt: EnvSettings,
    own_filter_env: bool,
    detune: f32,
    waves: [usize; 2],
    octaves: [f32; 2],
    semi2: f32,
    pw: f32,
    levels: [f32; 2],
    sub: f32,
    noise: f32,
    unison: usize,
    spread: f32,
    width: f32,
    filter: usize,
    drive: f32,
    key_track: f32,
    vel_cutoff: f32,
    vel_amp: f32,
    lfo_pitch: f32,
    lfo_cutoff: f32,
    lfo_amp: f32,
    lfo_pw: f32,
    mode: usize,
    glide_coef: f32,
    polyphony: usize,
}

/// A PolyBLEP correction at phase `t` with increment `dt`.
#[inline]
fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// One sample of a wave at phase `t`.
#[inline]
fn wave(kind: usize, t: f32, dt: f32, pw: f32) -> f32 {
    match kind {
        1 => {
            let naive = if t < pw { 1.0 } else { -1.0 };
            let mut down = t - pw;
            if down < 0.0 {
                down += 1.0;
            }
            naive + blep(t, dt) - blep(down, dt)
        }
        2 => 1.0 - 4.0 * (t - 0.5).abs(),
        3 => (2.0 * PI * t).sin(),
        _ => 2.0 * t - 1.0 - blep(t, dt),
    }
}

pub struct SynthProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    watching: Watching,
    sample_rate: f32,
    voices: [Voice; MAX_VOICES],
    counter: u64,
    channels: [ChannelState; 16],
    /// Mod-wheel vibrato phase (0..1).
    vibrato_phase: f32,
    lfo: Lfo,
    /// The last note's pitch (semitones from A4), for glide.
    last_pitch: Option<f32>,
    /// Keys held in mono modes, oldest first.
    held: [(u8, u8, u8); 16],
    held_len: usize,
    meters: [MeterTap; 2],
}

impl SynthProcessor {
    pub fn new(params: ParamValues, tap: Option<Arc<AnalysisTap>>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate as f32;
        Self {
            params,
            tap,
            watching: Watching::new(sr),
            sample_rate: sr,
            voices: [Voice::IDLE; MAX_VOICES],
            counter: 0,
            channels: [ChannelState::REST; 16],
            vibrato_phase: 0.0,
            lfo: Lfo::new(0x51_7e),
            last_pitch: None,
            held: [(0, 0, 0); 16],
            held_len: 0,
            meters: [MeterTap::new(sr); 2],
        }
    }

    fn get(&self, pid: u32) -> f32 {
        self.params.get(pid as usize)
    }

    fn settings(&self) -> Settings {
        let sr = self.sample_rate;
        let ms = |v: f32| (v * 0.001 * sr).max(1.0);
        // Exponential segments reach ~-60 dB after the given time.
        let coef = |t: f32| (-6.9 / ms(t)).exp();
        let env = |a, d, s, r| EnvSettings {
            attack_step: 1.0 / ms(self.get(a)),
            decay_coef: coef(self.get(d)),
            sustain: self.get(s).clamp(0.0, 1.0),
            release_coef: coef(self.get(r)),
        };
        let glide = self.get(id::GLIDE);
        Settings {
            gain: db_to_gain(self.get(id::VOLUME)),
            cutoff: self.get(id::CUTOFF),
            k: 2.0 - 1.9 * self.get(id::RESONANCE).clamp(0.0, 1.0),
            env_octaves: self.get(id::ENV_AMOUNT) * 6.0,
            amp: env(id::ATTACK, id::DECAY, id::SUSTAIN, id::RELEASE),
            filt: env(id::F_ATTACK, id::F_DECAY, id::F_SUSTAIN, id::F_RELEASE),
            own_filter_env: self.get(id::FILTER_ENV) >= 0.5,
            detune: self.get(id::DETUNE) / 1200.0,
            waves: [
                self.get(id::OSC1_WAVE).round().clamp(0.0, 3.0) as usize,
                self.get(id::OSC2_WAVE).round().clamp(0.0, 3.0) as usize,
            ],
            octaves: [
                self.get(id::OSC1_OCTAVE).round(),
                self.get(id::OSC2_OCTAVE).round(),
            ],
            semi2: self.get(id::OSC2_SEMI).round(),
            pw: self.get(id::OSC1_PW).clamp(0.05, 0.95),
            levels: [self.get(id::OSC1_LEVEL), self.get(id::OSC2_LEVEL)],
            sub: self.get(id::SUB),
            noise: self.get(id::NOISE),
            unison: (self.get(id::UNISON).round() as usize).clamp(1, MAX_UNISON),
            spread: self.get(id::UNISON_SPREAD) / 1200.0,
            width: self.get(id::WIDTH).clamp(0.0, 1.0),
            filter: self.get(id::FILTER_TYPE).round().clamp(0.0, 3.0) as usize,
            drive: self.get(id::DRIVE),
            key_track: self.get(id::KEY_TRACK),
            vel_cutoff: self.get(id::VELOCITY_CUTOFF),
            vel_amp: self.get(id::VELOCITY_AMP),
            lfo_pitch: self.get(id::LFO_PITCH) / 100.0,
            lfo_cutoff: self.get(id::LFO_CUTOFF),
            lfo_amp: self.get(id::LFO_AMP),
            lfo_pw: self.get(id::LFO_PW),
            mode: self.get(id::VOICE_MODE).round().clamp(0.0, 2.0) as usize,
            glide_coef: if glide < 0.5 {
                0.0
            } else {
                (-6.9 / ms(glide)).exp()
            },
            polyphony: (self.get(id::POLYPHONY).round() as usize).clamp(1, MAX_VOICES),
        }
    }

    fn start(
        &mut self,
        idx: usize,
        channel: u8,
        key: u8,
        velocity: u8,
        retrigger: bool,
        s: &Settings,
    ) {
        self.counter += 1;
        let pitch = key as f32 - 69.0;
        let glide = match self.last_pitch {
            Some(from) if s.glide_coef > 0.0 => {
                from - pitch
                    + if retrigger {
                        0.0
                    } else {
                        self.voices[idx].glide
                    }
            }
            _ => 0.0,
        };
        self.last_pitch = Some(pitch);
        let v = &mut self.voices[idx];
        if !retrigger {
            // Legato: the same voice slides on, envelopes untouched.
            v.key = key;
            v.channel = channel;
            v.glide = glide;
            v.age = self.counter;
            return;
        }
        let keep = v.sounding();
        let (amp, filt, phase, svf) = (v.amp.value, v.filt.value, v.phase, v.svf);
        *v = Voice {
            amp: Env {
                stage: Stage::Attack,
                value: if keep { amp } else { 0.0 },
            },
            filt: Env {
                stage: Stage::Attack,
                value: if keep { filt } else { 0.0 },
            },
            key,
            channel,
            velocity: velocity as f32 / 127.0,
            age: self.counter,
            glide,
            noise: 0x9e37_79b9 ^ (self.counter as u32).wrapping_mul(2_654_435_761),
            ..Voice::IDLE
        };
        if keep {
            v.phase = phase;
            v.svf = svf;
        } else {
            // Free-running oscillators: start each copy somewhere else.
            for (u, p) in v.phase.iter_mut().enumerate() {
                let r = ((self.counter as u32).wrapping_mul(747_796_405)
                    ^ (u as u32).wrapping_mul(2_891_336_453)) as f32
                    / u32::MAX as f32;
                *p = [r, (r + 0.37).fract()];
            }
        }
    }

    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        let s = self.settings();
        if s.mode > 0 {
            // Mono: one voice; legato keeps its envelopes while keys are
            // held.
            let legato = s.mode == 2 && self.held_len > 0 && self.voices[0].sounding();
            if self.held_len < self.held.len() {
                self.held[self.held_len] = (channel, key, velocity);
                self.held_len += 1;
            }
            self.start(0, channel, key, velocity, !legato, &s);
            return;
        }
        let voices = &self.voices[..s.polyphony];
        let idx = voices
            .iter()
            .position(|v| !v.sounding())
            .or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.amp.stage == Stage::Release)
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map_or(0, |(i, _)| i)
            });
        self.start(idx, channel, key, velocity, true, &s);
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        let s = self.settings();
        if s.mode > 0 {
            // Back to the last key still held, if any.
            if let Some(i) = self.held[..self.held_len]
                .iter()
                .position(|h| h.0 == channel && h.1 == key)
            {
                self.held.copy_within(i + 1..self.held_len, i);
                self.held_len -= 1;
            }
            let v0 = self.voices[0];
            if v0.key != key || v0.channel != channel {
                return;
            }
            if self.held_len > 0 {
                let (c, k, vel) = self.held[self.held_len - 1];
                self.start(0, c, k, vel, s.mode == 1, &s);
                return;
            }
        }
        let pedal = self.channels[(channel & 15) as usize].sustain;
        for v in &mut self.voices {
            if v.key == key && v.channel == channel && v.sounding() && v.amp.stage != Stage::Release
            {
                if pedal {
                    v.pedal_held = true;
                } else {
                    v.amp.release();
                    v.filt.release();
                }
            }
        }
    }

    fn set_sustain(&mut self, channel: u8, on: bool) {
        let c = (channel & 15) as usize;
        self.channels[c].sustain = on;
        if !on {
            for v in &mut self.voices {
                if v.channel == channel && v.pedal_held {
                    v.pedal_held = false;
                    v.amp.release();
                    v.filt.release();
                }
            }
        }
    }

    fn all_off(&mut self) {
        for v in &mut self.voices {
            v.amp.release();
            v.filt.release();
        }
        self.held_len = 0;
    }

    fn handle(&mut self, event: MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => self.note_on(channel, key, velocity),
            MidiEvent::NoteOff { channel, key, .. } => self.note_off(channel, key),
            MidiEvent::ControlChange { controller, .. }
                if controller == MidiEvent::CC_ALL_NOTES_OFF
                    || controller == MidiEvent::CC_ALL_SOUND_OFF =>
            {
                for ch in 0..16 {
                    self.set_sustain(ch, false);
                }
                self.all_off()
            }
            MidiEvent::ControlChange {
                channel,
                controller: 64,
                value,
            } => self.set_sustain(channel, value >= 64),
            MidiEvent::ControlChange {
                channel,
                controller: 1,
                value,
            } => self.channels[(channel & 15) as usize].modulation = value as f32 / 127.0,
            MidiEvent::PitchBend { channel, value } => {
                let c = &mut self.channels[(channel & 15) as usize];
                c.bend = (value as f32 - 8192.0) / 8192.0 * c.bend_range;
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                self.channels[(channel & 15) as usize].pressure = pressure as f32 / 127.0;
            }
            MidiEvent::ControlChange {
                channel,
                controller: 74,
                value,
            } => self.channels[(channel & 15) as usize].timbre = value as f32 / 127.0,
            MidiEvent::ControlChange {
                channel,
                controller: 101,
                value,
            } => self.channels[(channel & 15) as usize].rpn.0 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 100,
                value,
            } => self.channels[(channel & 15) as usize].rpn.1 = value,
            MidiEvent::ControlChange {
                channel,
                controller: 6,
                value,
            } => self.data_entry(channel & 15, value),
            MidiEvent::NoteExpression {
                channel,
                key,
                kind,
                value,
            } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.sounding() && v.channel == channel && v.key == key)
                {
                    v.express(kind, value.get() as f32);
                }
            }
            _ => {}
        }
    }

    /// RPN data entry: 0 = pitch bend range, 6 = MPE configuration (on the
    /// master channel: member channels get the MPE default range).
    fn data_entry(&mut self, channel: u8, value: u8) {
        match self.channels[channel as usize].rpn {
            (0, 0) => self.channels[channel as usize].bend_range = value.max(1) as f32,
            (0, 6) if channel == 0 => {
                for ch in 1..=(value.min(15) as usize) {
                    self.channels[ch].bend_range = MPE_BEND_RANGE;
                }
            }
            _ => {}
        }
    }

    /// Render `[start, end)` additively into `out_l`/`out_r`.
    // The oscillators index several per-copy arrays together.
    #[allow(clippy::needless_range_loop)]
    fn render(
        &mut self,
        s: &Settings,
        lfo: f32,
        out_l: &mut [f32],
        mut out_r: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        let sr = self.sample_rate;
        let nyquist_guard = sr * 0.45;
        // Mod-wheel vibrato at the segment's start.
        let vib = (self.vibrato_phase * 2.0 * PI).sin();
        self.vibrato_phase = (self.vibrato_phase + (end - start) as f32 * VIBRATO_HZ / sr).fract();
        let channels = self.channels;
        let n = s.unison;
        let norm = 1.0 / (n as f32).sqrt();
        // Where each copy's oscillators sit (−1 left … 1 right): one copy
        // keeps the first Synth's spread (oscillator 1 left of centre, 2
        // right), more fan out across the width.
        let mut pans = [[0.0f32; 2]; MAX_UNISON];
        for (u, p) in pans.iter_mut().enumerate().take(n) {
            let at = if n > 1 {
                -1.0 + 2.0 * u as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let off = if n > 1 { 0.35 } else { 1.0 };
            *p = [
                (s.width * (at - off)).clamp(-1.0, 1.0),
                (s.width * (at + off)).clamp(-1.0, 1.0),
            ];
        }
        let pw = (s.pw + s.lfo_pw * lfo).clamp(0.05, 0.95);
        let tremolo = 1.0 - s.lfo_amp * 0.5 * (1.0 - lfo);
        for v in self.voices.iter_mut().filter(|v| v.sounding()) {
            let ch = channels[(v.channel & 15) as usize];
            let (pan_l, pan_r) = ((1.0 - v.pan).min(1.0), (1.0 + v.pan).min(1.0));
            for i in start..end {
                let amp_env = v.amp.tick(&s.amp);
                let filt_env = if s.own_filter_env {
                    v.filt.tick(&s.filt)
                } else {
                    amp_env
                };
                if !v.sounding() {
                    break;
                }
                if v.control_counter == 0 {
                    // Pitch, then each oscillator's increment.
                    v.glide *= s.glide_coef;
                    let semis = v.key as f32 - 69.0
                        + v.glide
                        + ch.bend
                        + v.tuning
                        + vib * (ch.modulation + v.vibrato) * VIBRATO_DEPTH
                        + lfo * s.lfo_pitch;
                    let base = 440.0 * 2f32.powf(semis / 12.0);
                    for u in 0..n {
                        let spread = if n > 1 {
                            s.spread * (-1.0 + 2.0 * u as f32 / (n - 1) as f32)
                        } else {
                            0.0
                        };
                        let f1 = base * 2f32.powf(s.octaves[0]) * (1.0 - s.detune + spread);
                        let f2 = base
                            * 2f32.powf(s.octaves[1] + s.semi2 / 12.0)
                            * (1.0 + s.detune + spread);
                        v.inc[u] = [(f1 / sr).min(0.45), (f2 / sr).min(0.45)];
                    }
                    v.sub_inc = (base * 2f32.powf(s.octaves[0] - 1.0) / sr).min(0.45);
                    let vel = 1.0 - s.vel_cutoff + s.vel_cutoff * v.velocity;
                    let fc = (s.cutoff
                        * 2f32.powf(
                            s.env_octaves * filt_env * vel
                                + s.key_track * (v.key as f32 - 60.0) / 12.0
                                + lfo * s.lfo_cutoff
                                + (ch.timbre - 0.5 + v.brightness - 0.5) * 4.0,
                        ))
                    .clamp(20.0, nyquist_guard);
                    let g = (PI * fc / sr).tan();
                    v.coefs = if s.filter == 1 {
                        [svf_coefs(g, std::f32::consts::SQRT_2), svf_coefs(g, s.k)]
                    } else {
                        [svf_coefs(g, s.k), svf_coefs(g, s.k)]
                    };
                }
                v.control_counter = (v.control_counter + 1) % CONTROL_INTERVAL;
                // The oscillators, spread across the sides.
                let mut side = [0.0f32; 2];
                for u in 0..n {
                    for o in 0..2 {
                        let level = s.levels[o];
                        let t = v.phase[u][o];
                        let dt = v.inc[u][o];
                        if level > 0.0 {
                            let x = wave(s.waves[o], t, dt, pw) * level;
                            let q = pans[u][o];
                            side[0] += x * 0.5 * (1.0 - q);
                            side[1] += x * 0.5 * (1.0 + q);
                        }
                        let mut t = t + dt;
                        if t >= 1.0 {
                            t -= 1.0;
                        }
                        v.phase[u][o] = t;
                    }
                }
                side[0] *= norm;
                side[1] *= norm;
                if s.sub > 0.0 {
                    let x = wave(1, v.sub, v.sub_inc, 0.5) * s.sub * 0.5;
                    side[0] += x;
                    side[1] += x;
                }
                v.sub += v.sub_inc;
                if v.sub >= 1.0 {
                    v.sub -= 1.0;
                }
                if s.noise > 0.0 {
                    v.noise ^= v.noise << 13;
                    v.noise ^= v.noise >> 17;
                    v.noise ^= v.noise << 5;
                    let x = (v.noise as f32 / u32::MAX as f32 * 2.0 - 1.0) * s.noise * 0.5;
                    side[0] += x;
                    side[1] += x;
                }
                let amp = amp_env
                    * (1.0 - s.vel_amp + s.vel_amp * v.velocity)
                    * s.gain
                    * 0.35
                    * tremolo
                    * (1.0 + 0.5 * (ch.pressure + v.pressure))
                    * v.volume
                    * v.expression;
                let mut y = [0.0f32; 2];
                for c in 0..2 {
                    let mut x = side[c];
                    if s.drive > 0.0 {
                        let g = 1.0 + 4.0 * s.drive;
                        x = (x * g).tanh() / g.sqrt();
                    }
                    let (low, band, high) = v.svf[c][0].run(x, v.coefs[0]);
                    let f = match s.filter {
                        1 => v.svf[c][1].run(low, v.coefs[1]).0,
                        2 => band * v.coefs[0].3,
                        3 => high,
                        _ => low,
                    };
                    y[c] = f * amp;
                }
                match out_r.as_deref_mut() {
                    Some(r) => {
                        out_l[i] += y[0] * pan_l;
                        r[i] += y[1] * pan_r;
                    }
                    None => out_l[i] += 0.5 * (y[0] + y[1]),
                }
            }
            for side in &mut v.svf {
                for st in side {
                    if st.ic1.abs() < 1e-20 {
                        st.ic1 = 0.0;
                    }
                    if st.ic2.abs() < 1e-20 {
                        st.ic2 = 0.0;
                    }
                }
            }
        }
    }
}

impl PluginProcessor for SynthProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let settings = self.settings();
        let frames = io.frames;
        let watched = self
            .tap
            .as_ref()
            .is_some_and(|t| self.watching.check(t, frames));
        // The LFO for this block (free or locked to the song).
        let t = ctx.transport;
        let tempo = if t.tempo > 0.0 { t.tempo } else { 120.0 };
        let shape = Shape::from_index(self.get(id::LFO_SHAPE).round().max(0.0) as usize);
        let synced = self.get(id::LFO_SYNC) >= 0.5;
        let period_q = DIVISIONS
            [(self.get(id::LFO_DIVISION).round().max(0.0) as usize).min(DIVISIONS.len() - 1)]
        .1;
        if synced && t.playing {
            self.lfo.sync(t.quarter_position, period_q);
        }
        let lfo = self.lfo.value(shape, 0.0) as f32;
        let cycles = if synced {
            tempo / 60.0 / period_q / f64::from(self.sample_rate)
        } else {
            f64::from(self.get(id::LFO_RATE)) / f64::from(self.sample_rate)
        };
        self.lfo.advance(cycles * frames as f64);
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        out.clear();
        let events = io.events_in.first();
        let stereo = out.num_channels() >= 2;
        let mut pos = 0usize;
        if let Some(events) = events {
            for e in events.iter() {
                let at = (e.sample_offset as usize).min(frames);
                if at > pos {
                    if stereo {
                        let (l, r) = out.channel_pair_mut(0, 1);
                        self.render(&settings, lfo, l, Some(r), pos, at);
                    } else {
                        self.render(&settings, lfo, out.channel_mut(0), None, pos, at);
                    }
                    pos = at;
                }
                self.handle(e.event);
            }
        }
        if frames > pos {
            if stereo {
                let (l, r) = out.channel_pair_mut(0, 1);
                self.render(&settings, lfo, l, Some(r), pos, frames);
            } else {
                self.render(&settings, lfo, out.channel_mut(0), None, pos, frames);
            }
        }
        if let Some(tap) = &self.tap {
            let sounding = self.voices.iter().filter(|v| v.sounding()).count();
            tap.set_value(value::VOICES, sounding as f32);
            if let Some(v) = self
                .voices
                .iter()
                .filter(|v| v.sounding())
                .max_by_key(|v| v.age)
            {
                tap.set_value(value::NOTE, f32::from(v.key));
                tap.set_value(value::AMP_ENV, v.amp.value);
                let f = if settings.own_filter_env {
                    v.filt.value
                } else {
                    v.amp.value
                };
                tap.set_value(value::FILTER_ENV, f);
                let g = v.coefs[0].1 / v.coefs[0].0;
                tap.set_value(value::CUTOFF, g.atan() * self.sample_rate / PI);
            } else {
                tap.set_value(value::AMP_ENV, 0.0);
                tap.set_value(value::FILTER_ENV, 0.0);
            }
            tap.set_value(value::LFO, lfo);
            let channels = out.num_channels();
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
        if self.voices.iter().all(|v| !v.sounding()) {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; MAX_VOICES];
        self.channels = [ChannelState::REST; 16];
        self.held_len = 0;
        self.last_pitch = None;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{SR, bin_db};
    use faderframe_audio_graph::AudioBuffer;
    use faderframe_core::ChannelLayout;
    use faderframe_midi::{MidiBuffer, TimedMidiEvent};
    use faderframe_transport::TransportInfo;

    struct Play {
        p: SynthProcessor,
        out: Vec<AudioBuffer>,
        events: Vec<MidiBuffer>,
        transport: TransportInfo,
    }

    impl Play {
        fn new(set: &[(u32, f64)]) -> Self {
            let params = ParamValues::new(parameters());
            for (id, v) in set {
                params.set_by_id(ParameterId(*id), *v).unwrap();
            }
            let tap = Arc::new(AnalysisTap::new(params.clone(), TAP_VALUES));
            let p = SynthProcessor::new(params, Some(tap), &crate::devices::rig::config());
            let mut out = vec![AudioBuffer::new(ChannelLayout::Stereo, 256)];
            out[0].set_len(256);
            Self {
                p,
                out,
                events: vec![MidiBuffer::with_capacity(64)],
                transport: TransportInfo::default(),
            }
        }

        fn send(&mut self, e: MidiEvent) {
            self.events[0].push(TimedMidiEvent::new(0, e)).unwrap();
        }

        fn run(&mut self, seconds: f64) -> (Vec<f32>, Vec<f32>) {
            let (mut l, mut r) = (Vec::new(), Vec::new());
            for _ in 0..(seconds * SR / 256.0).ceil() as usize {
                let ctx = PluginProcessContext {
                    transport: &self.transport,
                    param_events: &[],
                };
                let mut io = NodeIo {
                    frames: 256,
                    audio_in: &[],
                    audio_out: &mut self.out,
                    events_in: &self.events,
                    events_out: &mut [],
                };
                self.p.process(&ctx, &mut io);
                self.events[0].clear();
                l.extend_from_slice(self.out[0].channel(0));
                r.extend_from_slice(self.out[0].channel(1));
            }
            (l, r)
        }
    }

    fn on(key: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity: 100,
        }
    }

    fn off(key: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            channel: 0,
            key,
            velocity: 0,
        }
    }

    #[test]
    fn oscillators_play_their_pitches_and_waves() {
        // Osc 2 a fifth up, osc 1 an octave down, open filter.
        let mut s = Play::new(&[
            (id::CUTOFF, 20_000.0),
            (id::ENV_AMOUNT, 0.0),
            (id::DETUNE, 0.0),
            (id::OSC1_OCTAVE, -1.0),
            (id::OSC2_SEMI, 7.0),
        ]);
        s.send(on(69));
        let (l, r) = s.run(0.5);
        // Osc 1 sits left of centre, osc 2 right: listen to both sides.
        let l: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a + b).collect();
        assert!(
            bin_db(&l, 220.0) > -30.0,
            "osc 1 at 220: {}",
            bin_db(&l, 220.0)
        );
        assert!(
            bin_db(&l, 659.26) > -30.0,
            "osc 2 at 659: {}",
            bin_db(&l, 659.26)
        );
        assert!(bin_db(&l, 440.0) > -40.0, "the saw's second harmonic");
        // A square has no even harmonics; a sine has no harmonics at all.
        let mut s = Play::new(&[
            (id::CUTOFF, 20_000.0),
            (id::ENV_AMOUNT, 0.0),
            (id::OSC1_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.0),
            (id::DETUNE, 0.0),
        ]);
        s.send(on(57));
        let (l, _) = s.run(0.5);
        assert!(bin_db(&l, 220.0 * 3.0) - bin_db(&l, 220.0) > -12.0);
        assert!(
            bin_db(&l, 440.0) - bin_db(&l, 220.0) < -40.0,
            "{}",
            bin_db(&l, 440.0) - bin_db(&l, 220.0)
        );
        let mut s = Play::new(&[
            (id::CUTOFF, 20_000.0),
            (id::ENV_AMOUNT, 0.0),
            (id::OSC1_WAVE, 3.0),
            (id::OSC2_LEVEL, 0.0),
            (id::DETUNE, 0.0),
        ]);
        s.send(on(57));
        let (l, _) = s.run(0.5);
        assert!(bin_db(&l, 660.0) - bin_db(&l, 220.0) < -60.0);
    }

    #[test]
    fn the_filter_types_shape_the_spectrum() {
        let level = |filter: f64, f: f64| {
            let mut s = Play::new(&[
                (id::CUTOFF, 1_000.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
                (id::FILTER_TYPE, filter),
                (id::NOISE, 1.0),
                (id::OSC1_LEVEL, 0.0),
                (id::OSC2_LEVEL, 0.0),
            ]);
            s.send(on(60));
            let (l, _) = s.run(1.0);
            (0..9).map(|k| bin_db(&l, f + k as f64 * 3.0)).sum::<f64>() / 9.0
        };
        // A 24 dB low pass takes more off two octaves up than a 12 dB one.
        let (lp12, lp24) = (
            level(0.0, 4_000.0) - level(0.0, 200.0),
            level(1.0, 4_000.0) - level(1.0, 200.0),
        );
        assert!(
            lp12 < -15.0 && lp24 < lp12 - 8.0,
            "12: {lp12:.1}, 24: {lp24:.1}"
        );
        // A high pass the other way round.
        assert!(level(3.0, 200.0) < level(3.0, 4_000.0) - 15.0);
    }

    #[test]
    fn mono_legato_glides_and_returns_to_the_held_key() {
        let mut s = Play::new(&[
            (id::VOICE_MODE, 2.0),
            (id::GLIDE, 50.0),
            (id::CUTOFF, 20_000.0),
            (id::DETUNE, 0.0),
            (id::OSC2_LEVEL, 0.0),
            (id::DETUNE, 0.0),
        ]);
        s.send(on(57));
        s.run(0.3);
        s.send(on(69));
        let (l, _) = s.run(0.6);
        // Glided up to A4 and only one voice sounds.
        assert!(bin_db(&l, 440.0) > bin_db(&l, 220.0) + 20.0);
        assert_eq!(s.p.voices.iter().filter(|v| v.sounding()).count(), 1);
        // Releasing A4 goes back to A3, still held.
        s.send(off(69));
        let (l, _) = s.run(0.6);
        assert!(bin_db(&l, 220.0) > bin_db(&l, 440.0) + 10.0);
    }

    #[test]
    fn unison_and_voices_stay_in_bounds() {
        let mut s = Play::new(&[
            (id::UNISON, 7.0),
            (id::UNISON_SPREAD, 40.0),
            (id::SUB, 1.0),
            (id::NOISE, 0.3),
            (id::DRIVE, 1.0),
            (id::RESONANCE, 1.0),
            (id::LFO_CUTOFF, 2.0),
            (id::LFO_PW, 0.4),
            (id::OSC1_WAVE, 1.0),
        ]);
        for k in 40..80 {
            s.send(on(k));
        }
        let (l, r) = s.run(1.0);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 8.0));
        assert_eq!(
            s.p.voices.iter().filter(|v| v.sounding()).count(),
            16,
            "the polyphony"
        );
        // Sides differ with unison spread.
        let diff: f64 = l.iter().zip(&r).map(|(a, b)| f64::from(a - b).abs()).sum();
        assert!(diff > 1.0);
    }
}
