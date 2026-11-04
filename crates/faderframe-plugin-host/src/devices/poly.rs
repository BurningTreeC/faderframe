//! What the Wavetable, FM and Granular instruments share, so they play
//! alike and like the Synth: the block of parameters each of them starts
//! with (ids `0..`[`COMMON`]: volume, the amplifier envelope, the filter —
//! the Synth's six types or none — and its envelope, a mod envelope, LFO 1
//! (one for all voices) and LFO 2 (per voice), voicing, the wheels, an
//! eight-slot modulation matrix and the Synth's effects), MIDI as the
//! Synth takes it (pedal, bend over its range or RPN 0, MPE zones,
//! pressure, CC 74, per-note expressions), voices (poly with stealing,
//! mono, legato, glide), the matrix's sources and per-note modulation of
//! parameters (a voice's own values, [`NoteMods`]).
//!
//! Each instrument appends its own parameters from [`COMMON`] on (`id ==
//! position`), and its own matrix targets after [`COMMON_TARGETS`].

use super::synth::{
    self, ChannelState, Env, EnvSettings, Fx, FxSettings, Stage, Svf, filter_coefs, ladder,
    spread_random,
};
use super::{on_off, param, pick, stepped};
use crate::dsp::lfo::{DIVISIONS, Lfo, Shape, division_name};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{NoteParamMod, ParamValues, ParameterInfo, ParameterUnit};
use faderframe_core::ParameterId;
use faderframe_midi::tuning::TuningTable;
use faderframe_midi::{MidiBuffer, MidiEvent, NoteExpressionKind};
use faderframe_transport::TransportInfo;
use std::f32::consts::PI;

/// Stable ids of the shared block (`id == position`).
pub mod id {
    pub const VOLUME: u32 = 0;
    pub const VELOCITY_AMP: u32 = 1;
    pub const ATTACK: u32 = 2;
    pub const DECAY: u32 = 3;
    pub const SUSTAIN: u32 = 4;
    pub const RELEASE: u32 = 5;
    /// Off, or one of the Synth's six filters.
    pub const FILTER_TYPE: u32 = 6;
    pub const CUTOFF: u32 = 7;
    pub const RESONANCE: u32 = 8;
    pub const DRIVE: u32 = 9;
    pub const KEY_TRACK: u32 = 10;
    pub const ENV_AMOUNT: u32 = 11;
    pub const VELOCITY_CUTOFF: u32 = 12;
    pub const F_ATTACK: u32 = 13;
    pub const F_DECAY: u32 = 14;
    pub const F_SUSTAIN: u32 = 15;
    pub const F_RELEASE: u32 = 16;
    pub const M_ATTACK: u32 = 17;
    pub const M_DECAY: u32 = 18;
    pub const M_SUSTAIN: u32 = 19;
    pub const M_RELEASE: u32 = 20;
    /// LFO 1: one for all voices, free or locked to the song.
    pub const LFO_RATE: u32 = 21;
    pub const LFO_SYNC: u32 = 22;
    pub const LFO_DIVISION: u32 = 23;
    pub const LFO_SHAPE: u32 = 24;
    /// LFO 2: per voice, restarted per note if wanted, faded in.
    pub const LFO2_RATE: u32 = 25;
    pub const LFO2_SYNC: u32 = 26;
    pub const LFO2_DIVISION: u32 = 27;
    pub const LFO2_SHAPE: u32 = 28;
    pub const LFO2_RETRIG: u32 = 29;
    pub const LFO2_FADE: u32 = 30;
    /// Poly, Mono, Legato.
    pub const VOICE_MODE: u32 = 31;
    pub const POLYPHONY: u32 = 32;
    pub const STEAL: u32 = 33;
    pub const GLIDE: u32 = 34;
    pub const GLIDE_MODE: u32 = 35;
    pub const BEND_RANGE: u32 = 36;
    /// Mod-wheel slot `k`: what it drives and how much.
    pub const fn wheel_target(k: usize) -> u32 {
        37 + 2 * k as u32
    }
    pub const fn wheel_amount(k: usize) -> u32 {
        38 + 2 * k as u32
    }
    /// Matrix slot `k`: source, target, amount.
    pub const fn mod_source(k: usize) -> u32 {
        43 + 3 * k as u32
    }
    pub const fn mod_target(k: usize) -> u32 {
        44 + 3 * k as u32
    }
    pub const fn mod_amount(k: usize) -> u32 {
        45 + 3 * k as u32
    }
    /// The Synth's effects after the voices.
    pub const CHORUS_MIX: u32 = 67;
    pub const CHORUS_RATE: u32 = 68;
    pub const CHORUS_DEPTH: u32 = 69;
    pub const CHORUS_MODE: u32 = 70;
    pub const DELAY_MIX: u32 = 71;
    pub const DELAY_TIME: u32 = 72;
    pub const DELAY_SYNC: u32 = 73;
    pub const DELAY_DIVISION: u32 = 74;
    pub const DELAY_FEEDBACK: u32 = 75;
    pub const DELAY_PINGPONG: u32 = 76;
    pub const DELAY_TONE: u32 = 77;
    pub const REVERB_MIX: u32 = 78;
    pub const REVERB_SIZE: u32 = 79;
    pub const REVERB_DECAY: u32 = 80;
    pub const REVERB_TONE: u32 = 81;
}

/// The first id after the shared block: where an instrument's own start.
pub const COMMON: u32 = 82;

/// Mod-wheel slots.
pub const WHEEL_SLOTS: usize = 3;
/// Matrix slots.
pub const MOD_SLOTS: usize = 8;
/// Most voices.
pub const MAX_VOICES: usize = 32;
/// Most matrix targets an instrument has.
pub const MAX_TARGETS: usize = 32;

/// Targets every instrument has (its own follow). At a source of 1 and
/// amount 100 %: vibrato a semitone deep, pitch +12 st, cutoff +4
/// octaves, resonance +100 %, filter envelope +6 octaves, drive +100 %,
/// volume brought in (negative: taken out), pan fully right, LFO 2 two
/// octaves faster.
pub const COMMON_TARGETS: [&str; 10] = [
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
];

/// [`COMMON_TARGETS`] by name.
pub mod target {
    pub const OFF: usize = 0;
    pub const VIBRATO: usize = 1;
    pub const PITCH: usize = 2;
    pub const CUTOFF: usize = 3;
    pub const RESONANCE: usize = 4;
    pub const FILTER_ENV: usize = 5;
    pub const DRIVE: usize = 6;
    pub const VOLUME: usize = 7;
    pub const PAN: usize = 8;
    pub const LFO2_RATE: usize = 9;
    /// An instrument's first own target.
    pub const OWN: usize = 10;
}

/// The matrix's sources: bipolar (−1…1) the LFOs, the random value a note
/// draws, the timbre (CC 74 and the brightness expression round their
/// middle) and the pitch wheel; unipolar (0…1) the rest.
pub const SOURCES: [&str; 13] = [
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
    "Timbre",
    "Pitch Bend",
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
    pub const TIMBRE: usize = 11;
    pub const BEND: usize = 12;
}

/// Published values every instrument has (as the Synth's): voices
/// sounding, the newest voice's key, its amplifier, filter and mod
/// envelopes (0…1) and cutoff (Hz), the output's peak, LFO 1 and the
/// newest voice's LFO 2 (−1…1). An instrument's own follow from
/// [`COMMON_VALUES`].
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
pub const COMMON_VALUES: usize = 9;

pub const FILTERS: [&str; 7] = ["Off", "LP 12", "LP 24", "Band", "High", "Ladder", "Notch"];
pub const MODES: [&str; 3] = ["Poly", "Mono", "Legato"];
pub const SHAPES: [&str; 6] = ["Sine", "Triangle", "Saw", "Square", "S&H", "Drift"];
pub const STEALS: [&str; 2] = ["Oldest", "Quietest"];
pub const GLIDE_MODES: [&str; 2] = ["Always", "Legato"];
pub const CHORUS_MODES: [&str; 2] = ["Chorus", "Ensemble"];

