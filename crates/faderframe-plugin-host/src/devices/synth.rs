//! Synth: a polyphonic virtual analogue.
//!
//! Per voice: two oscillators (saw, square with pulse width, triangle,
//! sine; octave, semitone, level) — oscillator 2 can be hard-synced to 1,
//! ring-modulated with it, modulate its phase (FM) and the filter's cutoff
//! at audio rate; phases free-running or reset per note —, up to seven
//! unison copies spread in pitch and across the stereo field, a sub
//! oscillator an octave down (square, sine or triangle, its level tracking
//! the keys) and white or pink noise; drive into the filter — a
//! state-variable 12 or 24 dB low pass, band pass, high pass or notch, or a
//! saturating four-pole ladder — with key tracking, velocity and an
//! envelope (its own, or the amplifier's as the first Synth had it); the
//! amplifier envelope. A mod envelope and a second LFO (per voice,
//! restarted per note and faded in if wanted) reach their targets through
//! a six-slot modulation matrix (sources also velocity, key, aftertouch,
//! the wheel, a random value per note and the other envelopes). One global
//! LFO (free or synced, six shapes) moves pitch, cutoff, level and pulse
//! width. Poly (voices stolen oldest or quietest first), mono or legato
//! with glide (always, or between held keys only), a little analogue
//! drift. After the voices: a chorus or ensemble, a tempo-synced stereo
//! delay and a small reverb, each with its mix.
//!
//! The first nine parameters are the old Synth's and keep their meaning,
//! as do its MIDI handling (sustain pedal, pitch bend over Pitch Bend Range
//! — a range a controller sets with RPN 0 wins on its channel —, MPE zones
//! at ±48, channel pressure louder, CC 74 brighter) and per-note
//! expressions (tuning, volume, pan, vibrato, expression, brightness,
//! pressure). The mod wheel (CC 1) drives what its three slots are
//! assigned to (a destination and a bipolar amount each: vibrato, pitch,
//! cutoff, resonance, …; by default vibrato), per channel and per voice.
//! Every parameter added since (id 50 on) defaults to off, so earlier
//! patches sound as they did.

use super::{on_off, param, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::filter::OnePole;
use crate::dsp::lfo::{DIVISIONS, Lfo, Shape, division_name};
use crate::dsp::smooth::Smoothed;
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

/// Stable parameter ids (`id == position` in [`parameters`]; only ever
/// appended to).
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
    /// Semitones a full pitch-wheel throw bends.
    pub const BEND_RANGE: u32 = 43;
    /// Mod-wheel slots: a destination ([`super::TARGETS`]) and an amount
    /// (−1…1) each.
    pub const fn wheel_target(slot: usize) -> u32 {
        44 + 2 * slot as u32
    }
    pub const fn wheel_amount(slot: usize) -> u32 {
        45 + 2 * slot as u32
    }
    // 50 on: the second wave — everything defaults to off.
    /// The second LFO: per voice.
    pub const LFO2_RATE: u32 = 50;
    pub const LFO2_SYNC: u32 = 51;
    pub const LFO2_DIVISION: u32 = 52;
    pub const LFO2_SHAPE: u32 = 53;
    /// Restart at every note.
    pub const LFO2_RETRIG: u32 = 54;
    /// Fade in over this long after the note (0 = at once).
    pub const LFO2_FADE: u32 = 55;
    /// The mod envelope (a source of the matrix).
    pub const M_ATTACK: u32 = 56;
    pub const M_DECAY: u32 = 57;
    pub const M_SUSTAIN: u32 = 58;
    pub const M_RELEASE: u32 = 59;
    /// Modulation matrix slots: a source ([`super::SOURCES`]), a target
    /// ([`super::TARGETS`]) and a bipolar amount each (60…77).
    pub const fn mod_source(slot: usize) -> u32 {
        60 + 3 * slot as u32
    }
    pub const fn mod_target(slot: usize) -> u32 {
        61 + 3 * slot as u32
    }
    pub const fn mod_amount(slot: usize) -> u32 {
        62 + 3 * slot as u32
    }
    /// Oscillator 2 restarts whenever oscillator 1 wraps.
    pub const SYNC: u32 = 78;
    /// The product of the oscillators, mixed in.
    pub const RING: u32 = 79;
    /// Oscillator 2 modulating oscillator 1's phase.
    pub const FM: u32 = 80;
    /// Oscillators start every note at zero phase.
    pub const PHASE_RESET: u32 = 81;
    /// The sub's level across the keys (±6 dB/octave from C4).
    pub const SUB_KEY: u32 = 82;
    pub const SUB_WAVE: u32 = 83;
    pub const NOISE_COLOUR: u32 = 84;
    /// Oscillator 2 moving the cutoff at audio rate.
    pub const FILTER_FM: u32 = 85;
    pub const CHORUS_MIX: u32 = 86;
    pub const CHORUS_RATE: u32 = 87;
    pub const CHORUS_DEPTH: u32 = 88;
    pub const CHORUS_MODE: u32 = 89;
    pub const DELAY_MIX: u32 = 90;
    pub const DELAY_TIME: u32 = 91;
    pub const DELAY_SYNC: u32 = 92;
    pub const DELAY_DIVISION: u32 = 93;
    pub const DELAY_FEEDBACK: u32 = 94;
    pub const DELAY_PINGPONG: u32 = 95;
    pub const DELAY_TONE: u32 = 96;
    pub const REVERB_MIX: u32 = 97;
    pub const REVERB_SIZE: u32 = 98;
    pub const REVERB_DECAY: u32 = 99;
    pub const REVERB_TONE: u32 = 100;
    /// Which voice goes when all are busy.
    pub const STEAL: u32 = 101;
    /// Glide always, or only between held (overlapping) keys.
    pub const GLIDE_MODE: u32 = 102;
    /// Analogue drift: a fixed offset per voice and a slow wander.
    pub const DRIFT: u32 = 103;
}

/// Mod-wheel slots.
pub const WHEEL_SLOTS: usize = 3;
/// Modulation matrix slots.
pub const MOD_SLOTS: usize = 6;

/// What a mod-wheel or matrix slot can drive (its target's index). At
/// full wheel (or a source of 1) and amount 100 %: vibrato 1 st deep,
/// pitch +12 st, cutoff +4 octaves, resonance +100 %, filter envelope +6
/// octaves, drive +100 %, the LFO brought in (negative: taken out), its
/// rate two octaves faster (the wheel only: there is one LFO 1), pulse
/// width +45 %, levels +100 %, detune and spread +50 ¢, volume brought in
/// (negative: taken out); then pan fully right, oscillator 2 +12 st, FM,
/// ring and filter FM +100 %, LFO 2 two octaves faster.
pub const TARGETS: [&str; 23] = [
    "Off",
    "Vibrato",
    "Pitch",
    "Cutoff",
    "Resonance",
    "Filter Env",
    "Drive",
    "LFO Amount",
    "LFO Rate",
    "Pulse Width",
    "Osc 1 Level",
    "Osc 2 Level",
    "Sub",
    "Noise",
    "Detune",
    "Unison Spread",
    "Volume",
    "Pan",
    "Osc 2 Pitch",
    "FM Amount",
    "Ring Mod",
    "Filter FM",
    "LFO 2 Rate",
];

/// [`TARGETS`] by name.
pub mod target {
    pub const OFF: usize = 0;
    pub const VIBRATO: usize = 1;
    pub const PITCH: usize = 2;
    pub const CUTOFF: usize = 3;
    pub const RESONANCE: usize = 4;
    pub const FILTER_ENV: usize = 5;
    pub const DRIVE: usize = 6;
    pub const LFO_AMOUNT: usize = 7;
    pub const LFO_RATE: usize = 8;
    pub const PULSE_WIDTH: usize = 9;
    pub const OSC1_LEVEL: usize = 10;
    pub const OSC2_LEVEL: usize = 11;
    pub const SUB: usize = 12;
    pub const NOISE: usize = 13;
    pub const DETUNE: usize = 14;
    pub const SPREAD: usize = 15;
    pub const VOLUME: usize = 16;
    pub const PAN: usize = 17;
    pub const OSC2_PITCH: usize = 18;
    pub const FM: usize = 19;
    pub const RING: usize = 20;
    pub const FILTER_FM: usize = 21;
    pub const LFO2_RATE: usize = 22;
}

/// The modulation matrix's sources: bipolar (−1…1) LFOs and the random
/// value a note draws, unipolar (0…1) the rest.
pub const SOURCES: [&str; 11] = [
    "Off",
    "LFO 1",
    "LFO 2",
    "Mod Env",
    "Velocity",
    "Key",
    "Aftertouch",
    "Mod Wheel",
    "Random",
    "Amp Env",
    "Filter Env",
];

/// [`SOURCES`] by name.
pub mod source {
    pub const OFF: usize = 0;
    pub const LFO1: usize = 1;
    pub const LFO2: usize = 2;
    pub const MOD_ENV: usize = 3;
    pub const VELOCITY: usize = 4;
    pub const KEY: usize = 5;
    pub const AFTERTOUCH: usize = 6;
    pub const WHEEL: usize = 7;
    pub const RANDOM: usize = 8;
    pub const AMP_ENV: usize = 9;
    pub const FILTER_ENV: usize = 10;
}

/// Published: voices sounding, the newest voice's key, amplifier and
/// filter envelopes (0…1) and cutoff (Hz), the output's peak (linear),
/// the LFO (−1…1), the newest voice's LFO 2 (−1…1) and mod envelope (0…1).
pub mod value {
    pub const VOICES: usize = 0;
    pub const NOTE: usize = 1;
    pub const AMP_ENV: usize = 2;
    pub const FILTER_ENV: usize = 3;
    pub const CUTOFF: usize = 4;
    pub const OUT_PEAK: usize = 5;
    pub const LFO: usize = 6;
    pub const LFO2: usize = 7;
    pub const MOD_ENV: usize = 8;
}
pub const TAP_VALUES: usize = 9;

pub const WAVES: [&str; 4] = ["Saw", "Square", "Triangle", "Sine"];
pub const FILTERS: [&str; 6] = ["LP 12", "LP 24", "Band", "High", "Ladder", "Notch"];
pub const MODES: [&str; 3] = ["Poly", "Mono", "Legato"];
pub const SHAPES: [&str; 6] = ["Sine", "Triangle", "Saw", "Square", "S&H", "Drift"];
pub const FILTER_ENVS: [&str; 2] = ["Amp", "Own"];
pub const SUB_WAVES: [&str; 3] = ["Square", "Sine", "Triangle"];
pub const NOISE_COLOURS: [&str; 2] = ["White", "Pink"];
pub const CHORUS_MODES: [&str; 2] = ["Chorus", "Ensemble"];
pub const STEALS: [&str; 2] = ["Oldest", "Quietest"];
pub const GLIDE_MODES: [&str; 2] = ["Always", "Legato"];

/// The filter types by index.
pub mod filter {
    pub const LP12: usize = 0;
    pub const LP24: usize = 1;
    pub const BAND: usize = 2;
    pub const HIGH: usize = 3;
    pub const LADDER: usize = 4;
    pub const NOTCH: usize = 5;
}