/// What an instrument starts its shared block from.
#[derive(Clone, Copy, Debug)]
pub struct Defaults {
    pub volume: f64,
    pub filter: f64,
    pub cutoff: f64,
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
}

fn ranged(id: u32, name: &str, min: f64, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, min, max, default, ParameterUnit::None)
    }
}

/// The shared block, its matrix and wheels choosing among `targets`.
pub fn parameters(targets: usize, d: &Defaults) -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let ms = |id, name: &str, min, max, default| param(id, name, min, max, default, Milliseconds);
    let mut p = vec![
        param(id::VOLUME, "Volume", -48.0, 6.0, d.volume, Decibels),
        param(
            id::VELOCITY_AMP,
            "Velocity to Level",
            0.0,
            1.0,
            0.8,
            Percent,
        ),
        ms(id::ATTACK, "Attack", 0.5, 10_000.0, d.attack),
        ms(id::DECAY, "Decay", 1.0, 10_000.0, d.decay),
        param(id::SUSTAIN, "Sustain", 0.0, 1.0, d.sustain, Percent),
        ms(id::RELEASE, "Release", 5.0, 10_000.0, d.release),
        stepped(
            id::FILTER_TYPE,
            "Filter Type",
            (FILTERS.len() - 1) as f64,
            d.filter,
        ),
        param(id::CUTOFF, "Cutoff", 20.0, 20_000.0, d.cutoff, Hertz),
        param(id::RESONANCE, "Resonance", 0.0, 1.0, 0.15, Percent),
        param(id::DRIVE, "Drive", 0.0, 1.0, 0.0, Percent),
        param(id::KEY_TRACK, "Key Tracking", 0.0, 1.0, 0.5, Percent),
        param(id::ENV_AMOUNT, "Env Amount", -1.0, 1.0, 0.0, Percent),
        param(
            id::VELOCITY_CUTOFF,
            "Velocity to Cutoff",
            0.0,
            1.0,
            0.5,
            Percent,
        ),
        ms(id::F_ATTACK, "Filter Attack", 0.5, 10_000.0, 5.0),
        ms(id::F_DECAY, "Filter Decay", 1.0, 10_000.0, 400.0),
        param(id::F_SUSTAIN, "Filter Sustain", 0.0, 1.0, 0.3, Percent),
        ms(id::F_RELEASE, "Filter Release", 5.0, 10_000.0, 400.0),
        ms(id::M_ATTACK, "Mod Attack", 0.5, 10_000.0, 5.0),
        ms(id::M_DECAY, "Mod Decay", 1.0, 10_000.0, 600.0),
        param(id::M_SUSTAIN, "Mod Sustain", 0.0, 1.0, 0.0, Percent),
        ms(id::M_RELEASE, "Mod Release", 5.0, 10_000.0, 400.0),
        param(id::LFO_RATE, "LFO Rate", 0.05, 20.0, 2.0, Hertz),
        stepped(id::LFO_SYNC, "LFO Sync", 1.0, 0.0),
        stepped(
            id::LFO_DIVISION,
            "LFO Division",
            (DIVISIONS.len() - 1) as f64,
            11.0,
        ),
        stepped(id::LFO_SHAPE, "LFO Shape", (SHAPES.len() - 1) as f64, 0.0),
        param(id::LFO2_RATE, "LFO 2 Rate", 0.05, 20.0, 5.0, Hertz),
        stepped(id::LFO2_SYNC, "LFO 2 Sync", 1.0, 0.0),
        stepped(
            id::LFO2_DIVISION,
            "LFO 2 Division",
            (DIVISIONS.len() - 1) as f64,
            8.0,
        ),
        stepped(
            id::LFO2_SHAPE,
            "LFO 2 Shape",
            (SHAPES.len() - 1) as f64,
            0.0,
        ),
        stepped(id::LFO2_RETRIG, "LFO 2 Retrigger", 1.0, 1.0),
        ms(id::LFO2_FADE, "LFO 2 Fade In", 0.0, 5_000.0, 0.0),
        stepped(id::VOICE_MODE, "Voice Mode", 2.0, 0.0),
        ranged(id::POLYPHONY, "Voices", 1.0, MAX_VOICES as f64, 16.0),
        stepped(id::STEAL, "Voice Stealing", 1.0, 0.0),
        ms(id::GLIDE, "Glide", 0.0, 2_000.0, 0.0),
        stepped(id::GLIDE_MODE, "Glide Mode", 1.0, 0.0),
        ranged(id::BEND_RANGE, "Pitch Bend Range", 1.0, 24.0, 2.0),
    ];
    const WHEEL_NAMES: [(&str, &str); WHEEL_SLOTS] = [
        ("Mod Wheel 1", "Mod Wheel 1 Amount"),
        ("Mod Wheel 2", "Mod Wheel 2 Amount"),
        ("Mod Wheel 3", "Mod Wheel 3 Amount"),
    ];
    for (k, (t, a)) in WHEEL_NAMES.iter().enumerate() {
        // The first slot a vibrato, as on the Synth.
        let (tv, av) = if k == 0 {
            (target::VIBRATO as f64, 0.5)
        } else {
            (0.0, 0.0)
        };
        p.push(stepped(id::wheel_target(k), t, (targets - 1) as f64, tv));
        p.push(param(id::wheel_amount(k), a, -1.0, 1.0, av, Percent));
    }
    const SLOT_NAMES: [(&str, &str, &str); MOD_SLOTS] = [
        ("Mod 1 Source", "Mod 1 Target", "Mod 1 Amount"),
        ("Mod 2 Source", "Mod 2 Target", "Mod 2 Amount"),
        ("Mod 3 Source", "Mod 3 Target", "Mod 3 Amount"),
        ("Mod 4 Source", "Mod 4 Target", "Mod 4 Amount"),
        ("Mod 5 Source", "Mod 5 Target", "Mod 5 Amount"),
        ("Mod 6 Source", "Mod 6 Target", "Mod 6 Amount"),
        ("Mod 7 Source", "Mod 7 Target", "Mod 7 Amount"),
        ("Mod 8 Source", "Mod 8 Target", "Mod 8 Amount"),
    ];
    for (k, (s, t, a)) in SLOT_NAMES.iter().enumerate() {
        p.push(stepped(
            id::mod_source(k),
            s,
            (SOURCES.len() - 1) as f64,
            0.0,
        ));
        p.push(stepped(id::mod_target(k), t, (targets - 1) as f64, 0.0));
        p.push(param(id::mod_amount(k), a, -1.0, 1.0, 0.0, Percent));
    }
    p.extend([
        param(id::CHORUS_MIX, "Chorus Mix", 0.0, 1.0, 0.0, Percent),
        param(id::CHORUS_RATE, "Chorus Rate", 0.05, 5.0, 0.8, Hertz),
        param(id::CHORUS_DEPTH, "Chorus Depth", 0.0, 1.0, 0.5, Percent),
        stepped(id::CHORUS_MODE, "Chorus Mode", 1.0, 0.0),
        param(id::DELAY_MIX, "Delay Mix", 0.0, 1.0, 0.0, Percent),
        ms(id::DELAY_TIME, "Delay Time", 1.0, 2_000.0, 375.0),
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
        ms(id::REVERB_DECAY, "Reverb Decay", 100.0, 10_000.0, 2_000.0),
        param(
            id::REVERB_TONE,
            "Reverb Tone",
            500.0,
            20_000.0,
            5_000.0,
            Hertz,
        ),
    ]);
    debug_assert_eq!(p.len(), COMMON as usize);
    p
}

/// A signed percentage ("+25 %", "0 %").
pub(crate) fn signed_percent(v: f64) -> String {
    format!("{:+.0} %", v * 100.0).replace("+0 %", "0 %")
}

/// The shared block's values as shown, its matrix naming `targets`.
pub fn format(pid: ParameterId, v: f64, targets: &[&str]) -> Option<String> {
    let wheels = id::wheel_target(0)..id::mod_source(0);
    let matrix = id::mod_source(0)..id::CHORUS_MIX;
    Some(match pid.0 {
        id::FILTER_TYPE => pick(&FILTERS, v),
        id::VOICE_MODE => pick(&MODES, v),
        id::LFO_SHAPE | id::LFO2_SHAPE => pick(&SHAPES, v),
        id::STEAL => pick(&STEALS, v),
        id::GLIDE_MODE => pick(&GLIDE_MODES, v),
        id::CHORUS_MODE => pick(&CHORUS_MODES, v),
        id::LFO_SYNC | id::LFO2_SYNC | id::LFO2_RETRIG | id::DELAY_SYNC | id::DELAY_PINGPONG => {
            on_off(v)
        }
        id::LFO_DIVISION | id::LFO2_DIVISION | id::DELAY_DIVISION => {
            division_name(v.round().max(0.0) as usize).into()
        }
        id::POLYPHONY => format!("{:.0}", v.round()),
        id::BEND_RANGE => format!("±{:.0} st", v.round()),
        id::ENV_AMOUNT => signed_percent(v),
        id::GLIDE if v < 0.5 => "Off".into(),
        id::LFO2_FADE if v < 0.5 => "Off".into(),
        id::REVERB_DECAY => format!("{:.1} s", v / 1000.0),
        x if wheels.contains(&x) => {
            if (x - wheels.start) % 2 == 0 {
                pick(targets, v)
            } else {
                signed_percent(v)
            }
        }
        x if matrix.contains(&x) => match (x - matrix.start) % 3 {
            0 => pick(&SOURCES, v),
            1 => pick(targets, v),
            _ => signed_percent(v),
        },
        _ => return None,
    })
}

/// Whether a shared parameter is one a voice reads for itself (per-note
/// modulation can move it): not LFO 1 (one for all), the voicing, the
/// effects, nor the stepped choices.
pub fn per_voice(pid: ParameterId) -> bool {
    !matches!(
        pid.0,
        id::LFO_RATE
            | id::LFO_SYNC
            | id::LFO_DIVISION
            | id::LFO_SHAPE
            | id::VOICE_MODE
            | id::POLYPHONY
            | id::STEAL
            | id::GLIDE
            | id::GLIDE_MODE
            | id::BEND_RANGE
    ) && pid.0 < id::CHORUS_MIX
}

/// What a control does (the editors' tooltips of the shared block).
pub fn tip(pid: ParameterId) -> Option<&'static str> {
    let wheels = id::wheel_target(0)..id::mod_source(0);
    let matrix = id::mod_source(0)..id::CHORUS_MIX;
    Some(match pid.0 {
        id::FILTER_TYPE => "Off, or one of the Synth's filters",
        id::KEY_TRACK => "The cutoff follows the keys",
        id::VELOCITY_CUTOFF => "How much velocity opens the filter envelope",
        id::VELOCITY_AMP => "How much velocity sets the level",
        id::VOICE_MODE => "Poly, Mono (always retriggers), Legato (slides between held keys)",
        id::GLIDE => "Slide from the last note",
        id::GLIDE_MODE => "Glide on every note, or only onto a note played over a held key",
        id::STEAL => "Which voice a new note takes when all are busy: the oldest, or the quietest",
        id::BEND_RANGE => {
            "Semitones a full pitch-wheel throw bends (a controller setting it with RPN 0 wins on its channel)"
        }
        id::LFO2_RETRIG => "LFO 2 restarts with every note",
        id::LFO2_FADE => "LFO 2 comes in over this time after the note",
        id::CHORUS_MODE => "Chorus: one voice a side; Ensemble: three with a fast shimmer",
        id::DELAY_PINGPONG => "Repeats alternate between the sides",
        id::DELAY_TONE => "The repeats lose highs above this",
        id::REVERB_DECAY => "How long the tail takes to fade by 60 dB",
        x if wheels.contains(&x) => {
            if (x - wheels.start) % 2 == 0 {
                "What the mod wheel drives in this slot"
            } else {
                "How much (at full wheel): negative takes away; for Volume, positive brings it in as the wheel goes up"
            }
        }
        x if matrix.contains(&x) => match (x - matrix.start) % 3 {
            0 => "What drives this slot",
            1 => "What it moves",
            _ => "How much: negative turns it the other way",
        },
        _ => return None,
    })
}

/// The shared block's settings for a block (or a voice), per sample.
#[derive(Clone, Copy)]
pub(crate) struct Common {
    pub gain: f32,
    pub vel_amp: f32,
    pub amp: EnvSettings,
    pub filt: EnvSettings,
    pub menv: EnvSettings,
    /// The Synth's filter type, or none.
    pub filter: Option<usize>,
    pub cutoff: f32,
    pub resonance: f32,
    /// The state-variable damping.
    pub k: f32,
    pub drive: f32,
    pub key_track: f32,
    pub env_octaves: f32,
    pub vel_cutoff: f32,
    pub lfo2_shape: Shape,
    pub lfo2_retrig: bool,
    /// LFO 2's fade-in per control tick (1: at once).
    pub lfo2_fade_step: f32,
    pub mode: usize,
    pub polyphony: usize,
    pub steal: usize,
    pub glide_coef: f32,
    pub glide_legato: bool,
    pub bend_range: f32,
    pub wheel: [(usize, f32); WHEEL_SLOTS],
    pub matrix: [(usize, usize, f32); MOD_SLOTS],
}

impl Common {
    /// From parameter values (`get` by id) at `sr`, with `targets` targets.
    pub fn read(get: &dyn Fn(u32) -> f32, sr: f32, targets: usize) -> Self {
        let ms = |v: f32| (v * 0.001 * sr).max(1.0);
        // Exponential segments reach ~-60 dB after the given time.
        let coef = |t: f32| (-6.9 / ms(t)).exp();
        let env = |a, d, s, r| EnvSettings {
            attack_step: 1.0 / ms(get(a)),
            decay_coef: coef(get(d)),
            sustain: get(s).clamp(0.0, 1.0),
            release_coef: coef(get(r)),
        };
        let choice = |pid: u32, n: usize| (get(pid).round().max(0.0) as usize).min(n - 1);
        let resonance = get(id::RESONANCE).clamp(0.0, 1.0);
        let glide = get(id::GLIDE);
        let fade = get(id::LFO2_FADE);
        let filter = choice(id::FILTER_TYPE, FILTERS.len());
        Common {
            gain: faderframe_core::db_to_gain(get(id::VOLUME)),
            vel_amp: get(id::VELOCITY_AMP).clamp(0.0, 1.0),
            amp: env(id::ATTACK, id::DECAY, id::SUSTAIN, id::RELEASE),
            filt: env(id::F_ATTACK, id::F_DECAY, id::F_SUSTAIN, id::F_RELEASE),
            menv: env(id::M_ATTACK, id::M_DECAY, id::M_SUSTAIN, id::M_RELEASE),
            filter: filter.checked_sub(1),
            cutoff: get(id::CUTOFF),
            resonance,
            k: 2.0 - 1.9 * resonance,
            drive: get(id::DRIVE).clamp(0.0, 1.0),
            key_track: get(id::KEY_TRACK),
            env_octaves: get(id::ENV_AMOUNT) * 6.0,
            vel_cutoff: get(id::VELOCITY_CUTOFF).clamp(0.0, 1.0),
            lfo2_shape: Shape::from_index(choice(id::LFO2_SHAPE, SHAPES.len())),
            lfo2_retrig: get(id::LFO2_RETRIG) >= 0.5,
            lfo2_fade_step: if fade < 0.5 {
                1.0
            } else {
                synth::CONTROL_INTERVAL as f32 / ms(fade)
            },
            mode: choice(id::VOICE_MODE, MODES.len()),
            polyphony: (get(id::POLYPHONY).round() as usize).clamp(1, MAX_VOICES),
            steal: choice(id::STEAL, STEALS.len()),
            glide_coef: if glide < 0.5 {
                0.0
            } else {
                (-6.9 / ms(glide)).exp()
            },
            glide_legato: get(id::GLIDE_MODE) >= 0.5,
            bend_range: get(id::BEND_RANGE).round().clamp(1.0, 24.0),
            wheel: std::array::from_fn(|k| {
                (
                    choice(id::wheel_target(k), targets),
                    get(id::wheel_amount(k)).clamp(-1.0, 1.0),
                )
            }),
            matrix: std::array::from_fn(|k| {
                (
                    choice(id::mod_source(k), SOURCES.len()),
                    choice(id::mod_target(k), targets),
                    get(id::mod_amount(k)).clamp(-1.0, 1.0),
                )
            }),
        }
    }
}