const MAX_VOICES: usize = 32;
const MAX_UNISON: usize = 7;
/// Filter coefficients and pitches are recomputed every this many samples.
const CONTROL_INTERVAL: u32 = 16;
/// Drift at full depth: cents (a fixed offset per voice and a slow
/// wander at this rate).
const DRIFT_CENTS: f32 = 12.0;
const DRIFT_HZ: f32 = 0.4;
/// FM: cycles of oscillator 1's phase a full-scale oscillator 2 moves at
/// amount 100 % (the amount is squared, so the low range is fine).
const FM_CYCLES: f32 = 1.5;
/// The effects: wet signal below this is a tail that has ended.
const TAIL_FLOOR: f32 = 1e-5;

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let mut p = vec![
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
        stepped(id::FILTER_TYPE, "Filter Type", 5.0, 0.0),
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
        ranged(id::BEND_RANGE, "Pitch Bend Range", 1.0, 24.0, 2.0),
    ];
    for k in 0..WHEEL_SLOTS {
        let names = [
            ("Mod Wheel 1", "Mod Wheel 1 Amount"),
            ("Mod Wheel 2", "Mod Wheel 2 Amount"),
            ("Mod Wheel 3", "Mod Wheel 3 Amount"),
        ][k];
        let (t, a) = if k == 0 {
            (target::VIBRATO as f64, 0.5)
        } else {
            (0.0, 0.0)
        };
        p.push(stepped(
            id::wheel_target(k),
            names.0,
            (TARGETS.len() - 1) as f64,
            t,
        ));
        p.push(param(id::wheel_amount(k), names.1, -1.0, 1.0, a, Percent));
    }
    p.extend([
        param(id::LFO2_RATE, "LFO 2 Rate", 0.05, 20.0, 2.0, Hertz),
        stepped(id::LFO2_SYNC, "LFO 2 Sync", 1.0, 0.0),
        stepped(
            id::LFO2_DIVISION,
            "LFO 2 Division",
            (DIVISIONS.len() - 1) as f64,
            11.0,
        ),
        stepped(id::LFO2_SHAPE, "LFO 2 Shape", 5.0, 0.0),
        stepped(id::LFO2_RETRIG, "LFO 2 Retrigger", 1.0, 0.0),
        param(
            id::LFO2_FADE,
            "LFO 2 Fade In",
            0.0,
            5_000.0,
            0.0,
            Milliseconds,
        ),
        param(id::M_ATTACK, "Mod Attack", 0.5, 10_000.0, 5.0, Milliseconds),
        param(id::M_DECAY, "Mod Decay", 1.0, 10_000.0, 300.0, Milliseconds),
        param(id::M_SUSTAIN, "Mod Sustain", 0.0, 1.0, 0.0, Percent),
        param(
            id::M_RELEASE,
            "Mod Release",
            5.0,
            10_000.0,
            350.0,
            Milliseconds,
        ),
    ]);
    const SLOT_NAMES: [(&str, &str, &str); MOD_SLOTS] = [
        ("Mod 1 Source", "Mod 1 Target", "Mod 1 Amount"),
        ("Mod 2 Source", "Mod 2 Target", "Mod 2 Amount"),
        ("Mod 3 Source", "Mod 3 Target", "Mod 3 Amount"),
        ("Mod 4 Source", "Mod 4 Target", "Mod 4 Amount"),
        ("Mod 5 Source", "Mod 5 Target", "Mod 5 Amount"),
        ("Mod 6 Source", "Mod 6 Target", "Mod 6 Amount"),
    ];
    for (k, (s, t, a)) in SLOT_NAMES.iter().enumerate() {
        p.push(stepped(
            id::mod_source(k),
            s,
            (SOURCES.len() - 1) as f64,
            0.0,
        ));
        p.push(stepped(
            id::mod_target(k),
            t,
            (TARGETS.len() - 1) as f64,
            0.0,
        ));
        p.push(param(id::mod_amount(k), a, -1.0, 1.0, 0.0, Percent));
    }
    p.extend([
        stepped(id::SYNC, "Osc Sync", 1.0, 0.0),
        param(id::RING, "Ring Mod", 0.0, 1.0, 0.0, Percent),
        param(id::FM, "FM Amount", 0.0, 1.0, 0.0, Percent),
        stepped(id::PHASE_RESET, "Phase Reset", 1.0, 0.0),
        param(id::SUB_KEY, "Sub Key Tracking", -1.0, 1.0, 0.0, Percent),
        stepped(id::SUB_WAVE, "Sub Wave", 2.0, 0.0),
        stepped(id::NOISE_COLOUR, "Noise Colour", 1.0, 0.0),
        param(id::FILTER_FM, "Filter FM", 0.0, 1.0, 0.0, Percent),
        param(id::CHORUS_MIX, "Chorus Mix", 0.0, 1.0, 0.0, Percent),
        param(id::CHORUS_RATE, "Chorus Rate", 0.05, 5.0, 0.8, Hertz),
        param(id::CHORUS_DEPTH, "Chorus Depth", 0.0, 1.0, 0.5, Percent),
        stepped(id::CHORUS_MODE, "Chorus Mode", 1.0, 0.0),
        param(id::DELAY_MIX, "Delay Mix", 0.0, 1.0, 0.0, Percent),
        param(
            id::DELAY_TIME,
            "Delay Time",
            1.0,
            2_000.0,
            375.0,
            Milliseconds,
        ),
        stepped(id::DELAY_SYNC, "Delay Sync", 1.0, 1.0),
        stepped(
            id::DELAY_DIVISION,
            "Delay Division",
            (DIVISIONS.len() - 1) as f64,
            10.0,
        ),
        param(
            id::DELAY_FEEDBACK,
            "Delay Feedback",
            0.0,
            0.95,
            0.4,
            Percent,
        ),
        stepped(id::DELAY_PINGPONG, "Delay Ping-Pong", 1.0, 0.0),
        param(
            id::DELAY_TONE,
            "Delay Tone",
            200.0,
            20_000.0,
            4_000.0,
            Hertz,
        ),
        param(id::REVERB_MIX, "Reverb Mix", 0.0, 1.0, 0.0, Percent),
        param(id::REVERB_SIZE, "Reverb Size", 0.0, 1.0, 0.5, Percent),
        param(
            id::REVERB_DECAY,
            "Reverb Decay",
            100.0,
            10_000.0,
            2_000.0,
            Milliseconds,
        ),
        param(
            id::REVERB_TONE,
            "Reverb Tone",
            500.0,
            20_000.0,
            5_000.0,
            Hertz,
        ),
        stepped(id::STEAL, "Voice Stealing", 1.0, 0.0),
        stepped(id::GLIDE_MODE, "Glide Mode", 1.0, 0.0),
        param(id::DRIFT, "Drift", 0.0, 1.0, 0.0, Percent),
    ]);
    p
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    let signed_percent = || format!("{:+.0} %", v * 100.0).replace("+0 %", "0 %");
    Some(match pid.0 {
        id::OSC1_WAVE | id::OSC2_WAVE => pick(&WAVES, v),
        id::FILTER_TYPE => pick(&FILTERS, v),
        id::VOICE_MODE => pick(&MODES, v),
        id::LFO_SHAPE | id::LFO2_SHAPE => pick(&SHAPES, v),
        id::FILTER_ENV => pick(&FILTER_ENVS, v),
        id::SUB_WAVE => pick(&SUB_WAVES, v),
        id::NOISE_COLOUR => pick(&NOISE_COLOURS, v),
        id::CHORUS_MODE => pick(&CHORUS_MODES, v),
        id::STEAL => pick(&STEALS, v),
        id::GLIDE_MODE => pick(&GLIDE_MODES, v),
        id::LFO_SYNC
        | id::LFO2_SYNC
        | id::LFO2_RETRIG
        | id::SYNC
        | id::PHASE_RESET
        | id::DELAY_SYNC
        | id::DELAY_PINGPONG => on_off(v),
        id::LFO_DIVISION | id::LFO2_DIVISION | id::DELAY_DIVISION => {
            division_name(v.round().max(0.0) as usize).into()
        }
        id::OSC1_OCTAVE | id::OSC2_OCTAVE | id::OSC2_SEMI => {
            format!("{:+.0}", v.round()).replace("+0", "0")
        }
        id::UNISON | id::POLYPHONY => format!("{:.0}", v.round()),
        id::DETUNE | id::UNISON_SPREAD | id::LFO_PITCH => format!("{v:.0} ¢"),
        id::LFO_CUTOFF => format!("{v:.1} oct"),
        id::BEND_RANGE => format!("±{:.0} st", v.round()),
        x if (44..44 + 2 * WHEEL_SLOTS as u32).contains(&x) && (x - 44) % 2 == 0 => {
            pick(&TARGETS, v)
        }
        x if (45..45 + 2 * WHEEL_SLOTS as u32).contains(&x) => signed_percent(),
        x if (60..60 + 3 * MOD_SLOTS as u32).contains(&x) => match (x - 60) % 3 {
            0 => pick(&SOURCES, v),
            1 => pick(&TARGETS, v),
            _ => signed_percent(),
        },
        id::LFO_PW => format!("{:.0} %", v * 100.0),
        id::ENV_AMOUNT | id::SUB_KEY => signed_percent(),
        id::GLIDE if v < 0.5 => "Off".into(),
        id::LFO2_FADE if v < 0.5 => "Off".into(),
        id::REVERB_DECAY => format!("{:.1} s", v / 1000.0),
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

/// A voice's filter coefficients for type `filter` at `g = tan(π fc / sr)`
/// with the state-variable damping `k` and the resonance (0…1): two SVF
/// stages, or for the ladder its one-pole gain's powers and (second
/// entry) `1 − G` and the feedback.
fn filter_coefs(filter: usize, g: f32, k: f32, resonance: f32) -> [(f32, f32, f32, f32); 2] {
    match filter {
        filter::LP24 => [svf_coefs(g, std::f32::consts::SQRT_2), svf_coefs(g, k)],
        filter::LADDER => {
            let gg = g / (1.0 + g);
            [
                (gg, gg * gg, gg * gg * gg, gg * gg * gg * gg),
                (1.0 - gg, 4.0 * resonance, 0.0, 0.0),
            ]
        }
        _ => [svf_coefs(g, k), svf_coefs(g, k)],
    }
}

/// A cheap tanh (within 2 % to ±3, clamped beyond).
#[inline]
fn fast_tanh(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// A four-pole ladder (zero-delay feedback solved linearly, the input and
/// feedback saturated): `c` from [`filter_coefs`].
#[inline]
fn ladder(st: &mut [f32; 4], x: f32, c: [(f32, f32, f32, f32); 2]) -> f32 {
    let (g, g2, g3, g4) = c[0];
    let (one_g, k, _, _) = c[1];
    let sum = one_g * (g3 * st[0] + g2 * st[1] + g * st[2] + st[3]);
    let y = (g4 * x + sum) / (1.0 + k * g4);
    let mut inp = fast_tanh(x - k * y);
    for s in st.iter_mut() {
        let v = g * (inp - *s);
        let out = v + *s;
        *s = out + v;
        inp = out;
    }
    // The passband loses 1 / (1 + k) with the resonance: most of it back.
    inp * (1.0 + 0.5 * k)
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    amp: Env,
    filt: Env,
    /// The mod envelope.
    menv: Env,
    key: u8,
    channel: u8,
    velocity: f32,
    /// Oscillator phases: per unison copy, oscillator 1 and 2.
    phase: [[f32; 2]; MAX_UNISON],
    sub: f32,
    noise: u32,
    /// The pink filter's state.
    pink: [f32; 3],
    /// Per side, two filter stages.
    svf: [[Svf; 2]; 2],
    /// Per side, the ladder's four stages.
    ladder: [[f32; 4]; 2],
    coefs: [(f32, f32, f32, f32); 2],
    /// `tan(π fc / sr)` at the last control tick (filter FM scales it).
    g: f32,
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
    /// LFO 2 (per voice) and how far it has faded in.
    lfo2: Lfo,
    lfo2_fade: f32,
    /// Drift: the wander and this voice's fixed offset (−1…1).
    drift: Lfo,
    drift_offset: f32,
    /// The random value this note drew (−1…1).
    random: f32,
    /// Under sync oscillator 2 is a sample late: the sample held back,
    /// and the correction the next one gets, per copy.
    held2: [f32; MAX_UNISON],
    pending2: [f32; MAX_UNISON],
}

impl Voice {
    const IDLE: Voice = Voice {
        amp: Env::IDLE,
        filt: Env::IDLE,
        menv: Env::IDLE,
        key: 0,
        channel: 0,
        velocity: 0.0,
        phase: [[0.0; 2]; MAX_UNISON],
        sub: 0.0,
        noise: 0x1234_5678,
        pink: [0.0; 3],
        svf: [[Svf { ic1: 0.0, ic2: 0.0 }; 2]; 2],
        ladder: [[0.0; 4]; 2],
        coefs: [(0.0, 0.0, 0.0, 1.4); 2],
        g: 0.1,
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
        lfo2: Lfo::new(1),
        lfo2_fade: 1.0,
        drift: Lfo::new(1),
        drift_offset: 0.0,
        random: 0.0,
        held2: [0.0; MAX_UNISON],
        pending2: [0.0; MAX_UNISON],
    };

    fn sounding(&self) -> bool {
        self.amp.stage != Stage::Idle
    }

    /// How quiet it is (for stealing): its level now, weighed by how hard
    /// it was played.
    fn quietness(&self) -> f32 {
        self.amp.value * (0.25 + 0.75 * self.velocity)
    }

    fn release(&mut self) {
        self.amp.release();
        self.filt.release();
        self.menv.release();
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

/// MPE member channels' default bend range.
const MPE_BEND_RANGE: f32 = 48.0;
/// A note's full vibrato expression: depth in semitones, at this rate (the
/// mod wheel's depth is its slot's).
const VIBRATO_DEPTH: f32 = 0.5;
const VIBRATO_HZ: f32 = 5.5;

/// Per-channel controller state.
#[derive(Clone, Copy, Debug)]
struct ChannelState {
    sustain: bool,
    /// The pitch wheel, −1…1.
    bend: f32,
    /// A range set by RPN 0 (or an MPE zone) for this channel; None: the
    /// Pitch Bend Range parameter's.
    bend_range: Option<f32>,
    modulation: f32,
    pressure: f32,
    timbre: f32,
    rpn: (u8, u8),
}

impl ChannelState {
    const REST: ChannelState = ChannelState {
        sustain: false,
        bend: 0.0,
        bend_range: None,
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
    menv: EnvSettings,
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
    bend_range: f32,
    resonance: f32,
    /// The mod-wheel slots: (target, amount).
    wheel: [(usize, f32); WHEEL_SLOTS],
    /// The matrix: (source, target, amount).
    matrix: [(usize, usize, f32); MOD_SLOTS],
    lfo2_shape: Shape,
    lfo2_retrig: bool,
    /// LFO 2's fade-in per control tick (1: at once).
    lfo2_fade_step: f32,
    sync: bool,
    ring: f32,
    fm: f32,
    phase_reset: bool,
    sub_key: f32,
    sub_wave: usize,
    pink: bool,
    filter_fm: f32,
    steal: usize,
    glide_legato: bool,
    drift: f32,
}

/// A voice's settings with its modulations applied, and what they add
/// outside them.
#[derive(Clone, Copy)]
struct Modded {
    s: Settings,
    /// Vibrato depth (semitones).
    vibrato: f32,
    /// Pitch offset (semitones), and oscillator 2's own.
    pitch: f32,
    pitch2: f32,
    /// The LFO's scale.
    lfo: f32,
    /// The level's scale.
    gain: f32,
    /// Pan offset (−1…1).
    pan: f32,
    /// LFO 2's speed.
    lfo2_speed: f32,
}

impl Modded {
    /// Move `target` by `m` (−1…1 at full); `scale` is what the
    /// bring-in/take-out targets (LFO amount, volume) multiply by.
    fn apply(&mut self, target: usize, m: f32, scale: f32) {
        let s = &mut self.s;
        match target {
            target::VIBRATO => self.vibrato += m.abs(),
            target::PITCH => self.pitch += 12.0 * m,
            target::CUTOFF => s.cutoff = (s.cutoff * 2f32.powf(4.0 * m)).clamp(20.0, 20_000.0),
            target::RESONANCE => {
                s.resonance = (s.resonance + m).clamp(0.0, 1.0);
                s.k = 2.0 - 1.9 * s.resonance;
            }
            target::FILTER_ENV => s.env_octaves += 6.0 * m,
            target::DRIVE => s.drive = (s.drive + m).clamp(0.0, 1.0),
            target::LFO_AMOUNT => self.lfo *= scale,
            target::PULSE_WIDTH => s.pw = (s.pw + 0.45 * m).clamp(0.05, 0.95),
            target::OSC1_LEVEL => s.levels[0] = (s.levels[0] + m).clamp(0.0, 1.0),
            target::OSC2_LEVEL => s.levels[1] = (s.levels[1] + m).clamp(0.0, 1.0),
            target::SUB => s.sub = (s.sub + m).clamp(0.0, 1.0),
            target::NOISE => s.noise = (s.noise + m).clamp(0.0, 1.0),
            target::DETUNE => s.detune += 50.0 * m / 1200.0,
            target::SPREAD => s.spread = (s.spread + 50.0 * m / 1200.0).max(0.0),
            target::VOLUME => self.gain *= scale,
            target::PAN => self.pan = (self.pan + m).clamp(-1.0, 1.0),
            target::OSC2_PITCH => self.pitch2 += 12.0 * m,
            target::FM => s.fm = (s.fm + m).clamp(0.0, 1.0),
            target::RING => s.ring = (s.ring + m).clamp(0.0, 1.0),
            target::FILTER_FM => s.filter_fm = (s.filter_fm + m).clamp(0.0, 1.0),
            target::LFO2_RATE => self.lfo2_speed *= 2f32.powf(2.0 * m),
            // LFO 1's rate is the processor's (one LFO): the wheel moves it.
            _ => {}
        }
    }
}

impl Settings {
    /// `self` with the mod wheel at `w` (0…1).
    fn wheeled(&self, w: f32) -> Modded {
        let mut out = Modded {
            s: *self,
            vibrato: 0.0,
            pitch: 0.0,
            pitch2: 0.0,
            lfo: 1.0,
            gain: 1.0,
            pan: 0.0,
            lfo2_speed: 1.0,
        };
        // Brought in (amount > 0) or taken out (< 0) by the wheel.
        let fade = |a: f32| {
            if a >= 0.0 {
                1.0 - a + a * w
            } else {
                1.0 + a * w
            }
        };
        for &(target, a) in &self.wheel {
            out.apply(target, a * w, fade(a));
        }
        out
    }
}

/// What a block gives every voice: the LFO's value, LFO 2's phase when
/// locked to the song, and its speed (cycles per control tick).
struct Block {
    lfo: f32,
    lfo2_phase: Option<f64>,
    lfo2_cycles: f64,
}

/// What a voice's samples read between control ticks.
#[derive(Clone, Copy)]
struct Segment {
    pw: f32,
    tremolo: f32,
    /// Semitones from the wheel and the modulations.
    bend: f32,
    pan_l: f32,
    pan_r: f32,
    /// The sub's key tracking.
    sub_scale: f32,
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

/// The wave without its band limiting (what a sync jump lands on).
#[inline]
fn naive(kind: usize, t: f32, pw: f32) -> f32 {
    match kind {
        1 => {
            if t < pw {
                1.0
            } else {
                -1.0
            }
        }
        2 => 1.0 - 4.0 * (t - 0.5).abs(),
        3 => (2.0 * PI * t).sin(),
        _ => 2.0 * t - 1.0,
    }
}

/// A random value in −1…1 from an integer.
fn spread_random(x: u32) -> f32 {
    let h = x.wrapping_mul(2_246_822_519) ^ (x >> 15);
    let h = h.wrapping_mul(3_266_489_917) ^ (h >> 13);
    h as f32 / u32::MAX as f32 * 2.0 - 1.0
}

/// The effects' block settings.
struct FxSettings {
    chorus_mix: f32,
    chorus_rate: f64,
    chorus_depth: f64,
    ensemble: bool,
    delay_mix: f32,
    delay_samples: f64,
    feedback: f64,
    pingpong: bool,
    delay_tone: f64,
    reverb_mix: f32,
    size: f64,
    /// Seconds to −60 dB.
    decay: f64,
    reverb_tone: f64,
}

/// The reverb's delay lines (ms at size 1).
const REVERB_LINES: [f64; 8] = [23.1, 29.7, 37.1, 41.3, 47.9, 56.3, 67.1, 79.3];
/// Its input diffusers (ms).
const REVERB_DIFFUSERS: [f64; 2] = [7.7, 5.1];
/// Which lines feed which side (two orthogonal rows of the Hadamard
/// matrix), and how the input enters them.
const REVERB_LEFT: [f32; 8] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];
const REVERB_RIGHT: [f32; 8] = [1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0];
const REVERB_IN: [f64; 8] = [1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0];

/// The effects after the voices: a chorus/ensemble, a stereo delay and a
/// small feedback-delay-network reverb. Each is run only while it is
/// mixed in; `ringing` says whether a tail is still on its way.
struct Fx {
    sr: f64,
    chorus: [DelayLine; 2],
    chorus_phase: f64,
    chorus_fast: f64,
    delay: [DelayLine; 2],
    delay_time: Smoothed,
    delay_tone: [OnePole; 2],
    reverb: [DelayLine; 8],
    reverb_damp: [OnePole; 8],
    reverb_ap: [DelayLine; 2],
    reverb_size: Smoothed,
    reverb_phase: f64,
    /// Samples of tail still to come, per effect.
    tails: [u32; 3],
    /// Whether each was mixed in last block.
    was: [bool; 3],
}

impl Fx {
    fn new(sr: f64) -> Self {
        Self {
            sr,
            chorus: std::array::from_fn(|_| DelayLine::new((0.03 * sr) as usize)),
            chorus_phase: 0.0,
            chorus_fast: 0.0,
            delay: std::array::from_fn(|_| DelayLine::new((2.05 * sr) as usize)),
            delay_time: Smoothed::new(0.375 * sr, 40.0, sr),
            delay_tone: [OnePole::new(4_000.0, sr); 2],
            reverb: std::array::from_fn(|_| DelayLine::new((0.13 * sr) as usize)),
            reverb_damp: [OnePole::new(5_000.0, sr); 8],
            reverb_ap: std::array::from_fn(|i| {
                DelayLine::new((REVERB_DIFFUSERS[i] * 0.001 * sr) as usize + 8)
            }),
            reverb_size: Smoothed::new(0.95, 200.0, sr),
            reverb_phase: 0.0,
            tails: [0; 3],
            was: [false; 3],
        }
    }

    fn reset(&mut self) {
        for l in self
            .chorus
            .iter_mut()
            .chain(&mut self.delay)
            .chain(&mut self.reverb)
            .chain(&mut self.reverb_ap)
        {
            l.reset();
        }
        for d in self.delay_tone.iter_mut().chain(&mut self.reverb_damp) {
            d.reset();
        }
        self.tails = [0; 3];
    }

    fn ringing(&self) -> bool {
        self.tails.iter().any(|t| *t > 0)
    }

    /// Run the effects over `l`/`r` (`r` None: mono, the wet sides
    /// averaged), where `input_peak` is the loudest the voices got this
    /// block.
    fn process(
        &mut self,
        set: &FxSettings,
        l: &mut [f32],
        mut r: Option<&mut [f32]>,
        input_peak: f32,
    ) {
        let frames = l.len();
        let sr = self.sr;
        let on = [
            set.chorus_mix > 0.0,
            set.delay_mix > 0.0,
            set.reverb_mix > 0.0,
        ];
        // Tails: how long after this block's input the effects still
        // sound (the delay's repeats until they are below the floor).
        if input_peak > TAIL_FLOOR {
            let repeats = if set.feedback > 0.001 {
                (TAIL_FLOOR as f64).ln() / set.feedback.ln()
            } else {
                1.0
            };
            let tails = [
                0.05 * sr,
                (set.delay_samples * (1.0 + repeats)).min(60.0 * sr),
                set.decay * 1.8 * sr,
            ];
            for i in 0..3 {
                if on[i] {
                    self.tails[i] = self.tails[i].max(tails[i] as u32);
                }
            }
        }
        for i in 0..3 {
            if !on[i] {
                self.tails[i] = 0;
            }
        }
        if !on[0] && !on[1] && !on[2] {
            self.was = on;
            return;
        }
        // Block settings.
        if on[0] && !self.was[0] {
            for c in &mut self.chorus {
                c.reset();
            }
        }
        if on[1] {
            self.delay_time.set(set.delay_samples);
            if !self.was[1] {
                self.delay_time.snap();
                for d in &mut self.delay {
                    d.reset();
                }
                for t in &mut self.delay_tone {
                    t.reset();
                }
            }
            for t in &mut self.delay_tone {
                t.set(set.delay_tone, sr);
            }
        }
        let mut gains = [0.0f64; 8];
        let mut lens = [0.0f64; 8];
        if on[2] {
            self.reverb_size.set(0.4 + 1.1 * set.size);
            if !self.was[2] {
                self.reverb_size.snap();
                for d in self.reverb.iter_mut().chain(&mut self.reverb_ap) {
                    d.reset();
                }
                for d in &mut self.reverb_damp {
                    d.reset();
                }
            }
            for d in &mut self.reverb_damp {
                d.set(set.reverb_tone, sr);
            }
            let factor = self.reverb_size.target;
            for i in 0..8 {
                lens[i] = REVERB_LINES[i] * 0.001 * sr * factor;
                gains[i] = 10f64.powf(-3.0 * lens[i] / (set.decay * sr));
            }
        }
        self.was = on;
        let chorus_inc = set.chorus_rate / sr;
        let fast_inc = 6.3 / sr;
        let base = 0.010 * sr;
        let depth = set.chorus_depth * 0.005 * sr;
        let ap_n = [
            (REVERB_DIFFUSERS[0] * 0.001 * sr) as usize,
            (REVERB_DIFFUSERS[1] * 0.001 * sr) as usize,
        ];
        let reverb_inc = 0.3 / sr;
        let reverb_depth = 0.0003 * sr;
        let mono = r.is_none();
        for i in 0..frames {
            let (xl, xr) = match r.as_deref() {
                Some(r) => (l[i], r[i]),
                None => (l[i], l[i]),
            };
            let (mut yl, mut yr) = (xl, xr);
            if on[0] {
                self.chorus[0].push(f64::from(xl));
                self.chorus[1].push(f64::from(xr));
                let p = self.chorus_phase;
                let (wl, wr) = if set.ensemble {
                    let f = self.chorus_fast;
                    let (mut wl, mut wr) = (0.0, 0.0);
                    for j in 0..3 {
                        let o = j as f64 / 3.0;
                        let sweep = |p0: f64, f0: f64| {
                            0.8 * (std::f64::consts::TAU * (p + p0 + o)).sin()
                                + 0.2 * (std::f64::consts::TAU * (f + f0 + o)).sin()
                        };
                        wl += self.chorus[0].read(base + depth * sweep(0.0, 0.0));
                        wr += self.chorus[1].read(base + depth * sweep(1.0 / 6.0, 0.5));
                    }
                    (wl / 3.0, wr / 3.0)
                } else {
                    let m = (std::f64::consts::TAU * p).sin();
                    (
                        self.chorus[0].read(base + depth * m),
                        self.chorus[1].read(base - depth * m),
                    )
                };
                self.chorus_phase = (p + chorus_inc).fract();
                self.chorus_fast = (self.chorus_fast + fast_inc).fract();
                yl += set.chorus_mix * wl as f32;
                yr += set.chorus_mix * wr as f32;
            }
            if on[1] {
                let d = self.delay_time.tick();
                let (dl, dr) = (self.delay[0].read(d), self.delay[1].read(d));
                let fl = self.delay_tone[0].low(dl) * set.feedback;
                let fr = self.delay_tone[1].low(dr) * set.feedback;
                if set.pingpong {
                    self.delay[0].push(0.5 * f64::from(yl + yr) + fr);
                    self.delay[1].push(fl);
                } else {
                    self.delay[0].push(f64::from(yl) + fl);
                    self.delay[1].push(f64::from(yr) + fr);
                }
                yl += set.delay_mix * dl as f32;
                yr += set.delay_mix * dr as f32;
            }
            if on[2] {
                let mut d = 0.5 * f64::from(yl + yr);
                for (ap, n) in self.reverb_ap.iter_mut().zip(ap_n) {
                    let z = ap.tap(n - 1);
                    let v = d + 0.5 * z;
                    ap.push(v);
                    d = z - 0.5 * v;
                }
                let glide = self.reverb_size.tick() / self.reverb_size.target.max(1e-6);
                let ph = self.reverb_phase;
                self.reverb_phase = (ph + reverb_inc).fract();
                let mut v = [0.0f64; 8];
                for k in 0..8 {
                    let len = lens[k] * glide
                        + reverb_depth * (std::f64::consts::TAU * (ph + k as f64 / 8.0)).sin();
                    v[k] = self.reverb_damp[k].low(self.reverb[k].read(len)) * gains[k];
                }
                hadamard8(&mut v);
                let (mut wl, mut wr) = (0.0f32, 0.0f32);
                for k in 0..8 {
                    wl += v[k] as f32 * REVERB_LEFT[k];
                    wr += v[k] as f32 * REVERB_RIGHT[k];
                    self.reverb[k].push(v[k] + d * REVERB_IN[k]);
                }
                yl += set.reverb_mix * 0.35 * wl;
                yr += set.reverb_mix * 0.35 * wr;
            }
            match r.as_deref_mut() {
                Some(r) => {
                    l[i] = yl;
                    r[i] = yr;
                }
                None => l[i] = 0.5 * (yl + yr),
            }
        }
        let _ = mono;
        for t in &mut self.tails {
            *t = t.saturating_sub(frames as u32);
        }
        for d in self.delay_tone.iter_mut().chain(&mut self.reverb_damp) {
            d.flush();
        }
    }
}

/// An 8-point Hadamard transform, scaled to keep energy.
#[inline]
fn hadamard8(v: &mut [f64; 8]) {
    for s in [1usize, 2, 4] {
        for i in 0..8 {
            if i & s == 0 {
                let (a, b) = (v[i], v[i + s]);
                v[i] = a + b;
                v[i + s] = a - b;
            }
        }
    }
    for x in v.iter_mut() {
        *x *= std::f64::consts::FRAC_1_SQRT_2 * 0.5;
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
    /// The mod wheel as last moved (any channel).
    wheel_last: f32,
    lfo: Lfo,
    /// LFO 2's free-running phase (voices not retriggered take it up).
    lfo2: Lfo,
    /// The last note's pitch (semitones from A4), for glide.
    last_pitch: Option<f32>,
    /// Keys held in mono modes, oldest first.
    held: [(u8, u8, u8); 16],
    held_len: usize,
    /// Keys down right now (any mode), for glide between held keys.
    keys_down: u32,
    fx: Fx,
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
            wheel_last: 0.0,
            lfo: Lfo::new(0x51_7e),
            lfo2: Lfo::new(0x2b_91),
            last_pitch: None,
            held: [(0, 0, 0); 16],
            held_len: 0,
            keys_down: 0,
            fx: Fx::new(f64::from(sr)),
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
        let fade = self.get(id::LFO2_FADE);
        let choice = |pid: u32, n: usize| (self.get(pid).round().max(0.0) as usize).min(n - 1);
        Settings {
            gain: db_to_gain(self.get(id::VOLUME)),
            cutoff: self.get(id::CUTOFF),
            k: 2.0 - 1.9 * self.get(id::RESONANCE).clamp(0.0, 1.0),
            env_octaves: self.get(id::ENV_AMOUNT) * 6.0,
            amp: env(id::ATTACK, id::DECAY, id::SUSTAIN, id::RELEASE),
            filt: env(id::F_ATTACK, id::F_DECAY, id::F_SUSTAIN, id::F_RELEASE),
            menv: env(id::M_ATTACK, id::M_DECAY, id::M_SUSTAIN, id::M_RELEASE),
            own_filter_env: self.get(id::FILTER_ENV) >= 0.5,
            detune: self.get(id::DETUNE) / 1200.0,
            waves: [choice(id::OSC1_WAVE, 4), choice(id::OSC2_WAVE, 4)],
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
            filter: choice(id::FILTER_TYPE, FILTERS.len()),
            drive: self.get(id::DRIVE),
            key_track: self.get(id::KEY_TRACK),
            vel_cutoff: self.get(id::VELOCITY_CUTOFF),
            vel_amp: self.get(id::VELOCITY_AMP),
            lfo_pitch: self.get(id::LFO_PITCH) / 100.0,
            lfo_cutoff: self.get(id::LFO_CUTOFF),
            lfo_amp: self.get(id::LFO_AMP),
            lfo_pw: self.get(id::LFO_PW),
            mode: choice(id::VOICE_MODE, 3),
            glide_coef: if glide < 0.5 {
                0.0
            } else {
                (-6.9 / ms(glide)).exp()
            },
            polyphony: (self.get(id::POLYPHONY).round() as usize).clamp(1, MAX_VOICES),
            bend_range: self.get(id::BEND_RANGE).round().clamp(1.0, 24.0),
            resonance: self.get(id::RESONANCE).clamp(0.0, 1.0),
            wheel: std::array::from_fn(|k| {
                (
                    choice(id::wheel_target(k), TARGETS.len()),
                    self.get(id::wheel_amount(k)).clamp(-1.0, 1.0),
                )
            }),
            matrix: std::array::from_fn(|k| {
                (
                    choice(id::mod_source(k), SOURCES.len()),
                    choice(id::mod_target(k), TARGETS.len()),
                    self.get(id::mod_amount(k)).clamp(-1.0, 1.0),
                )
            }),
            lfo2_shape: Shape::from_index(choice(id::LFO2_SHAPE, 6)),
            lfo2_retrig: self.get(id::LFO2_RETRIG) >= 0.5,
            lfo2_fade_step: if fade < 0.5 {
                1.0
            } else {
                CONTROL_INTERVAL as f32 / ms(fade)
            },
            sync: self.get(id::SYNC) >= 0.5,
            ring: self.get(id::RING).clamp(0.0, 1.0),
            fm: self.get(id::FM).clamp(0.0, 1.0),
            phase_reset: self.get(id::PHASE_RESET) >= 0.5,
            sub_key: self.get(id::SUB_KEY).clamp(-1.0, 1.0),
            sub_wave: choice(id::SUB_WAVE, SUB_WAVES.len()),
            pink: self.get(id::NOISE_COLOUR) >= 0.5,
            filter_fm: self.get(id::FILTER_FM).clamp(0.0, 1.0),
            steal: choice(id::STEAL, 2),
            glide_legato: self.get(id::GLIDE_MODE) >= 0.5,
            drift: self.get(id::DRIFT).clamp(0.0, 1.0),
        }
    }

    fn fx_settings(&self, tempo: f64) -> FxSettings {
        let sr = f64::from(self.sample_rate);
        let synced = self.get(id::DELAY_SYNC) >= 0.5;
        let seconds = if synced {
            let q = DIVISIONS
                [(self.get(id::DELAY_DIVISION).round().max(0.0) as usize).min(DIVISIONS.len() - 1)]
            .1;
            q * 60.0 / tempo
        } else {
            f64::from(self.get(id::DELAY_TIME)) * 0.001
        };
        FxSettings {
            chorus_mix: self.get(id::CHORUS_MIX).clamp(0.0, 1.0),
            chorus_rate: f64::from(self.get(id::CHORUS_RATE)),
            chorus_depth: f64::from(self.get(id::CHORUS_DEPTH).clamp(0.0, 1.0)),
            ensemble: self.get(id::CHORUS_MODE) >= 0.5,
            delay_mix: self.get(id::DELAY_MIX).clamp(0.0, 1.0),
            delay_samples: (seconds * sr).clamp(1.0, 2.0 * sr),
            feedback: f64::from(self.get(id::DELAY_FEEDBACK).clamp(0.0, 0.95)),
            pingpong: self.get(id::DELAY_PINGPONG) >= 0.5,
            delay_tone: f64::from(self.get(id::DELAY_TONE)),
            reverb_mix: self.get(id::REVERB_MIX).clamp(0.0, 1.0),
            size: f64::from(self.get(id::REVERB_SIZE).clamp(0.0, 1.0)),
            decay: f64::from(self.get(id::REVERB_DECAY)).max(100.0) * 0.001,
            reverb_tone: f64::from(self.get(id::REVERB_TONE)),
        }
    }

    /// Start (or, `!retrigger`, slide) a voice; `legato`: other keys were
    /// down already.
    fn start(
        &mut self,
        idx: usize,
        channel: u8,
        key: u8,
        velocity: u8,
        retrigger: bool,
        legato: bool,
        s: &Settings,
    ) {
        self.counter += 1;
        let pitch = key as f32 - 69.0;
        let glide = match self.last_pitch {
            Some(from) if s.glide_coef > 0.0 && (legato || !s.glide_legato) => {
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
        let (amp, filt, menv, phase, svf, ladder) = (
            v.amp.value,
            v.filt.value,
            v.menv.value,
            v.phase,
            v.svf,
            v.ladder,
        );
        let n = self.counter as u32;
        let mut lfo2 = self.lfo2;
        if s.lfo2_retrig {
            lfo2.phase = 0.0;
        }
        let mut drift = Lfo::new(n.wrapping_mul(0x9e37_79b9) | 1);
        drift.phase = f64::from(0.5 + 0.5 * spread_random(n.wrapping_add(7)));
        *v = Voice {
            amp: Env {
                stage: Stage::Attack,
                value: if keep { amp } else { 0.0 },
            },
            filt: Env {
                stage: Stage::Attack,
                value: if keep { filt } else { 0.0 },
            },
            menv: Env {
                stage: Stage::Attack,
                value: if keep { menv } else { 0.0 },
            },
            key,
            channel,
            velocity: velocity as f32 / 127.0,
            age: self.counter,
            glide,
            noise: 0x9e37_79b9 ^ n.wrapping_mul(2_654_435_761),
            lfo2,
            lfo2_fade: if s.lfo2_fade_step >= 1.0 { 1.0 } else { 0.0 },
            drift,
            drift_offset: spread_random(n.wrapping_add(3)),
            random: spread_random(n),
            ..Voice::IDLE
        };
        if keep {
            v.phase = phase;
            v.svf = svf;
            v.ladder = ladder;
        } else if s.phase_reset {
            v.phase = [[0.0; 2]; MAX_UNISON];
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
        let others = self.keys_down > 0;
        self.keys_down += 1;
        if s.mode > 0 {
            // Mono: one voice; legato keeps its envelopes while keys are
            // held.
            let legato = s.mode == 2 && self.held_len > 0 && self.voices[0].sounding();
            if self.held_len < self.held.len() {
                self.held[self.held_len] = (channel, key, velocity);
                self.held_len += 1;
            }
            self.start(0, channel, key, velocity, !legato, others, &s);
            return;
        }
        let voices = &self.voices[..s.polyphony];
        let quietest = |set: &mut dyn Iterator<Item = (usize, &Voice)>| {
            set.min_by(|a, b| a.1.quietness().total_cmp(&b.1.quietness()))
                .map(|(i, _)| i)
        };
        let oldest = |set: &mut dyn Iterator<Item = (usize, &Voice)>| {
            set.min_by_key(|(_, v)| v.age).map(|(i, _)| i)
        };
        let pick = |set: &mut dyn Iterator<Item = (usize, &Voice)>| {
            if s.steal == 1 {
                quietest(set)
            } else {
                oldest(set)
            }
        };
        let idx = voices
            .iter()
            .position(|v| !v.sounding())
            .or_else(|| {
                pick(
                    &mut voices
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.amp.stage == Stage::Release),
                )
            })
            .or_else(|| pick(&mut voices.iter().enumerate()))
            .unwrap_or(0);
        self.start(idx, channel, key, velocity, true, others, &s);
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        let s = self.settings();
        self.keys_down = self.keys_down.saturating_sub(1);
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
                self.start(0, c, k, vel, s.mode == 1, true, &s);
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
                    v.release();
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
                    v.release();
                }
            }
        }
    }

    fn all_off(&mut self) {
        for v in &mut self.voices {
            v.release();
        }
        self.held_len = 0;
        self.keys_down = 0;
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
            } => {
                let w = value as f32 / 127.0;
                self.channels[(channel & 15) as usize].modulation = w;
                // The one LFO's rate follows the wheel moved last.
                self.wheel_last = w;
            }
            MidiEvent::PitchBend { channel, value } => {
                let c = &mut self.channels[(channel & 15) as usize];
                c.bend = ((value as f32 - 8192.0) / 8192.0).clamp(-1.0, 1.0);
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                self.channels[(channel & 15) as usize].pressure = pressure as f32 / 127.0;
            }
            MidiEvent::PolyPressure {
                channel,
                key,
                pressure,
            } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.sounding() && v.channel == channel && v.key == key)
                {
                    v.pressure = pressure as f32 / 127.0;
                }
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
            (0, 0) => self.channels[channel as usize].bend_range = Some(value.max(1) as f32),
            (0, 6) if channel == 0 => {
                for ch in 1..=(value.min(15) as usize) {
                    self.channels[ch].bend_range = Some(MPE_BEND_RANGE);
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
        b: &Block,
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
        let drift_cycles = f64::from(DRIFT_HZ * CONTROL_INTERVAL as f32 / sr);
        for v in self.voices.iter_mut().filter(|v| v.sounding()) {
            let ch = channels[(v.channel & 15) as usize];
            // The settings as the channel's mod wheel makes them, and the
            // matrix on top from the sources as they stand (at the
            // segment's start nothing is advanced; every control tick
            // afterwards).
            let wheeled = s.wheeled(ch.modulation);
            if let Some(p) = b.lfo2_phase {
                v.lfo2.phase = p;
            }
            let modulate = |v: &Voice, amp_env: f32, filt_env: f32| -> Modded {
                let mut cur = wheeled;
                let lfo2 = v.lfo2.value(s.lfo2_shape, 0.0) as f32 * v.lfo2_fade;
                for &(src, tgt, amount) in &s.matrix {
                    if src == source::OFF || tgt == target::OFF || amount == 0.0 {
                        continue;
                    }
                    let x = match src {
                        source::LFO1 => b.lfo,
                        source::LFO2 => lfo2,
                        source::MOD_ENV => v.menv.value,
                        source::VELOCITY => v.velocity,
                        source::KEY => ((v.key as f32 - 60.0) / 36.0).clamp(-1.0, 1.0),
                        source::AFTERTOUCH => (ch.pressure + v.pressure).min(1.0),
                        source::WHEEL => ch.modulation,
                        source::RANDOM => v.random,
                        source::AMP_ENV => amp_env,
                        source::FILTER_ENV => filt_env,
                        _ => 0.0,
                    };
                    let m = amount * x;
                    cur.apply(tgt, m, (1.0 + m).max(0.0));
                }
                cur
            };
            // What the samples read until the next tick.
            let segment = |cur: &Modded, v: &Voice| -> Segment {
                let s = &cur.s;
                let lfo = b.lfo * cur.lfo;
                let pan = (v.pan + cur.pan).clamp(-1.0, 1.0);
                Segment {
                    pw: (s.pw + s.lfo_pw * lfo).clamp(0.05, 0.95),
                    tremolo: 1.0 - s.lfo_amp * 0.5 * (1.0 - lfo),
                    bend: ch.bend * ch.bend_range.unwrap_or(s.bend_range) + cur.pitch,
                    pan_l: (1.0 - pan).min(1.0),
                    pan_r: (1.0 + pan).min(1.0),
                    sub_scale: if s.sub_key != 0.0 {
                        2f32.powf(-s.sub_key * (v.key as f32 - 60.0) / 12.0)
                            .clamp(0.0625, 4.0)
                    } else {
                        1.0
                    },
                }
            };
            let mut cur = modulate(v, v.amp.value, v.filt.value);
            let mut seg = segment(&cur, v);
            for i in start..end {
                let amp_env = v.amp.tick(&cur.s.amp);
                let filt_env = if cur.s.own_filter_env {
                    v.filt.tick(&cur.s.filt)
                } else {
                    amp_env
                };
                v.menv.tick(&cur.s.menv);
                if !v.sounding() {
                    break;
                }
                if v.control_counter == 0 {
                    v.glide *= s.glide_coef;
                    v.lfo2.advance(b.lfo2_cycles * f64::from(cur.lfo2_speed));
                    v.lfo2_fade = (v.lfo2_fade + s.lfo2_fade_step).min(1.0);
                    v.drift.advance(drift_cycles);
                    cur = modulate(v, amp_env, filt_env);
                    seg = segment(&cur, v);
                    let s = &cur.s;
                    let lfo = b.lfo * cur.lfo;
                    // Pitch, then each oscillator's increment.
                    let drift = s.drift
                        * (DRIFT_CENTS / 100.0)
                        * (0.6 * v.drift_offset + 0.4 * v.drift.value(Shape::Drift, 0.0) as f32);
                    let semis = v.key as f32 - 69.0
                        + v.glide
                        + seg.bend
                        + v.tuning
                        + vib * (cur.vibrato + v.vibrato * VIBRATO_DEPTH)
                        + lfo * s.lfo_pitch
                        + drift;
                    let base = 440.0 * 2f32.powf(semis / 12.0);
                    for u in 0..n {
                        let spread = if n > 1 {
                            s.spread * (-1.0 + 2.0 * u as f32 / (n - 1) as f32)
                        } else {
                            0.0
                        };
                        let f1 = base * 2f32.powf(s.octaves[0]) * (1.0 - s.detune + spread);
                        let f2 = base
                            * 2f32.powf(s.octaves[1] + (s.semi2 + cur.pitch2) / 12.0)
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
                                + (ch.timbre - 0.5 + v.brightness - 0.5) * 4.0
                                + 0.05 * s.drift * v.drift_offset,
                        ))
                    .clamp(20.0, nyquist_guard);
                    let g = (PI * fc / sr).tan();
                    v.g = g;
                    v.coefs = filter_coefs(s.filter, g, s.k, s.resonance);
                }
                v.control_counter = (v.control_counter + 1) % CONTROL_INTERVAL;
                let s = &cur.s;
                // The oscillators, spread across the sides.
                let mut side = [0.0f32; 2];
                // The first copy's oscillator 2 (for filter FM).
                let mut x2c = 0.0f32;
                for u in 0..n {
                    let (t1, dt1) = (v.phase[u][0], v.inc[u][0]);
                    let (t2, dt2) = (v.phase[u][1], v.inc[u][1]);
                    // Oscillator 2 first: it modulates oscillator 1.
                    let mut x2 = wave(s.waves[1], t2, dt2, seg.pw);
                    if s.sync {
                        x2 += v.pending2[u];
                        v.pending2[u] = 0.0;
                        std::mem::swap(&mut x2, &mut v.held2[u]);
                    }
                    let t1m = if s.fm > 0.0 {
                        let t = t1 + FM_CYCLES * s.fm * s.fm * x2;
                        t - t.floor()
                    } else {
                        t1
                    };
                    let x1 = wave(s.waves[0], t1m, dt1, seg.pw);
                    if u == 0 {
                        x2c = x2;
                    }
                    let (q1, q2) = (pans[u][0], pans[u][1]);
                    if s.levels[0] > 0.0 {
                        let x = x1 * s.levels[0];
                        side[0] += x * 0.5 * (1.0 - q1);
                        side[1] += x * 0.5 * (1.0 + q1);
                    }
                    if s.levels[1] > 0.0 {
                        let x = x2 * s.levels[1];
                        side[0] += x * 0.5 * (1.0 - q2);
                        side[1] += x * 0.5 * (1.0 + q2);
                    }
                    if s.ring > 0.0 {
                        let x = x1 * x2 * s.ring;
                        let q = 0.5 * (q1 + q2);
                        side[0] += x * 0.5 * (1.0 - q);
                        side[1] += x * 0.5 * (1.0 + q);
                    }
                    let mut t1n = t1 + dt1;
                    let wrapped = t1n >= 1.0;
                    if wrapped {
                        t1n -= 1.0;
                    }
                    let mut t2n = t2 + dt2;
                    if t2n >= 1.0 {
                        t2n -= 1.0;
                    }
                    if s.sync && wrapped {
                        // Oscillator 1 wrapped `f` of a sample ago:
                        // oscillator 2 restarts there, the step it makes
                        // band-limited over the sample before (held
                        // back) and the one after.
                        let f = t1n / dt1;
                        let restart = f * dt2;
                        let h = naive(s.waves[1], restart, seg.pw) - naive(s.waves[1], t2n, seg.pw);
                        v.held2[u] += 0.5 * h * f * f;
                        v.pending2[u] = -0.5 * h * (1.0 - f) * (1.0 - f);
                        t2n = restart;
                    }
                    v.phase[u] = [t1n, t2n];
                }
                side[0] *= norm;
                side[1] *= norm;
                if s.sub > 0.0 {
                    let x = match s.sub_wave {
                        1 => (2.0 * PI * v.sub).sin(),
                        2 => 1.0 - 4.0 * (v.sub - 0.5).abs(),
                        _ => wave(1, v.sub, v.sub_inc, 0.5),
                    } * s.sub
                        * 0.5
                        * seg.sub_scale;
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
                    let w = v.noise as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    let w = if s.pink {
                        // Paul Kellet's economy pink filter.
                        let p = &mut v.pink;
                        p[0] = 0.997_65 * p[0] + w * 0.099_046;
                        p[1] = 0.963 * p[1] + w * 0.296_516_4;
                        p[2] = 0.57 * p[2] + w * 1.052_691_3;
                        (p[0] + p[1] + p[2] + w * 0.1848) * 0.25
                    } else {
                        w
                    };
                    let x = w * s.noise * 0.5;
                    side[0] += x;
                    side[1] += x;
                }
                let amp = amp_env
                    * (1.0 - s.vel_amp + s.vel_amp * v.velocity)
                    * s.gain
                    * 0.35
                    * seg.tremolo
                    * cur.gain
                    * (1.0 + 0.5 * (ch.pressure + v.pressure))
                    * v.volume
                    * v.expression;
                // Filter FM: oscillator 2 moves the cutoff at audio rate.
                let coefs = if s.filter_fm > 0.0 {
                    let g = (v.g * (1.0 + 0.9 * s.filter_fm * x2c)).max(1e-3);
                    filter_coefs(s.filter, g, s.k, s.resonance)
                } else {
                    v.coefs
                };
                let mut y = [0.0f32; 2];
                for c in 0..2 {
                    let mut x = side[c];
                    if s.drive > 0.0 {
                        let g = 1.0 + 4.0 * s.drive;
                        x = (x * g).tanh() / g.sqrt();
                    }
                    let f = if s.filter == filter::LADDER {
                        ladder(&mut v.ladder[c], x, coefs)
                    } else {
                        let (low, band, high) = v.svf[c][0].run(x, coefs[0]);
                        match s.filter {
                            filter::LP24 => v.svf[c][1].run(low, coefs[1]).0,
                            filter::BAND => band * coefs[0].3,
                            filter::HIGH => high,
                            filter::NOTCH => low + high,
                            _ => low,
                        }
                    };
                    y[c] = f * amp;
                }
                match out_r.as_deref_mut() {
                    Some(r) => {
                        out_l[i] += y[0] * seg.pan_l;
                        r[i] += y[1] * seg.pan_r;
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
            for side in &mut v.ladder {
                for st in side {
                    if st.abs() < 1e-20 {
                        *st = 0.0;
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
        let sr = f64::from(self.sample_rate);
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
            tempo / 60.0 / period_q / sr
        } else {
            // A mod-wheel slot on LFO Rate speeds it (or slows it).
            let speed: f32 = (0..WHEEL_SLOTS)
                .filter(|k| self.get(id::wheel_target(*k)).round() as usize == target::LFO_RATE)
                .map(|k| 2f32.powf(2.0 * self.get(id::wheel_amount(k)) * self.wheel_last))
                .product();
            f64::from(self.get(id::LFO_RATE) * speed) / sr
        };
        self.lfo.advance(cycles * frames as f64);
        // LFO 2: its free-running phase for new voices, or the song's.
        let synced2 = self.get(id::LFO2_SYNC) >= 0.5;
        let period2 = DIVISIONS
            [(self.get(id::LFO2_DIVISION).round().max(0.0) as usize).min(DIVISIONS.len() - 1)]
        .1;
        let cycles2 = if synced2 {
            tempo / 60.0 / period2 / sr
        } else {
            f64::from(self.get(id::LFO2_RATE)) / sr
        };
        let lfo2_phase = if synced2 && t.playing {
            self.lfo2.sync(t.quarter_position, period2);
            Some(self.lfo2.phase)
        } else {
            None
        };
        self.lfo2.advance(cycles2 * frames as f64);
        let block = Block {
            lfo,
            lfo2_phase,
            lfo2_cycles: cycles2 * f64::from(CONTROL_INTERVAL),
        };
        let fx = self.fx_settings(tempo);
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
                        self.render(&settings, &block, l, Some(r), pos, at);
                    } else {
                        self.render(&settings, &block, out.channel_mut(0), None, pos, at);
                    }
                    pos = at;
                }
                self.handle(e.event);
            }
        }
        if frames > pos {
            if stereo {
                let (l, r) = out.channel_pair_mut(0, 1);
                self.render(&settings, &block, l, Some(r), pos, frames);
            } else {
                self.render(&settings, &block, out.channel_mut(0), None, pos, frames);
            }
        }
        // The effects.
        let voices_peak = (0..2.min(out.num_channels()))
            .flat_map(|c| out.channel(c)[..frames].iter())
            .fold(0.0f32, |m, v| m.max(v.abs()));
        if stereo {
            let (l, r) = out.channel_pair_mut(0, 1);
            self.fx
                .process(&fx, &mut l[..frames], Some(&mut r[..frames]), voices_peak);
        } else {
            let l = out.channel_mut(0);
            self.fx.process(&fx, &mut l[..frames], None, voices_peak);
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
                tap.set_value(value::MOD_ENV, v.menv.value);
                tap.set_value(
                    value::LFO2,
                    v.lfo2.value(settings.lfo2_shape, 0.0) as f32 * v.lfo2_fade,
                );
                tap.set_value(value::CUTOFF, v.g.atan() * self.sample_rate / PI);
            } else {
                tap.set_value(value::AMP_ENV, 0.0);
                tap.set_value(value::FILTER_ENV, 0.0);
                tap.set_value(value::MOD_ENV, 0.0);
                tap.set_value(value::LFO2, 0.0);
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
        if self.voices.iter().all(|v| !v.sounding()) && !self.fx.ringing() {
            // Nothing to come: the lines' leftovers must not surface
            // whenever the next note wakes the processor.
            self.fx.reset();
            ProcessStatus::Sleep
        } else {
            ProcessStatus::Continue
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; MAX_VOICES];
        self.channels = [ChannelState::REST; 16];
        self.held_len = 0;
        self.keys_down = 0;
        self.last_pitch = None;
        self.fx.reset();
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

    /// The loudest frequency of `l` between `lo` and `hi` Hz (a semitone
    /// apart: 1/24 octave steps).
    fn peak_hz(l: &[f32], lo: f64, hi: f64) -> f64 {
        let mut best = (lo, f64::MIN);
        let mut f = lo;
        while f <= hi {
            let db = bin_db(l, f);
            if db > best.1 {
                best = (f, db);
            }
            f *= 2f64.powf(1.0 / 48.0);
        }
        best.0
    }

    /// The pitch wheel bends as far as Pitch Bend Range says (a range a
    /// controller sets with RPN 0 wins on its channel); the mod wheel opens
    /// the filter as much as Mod Wheel to Cutoff says.
    #[test]
    fn the_wheels_bend_and_modulate_as_set() {
        let plain = |extra: &[(u32, f64)]| {
            let mut set = vec![
                (id::CUTOFF, 20_000.0),
                (id::ENV_AMOUNT, 0.0),
                (id::DETUNE, 0.0),
                (id::OSC1_WAVE, 3.0),
                (id::OSC2_LEVEL, 0.0),
                (id::SUB, 0.0),
            ];
            set.extend_from_slice(extra);
            Play::new(&set)
        };
        // Full bend up: a whole tone by default, an octave at ±12.
        for (range, want) in [(2.0, 220.0 * 2f64.powf(2.0 / 12.0)), (12.0, 440.0)] {
            let mut s = plain(&[(id::BEND_RANGE, range)]);
            s.send(MidiEvent::PitchBend {
                channel: 0,
                value: 16383,
            });
            s.send(on(57));
            let (l, _) = s.run(0.4);
            let got = peak_hz(&l, 150.0, 600.0);
            assert!(
                (got / want - 1.0).abs() < 0.02,
                "±{range}: {got} for {want}"
            );
        }
        // RPN 0 from the controller: ±7 on its channel, whatever the knob.
        let mut s = plain(&[(id::BEND_RANGE, 12.0)]);
        for (cc, v) in [(101, 0), (100, 0), (6, 7)] {
            s.send(MidiEvent::ControlChange {
                channel: 0,
                controller: cc,
                value: v,
            });
        }
        s.send(MidiEvent::PitchBend {
            channel: 0,
            value: 16383,
        });
        s.send(on(57));
        let (l, _) = s.run(0.4);
        let want = 220.0 * 2f64.powf(7.0 / 12.0);
        let got = peak_hz(&l, 150.0, 600.0);
        assert!((got / want - 1.0).abs() < 0.02, "RPN ±7: {got}");
        // Slot 1 on the cutoff (+3 octaves), slot 2 on the volume (taken
        // out by half), slot 3 off.
        let play = |wheel: u8, set: &[(u32, f64)]| {
            let mut all = vec![
                (id::CUTOFF, 300.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
            ];
            all.extend_from_slice(set);
            let mut s = Play::new(&all);
            s.send(MidiEvent::ControlChange {
                channel: 0,
                controller: 1,
                value: wheel,
            });
            s.send(on(57));
            let (l, _) = s.run(0.4);
            l
        };
        let slots = [
            (id::wheel_target(0), target::CUTOFF as f64),
            (id::wheel_amount(0), 0.75),
        ];
        let (closed, open) = (play(0, &slots), play(127, &slots));
        let (a, b) = (bin_db(&closed, 220.0 * 8.0), bin_db(&open, 220.0 * 8.0));
        assert!(b - a > 12.0, "the wheel opens the filter: {a} → {b} dB");
        let both = [
            (id::wheel_target(0), target::CUTOFF as f64),
            (id::wheel_amount(0), 0.75),
            (id::wheel_target(1), target::VOLUME as f64),
            (id::wheel_amount(1), -0.5),
        ];
        let quiet = play(127, &both);
        let fundamental = |l: &[f32]| bin_db(l, 220.0);
        let drop = fundamental(&open) - fundamental(&quiet);
        assert!((drop - 6.0).abs() < 1.0, "half the level: {drop} dB");
        assert!(bin_db(&quiet, 220.0 * 8.0) - a > 6.0, "and still opened");
        // Off: the wheel does nothing.
        let off = [(id::wheel_target(0), target::OFF as f64)];
        let (x, y) = (play(0, &off), play(127, &off));
        assert!((bin_db(&x, 220.0 * 8.0) - bin_db(&y, 220.0 * 8.0)).abs() < 0.5);
        assert_eq!(
            format(ParameterId(id::wheel_target(1)), target::RESONANCE as f64).as_deref(),
            Some("Resonance")
        );
    }

    /// A sine note with everything else off.
    fn sine(extra: &[(u32, f64)]) -> Vec<(u32, f64)> {
        let mut set = vec![
            (id::CUTOFF, 20_000.0),
            (id::ENV_AMOUNT, 0.0),
            (id::RESONANCE, 0.0),
            (id::DETUNE, 0.0),
            (id::OSC1_WAVE, 3.0),
            (id::OSC2_WAVE, 3.0),
            (id::OSC2_LEVEL, 0.0),
            (id::WIDTH, 0.0),
            (id::wheel_target(0), target::OFF as f64),
        ];
        set.extend_from_slice(extra);
        set
    }

    #[test]
    fn the_parameters_are_their_positions() {
        for (i, p) in parameters().iter().enumerate() {
            assert_eq!(p.id.0 as usize, i, "{}", p.name);
            assert!(p.default >= p.min && p.default <= p.max, "{}", p.name);
        }
        assert_eq!(parameters().len(), id::DRIFT as usize + 1);
        assert_eq!(
            format(ParameterId(id::mod_source(2)), source::MOD_ENV as f64).as_deref(),
            Some("Mod Env")
        );
        assert_eq!(
            format(ParameterId(id::mod_target(5)), target::LFO2_RATE as f64).as_deref(),
            Some("LFO 2 Rate")
        );
        assert_eq!(
            format(ParameterId(id::FILTER_TYPE), filter::LADDER as f64).as_deref(),
            Some("Ladder")
        );
    }

    /// The mod envelope and the second LFO reach what the matrix sends
    /// them to; the other sources do their part.
    #[test]
    fn the_matrix_routes_its_sources() {
        // Mod Env → Pitch +100 %: an octave up while it sustains.
        let mut s = Play::new(&sine(&[
            (id::M_ATTACK, 0.5),
            (id::M_DECAY, 1.0),
            (id::M_SUSTAIN, 1.0),
            (id::mod_source(0), source::MOD_ENV as f64),
            (id::mod_target(0), target::PITCH as f64),
            (id::mod_amount(0), 1.0),
        ]));
        s.send(on(57));
        let (l, _) = s.run(0.4);
        let got = peak_hz(&l, 150.0, 600.0);
        assert!(
            (got / 440.0 - 1.0).abs() < 0.02,
            "mod env an octave up: {got}"
        );
        // LFO 2 (square, slow, restarted) → Cutoff: the filter opens and
        // closes with it — the first half cycle open, the second closed.
        let play = |extra: &[(u32, f64)]| {
            let mut all = vec![
                (id::CUTOFF, 300.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
                (id::wheel_target(0), target::OFF as f64),
            ];
            all.extend_from_slice(extra);
            let mut s = Play::new(&all);
            s.send(on(57));
            s.run(0.5)
        };
        let (closed, _) = play(&[]);
        let (lfo2, _) = play(&[
            (id::LFO2_SHAPE, 3.0),
            (id::LFO2_RATE, 1.0),
            (id::LFO2_RETRIG, 1.0),
            (id::mod_source(0), source::LFO2 as f64),
            (id::mod_target(0), target::CUTOFF as f64),
            (id::mod_amount(0), 1.0),
        ]);
        let bright = |x: &[f32]| bin_db(x, 220.0 * 8.0);
        // The second half of the second quarter second: the LFO's high
        // half (0…0.5 s at 1 Hz) ends at 0.5 s; look at 0.25…0.5.
        let n = (0.25 * SR) as usize;
        assert!(
            bright(&lfo2[..2 * n]) - bright(&closed[..2 * n]) > 12.0,
            "LFO 2 opens the filter: {} vs {}",
            bright(&lfo2[..2 * n]),
            bright(&closed[..2 * n])
        );
        // Velocity → Volume −100 %: a soft note is taken down a lot more.
        let level = |velocity: u8, extra: &[(u32, f64)]| {
            let mut set = sine(&[(id::VELOCITY_AMP, 0.0)]);
            set.extend_from_slice(extra);
            let mut s = Play::new(&set);
            s.send(MidiEvent::NoteOn {
                channel: 0,
                key: 57,
                velocity,
            });
            let (l, _) = s.run(0.3);
            bin_db(&l, 220.0)
        };
        let route = [
            (id::mod_source(1), source::VELOCITY as f64),
            (id::mod_target(1), target::VOLUME as f64),
            (id::mod_amount(1), -1.0),
        ];
        assert!(
            (level(100, &[]) - level(20, &[])).abs() < 0.1,
            "no velocity"
        );
        let (loud, soft) = (level(127, &route), level(32, &route));
        assert!(
            soft - loud > 6.0 && loud < level(127, &[]) - 20.0,
            "velocity takes the volume out: {loud} / {soft}"
        );
        // Random → Pitch: two notes land differently; Off: alike.
        let pitch = |extra: &[(u32, f64)]| -> Vec<f64> {
            (0..3)
                .map(|_| {
                    let mut s = Play::new(&sine(extra));
                    for _ in 0..3 {
                        s.send(on(57));
                        s.run(0.05);
                        s.send(off(57));
                        s.run(0.5);
                    }
                    s.send(on(57));
                    let (l, _) = s.run(0.4);
                    peak_hz(&l, 150.0, 600.0)
                })
                .collect()
        };
        let random = pitch(&[
            (id::mod_source(2), source::RANDOM as f64),
            (id::mod_target(2), target::PITCH as f64),
            (id::mod_amount(2), 1.0),
            (id::RELEASE, 5.0),
        ]);
        assert!(
            random.iter().any(|p| (p / 220.0 - 1.0).abs() > 0.03),
            "random moved the pitch: {random:?}"
        );
        // Key → Cutoff: a high key opens the filter (a low one closes it
        // a little: the source is bipolar round C4).
        let high = |route: bool| {
            let mut set = vec![
                (id::CUTOFF, 300.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
                (id::DETUNE, 0.0),
                (id::OSC2_LEVEL, 0.0),
            ];
            if route {
                set.extend([
                    (id::mod_source(0), source::KEY as f64),
                    (id::mod_target(0), target::CUTOFF as f64),
                    (id::mod_amount(0), 1.0),
                ]);
            }
            let mut s = Play::new(&set);
            s.send(on(93));
            let (l, _) = s.run(0.5);
            // The second harmonic against the fundamental (a saw's is 6
            // dB down; the filter two octaves below takes much more).
            bin_db(&l, 3520.0) - bin_db(&l, 1760.0)
        };
        let (closed, open) = (high(false), high(true));
        assert!(
            open > closed + 6.0 && open > -12.0,
            "the key opens the filter: {closed} → {open}"
        );
    }

    /// LFO 2 fades in over its time and locks to the song when synced.
    #[test]
    fn lfo_2_fades_in_and_syncs() {
        let play = |extra: &[(u32, f64)], playing: bool| {
            let mut set = sine(&[
                (id::LFO2_RATE, 6.0),
                (id::LFO2_SHAPE, 0.0),
                (id::mod_source(0), source::LFO2 as f64),
                (id::mod_target(0), target::PITCH as f64),
                (id::mod_amount(0), 1.0),
            ]);
            set.extend_from_slice(extra);
            let mut s = Play::new(&set);
            s.transport.playing = playing;
            s.transport.tempo = 120.0;
            s.send(on(57));
            s.run(1.0)
        };
        // Pitch wobble: how far the fundamental's neighbours carry.
        let wobble = |x: &[f32]| bin_db(x, 220.0 * 1.3) - bin_db(x, 220.0);
        let (fast, _) = play(&[], false);
        let (faded, _) = play(&[(id::LFO2_FADE, 4_000.0)], false);
        let early = (0.3 * SR) as usize;
        assert!(
            wobble(&faded[..early]) < wobble(&fast[..early]) - 10.0,
            "faded in: {} vs {}",
            wobble(&faded[..early]),
            wobble(&fast[..early])
        );
        // Synced to a quarter: the phase comes from the song's position,
        // which this harness holds at zero — so a square LFO 2 sits on
        // its high half and the pitch stays up, where free-running it
        // alternates.
        let (synced, _) = play(
            &[
                (id::LFO2_SYNC, 1.0),
                (id::LFO2_DIVISION, 11.0),
                (id::LFO2_SHAPE, 3.0),
                (id::mod_amount(0), 0.1),
            ],
            true,
        );
        let got = peak_hz(&synced, 150.0, 400.0);
        let want = 220.0 * 2f64.powf(1.2 / 12.0);
        assert!(
            (got / want - 1.0).abs() < 0.02,
            "held at the song's phase: {got}"
        );
        assert!(bin_db(&synced, 220.0 * 2f64.powf(-1.2 / 12.0)) < -50.0);
    }

    /// Hard sync makes oscillator 2 periodic at oscillator 1's pitch.
    #[test]
    fn hard_sync_locks_osc_2_to_osc_1() {
        let play = |sync: f64| {
            let mut s = Play::new(&[
                (id::CUTOFF, 20_000.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
                (id::DETUNE, 0.0),
                (id::OSC1_LEVEL, 0.0),
                (id::OSC2_SEMI, 7.0),
                (id::WIDTH, 0.0),
                (id::wheel_target(0), target::OFF as f64),
                (id::SYNC, sync),
            ]);
            s.send(on(57));
            s.run(0.5).0
        };
        let (free, synced) = (play(0.0), play(1.0));
        // Free: the fifth (329.6 Hz) and nothing at 220. Synced: the
        // waveform repeats at 220 Hz, so 220 and its harmonics carry.
        assert!(bin_db(&free, 220.0) < -50.0, "{}", bin_db(&free, 220.0));
        assert!(
            bin_db(&synced, 220.0) > -40.0 && bin_db(&synced, 220.0) > bin_db(&free, 220.0) + 15.0,
            "synced fundamental: {} (free {})",
            bin_db(&synced, 220.0),
            bin_db(&free, 220.0)
        );
        assert!(bin_db(&synced, 440.0) > -45.0, "{}", bin_db(&synced, 440.0));
        assert!(synced.iter().all(|v| v.is_finite() && v.abs() < 2.0));
        // Osc 1's own level stays out of it: the test set it to zero.
    }

    /// Ring modulation of two sines gives their sum and difference; FM of
    /// a sine by a sine at the same pitch gives it harmonics.
    #[test]
    fn ring_mod_and_fm_add_what_they_should() {
        let mut s = Play::new(&sine(&[
            (id::OSC1_LEVEL, 0.0),
            (id::OSC2_SEMI, 7.0),
            (id::RING, 1.0),
        ]));
        s.send(on(69));
        let (l, _) = s.run(0.5);
        let (diff, sum) = (659.26 - 440.0, 659.26 + 440.0);
        assert!(bin_db(&l, diff) > -40.0, "difference: {}", bin_db(&l, diff));
        assert!(bin_db(&l, sum) > -40.0, "sum: {}", bin_db(&l, sum));
        assert!(bin_db(&l, 440.0) < bin_db(&l, sum) - 20.0, "no carrier");
        assert!(bin_db(&l, 659.26) < bin_db(&l, sum) - 20.0, "no modulator");
        let play = |fm: f64| {
            let mut s = Play::new(&sine(&[(id::FM, fm)]));
            s.send(on(69));
            s.run(0.5).0
        };
        let (plain, fm) = (play(0.0), play(0.4));
        assert!(bin_db(&plain, 880.0) - bin_db(&plain, 440.0) < -50.0);
        assert!(
            bin_db(&fm, 880.0) - bin_db(&fm, 440.0) > -20.0,
            "FM harmonics: {}",
            bin_db(&fm, 880.0) - bin_db(&fm, 440.0)
        );
        assert!(bin_db(&fm, 1320.0) - bin_db(&fm, 440.0) > -30.0);
    }

    /// With phase reset every note starts alike; free-running, they do not.
    #[test]
    fn phase_reset_starts_every_note_the_same() {
        let starts = |reset: f64| -> Vec<Vec<f32>> {
            let mut s = Play::new(&sine(&[(id::PHASE_RESET, reset), (id::RELEASE, 5.0)]));
            (0..3)
                .map(|_| {
                    s.send(on(57));
                    let (l, _) = s.run(0.02);
                    s.send(off(57));
                    s.run(0.3);
                    l[..256].to_vec()
                })
                .collect()
        };
        let same = starts(1.0);
        assert!(same[0].iter().any(|v| v.abs() > 1e-4));
        assert_eq!(same[0], same[1]);
        assert_eq!(same[1], same[2]);
        let free = starts(0.0);
        assert_ne!(free[0], free[1]);
    }

    /// The sub's level follows the keys as set, and its wave can be a
    /// sine; pink noise falls off 3 dB an octave.
    #[test]
    fn the_sub_tracks_the_keys_and_the_noise_turns_pink() {
        let sub_db = |key: u8, extra: &[(u32, f64)]| {
            let mut set = sine(&[(id::OSC1_LEVEL, 0.0), (id::SUB, 1.0), (id::SUB_WAVE, 1.0)]);
            set.extend_from_slice(extra);
            let mut s = Play::new(&set);
            s.send(on(key));
            let (l, _) = s.run(0.5);
            let f = 440.0 * 2f64.powf((key as f64 - 69.0) / 12.0) / 2.0;
            (bin_db(&l, f), bin_db(&l, f * 3.0))
        };
        let (low, high) = (sub_db(48, &[]).0, sub_db(72, &[]).0);
        assert!((low - high).abs() < 1.0, "no tracking: {low} / {high}");
        let track = [(id::SUB_KEY, 1.0)];
        let (low, high) = (sub_db(48, &track).0, sub_db(72, &track).0);
        assert!(
            (low - high - 12.0).abs() < 1.5,
            "±6 dB an octave: {low} / {high}"
        );
        let (f, h3) = sub_db(60, &[]);
        assert!(
            h3 < f - 50.0,
            "a sine sub has no third harmonic: {f} / {h3}"
        );
        let noise = |colour: f64| {
            let mut s = Play::new(&[
                (id::CUTOFF, 20_000.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, 0.0),
                (id::OSC1_LEVEL, 0.0),
                (id::OSC2_LEVEL, 0.0),
                (id::NOISE, 1.0),
                (id::NOISE_COLOUR, colour),
            ]);
            s.send(on(60));
            let (l, _) = s.run(2.0);
            let band = |f: f64| {
                (0..16)
                    .map(|k| bin_db(&l, f * (1.0 + k as f64 * 0.01)))
                    .sum::<f64>()
                    / 16.0
            };
            band(4_000.0) - band(250.0)
        };
        let (white, pink) = (noise(0.0), noise(1.0));
        assert!(white.abs() < 3.0, "white is flat: {white}");
        assert!(
            (pink + 12.0).abs() < 3.0,
            "pink falls 3 dB an octave: {pink}"
        );
    }

    /// The ladder is a 24 dB low pass that resonates; the notch takes the
    /// cutoff out.
    #[test]
    fn the_ladder_and_notch_filters() {
        let level = |filter: f64, resonance: f64, f: f64| {
            let mut s = Play::new(&[
                (id::CUTOFF, 1_000.0),
                (id::ENV_AMOUNT, 0.0),
                (id::RESONANCE, resonance),
                (id::FILTER_TYPE, filter),
                (id::NOISE, 1.0),
                (id::OSC1_LEVEL, 0.0),
                (id::OSC2_LEVEL, 0.0),
            ]);
            s.send(on(60));
            let (l, _) = s.run(1.0);
            assert!(l.iter().all(|v| v.is_finite() && v.abs() < 4.0));
            (0..9).map(|k| bin_db(&l, f + k as f64 * 3.0)).sum::<f64>() / 9.0
        };
        let ladder = filter::LADDER as f64;
        let slope = level(ladder, 0.0, 4_000.0) - level(ladder, 0.0, 200.0);
        assert!(slope < -30.0, "24 dB an octave: {slope:.1}");
        let peak = level(ladder, 0.85, 1_000.0) - level(ladder, 0.0, 1_000.0);
        assert!(peak > 6.0, "resonance: {peak:.1}");
        let notch = filter::NOTCH as f64;
        let dip = level(notch, 0.5, 1_000.0) - level(notch, 0.5, 250.0);
        assert!(dip < -15.0, "notch: {dip:.1}");
        assert!((level(notch, 0.5, 4_000.0) - level(notch, 0.5, 250.0)).abs() < 4.0);
    }

    /// Filter FM by a sine at the note's pitch spreads a sine into
    /// harmonics.
    #[test]
    fn filter_fm_spreads_the_spectrum() {
        let play = |ffm: f64| {
            let mut s = Play::new(&sine(&[
                (id::CUTOFF, 2_000.0),
                (id::RESONANCE, 0.5),
                (id::FILTER_FM, ffm),
            ]));
            s.send(on(69));
            s.run(0.5).0
        };
        let (plain, fm) = (play(0.0), play(1.0));
        let h2 = |x: &[f32]| bin_db(x, 880.0) - bin_db(x, 440.0);
        assert!(h2(&plain) < -50.0, "{}", h2(&plain));
        assert!(h2(&fm) > -25.0, "filter FM: {}", h2(&fm));
        assert!(fm.iter().all(|v| v.is_finite()));
    }

    /// The delay repeats a short note after its time, the reverb leaves a
    /// tail, the chorus widens, and the processor stays awake for them.
    #[test]
    fn the_effects_echo_tail_and_widen() {
        let play = |extra: &[(u32, f64)], seconds: f64| {
            let mut set = sine(&[(id::ATTACK, 1.0), (id::RELEASE, 5.0)]);
            set.extend_from_slice(extra);
            let mut s = Play::new(&set);
            s.send(on(69));
            let (mut l, mut r) = s.run(0.1);
            s.send(off(69));
            let (l2, r2) = s.run(seconds);
            l.extend(l2);
            r.extend(r2);
            (s, l, r)
        };
        let rms = |x: &[f32]| {
            20.0 * (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64)
                .sqrt()
                .max(1e-12)
                .log10()
        };
        fn at(x: &[f32], a: f64, b: f64) -> &[f32] {
            &x[(a * SR) as usize..(b * SR) as usize]
        }
        // Delay: 250 ms, no feedback → one echo from 0.25 s to 0.35 s.
        let (s, l, _) = play(
            &[
                (id::DELAY_MIX, 1.0),
                (id::DELAY_SYNC, 0.0),
                (id::DELAY_TIME, 250.0),
                (id::DELAY_FEEDBACK, 0.0),
            ],
            1.0,
        );
        assert!(
            rms(at(&l, 0.15, 0.24)) < -70.0,
            "{}",
            rms(at(&l, 0.15, 0.24))
        );
        assert!(
            rms(at(&l, 0.27, 0.34)) > -30.0,
            "{}",
            rms(at(&l, 0.27, 0.34))
        );
        assert!(rms(at(&l, 0.6, 1.0)) < -70.0);
        assert!(!s.p.voices.iter().any(|v| v.sounding()));
        // Ping-pong: the second repeat on the other side.
        let (_, l, r) = play(
            &[
                (id::DELAY_MIX, 1.0),
                (id::DELAY_SYNC, 0.0),
                (id::DELAY_TIME, 250.0),
                (id::DELAY_FEEDBACK, 0.8),
                (id::DELAY_PINGPONG, 1.0),
            ],
            1.0,
        );
        assert!(rms(at(&l, 0.27, 0.34)) > rms(at(&r, 0.27, 0.34)) + 20.0);
        assert!(rms(at(&r, 0.52, 0.59)) > rms(at(&l, 0.52, 0.59)) + 20.0);
        // Reverb: a tail a second on, decaying; none without.
        let (_, dry, _) = play(&[], 1.5);
        assert!(rms(at(&dry, 0.5, 1.0)) < -80.0);
        let (s, wet, wet_r) = play(&[(id::REVERB_MIX, 1.0), (id::REVERB_DECAY, 3_000.0)], 1.5);
        let (early, late) = (rms(at(&wet, 0.2, 0.5)), rms(at(&wet, 1.0, 1.5)));
        assert!(early > -40.0, "a tail: {early}");
        assert!(
            late < early - 3.0 && late > -70.0,
            "decaying: {early} → {late}"
        );
        let diff: f64 = at(&wet, 0.2, 0.5)
            .iter()
            .zip(at(&wet_r, 0.2, 0.5))
            .map(|(a, b)| f64::from(a - b).abs())
            .sum();
        assert!(diff > 1.0, "a stereo tail");
        // Still awake while the tail runs, asleep once it is gone.
        assert!(s.p.fx.ringing());
        // Chorus: a mono patch comes out with sides that differ, at
        // about the same level.
        let (_, cl, cr) = play(&[(id::CHORUS_MIX, 1.0)], 0.1);
        let side: f64 = cl
            .iter()
            .zip(&cr)
            .map(|(a, b)| f64::from(a - b).abs())
            .sum();
        assert!(side > 1.0, "the chorus widens");
        assert!((rms(&cl[..4000]) - rms(&dry[..4000])).abs() < 6.0);
        let (_, el, er) = play(&[(id::CHORUS_MIX, 1.0), (id::CHORUS_MODE, 1.0)], 0.1);
        assert!(el.iter().chain(&er).all(|v| v.is_finite()));
    }

    /// The processor sleeps only when the effects have nothing left.
    #[test]
    fn the_effects_keep_the_processor_awake() {
        let mut s = Play::new(&sine(&[
            (id::RELEASE, 5.0),
            (id::DELAY_MIX, 1.0),
            (id::DELAY_SYNC, 0.0),
            (id::DELAY_TIME, 500.0),
            (id::DELAY_FEEDBACK, 0.0),
        ]));
        s.send(on(69));
        s.run(0.05);
        s.send(off(69));
        s.run(0.1);
        assert!(!s.p.voices.iter().any(|v| v.sounding()));
        let status = |s: &mut Play| {
            let ctx = PluginProcessContext {
                transport: &s.transport,
                param_events: &[],
                harmony: &crate::NO_HARMONY,
                param_mods: &[],
                note_mods: &[],
            };
            let mut io = NodeIo {
                frames: 256,
                audio_in: &[],
                audio_out: &mut s.out,
                events_in: &s.events,
                events_out: &mut [],
            };
            s.p.process(&ctx, &mut io)
        };
        assert_eq!(status(&mut s), ProcessStatus::Continue, "the echo is due");
        s.run(2.0);
        assert_eq!(status(&mut s), ProcessStatus::Sleep);
    }

    /// With Quietest, a soft note goes before an old loud one.
    #[test]
    fn voices_are_stolen_quietest_or_oldest_first() {
        let keys = |steal: f64| -> Vec<u8> {
            let mut s = Play::new(&sine(&[
                (id::POLYPHONY, 2.0),
                (id::STEAL, steal),
                (id::VELOCITY_AMP, 1.0),
                (id::ATTACK, 0.5),
            ]));
            s.send(MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 127,
            });
            s.run(0.1);
            s.send(MidiEvent::NoteOn {
                channel: 0,
                key: 64,
                velocity: 20,
            });
            s.run(0.1);
            s.send(MidiEvent::NoteOn {
                channel: 0,
                key: 67,
                velocity: 100,
            });
            s.run(0.1);
            let mut k: Vec<u8> =
                s.p.voices
                    .iter()
                    .filter(|v| v.sounding())
                    .map(|v| v.key)
                    .collect();
            k.sort_unstable();
            k
        };
        assert_eq!(keys(0.0), vec![64, 67], "oldest");
        assert_eq!(keys(1.0), vec![60, 67], "quietest");
    }

    /// In Legato glide mode a note after a gap starts on pitch; one played
    /// over a held key slides.
    #[test]
    fn legato_glide_only_slides_between_held_keys() {
        let start = |legato: f64, overlap: bool| {
            let mut s = Play::new(&sine(&[
                (id::GLIDE, 400.0),
                (id::GLIDE_MODE, legato),
                (id::RELEASE, 5.0),
            ]));
            s.send(on(45));
            s.run(0.2);
            if !overlap {
                s.send(off(45));
                s.run(0.2);
            }
            s.send(on(69));
            let (l, _) = s.run(0.1);
            // Only the new note's start (an octave lower would be 220).
            peak_hz(&l, 150.0, 600.0)
        };

        assert!(
            (start(1.0, false) / 440.0 - 1.0).abs() < 0.03,
            "Legato after a gap: {}",
            start(1.0, false)
        );
        assert!(
            start(1.0, true) < 330.0,
            "Legato over a held key: {}",
            start(1.0, true)
        );
    }

    /// Drift moves each voice a few cents, no more, and keeps changing.
    #[test]
    fn analogue_drift_moves_the_voices_a_little() {
        let play = |drift: f64| {
            let mut s = Play::new(&sine(&[(id::DRIFT, drift)]));
            s.send(on(69));
            s.run(2.0).0
        };
        let (still, drifting) = (play(0.0), play(1.0));
        assert_ne!(still, drifting);
        let got = peak_hz(&drifting, 300.0, 600.0);
        assert!((got / 440.0 - 1.0).abs() < 0.02, "{got}");
        // The spectrum is smeared a touch, within a quarter tone.
        let near = |x: &[f32], c: f64| bin_db(x, 440.0 * 2f64.powf(c / 1200.0));
        assert!(near(&drifting, 15.0) > near(&still, 15.0) + 3.0);
        assert!(near(&drifting, 50.0) < -45.0, "{}", near(&drifting, 50.0));
    }

    /// Several first-wave patches render exactly as the first Synth
    /// rendered them (checksums taken before the second wave): projects
    /// from then sound as they did.
    #[test]
    fn earlier_patches_sound_as_they_did() {
        const WANT: [u64; 5] = [
            0x3be1_e48a_4671_4746,
            0xeceb_0694_d5fa_1ba7,
            0x2a74_5a0d_2fb2_2c0c,
            0xc070_4dc5_1afa_4a3a,
            0x5c98_b7e7_9343_9039,
        ];
        let patches: [&[(u32, f64)]; 5] = [
            &[],
            &[
                (19, 5.0),
                (20, 30.0),
                (17, 0.5),
                (18, 0.2),
                (22, 1.0),
                (23, 0.4),
                (26, 1.0),
            ],
            &[
                (39, 2.0),
                (40, 80.0),
                (9, 1.0),
                (38, 0.3),
                (36, 1.5),
                (35, 20.0),
            ],
            &[
                (22, 2.0),
                (2, 0.9),
                (44, 3.0),
                (45, 0.7),
                (46, 16.0),
                (47, -0.5),
                (24, 0.7),
            ],
            &[
                (22, 3.0),
                (13, 2.0),
                (15, 7.0),
                (14, -1.0),
                (21, 1.0),
                (19, 7.0),
            ],
        ];
        for (n, set) in patches.iter().enumerate() {
            let mut s = Play::new(set);
            s.send(MidiEvent::ControlChange {
                channel: 0,
                controller: 1,
                value: 90,
            });
            s.send(on(48));
            s.send(on(55));
            let (mut l, mut r) = s.run(0.3);
            s.send(MidiEvent::PitchBend {
                channel: 0,
                value: 10000,
            });
            s.send(on(64));
            let (l2, r2) = s.run(0.3);
            s.send(off(48));
            s.send(off(64));
            let (l3, r3) = s.run(0.4);
            l.extend(l2);
            l.extend(l3);
            r.extend(r2);
            r.extend(r3);
            let mut h: u64 = 1469598103934665603;
            for v in l.iter().chain(&r) {
                h ^= u64::from(v.to_bits());
                h = h.wrapping_mul(1099511628211);
            }
            assert_eq!(h, WANT[n], "patch {n}: {h:016x}");
        }
    }
}