/// The effects' settings from the shared block.
pub(crate) fn fx_settings(get: &dyn Fn(u32) -> f32, sr: f64, tempo: f64) -> FxSettings {
    let division =
        |pid: u32| DIVISIONS[(get(pid).round().max(0.0) as usize).min(DIVISIONS.len() - 1)].1;
    let seconds = if get(id::DELAY_SYNC) >= 0.5 {
        division(id::DELAY_DIVISION) * 60.0 / tempo
    } else {
        f64::from(get(id::DELAY_TIME)) * 0.001
    };
    FxSettings {
        chorus_mix: get(id::CHORUS_MIX).clamp(0.0, 1.0),
        chorus_rate: f64::from(get(id::CHORUS_RATE)),
        chorus_depth: f64::from(get(id::CHORUS_DEPTH).clamp(0.0, 1.0)),
        ensemble: get(id::CHORUS_MODE) >= 0.5,
        delay_mix: get(id::DELAY_MIX).clamp(0.0, 1.0),
        delay_samples: (seconds * sr).clamp(1.0, 2.0 * sr),
        feedback: f64::from(get(id::DELAY_FEEDBACK).clamp(0.0, 0.95)),
        pingpong: get(id::DELAY_PINGPONG) >= 0.5,
        delay_tone: f64::from(get(id::DELAY_TONE)),
        reverb_mix: get(id::REVERB_MIX).clamp(0.0, 1.0),
        size: f64::from(get(id::REVERB_SIZE).clamp(0.0, 1.0)),
        decay: f64::from(get(id::REVERB_DECAY)).max(100.0) * 0.001,
        reverb_tone: f64::from(get(id::REVERB_TONE)),
    }
}

/// A note's expressions (MPE dimensions and per-note expressions), as the
/// Synth keeps them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Expr {
    /// Semitones.
    pub tuning: f32,
    pub volume: f32,
    pub pan: f32,
    pub vibrato: f32,
    pub expression: f32,
    pub brightness: f32,
    pub pressure: f32,
}

impl Expr {
    pub const REST: Expr = Expr {
        tuning: 0.0,
        volume: 1.0,
        pan: 0.0,
        vibrato: 0.0,
        expression: 1.0,
        brightness: 0.5,
        pressure: 0.0,
    };

    pub fn set(&mut self, kind: NoteExpressionKind, v: f32) {
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

/// Most parameters one voice has modulation of its own on.
pub const NOTE_MODS: usize = 8;

/// A voice's own modulation of parameters (the engine's per-note
/// modulation): per parameter a share of its range, on top of what the
/// whole device gets. A voice with any reads its settings for itself.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NoteMods {
    pub len: usize,
    pub index: [u16; NOTE_MODS],
    pub share: [f32; NOTE_MODS],
    /// Changed since the voice last read its settings.
    pub dirty: bool,
}

impl NoteMods {
    pub const NONE: NoteMods = NoteMods {
        len: 0,
        index: [0; NOTE_MODS],
        share: [0.0; NOTE_MODS],
        dirty: false,
    };

    /// Set parameter `index`'s share (a full table keeps what it has).
    pub fn set(&mut self, index: usize, share: f32) {
        let i = index as u16;
        if let Some(k) = self.index[..self.len].iter().position(|x| *x == i) {
            if self.share[k] != share {
                self.share[k] = share;
                self.dirty = true;
            }
        } else if self.len < NOTE_MODS {
            self.index[self.len] = i;
            self.share[self.len] = share;
            self.len += 1;
            self.dirty = true;
        }
    }

    /// The share on parameter `index` (0: none).
    pub fn of(&self, index: u32) -> f32 {
        self.index[..self.len]
            .iter()
            .position(|x| u32::from(*x) == index)
            .map_or(0.0, |k| self.share[k])
    }
}

/// A voice's filter: the Synth's, per side.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VoiceFilter {
    svf: [[Svf; 2]; 2],
    ladder: [[f32; 4]; 2],
    coefs: [(f32, f32, f32, f32); 2],
    /// `tan(π fc / sr)` at the last control tick.
    pub g: f32,
}

impl VoiceFilter {
    pub const IDLE: VoiceFilter = VoiceFilter {
        svf: [[Svf { ic1: 0.0, ic2: 0.0 }; 2]; 2],
        ladder: [[0.0; 4]; 2],
        coefs: [(0.0, 0.0, 0.0, 1.4); 2],
        g: 0.1,
    };

    /// Coefficients for the Synth's filter `kind` at `fc` (Hz).
    pub fn set(&mut self, kind: usize, fc: f32, sr: f32, c: &Common) {
        let g = (PI * fc / sr).tan();
        self.g = g;
        self.coefs = filter_coefs(kind, g, c.k, c.resonance);
    }

    /// One sample of side `side` through filter `kind`.
    #[inline]
    pub fn run(&mut self, kind: usize, side: usize, x: f32) -> f32 {
        use synth::filter;
        if kind == filter::LADDER {
            return ladder(&mut self.ladder[side], x, self.coefs);
        }
        let (low, band, high) = self.svf[side][0].run(x, self.coefs[0]);
        match kind {
            filter::LP24 => self.svf[side][1].run(low, self.coefs[1]).0,
            filter::BAND => band * self.coefs[0].3,
            filter::HIGH => high,
            filter::NOTCH => low + high,
            _ => low,
        }
    }

    /// Flush the states' denormals (after a segment).
    pub fn settle(&mut self) {
        for side in &mut self.svf {
            for st in side {
                if st.ic1.abs() < 1e-20 {
                    st.ic1 = 0.0;
                }
                if st.ic2.abs() < 1e-20 {
                    st.ic2 = 0.0;
                }
            }
        }
        for side in &mut self.ladder {
            for st in side {
                if st.abs() < 1e-20 {
                    *st = 0.0;
                }
            }
        }
    }
}

/// What every voice has, whatever makes its sound.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Head {
    pub amp: Env,
    pub filt: Env,
    pub menv: Env,
    pub key: u8,
    pub channel: u8,
    /// What the key sounds in the track's tuning (semitones from A4 at
    /// 440 Hz).
    pub pitch: f32,
    pub velocity: f32,
    pub age: u64,
    pub pedal_held: bool,
    /// Glide: semitones still to go (falls to zero).
    pub glide: f32,
    pub expr: Expr,
    pub lfo2: Lfo,
    pub lfo2_fade: f32,
    /// The random value this note drew (−1…1).
    pub random: f32,
    /// The note's serial number (seeds a voice's own randomness).
    pub serial: u32,
    pub filter: VoiceFilter,
    pub notes: NoteMods,
    /// Samples to the next control tick.
    pub control: u32,
}

impl Head {
    pub const IDLE: Head = Head {
        amp: Env::IDLE,
        filt: Env::IDLE,
        menv: Env::IDLE,
        key: 0,
        channel: 0,
        pitch: 0.0,
        velocity: 0.0,
        age: 0,
        pedal_held: false,
        glide: 0.0,
        expr: Expr::REST,
        lfo2: Lfo::new(1),
        lfo2_fade: 1.0,
        random: 0.0,
        serial: 0,
        filter: VoiceFilter::IDLE,
        notes: NoteMods::NONE,
        control: 0,
    };

    pub fn sounding(&self) -> bool {
        self.amp.stage != Stage::Idle
    }

    pub fn releasing(&self) -> bool {
        self.amp.stage == Stage::Release
    }

    /// How quiet it is (for stealing).
    pub fn quietness(&self) -> f32 {
        self.amp.value * (0.25 + 0.75 * self.velocity)
    }

    pub fn release(&mut self) {
        self.amp.release();
        self.filt.release();
        self.menv.release();
    }

    /// Advance the envelopes a sample: the amplifier's and the filter's
    /// values.
    #[inline]
    pub fn tick(&mut self, c: &Common) -> (f32, f32) {
        let a = self.amp.tick(&c.amp);
        let f = self.filt.tick(&c.filt);
        self.menv.tick(&c.menv);
        (a, f)
    }

    /// At a segment's start: LFO 2 takes the song's phase when locked
    /// to it.
    pub fn begin_segment(&mut self, b: &Block) {
        if let Some(p) = b.lfo2_phase {
            self.lfo2.phase = p;
        }
    }

    /// Where the key is, bipolar round C4 (the Synth's Key source).
    pub fn key_position(&self) -> f32 {
        ((self.pitch + 9.0) / 36.0).clamp(-1.0, 1.0)
    }
}

/// A voice of an instrument: its [`Head`] and whatever makes its sound.
pub(crate) trait Voiced: Copy {
    fn head(&self) -> &Head;
    fn head_mut(&mut self) -> &mut Head;
}

/// What a new note (or a legato slide) asks of a voice's own parts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NoteStart {
    /// A new attack (false: legato, the same voice slides on).
    pub retrigger: bool,
    /// The voice was sounding (a stolen or retriggered one): keep its
    /// phases and filter so nothing clicks.
    pub kept: bool,
}

/// What a block gives every voice: LFO 1's value, LFO 2's phase when
/// locked to the song and its speed (cycles per control tick), and the
/// mod-wheel vibrato's phase.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Block {
    pub lfo: f32,
    pub lfo2_phase: Option<f64>,
    pub lfo2_cycles: f64,
}

/// The modulations of a voice at a control tick: per target the sum of
/// what moves it (−1…1 at full), the vibrato's depth and the volume's
/// scale.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mods {
    pub add: [f32; MAX_TARGETS],
    pub vibrato: f32,
    pub gain: f32,
}

impl Mods {
    #[inline]
    pub fn get(&self, target: usize) -> f32 {
        self.add.get(target).copied().unwrap_or(0.0)
    }
}

/// A source's value for a voice.
#[inline]
fn source_value(
    src: usize,
    h: &Head,
    ch: &ChannelState,
    b: &Block,
    c: &Common,
    filt_env: f32,
) -> f32 {
    match src {
        source::LFO1 => b.lfo,
        source::LFO2 => h.lfo2.value(c.lfo2_shape, 0.0) as f32 * h.lfo2_fade,
        source::MOD_ENV => h.menv.value,
        source::VELOCITY => h.velocity,
        source::KEY => h.key_position(),
        source::AFTERTOUCH => (ch.pressure + h.expr.pressure).min(1.0),
        source::WHEEL => ch.modulation,
        source::RANDOM => h.random,
        source::AMP_ENV => h.amp.value,
        source::FILTER_ENV => filt_env,
        source::TIMBRE => (ch.timbre + h.expr.brightness - 1.0).clamp(-1.0, 1.0),
        source::BEND => ch.bend,
        _ => 0.0,
    }
}

/// The wheel's and the matrix's modulations of a voice now.
pub(crate) fn modulate(c: &Common, h: &Head, ch: &ChannelState, b: &Block) -> Mods {
    let mut m = Mods {
        add: [0.0; MAX_TARGETS],
        vibrato: 0.0,
        gain: 1.0,
    };
    let w = ch.modulation;
    for &(t, a) in &c.wheel {
        match t {
            target::OFF => {}
            target::VIBRATO => m.vibrato += (a * w).abs(),
            // Brought in (amount > 0) or taken out (< 0) by the wheel.
            target::VOLUME => {
                m.gain *= if a >= 0.0 {
                    1.0 - a + a * w
                } else {
                    1.0 + a * w
                }
            }
            _ => {
                if let Some(x) = m.add.get_mut(t) {
                    *x += a * w;
                }
            }
        }
    }
    for &(src, t, amount) in &c.matrix {
        if src == source::OFF || t == target::OFF || amount == 0.0 {
            continue;
        }
        let x = amount * source_value(src, h, ch, b, c, h.filt.value);
        match t {
            target::VIBRATO => m.vibrato += x.abs(),
            target::VOLUME => m.gain *= (1.0 + x).max(0.0),
            _ => {
                if let Some(v) = m.add.get_mut(t) {
                    *v += x;
                }
            }
        }
    }
    m
}

/// A voice's pitch now (semitones from A4): the key, glide, the wheel's
/// bend, its expression's tuning, vibrato and the Pitch target.
#[inline]
pub(crate) fn semitones(
    h: &Head,
    c: &Common,
    ch: &ChannelState,
    m: &Mods,
    vibrato_sine: f32,
) -> f32 {
    h.pitch
        + h.glide
        + ch.bend * ch.bend_range.unwrap_or(c.bend_range)
        + h.expr.tuning
        + vibrato_sine * (m.vibrato + h.expr.vibrato * synth::VIBRATO_DEPTH)
        + 12.0 * m.get(target::PITCH)
}

/// The cutoff a voice's filter has now (Hz).
#[inline]
pub(crate) fn cutoff(
    c: &Common,
    h: &Head,
    ch: &ChannelState,
    m: &Mods,
    filt_env: f32,
    sr: f32,
) -> f32 {
    let vel = 1.0 - c.vel_cutoff + c.vel_cutoff * h.velocity;
    let env = (c.env_octaves + 6.0 * m.get(target::FILTER_ENV)) * filt_env * vel;
    (c.cutoff
        * 2f32.powf(
            env + c.key_track * (h.pitch + 9.0) / 12.0
                + 4.0 * m.get(target::CUTOFF)
                + (ch.timbre - 0.5 + h.expr.brightness - 0.5) * 4.0,
        ))
    .clamp(20.0, sr * 0.45)
}

/// A voice's level now (before its filter): the amplifier envelope, its
/// velocity, the volume, pressure and its expressions.
#[inline]
pub(crate) fn level(c: &Common, h: &Head, ch: &ChannelState, m: &Mods, amp_env: f32) -> f32 {
    amp_env
        * (1.0 - c.vel_amp + c.vel_amp * h.velocity)
        * c.gain
        * m.gain
        * (1.0 + 0.5 * (ch.pressure + h.expr.pressure))
        * h.expr.volume
        * h.expr.expression
}

/// The resonance and drive a voice's filter has with its modulations.
pub(crate) fn filter_settings(c: &Common, m: &Mods) -> Common {
    let mut out = *c;
    let r = m.get(target::RESONANCE);
    if r != 0.0 {
        out.resonance = (c.resonance + r).clamp(0.0, 1.0);
        out.k = 2.0 - 1.9 * out.resonance;
    }
    out.drive = (c.drive + m.get(target::DRIVE)).clamp(0.0, 1.0);
    out
}

/// Drive into the filter.
#[inline]
pub(crate) fn drive(x: f32, amount: f32) -> f32 {
    if amount > 0.0 {
        let g = 1.0 + 4.0 * amount;
        (x * g).tanh() / g.sqrt()
    } else {
        x
    }
}

/// An item of a block: render frames, or take an event or a voice's
/// modulation, in time order.
pub(crate) enum Step<'a> {
    Render(usize, usize),
    Midi(MidiEvent),
    NoteMod(&'a NoteParamMod),
}

/// A block's MIDI events and per-note modulations merged in time.
pub(crate) struct Steps<'a> {
    events: Option<&'a MidiBuffer>,
    next_event: usize,
    mods: &'a [NoteParamMod],
    next_mod: usize,
    pos: usize,
    frames: usize,
}

impl<'a> Steps<'a> {
    pub fn new(events: Option<&'a MidiBuffer>, mods: &'a [NoteParamMod], frames: usize) -> Self {
        Self {
            events,
            next_event: 0,
            mods,
            next_mod: 0,
            pos: 0,
            frames,
        }
    }

    /// The next step (`None` once the block is done).
    pub fn next_step(&mut self) -> Option<Step<'a>> {
        let event = self
            .events
            .and_then(|e| e.iter().nth(self.next_event))
            .map(|e| ((e.sample_offset as usize).min(self.frames), e.event));
        let m = self
            .mods
            .get(self.next_mod)
            .map(|m| ((m.sample_offset as usize).min(self.frames), m));
        let at = match (event, m) {
            (Some((a, _)), Some((b, _))) => a.min(b),
            (Some((a, _)), None) => a,
            (None, Some((b, _))) => b,
            (None, None) => self.frames,
        };
        if at > self.pos {
            let from = self.pos;
            self.pos = at;
            return Some(Step::Render(from, at));
        }
        match (event, m) {
            // Events first at the same offset (a note's modulation comes
            // after its note-on).
            (Some((a, e)), _) if a == at => {
                self.next_event += 1;
                Some(Step::Midi(e))
            }
            (_, Some((b, nm))) if b == at => {
                self.next_mod += 1;
                Some(Step::NoteMod(nm))
            }
            _ => None,
        }
    }
}

/// An instrument's voices and everything about playing them that is not
/// its sound: the MIDI channels, held keys, glide, the tuning, the LFOs.
pub(crate) struct Chassis<V> {
    pub voices: Vec<V>,
    pub channels: [ChannelState; 16],
    /// The tuning: what each key sounds.
    pub tuning: TuningTable,
    /// Keys held in mono modes, oldest first.
    held: [(u8, u8, u8); 16],
    held_len: usize,
    /// Keys down right now (any mode), for glide between held keys.
    keys_down: u32,
    counter: u64,
    /// The last note's pitch (semitones from A4), for glide.
    last_pitch: Option<f32>,
    pub lfo: Lfo,
    /// LFO 2's free-running phase (voices not retriggered take it up).
    pub lfo2: Lfo,
    /// The mod-wheel vibrato's phase (0…1).
    vibrato_phase: f32,
    idle: V,
}

impl<V: Voiced> Chassis<V> {
    /// With room for every voice (made here, never on the audio thread).
    pub fn new(idle: V) -> Self {
        Self {
            voices: vec![idle; MAX_VOICES],
            channels: [ChannelState::REST; 16],
            tuning: TuningTable::STANDARD,
            held: [(0, 0, 0); 16],
            held_len: 0,
            keys_down: 0,
            counter: 0,
            last_pitch: None,
            lfo: Lfo::new(0x51_7e),
            lfo2: Lfo::new(0x2b_91),
            vibrato_phase: 0.0,
            idle,
        }
    }

    pub fn reset(&mut self) {
        let idle = self.idle;
        self.voices.iter_mut().for_each(|v| *v = idle);
        self.channels = [ChannelState::REST; 16];
        self.held_len = 0;
        self.keys_down = 0;
        self.last_pitch = None;
    }

    pub fn sounding(&self) -> usize {
        self.voices.iter().filter(|v| v.head().sounding()).count()
    }

    /// The newest voice sounding.
    pub fn newest(&self) -> Option<&V> {
        self.voices
            .iter()
            .filter(|v| v.head().sounding())
            .max_by_key(|v| v.head().age)
    }

    /// The LFOs for a block of `frames` (`get` by id) and the mod wheel's
    /// vibrato sine for its start.
    pub fn begin_block(
        &mut self,
        get: &dyn Fn(u32) -> f32,
        t: &TransportInfo,
        frames: usize,
        sr: f32,
    ) -> Block {
        let tempo = if t.tempo > 0.0 { t.tempo } else { 120.0 };
        let sr = f64::from(sr);
        let division =
            |pid: u32| DIVISIONS[(get(pid).round().max(0.0) as usize).min(DIVISIONS.len() - 1)].1;
        let shape = Shape::from_index(get(id::LFO_SHAPE).round().max(0.0) as usize);
        let synced = get(id::LFO_SYNC) >= 0.5;
        let period = division(id::LFO_DIVISION);
        if synced && t.playing {
            self.lfo.sync(t.quarter_position, period);
        }
        let lfo = self.lfo.value(shape, 0.0) as f32;
        let cycles = if synced {
            tempo / 60.0 / period / sr
        } else {
            f64::from(get(id::LFO_RATE)) / sr
        };
        self.lfo.advance(cycles * frames as f64);
        let synced2 = get(id::LFO2_SYNC) >= 0.5;
        let period2 = division(id::LFO2_DIVISION);
        let cycles2 = if synced2 {
            tempo / 60.0 / period2 / sr
        } else {
            f64::from(get(id::LFO2_RATE)) / sr
        };
        let lfo2_phase = if synced2 && t.playing {
            self.lfo2.sync(t.quarter_position, period2);
            Some(self.lfo2.phase)
        } else {
            None
        };
        self.lfo2.advance(cycles2 * frames as f64);
        Block {
            lfo,
            lfo2_phase,
            lfo2_cycles: cycles2 * f64::from(synth::CONTROL_INTERVAL),
        }
    }

    /// The mod-wheel vibrato's sine at a segment's start, moved on by its
    /// length.
    pub fn vibrato(&mut self, frames: usize, sr: f32) -> f32 {
        let v = (self.vibrato_phase * 2.0 * PI).sin();
        self.vibrato_phase = (self.vibrato_phase + frames as f32 * synth::VIBRATO_HZ / sr).fract();
        v
    }

    /// Take a MIDI event; `start` fills a voice's own parts when a note
    /// starts on it (its [`Head`] is set already).
    pub fn handle(
        &mut self,
        event: MidiEvent,
        c: &Common,
        start: &mut dyn FnMut(&mut V, NoteStart),
    ) {
        match event {
            // A key the tuning leaves out plays nothing.
            MidiEvent::NoteOn { key, .. } if !self.tuning.is_mapped(key) => {}
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => self.note_on(channel, key, velocity, c, start),
            MidiEvent::NoteOff { channel, key, .. } => self.note_off(channel, key, c, start),
            MidiEvent::ControlChange { controller, .. }
                if controller == MidiEvent::CC_ALL_NOTES_OFF
                    || controller == MidiEvent::CC_ALL_SOUND_OFF =>
            {
                for ch in 0..16 {
                    self.set_sustain(ch, false);
                }
                for v in &mut self.voices {
                    v.head_mut().release();
                }
                self.held_len = 0;
                self.keys_down = 0;
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
            } => self.channels[(channel & 15) as usize].modulation = f32::from(value) / 127.0,
            MidiEvent::ControlChange {
                channel,
                controller: 74,
                value,
            } => self.channels[(channel & 15) as usize].timbre = f32::from(value) / 127.0,
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
            MidiEvent::PitchBend { channel, value } => {
                self.channels[(channel & 15) as usize].bend =
                    ((f32::from(value) - 8192.0) / 8192.0).clamp(-1.0, 1.0);
            }
            MidiEvent::ChannelPressure { channel, pressure } => {
                self.channels[(channel & 15) as usize].pressure = f32::from(pressure) / 127.0;
            }
            MidiEvent::PolyPressure {
                channel,
                key,
                pressure,
            } => {
                for v in self.voices.iter_mut() {
                    let h = v.head_mut();
                    if h.sounding() && h.channel == channel && h.key == key {
                        h.expr.pressure = f32::from(pressure) / 127.0;
                    }
                }
            }
            MidiEvent::NoteExpression {
                channel,
                key,
                kind,
                value,
            } => {
                for v in self.voices.iter_mut() {
                    let h = v.head_mut();
                    if h.sounding() && h.channel == channel && h.key == key {
                        h.expr.set(kind, value.get() as f32);
                    }
                }
            }
            _ => {}
        }
    }

    /// A voice's own modulation of parameter `index` (`share` of its
    /// range), for the voices playing its note.
    pub fn note_mod(&mut self, m: &NoteParamMod, index: usize, share: f32) {
        for v in self.voices.iter_mut() {
            let h = v.head_mut();
            if h.sounding() && h.channel == m.channel && h.key == m.key {
                h.notes.set(index, share);
            }
        }
    }

    /// RPN data entry: 0 = pitch bend range, 6 = MPE configuration (on the
    /// master channel: member channels get the MPE default range).
    fn data_entry(&mut self, channel: u8, value: u8) {
        match self.channels[channel as usize].rpn {
            (0, 0) => self.channels[channel as usize].bend_range = Some(f32::from(value.max(1))),
            (0, 6) if channel == 0 => {
                for ch in 1..=(value.min(15) as usize) {
                    self.channels[ch].bend_range = Some(synth::MPE_BEND_RANGE);
                }
            }
            _ => {}
        }
    }

    fn set_sustain(&mut self, channel: u8, on: bool) {
        self.channels[(channel & 15) as usize].sustain = on;
        if !on {
            for v in &mut self.voices {
                let h = v.head_mut();
                if h.channel == channel && h.pedal_held {
                    h.pedal_held = false;
                    h.release();
                }
            }
        }
    }

    fn note_on(
        &mut self,
        channel: u8,
        key: u8,
        velocity: u8,
        c: &Common,
        start: &mut dyn FnMut(&mut V, NoteStart),
    ) {
        let others = self.keys_down > 0;
        self.keys_down += 1;
        if c.mode > 0 {
            // Mono: one voice; legato keeps its envelopes while keys are
            // held.
            let legato = c.mode == 2 && self.held_len > 0 && self.voices[0].head().sounding();
            if self.held_len < self.held.len() {
                self.held[self.held_len] = (channel, key, velocity);
                self.held_len += 1;
            }
            self.start(0, channel, key, velocity, !legato, others, c, start);
            return;
        }
        let voices = &self.voices[..c.polyphony.min(self.voices.len())];
        let pick = |set: &mut dyn Iterator<Item = (usize, &V)>| {
            if c.steal == 1 {
                set.min_by(|a, b| a.1.head().quietness().total_cmp(&b.1.head().quietness()))
                    .map(|(i, _)| i)
            } else {
                set.min_by_key(|(_, v)| v.head().age).map(|(i, _)| i)
            }
        };
        let idx = voices
            .iter()
            .position(|v| !v.head().sounding())
            .or_else(|| {
                pick(
                    &mut voices
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.head().releasing()),
                )
            })
            .or_else(|| pick(&mut voices.iter().enumerate()))
            .unwrap_or(0);
        self.start(idx, channel, key, velocity, true, others, c, start);
    }

    fn note_off(
        &mut self,
        channel: u8,
        key: u8,
        c: &Common,
        start: &mut dyn FnMut(&mut V, NoteStart),
    ) {
        self.keys_down = self.keys_down.saturating_sub(1);
        if c.mode > 0 {
            // Back to the last key still held, if any.
            if let Some(i) = self.held[..self.held_len]
                .iter()
                .position(|h| h.0 == channel && h.1 == key)
            {
                self.held.copy_within(i + 1..self.held_len, i);
                self.held_len -= 1;
            }
            let h0 = *self.voices[0].head();
            if h0.key != key || h0.channel != channel {
                return;
            }
            if self.held_len > 0 {
                let (ch, k, vel) = self.held[self.held_len - 1];
                self.start(0, ch, k, vel, c.mode == 1, true, c, start);
                return;
            }
        }
        let pedal = self.channels[(channel & 15) as usize].sustain;
        for v in &mut self.voices {
            let h = v.head_mut();
            if h.key == key && h.channel == channel && h.sounding() && !h.releasing() {
                if pedal {
                    h.pedal_held = true;
                } else {
                    h.release();
                }
            }
        }
    }

    /// Start (or, `!retrigger`, slide) voice `idx`; `legato`: other keys
    /// were down already.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        idx: usize,
        channel: u8,
        key: u8,
        velocity: u8,
        retrigger: bool,
        legato: bool,
        c: &Common,
        start: &mut dyn FnMut(&mut V, NoteStart),
    ) {
        self.counter += 1;
        let pitch = (self.tuning.pitch_or_key(key) - 69.0) as f32;
        let glide = match self.last_pitch {
            Some(from) if c.glide_coef > 0.0 && (legato || !c.glide_legato) => {
                from - pitch
                    + if retrigger {
                        0.0
                    } else {
                        self.voices[idx].head().glide
                    }
            }
            _ => 0.0,
        };
        self.last_pitch = Some(pitch);
        let n = self.counter as u32;
        let mut lfo2 = self.lfo2;
        if c.lfo2_retrig {
            lfo2.phase = 0.0;
        }
        let counter = self.counter;
        let v = &mut self.voices[idx];
        let h = v.head_mut();
        if !retrigger {
            // Legato: the same voice slides on, envelopes untouched.
            h.key = key;
            h.pitch = pitch;
            h.channel = channel;
            h.glide = glide;
            h.age = counter;
            start(
                v,
                NoteStart {
                    retrigger: false,
                    kept: true,
                },
            );
            return;
        }
        let kept = h.sounding();
        let at = |e: Env| Env {
            stage: Stage::Attack,
            value: if kept { e.value } else { 0.0 },
        };
        *h = Head {
            amp: at(h.amp),
            filt: at(h.filt),
            menv: at(h.menv),
            key,
            channel,
            pitch,
            velocity: f32::from(velocity) / 127.0,
            age: counter,
            glide,
            lfo2,
            lfo2_fade: if c.lfo2_fade_step >= 1.0 { 1.0 } else { 0.0 },
            random: spread_random(n),
            serial: n,
            filter: if kept { h.filter } else { VoiceFilter::IDLE },
            ..Head::IDLE
        };
        start(
            v,
            NoteStart {
                retrigger: true,
                kept,
            },
        );
    }
}

/// A voice's control tick: its glide, LFO 2 and fade move on and its
/// modulations are read again; then the filter's coefficients.
#[inline]
pub(crate) fn control_tick(
    h: &mut Head,
    c: &Common,
    ch: &ChannelState,
    b: &Block,
    sr: f32,
) -> Mods {
    h.glide *= c.glide_coef;
    let m0 = modulate(c, h, ch, b);
    let speed = 2f32.powf(2.0 * m0.get(target::LFO2_RATE));
    h.lfo2.advance(b.lfo2_cycles * f64::from(speed));
    h.lfo2_fade = (h.lfo2_fade + c.lfo2_fade_step).min(1.0);
    let m = modulate(c, h, ch, b);
    if let Some(kind) = c.filter {
        let fs = filter_settings(c, &m);
        let fc = cutoff(c, h, ch, &m, h.filt.value, sr);
        h.filter.set(kind, fc, sr, &fs);
    }
    m
}

/// A voice's pan gains (left, right) with its expression and the Pan
/// target.
#[inline]
pub(crate) fn pan_gains(h: &Head, m: &Mods) -> (f32, f32) {
    let pan = (h.expr.pan + m.get(target::PAN)).clamp(-1.0, 1.0);
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

/// A voice's two sides through its drive and filter, at `gain`, added to
/// the output at `i`.
#[inline]
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish(
    h: &mut Head,
    c: &Common,
    side: [f32; 2],
    gain: f32,
    pans: (f32, f32),
    drive_amount: f32,
    out_l: &mut [f32],
    out_r: Option<&mut [f32]>,
    i: usize,
) {
    let mut y = [0.0f32; 2];
    for (k, x) in side.into_iter().enumerate() {
        let x = drive(x, drive_amount);
        y[k] = match c.filter {
            Some(kind) => h.filter.run(kind, k, x),
            None => x,
        } * gain;
    }
    match out_r {
        Some(r) => {
            out_l[i] += y[0] * pans.0;
            r[i] += y[1] * pans.1;
        }
        None => out_l[i] += 0.5 * (y[0] + y[1]),
    }
}

/// What an instrument publishes and meters after a block, and its
/// editor's output ring.
pub(crate) struct Output {
    pub watching: Watching,
    meters: [MeterTap; 2],
}

impl Output {
    pub fn new(sr: f32) -> Self {
        Self {
            watching: Watching::new(sr),
            meters: [MeterTap::new(sr); 2],
        }
    }

    /// Publish the shared values for `newest` (with its filter envelope)
    /// and the output's meters.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        tap: &AnalysisTap,
        newest: Option<&Head>,
        voices: usize,
        lfo: f32,
        lfo2_shape: Shape,
        sr: f32,
        out: &faderframe_audio_graph::AudioBuffer,
        frames: usize,
        watched: bool,
    ) {
        tap.set_value(value::VOICES, voices as f32);
        match newest {
            Some(h) => {
                tap.set_value(value::NOTE, f32::from(h.key));
                tap.set_value(value::AMP_ENV, h.amp.value);
                tap.set_value(value::FILTER_ENV, h.filt.value);
                tap.set_value(value::MOD_ENV, h.menv.value);
                tap.set_value(
                    value::LFO2,
                    h.lfo2.value(lfo2_shape, 0.0) as f32 * h.lfo2_fade,
                );
                tap.set_value(value::CUTOFF, h.filter.g.atan() * sr / PI);
            }
            None => {
                for v in [
                    value::AMP_ENV,
                    value::FILTER_ENV,
                    value::MOD_ENV,
                    value::LFO2,
                ] {
                    tap.set_value(v, 0.0);
                }
            }
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
}

/// The share of its range a voice's modulation of parameter `index` is
/// (`amount` in plain units).
pub(crate) fn share_of(params: &ParamValues, index: usize, amount: f32) -> f32 {
    params.infos().get(index).map_or(0.0, |i| {
        let range = (i.max - i.min) as f32;
        if range > 0.0 { amount / range } else { 0.0 }
    })
}

/// Run the effects over a block's output and say whether anything is
/// left to come.
pub(crate) fn effects(
    fx: &mut Fx,
    set: &FxSettings,
    out: &mut faderframe_audio_graph::AudioBuffer,
    frames: usize,
) {
    let peak = (0..2.min(out.num_channels()))
        .flat_map(|c| out.channel(c)[..frames].iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    if out.num_channels() >= 2 {
        let (l, r) = out.channel_pair_mut(0, 1);
        fx.process(set, &mut l[..frames], Some(&mut r[..frames]), peak);
    } else {
        let l = out.channel_mut(0);
        fx.process(set, &mut l[..frames], None, peak);
    }
}

/// A rig the instruments' tests play notes on.
#[cfg(test)]
pub(crate) mod rig {
    #![allow(clippy::unwrap_used)]

    use crate::{NoteParamMod, PluginProcessContext, PluginProcessor};
    use faderframe_audio_graph::{AudioBuffer, NodeIo};
    use faderframe_core::ChannelLayout;
    use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
    use faderframe_transport::TransportInfo;

    pub const SR: f64 = 48_000.0;
    pub const BLOCK: usize = 256;

    pub struct Play<P> {
        pub p: P,
        out: Vec<AudioBuffer>,
        events: Vec<MidiBuffer>,
        pub transport: TransportInfo,
        /// Per-note modulations for the next block.
        pub mods: Vec<NoteParamMod>,
    }

    impl<P: PluginProcessor> Play<P> {
        pub fn new(p: P) -> Self {
            let mut out = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
            out[0].set_len(BLOCK);
            Self {
                p,
                out,
                events: vec![MidiBuffer::with_capacity(64)],
                transport: TransportInfo::default(),
                mods: Vec::new(),
            }
        }

        pub fn send(&mut self, e: MidiEvent) {
            self.events[0].push(TimedMidiEvent::new(0, e)).unwrap();
        }

        pub fn run(&mut self, seconds: f64) -> (Vec<f32>, Vec<f32>) {
            let (mut l, mut r) = (Vec::new(), Vec::new());
            for _ in 0..(seconds * SR / BLOCK as f64).ceil() as usize {
                let ctx = PluginProcessContext {
                    transport: &self.transport,
                    param_events: &[],
                    harmony: &crate::NO_HARMONY,
                    param_mods: &[],
                    note_mods: &self.mods,
                };
                let mut io = NodeIo {
                    frames: BLOCK,
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

    pub fn on(key: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 0,
            key,
            velocity: 100,
        }
    }

    pub fn off(key: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            channel: 0,
            key,
            velocity: 0,
        }
    }

    /// The magnitude spectrum (a 4-term Blackman–Harris window: its
    /// leakage is 92 dB down) of the last power of two of `x`, and its bin
    /// width in Hz.
    pub fn spectrum(x: &[f32]) -> (Vec<f64>, f64) {
        let n = 1usize << (usize::BITS - 1 - x.len().leading_zeros());
        let tail = &x[x.len() - n..];
        let mut planner = realfft::RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n);
        let mut input: Vec<f64> = tail
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let t = std::f64::consts::TAU * i as f64 / n as f64;
                let w = 0.358_75 - 0.488_29 * t.cos() + 0.141_28 * (2.0 * t).cos()
                    - 0.011_68 * (3.0 * t).cos();
                f64::from(*v) * w
            })
            .collect();
        let mut out = fft.make_output_vec();
        fft.process(&mut input, &mut out).unwrap();
        (out.iter().map(|c| c.norm()).collect(), SR / n as f64)
    }

    /// The energy (dB) of what is not a harmonic of `f0` against what is
    /// (bins within six of a harmonic count as it).
    pub fn inharmonic_db(x: &[f32], f0: f64) -> f64 {
        let (mags, bin) = spectrum(x);
        let (mut on, mut off) = (0.0f64, 0.0f64);
        for (k, m) in mags.iter().enumerate().skip(2) {
            let f = k as f64 * bin;
            let h = (f / f0).round().max(1.0);
            let e = m * m;
            if (f - h * f0).abs() <= 6.0 * bin {
                on += e;
            } else {
                off += e;
            }
        }
        10.0 * (off.max(1e-30) / on.max(1e-30)).log10()
    }

    /// The loudest frequency (Hz) of a signal.
    pub fn peak_hz(x: &[f32]) -> f64 {
        let (mags, bin) = spectrum(x);
        let k = mags
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(k, _)| k);
        k as f64 * bin
    }

    pub fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_shared_block_keeps_its_ids_at_their_positions() {
        let d = Defaults {
            volume: -6.0,
            filter: 0.0,
            cutoff: 20_000.0,
            attack: 2.0,
            decay: 400.0,
            sustain: 0.8,
            release: 300.0,
        };
        let p = parameters(20, &d);
        assert_eq!(p.len(), COMMON as usize);
        for (i, info) in p.iter().enumerate() {
            assert_eq!(info.id.0 as usize, i, "{}", info.name);
        }
        assert_eq!(id::mod_amount(MOD_SLOTS - 1) + 1, id::CHORUS_MIX);
        assert_eq!(id::wheel_amount(WHEEL_SLOTS - 1) + 1, id::mod_source(0));
        let targets = ["Off", "Vibrato", "Pitch"];
        assert_eq!(
            format(ParameterId(id::mod_source(2)), 4.0, &targets).as_deref(),
            Some("Velocity")
        );
        assert_eq!(
            format(ParameterId(id::mod_target(7)), 2.0, &targets).as_deref(),
            Some("Pitch")
        );
        assert_eq!(
            format(ParameterId(id::wheel_amount(1)), -0.25, &targets).as_deref(),
            Some("-25 %")
        );
        assert!(per_voice(ParameterId(id::CUTOFF)));
        assert!(!per_voice(ParameterId(id::LFO_RATE)));
        assert!(!per_voice(ParameterId(id::REVERB_MIX)));
    }

    #[test]
    fn note_mods_keep_a_share_per_parameter() {
        let mut m = NoteMods::NONE;
        m.set(7, 0.25);
        m.set(3, -0.1);
        m.set(7, 0.5);
        assert_eq!(m.len, 2);
        assert_eq!(m.of(7), 0.5);
        assert_eq!(m.of(3), -0.1);
        assert_eq!(m.of(9), 0.0);
        for i in 0..20 {
            m.set(100 + i, 0.1);
        }
        assert_eq!(m.len, NOTE_MODS, "a full table keeps what it has");
    }
}
