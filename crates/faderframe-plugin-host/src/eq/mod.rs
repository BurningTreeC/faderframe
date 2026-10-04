//! FaderFrame EQ: a 24 band parametric equaliser.
//!
//! * Bells, low and high shelves, low and high cuts (any slope from 0 to
//!   96 dB/oct, fractional ones included, and brickwall), notches, band
//!   passes, tilt shelves, flat tilts and all passes, each designed to keep
//!   the analog shape up to Nyquist ([`design`]).
//! * Three processing modes: zero latency (matched minimum phase
//!   sections), natural phase (those sections followed by a short FIR
//!   that makes magnitude *and* phase the analog filter's, [`natural`]) and
//!   linear phase in five resolutions ([`linear`]).
//! * Per band stereo placement (both channels, left, right, mid, side).
//! * Dynamic bands: the band moves by up to its range as the level in its
//!   region rises over a threshold, with a soft knee. In auto mode the
//!   threshold follows the trigger's own level and attack and release
//!   follow the band's frequency ([`dynamics`]); in custom mode the
//!   threshold, attack, release, the trigger's source (the input or the
//!   sidechain) and its filtering (the band's region, or free low and high
//!   cuts) are the band's own. Spectral bands act on each frequency of
//!   their region on its own, linear phase ([`spectral`]).
//! * Character modes (clean, subtle transformer, warm tube; [`character`]),
//!   output gain, pan (left/right or mid/side) and phase invert, auto gain,
//!   a gain scale, gain-Q interaction and a soft, latency compensated
//!   bypass.
//! * Changes are click free: frequency, gain, Q and slope glide, and a band
//!   that is switched on or off or changes its structure fades out and back
//!   in. Any band can be heard on its own, and so can its trigger.
//!
//! The processor publishes each band's live dynamic gain, trigger level and
//! threshold (and the spectral bands' gain per frequency) through the
//! [`AnalysisTap`] and fills its audio rings (input, output, sidechain) for
//! the editor's analyser.

pub mod character;
pub mod design;
pub mod dynamics;
mod fir;
pub mod linear;
pub mod matching;
pub mod natural;
pub mod spectral;

use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use character::{Character, CharacterStage};
use design::{BandShape, BandType, Coefs, MAX_SECTIONS};
use dynamics::{Detector, TRIGGER_SECTIONS};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

/// Bands the EQ has.
pub const BANDS: usize = 24;
/// Global parameters.
pub const GLOBALS: usize = 11;
/// Parameters per band.
pub const FIELDS: usize = 20;
/// Parameter ids of band `b`: `BAND_BASE + b * BAND_STRIDE + slot` for the
/// first sixteen slots, `BAND_BASE2 + b * BAND_STRIDE + slot − 16` after.
pub const BAND_BASE: u32 = 100;
pub const BAND_BASE2: u32 = 500;
pub const BAND_STRIDE: u32 = 16;

/// Global parameters (indexes into the parameter list).
pub mod global {
    pub const OUTPUT: usize = 0;
    pub const AUTO_GAIN: usize = 1;
    pub const GAIN_SCALE: usize = 2;
    pub const PHASE: usize = 3;
    pub const QUALITY: usize = 4;
    pub const CHARACTER: usize = 5;
    pub const PAN: usize = 6;
    pub const PAN_MODE: usize = 7;
    pub const INVERT: usize = 8;
    pub const BYPASS: usize = 9;
    pub const GAIN_Q: usize = 10;
}

/// Ids of the global parameters (5 to 7 belonged to the first version's
/// global attack, release and sidechain switch).
const GLOBAL_IDS: [u32; GLOBALS] = [0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 13];

/// A band's parameters, in order. `Enabled` is the band's state:
/// 0 unused (not shown), 1 on, 2 bypassed (shown, not heard).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Enabled,
    Type,
    Freq,
    Gain,
    Q,
    /// dB per octave ([`design::BRICKWALL`] above 96).
    Slope,
    Placement,
    /// The dynamic range (0: a static band).
    Range,
    /// The threshold (dBFS); its top (0) is "auto".
    Threshold,
    /// Trigger from the sidechain input instead of the band's input.
    Key,
    /// Attack and release as a percentage of the automatic times (50 %
    /// is automatic; less is faster).
    Attack,
    Release,
    /// The trigger's filtering: the band's region (0) or free (1).
    Trigger,
    /// A free trigger's low and high cuts (Hz).
    TriggerLow,
    TriggerHigh,
    /// Spectral dynamics.
    Spectral,
    /// How selectively a spectral band picks frequencies (%).
    Density,
    /// A spectral band's trigger tilted by 3 dB/oct.
    SpectralTilt,
    /// The dynamics' own settings in effect (1) or all automatic (0).
    Dynamics,
    /// The band's dynamics bypassed.
    DynBypass,
}

impl Field {
    pub const ALL: [Field; FIELDS] = [
        Field::Enabled,
        Field::Type,
        Field::Freq,
        Field::Gain,
        Field::Q,
        Field::Slope,
        Field::Placement,
        Field::Range,
        Field::Threshold,
        Field::Key,
        Field::Attack,
        Field::Release,
        Field::Trigger,
        Field::TriggerLow,
        Field::TriggerHigh,
        Field::Spectral,
        Field::Density,
        Field::SpectralTilt,
        Field::Dynamics,
        Field::DynBypass,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The id slot (slot 5 held the first version's stepped slope).
    fn slot(self) -> u32 {
        const SLOTS: [u32; FIELDS] = [
            0, 1, 2, 3, 4, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 17, 18, 19, 20,
        ];
        SLOTS[self as usize]
    }
}

/// Which part of a stereo signal a band works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Stereo,
    Left,
    Right,
    Mid,
    Side,
}

impl Placement {
    pub const ALL: [Placement; 5] = [
        Placement::Stereo,
        Placement::Left,
        Placement::Right,
        Placement::Mid,
        Placement::Side,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Placement::Stereo)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            Placement::Stereo => "Stereo",
            Placement::Left => "Left",
            Placement::Right => "Right",
            Placement::Mid => "Mid",
            Placement::Side => "Side",
        }
    }

    pub fn letter(self) -> &'static str {
        match self {
            Placement::Stereo => "",
            Placement::Left => "L",
            Placement::Right => "R",
            Placement::Mid => "M",
            Placement::Side => "S",
        }
    }

    /// The other half of a split (left ↔ right, mid ↔ side).
    pub fn partner(self) -> Option<Placement> {
        match self {
            Placement::Left => Some(Placement::Right),
            Placement::Right => Some(Placement::Left),
            Placement::Mid => Some(Placement::Side),
            Placement::Side => Some(Placement::Mid),
            Placement::Stereo => None,
        }
    }
}

/// How the EQ processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseMode {
    ZeroLatency,
    Linear,
    Natural,
}

impl PhaseMode {
    /// The order menus show them in.
    pub const MENU: [PhaseMode; 3] = [
        PhaseMode::ZeroLatency,
        PhaseMode::Natural,
        PhaseMode::Linear,
    ];

    /// The parameter's value for the mode.
    pub fn value(self) -> f64 {
        match self {
            PhaseMode::ZeroLatency => 0.0,
            PhaseMode::Linear => 1.0,
            PhaseMode::Natural => 2.0,
        }
    }

    pub fn from_value(v: f64) -> Self {
        match v.round() as i64 {
            1 => PhaseMode::Linear,
            2 => PhaseMode::Natural,
            _ => PhaseMode::ZeroLatency,
        }
    }

    pub fn of(params: &ParamValues) -> Self {
        Self::from_value(f64::from(params.get(global::PHASE)))
    }

    pub fn name(self) -> &'static str {
        match self {
            PhaseMode::ZeroLatency => "Zero Latency",
            PhaseMode::Natural => "Natural Phase",
            PhaseMode::Linear => "Linear Phase",
        }
    }
}

/// The linear phase resolutions.
pub const QUALITIES: [&str; 5] = ["Low", "Medium", "High", "Very High", "Maximum"];

/// The quality setting the parameters ask for.
pub fn quality(params: &ParamValues) -> usize {
    (params.get(global::QUALITY).round().max(0.0) as usize).min(QUALITIES.len() - 1)
}

/// The EQ's latency for its parameters (samples).
pub fn latency(params: &ParamValues) -> u32 {
    stage_latencies(params).iter().sum()
}

/// The latencies of the spectral stage and of the FIR.
fn stage_latencies(params: &ParamValues) -> [u32; 2] {
    let q = quality(params);
    let spectral = if spectral::wanted(params) {
        spectral::frame(q) as u32
    } else {
        0
    };
    let fir = match PhaseMode::of(params) {
        PhaseMode::ZeroLatency => 0,
        PhaseMode::Linear => linear::latency(q),
        PhaseMode::Natural => natural::LATENCY,
    };
    [spectral, fir]
}

/// Index of a global parameter's value.
pub const fn global_index(g: usize) -> usize {
    g
}

/// Index of a band parameter's value.
pub const fn band_index(band: usize, field: Field) -> usize {
    GLOBALS + band * FIELDS + field as usize
}

/// Id of a band parameter.
pub fn band_id(band: usize, field: Field) -> ParameterId {
    let slot = field.slot();
    let b = band as u32 * BAND_STRIDE;
    ParameterId(if slot < BAND_STRIDE {
        BAND_BASE + b + slot
    } else {
        BAND_BASE2 + b + slot - BAND_STRIDE
    })
}

/// Id of a global parameter.
pub fn global_id(g: usize) -> ParameterId {
    ParameterId(GLOBAL_IDS.get(g).copied().unwrap_or(u32::MAX))
}

/// The global a parameter id names.
pub fn global_of(id: ParameterId) -> Option<usize> {
    GLOBAL_IDS.iter().position(|g| *g == id.0)
}

/// The band and field a parameter id names.
pub fn field_of(id: ParameterId) -> Option<(usize, Field)> {
    let raw = id.0;
    let span = BANDS as u32 * BAND_STRIDE;
    let (band, slot) = if (BAND_BASE..BAND_BASE + span).contains(&raw) {
        let r = raw - BAND_BASE;
        (r / BAND_STRIDE, r % BAND_STRIDE)
    } else if (BAND_BASE2..BAND_BASE2 + span).contains(&raw) {
        let r = raw - BAND_BASE2;
        (r / BAND_STRIDE, r % BAND_STRIDE + BAND_STRIDE)
    } else {
        return None;
    };
    let field = Field::ALL.iter().find(|f| f.slot() == slot)?;
    Some((band as usize, *field))
}

/// Where a new band's frequency sits by default.
pub fn default_freq(band: usize) -> f64 {
    let t = band as f64 / (BANDS - 1) as f64;
    40.0 * (12_000.0f64 / 40.0).powf(t)
}

/// The values the processor publishes through the tap: per band its
/// dynamic gain, trigger level and threshold (dB), then per spectral band
/// its gain at [`SPECTRAL_POINTS`] frequencies.
pub mod value {
    use super::BANDS;
    pub const DYN: usize = 0;
    pub const KEY: usize = BANDS;
    pub const THRESHOLD: usize = 2 * BANDS;
    pub const SPECTRAL: usize = 3 * BANDS;
}

/// Frequencies a spectral band's live gain is published at.
pub const SPECTRAL_POINTS: usize = 64;
/// Values the EQ's tap holds.
pub const TAP_VALUES: usize = value::SPECTRAL + BANDS * SPECTRAL_POINTS;

/// The frequency of published point `i` (10 Hz to 30 kHz, logarithmic).
pub fn spectral_point_hz(i: usize) -> f64 {
    10.0 * 3_000.0f64.powf(i as f64 / (SPECTRAL_POINTS - 1) as f64)
}

/// The listen setting meaning "the trigger of band `b`" (below it, a band
/// itself).
pub const fn listen_key(band: usize) -> usize {
    BANDS + band
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let p = |id: ParameterId, name: String, min: f64, max: f64, default: f64, unit, stepped| {
        ParameterInfo {
            id,
            name,
            min,
            max,
            default,
            unit,
            automatable: true,
            stepped,
        }
    };
    let g = |i: usize, name: &str, min, max, default, unit, stepped| {
        p(
            global_id(i),
            name.to_string(),
            min,
            max,
            default,
            unit,
            stepped,
        )
    };
    let fixed = |info: ParameterInfo| ParameterInfo {
        automatable: false,
        ..info
    };
    let mut out = vec![
        g(global::OUTPUT, "Output", -36.0, 36.0, 0.0, Decibels, false),
        g(global::AUTO_GAIN, "Auto Gain", 0.0, 1.0, 0.0, None, true),
        g(
            global::GAIN_SCALE,
            "Gain Scale",
            0.0,
            2.0,
            1.0,
            Percent,
            false,
        ),
        // They change the latency: not automatable.
        fixed(g(
            global::PHASE,
            "Processing Mode",
            0.0,
            2.0,
            0.0,
            None,
            true,
        )),
        fixed(g(
            global::QUALITY,
            "Linear Phase Resolution",
            0.0,
            (QUALITIES.len() - 1) as f64,
            1.0,
            None,
            true,
        )),
        g(global::CHARACTER, "Character", 0.0, 2.0, 0.0, None, true),
        g(global::PAN, "Output Pan", -1.0, 1.0, 0.0, None, false),
        g(global::PAN_MODE, "Pan Mode", 0.0, 1.0, 0.0, None, true),
        g(global::INVERT, "Phase Invert", 0.0, 1.0, 0.0, None, true),
        g(global::BYPASS, "Bypass", 0.0, 1.0, 0.0, None, true),
        g(
            global::GAIN_Q,
            "Gain-Q Interaction",
            0.0,
            1.0,
            0.0,
            None,
            true,
        ),
    ];
    debug_assert_eq!(out.len(), GLOBALS);
    for b in 0..BANDS {
        let n = b + 1;
        let f = |field: Field, name: &str, min, max, default, unit, stepped| {
            p(
                band_id(b, field),
                format!("Band {n} {name}"),
                min,
                max,
                default,
                unit,
                stepped,
            )
        };
        out.extend([
            f(Field::Enabled, "State", 0.0, 2.0, 0.0, None, true),
            f(
                Field::Type,
                "Shape",
                0.0,
                (BandType::ALL.len() - 1) as f64,
                0.0,
                None,
                true,
            ),
            f(
                Field::Freq,
                "Frequency",
                10.0,
                30_000.0,
                default_freq(b),
                Hertz,
                false,
            ),
            f(Field::Gain, "Gain", -30.0, 30.0, 0.0, Decibels, false),
            f(
                Field::Q,
                "Q",
                0.025,
                40.0,
                std::f64::consts::FRAC_1_SQRT_2,
                None,
                false,
            ),
            f(
                Field::Slope,
                "Slope",
                0.0,
                design::BRICKWALL,
                12.0,
                None,
                false,
            ),
            f(
                Field::Placement,
                "Placement",
                0.0,
                (Placement::ALL.len() - 1) as f64,
                0.0,
                None,
                true,
            ),
            f(
                Field::Range,
                "Dynamic Range",
                -30.0,
                30.0,
                0.0,
                Decibels,
                false,
            ),
            f(
                Field::Threshold,
                "Threshold",
                -80.0,
                0.0,
                0.0,
                Decibels,
                false,
            ),
            f(Field::Key, "External Sidechain", 0.0, 1.0, 0.0, None, true),
            f(Field::Attack, "Attack", 0.0, 1.0, 0.5, Percent, false),
            f(Field::Release, "Release", 0.0, 1.0, 0.5, Percent, false),
            f(Field::Trigger, "Free Trigger", 0.0, 1.0, 0.0, None, true),
            f(
                Field::TriggerLow,
                "Trigger Low Cut",
                10.0,
                30_000.0,
                10.0,
                Hertz,
                false,
            ),
            f(
                Field::TriggerHigh,
                "Trigger High Cut",
                10.0,
                30_000.0,
                30_000.0,
                Hertz,
                false,
            ),
            f(Field::Spectral, "Spectral", 0.0, 1.0, 0.0, None, true),
            f(
                Field::Density,
                "Spectral Density",
                0.0,
                1.0,
                0.5,
                Percent,
                false,
            ),
            f(
                Field::SpectralTilt,
                "Spectral Tilt",
                0.0,
                1.0,
                1.0,
                None,
                true,
            ),
            f(
                Field::Dynamics,
                "Dynamics Custom",
                0.0,
                1.0,
                0.0,
                None,
                true,
            ),
            f(
                Field::DynBypass,
                "Dynamics Bypass",
                0.0,
                1.0,
                0.0,
                None,
                true,
            ),
        ]);
    }
    out
}

/// A parameter value as the EQ shows it (`None`: the generic formatting).
pub fn format(id: ParameterId, value: f64) -> Option<String> {
    let on_off = |v: f64| Some(if v >= 0.5 { "On" } else { "Off" }.to_string());
    if let Some(g) = global_of(id) {
        let i = value.round().max(0.0) as usize;
        return match g {
            global::AUTO_GAIN | global::INVERT | global::BYPASS | global::GAIN_Q => on_off(value),
            global::PHASE => Some(PhaseMode::from_value(value).name().into()),
            global::QUALITY => Some(QUALITIES[i.min(QUALITIES.len() - 1)].into()),
            global::GAIN_SCALE => Some(format!("{:.0} %", value * 100.0)),
            global::CHARACTER => Some(Character::from_index(i).name().into()),
            global::PAN_MODE => Some(
                if value >= 0.5 {
                    "Mid/Side"
                } else {
                    "Left/Right"
                }
                .into(),
            ),
            global::PAN => Some(format_pan(value, false)),
            _ => None,
        };
    }
    let (_, field) = field_of(id)?;
    let i = value.round().max(0.0) as usize;
    match field {
        Field::Enabled => Some(
            match value.round() as i64 {
                1 => "On",
                2 => "Bypassed",
                _ => "Unused",
            }
            .into(),
        ),
        Field::Type => Some(BandType::from_index(i).name().into()),
        Field::Slope => Some(design::slope_name(value)),
        Field::Placement => Some(Placement::from_index(i).name().into()),
        Field::Freq | Field::TriggerLow | Field::TriggerHigh => Some(format_hz(value)),
        Field::Q => Some(format!("{value:.2}")),
        Field::Range if value.abs() < 0.05 => Some("Off".into()),
        Field::Threshold if value >= -0.05 => Some("Auto".into()),
        Field::Attack | Field::Release | Field::Density => Some(format!("{:.0} %", value * 100.0)),
        Field::Key => Some(if value >= 0.5 { "Sidechain" } else { "Input" }.into()),
        Field::Trigger => Some(if value >= 0.5 { "Free" } else { "Band" }.into()),
        Field::Dynamics => Some(if value >= 0.5 { "Custom" } else { "Auto" }.into()),
        Field::Spectral | Field::SpectralTilt | Field::DynBypass => on_off(value),
        _ => None,
    }
}

/// An output pan as the EQ shows it.
pub fn format_pan(value: f64, mid_side: bool) -> String {
    let pc = (value.abs() * 100.0).round();
    if pc < 0.5 {
        "Centre".into()
    } else {
        let (a, b) = if mid_side { ("M", "S") } else { ("L", "R") };
        format!("{}{pc:.0}", if value < 0.0 { a } else { b })
    }
}

pub fn format_hz(hz: f64) -> String {
    if hz >= 10_000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else if hz >= 1000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else if hz >= 100.0 {
        format!("{hz:.0} Hz")
    } else {
        format!("{hz:.1} Hz")
    }
}

/// Gain-Q interaction (bells): the Q narrows as the gain rises, and a very
/// narrow bell gains a little.
pub fn gain_q(kind: BandType, gain: f64, q: f64) -> (f64, f64) {
    if kind != BandType::Bell {
        return (gain, q);
    }
    let q = q * (1.0 + 0.03 * gain.abs());
    let gain = if q > 3.0 {
        gain * (1.0 + 0.12 * (q / 3.0).log2())
    } else {
        gain
    };
    (gain.clamp(-36.0, 36.0), q)
}

/// A band's settings read off the parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandParams {
    /// Heard (state "on").
    pub enabled: bool,
    /// Shown (on or bypassed).
    pub used: bool,
    pub kind: BandType,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub slope: f64,
    pub placement: Placement,
    pub range: f64,
    pub threshold: f64,
    pub external: bool,
    pub attack: f64,
    pub release: f64,
    pub free: bool,
    pub trigger_low: f64,
    pub trigger_high: f64,
    pub spectral: bool,
    pub density: f64,
    pub spectral_tilt: bool,
    pub custom: bool,
    pub dyn_bypass: bool,
}

impl BandParams {
    pub fn read(params: &ParamValues, band: usize) -> Self {
        let v = |f: Field| f64::from(params.get(band_index(band, f)));
        let on = |f: Field| v(f) >= 0.5;
        let state = v(Field::Enabled).round();
        Self {
            enabled: state == 1.0,
            used: state >= 1.0,
            kind: BandType::from_index(v(Field::Type).round().max(0.0) as usize),
            freq: v(Field::Freq),
            gain: v(Field::Gain),
            q: v(Field::Q),
            slope: v(Field::Slope),
            placement: Placement::from_index(v(Field::Placement).round().max(0.0) as usize),
            range: v(Field::Range),
            threshold: v(Field::Threshold),
            external: on(Field::Key),
            attack: v(Field::Attack),
            release: v(Field::Release),
            free: on(Field::Trigger),
            trigger_low: v(Field::TriggerLow),
            trigger_high: v(Field::TriggerHigh),
            spectral: on(Field::Spectral),
            density: v(Field::Density),
            spectral_tilt: on(Field::SpectralTilt),
            custom: on(Field::Dynamics),
            dyn_bypass: on(Field::DynBypass),
        }
    }

    /// The band's static shape with the gain scaled by `scale` (and
    /// gain-Q interaction when `interact`).
    pub fn shape(&self, scale: f64, interact: bool) -> BandShape {
        self.shape_with(self.gain * scale, interact)
    }

    /// The shape at gain `gain` (dB, already scaled).
    pub fn shape_with(&self, gain: f64, interact: bool) -> BandShape {
        let (gain, q) = if interact {
            gain_q(self.kind, gain, self.q)
        } else {
            (gain, self.q)
        };
        BandShape {
            kind: self.kind,
            freq: self.freq,
            gain,
            q,
            slope: self.kind.snap_slope(self.slope),
        }
    }

    /// Whether the band moves with the level.
    pub fn dynamic(&self) -> bool {
        self.kind.has_gain() && self.range.abs() >= 0.05
    }

    /// Whether the band works spectrally (its dynamics per frequency).
    pub fn is_spectral(&self) -> bool {
        self.kind.has_gain() && self.spectral
    }

    /// Whether the dynamics act (dynamic and not bypassed).
    pub fn dynamics_on(&self) -> bool {
        self.dynamic() && !self.dyn_bypass
    }

    /// Whether the threshold follows the trigger.
    pub fn auto_threshold(&self) -> bool {
        !self.custom || self.threshold >= -0.05
    }

    /// Attack and release factors of the automatic times (custom mode).
    pub fn time_factors(&self) -> (f64, f64) {
        if !self.custom {
            return (1.0, 1.0);
        }
        let f = |p: f64| 8f64.powf((p.clamp(0.0, 1.0) - 0.5) * 2.0);
        (f(self.attack), f(self.release))
    }

    /// Whether the trigger is the sidechain.
    pub fn keyed_externally(&self) -> bool {
        self.custom && self.external
    }

    /// Whether the trigger is filtered freely.
    pub fn free_trigger(&self) -> bool {
        self.custom && self.free
    }
}

/// The level the dynamic range is fully applied at, over the threshold, is
/// as many dB as the range itself (at least this many).
const DYN_SPAN_MIN: f64 = 3.0;
/// The soft knee's width (dB).
pub const KNEE: f64 = 6.0;

/// How far a dynamic band has moved for a key level `env_db`: from the
/// knee's start below the threshold, by the range over as many dB as the
/// range itself.
pub fn dynamic_gain(range: f64, threshold: f64, env_db: f64) -> f64 {
    let over = env_db - threshold;
    let soft = if over <= -KNEE / 2.0 {
        0.0
    } else if over >= KNEE / 2.0 {
        over
    } else {
        (over + KNEE / 2.0).powi(2) / (2.0 * KNEE)
    };
    let span = range.abs().max(DYN_SPAN_MIN);
    range * (soft / span).min(1.0)
}

/// The loudness change the bands make to pink noise, in dB.
pub fn loudness_change(bands: &[BandParams], scale: f64, interact: bool, sample_rate: f64) -> f64 {
    let mut scratch = [([Coefs::IDENTITY; MAX_SECTIONS], 0usize); BANDS];
    loudness_change_with(bands, scale, interact, sample_rate, &mut scratch)
}

/// [`loudness_change`] with the band designs in `scratch` (allocation
/// free, for the audio thread).
fn loudness_change_with(
    bands: &[BandParams],
    scale: f64,
    interact: bool,
    sample_rate: f64,
    scratch: &mut [([Coefs; MAX_SECTIONS], usize); BANDS],
) -> f64 {
    const POINTS: usize = 48;
    let mut used = 0;
    for b in bands.iter().filter(|b| b.enabled).take(BANDS) {
        let (s, n) = &mut scratch[used];
        *n = design::design(&b.shape(scale, interact), sample_rate, s);
        used += 1;
    }
    let mut power = 0.0;
    for i in 0..POINTS {
        // Equal weight per octave from 30 Hz to 16 kHz: pink noise.
        let f = 30.0 * (16_000.0f64 / 30.0).powf(i as f64 / (POINTS - 1) as f64);
        let w = 2.0 * std::f64::consts::PI * f / sample_rate;
        let m: f64 = scratch[..used]
            .iter()
            .flat_map(|(s, n)| s[..*n].iter())
            .map(|c| c.magnitude2(w))
            .product();
        power += m;
    }
    10.0 * (power / POINTS as f64).max(1e-12).log10()
}

/// Control steps between two loudness estimates while the bands move.
const AUTO_EVERY: u32 = 8;

/// Samples per control step: parameters are read, smoothed and turned into
/// coefficients this often.
const STEP: usize = 16;
/// Time constant of the parameter glide (seconds).
const GLIDE: f64 = 0.015;
/// How long a band takes to fade in or out (seconds).
const FADE: f64 = 0.008;
/// How long the bypass takes to cross over (seconds).
const BYPASS_RAMP: f64 = 0.01;
/// How fast a dynamic band's gain follows its detector (seconds): just
/// enough to keep the coefficient steps inaudible.
const DYN_SMOOTH: f64 = 0.001;

#[derive(Clone, Copy, Default)]
pub(crate) struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    pub(crate) fn run(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    pub(crate) fn flush(&mut self) {
        if self.z1.abs() < 1e-25 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-25 {
            self.z2 = 0.0;
        }
    }
}

/// What makes a band's sections a different structure (changing it fades).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Structure {
    kind: BandType,
    /// Cuts: the whole order and whether a fraction follows (the fraction
    /// itself glides); others: the slope.
    order: (u32, bool),
    placement: Placement,
    /// Where the band runs: the minimum phase sections (or elsewhere: the
    /// linear phase FIR, the spectral stage).
    sections: bool,
}

fn structure(p: &BandParams, mode: PhaseMode) -> Structure {
    let slope = p.kind.snap_slope(p.slope);
    let order = if p.kind.is_cut() {
        if slope > 96.5 {
            (32, false)
        } else {
            let o = slope / 6.0;
            let whole = (o + 0.03).floor();
            (whole as u32, o - whole > 0.03)
        }
    } else {
        (slope.round() as u32, false)
    };
    let sections = !p.is_spectral() && (mode != PhaseMode::Linear || p.dynamic());
    Structure {
        kind: p.kind,
        order,
        placement: p.placement,
        sections,
    }
}

/// The live part of one band.
struct BandDsp {
    sections: [Coefs; MAX_SECTIONS],
    used: usize,
    state: [[State; MAX_SECTIONS]; 2],
    /// The structure running (what changes by fading).
    structure: Structure,
    /// Gliding values: log2 frequency, gain (dB), log2 Q, slope.
    lf: f64,
    gain: f64,
    lq: f64,
    slope: f64,
    /// What the coefficients were designed for.
    designed: Option<[f64; 6]>,
    /// 0 (out) to 1 (in), and where it is going.
    mix: f64,
    mix_target: f64,
    detector: Detector,
    /// Whether the detector runs (the band's dynamics act here).
    detecting: bool,
    dyn_db: f64,
    fresh: bool,
}

impl Default for BandDsp {
    fn default() -> Self {
        Self {
            sections: [Coefs::IDENTITY; MAX_SECTIONS],
            used: 0,
            state: [[State::default(); MAX_SECTIONS]; 2],
            structure: Structure {
                kind: BandType::Bell,
                order: (12, false),
                placement: Placement::Stereo,
                sections: true,
            },
            lf: 10.0,
            gain: 0.0,
            lq: -0.5,
            slope: 12.0,
            designed: None,
            mix: 0.0,
            mix_target: 0.0,
            detector: Detector::default(),
            detecting: false,
            dyn_db: 0.0,
            fresh: true,
        }
    }
}

impl BandDsp {
    fn reset_state(&mut self) {
        self.state = [[State::default(); MAX_SECTIONS]; 2];
        self.detector.reset();
        self.dyn_db = 0.0;
    }

    fn active(&self) -> bool {
        self.mix > 0.0 || self.mix_target > 0.0
    }
}

/// A delay line of `C` channels.
struct Delay<const C: usize> {
    buf: Vec<[f64; C]>,
    pos: usize,
}

impl<const C: usize> Delay<C> {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![[0.0; C]; len],
            pos: 0,
        }
    }

    #[inline]
    fn tick(&mut self, x: [f64; C]) -> [f64; C] {
        if self.buf.is_empty() {
            return x;
        }
        let y = std::mem::replace(&mut self.buf[self.pos], x);
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        y
    }

    fn reset(&mut self) {
        self.buf.fill([0.0; C]);
    }
}

/// Listening to a band's region, or to its trigger.
struct Listen {
    setting: usize,
    filters: [Coefs; TRIGGER_SECTIONS],
    used: usize,
    state: [[State; TRIGGER_SECTIONS]; 2],
}

/// The output stage's smoothed settings.
#[derive(Clone, Copy)]
struct Output {
    gain: f64,
    /// Pan gains: left and right (or mid and side when `mid_side`).
    pan: [f64; 2],
    mid_side: bool,
    sign: f64,
    /// 0: processed, 1: bypassed.
    bypass: f64,
}

pub struct EqProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    /// Fixed when the processor is made (a change rebuilds it, since the
    /// latency changes).
    mode: PhaseMode,
    bands: Box<[BandDsp; BANDS]>,
    glide: f64,
    fade_step: f64,
    dyn_coeff: f64,
    out: Output,
    auto: f64,
    auto_target: f64,
    /// The static settings the auto gain was computed for.
    auto_for: Option<([BandParams; BANDS], f64, bool)>,
    /// Steps until the next loudness estimate may run, and its scratch.
    auto_wait: u32,
    auto_scratch: Box<[([Coefs; MAX_SECTIONS], usize); BANDS]>,
    listen: Option<Listen>,
    character: CharacterStage,
    meters: [[MeterTap; 2]; 2],
    /// Inputs copied for the rings when the output buffer is the input.
    scratch: [Vec<f32>; 2],
    /// Spectral bands (when any band is spectral).
    spectral: Option<Box<spectral::Spectral>>,
    /// Linear phase: the static bands; natural phase: the correction after
    /// the sections.
    fir: Option<Box<fir::Convolver>>,
    /// The triggers (input and sidechain), delayed to where the sections
    /// run.
    keys: Delay<4>,
    /// The input, delayed by the whole latency (for the bypass).
    dry: Delay<2>,
}

impl EqProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mode = PhaseMode::of(&params);
        let [spectral_latency, fir_latency] = stage_latencies(&params);
        let spectral = (spectral_latency > 0).then(|| {
            Box::new(spectral::Spectral::new(
                params.clone(),
                Arc::clone(&tap),
                sr,
                spectral::frame(quality(&params)),
            ))
        });
        let fir = match mode {
            PhaseMode::ZeroLatency => None,
            PhaseMode::Linear => Some(Box::new(fir::Convolver::new(
                fir::Kind::Linear(linear::LENGTHS[quality(&params)]),
                &params,
                sr,
            ))),
            PhaseMode::Natural => Some(Box::new(fir::Convolver::new(
                fir::Kind::Natural,
                &params,
                sr,
            ))),
        };
        debug_assert_eq!(fir.as_ref().map_or(0, |f| f.latency()), fir_latency);
        debug_assert_eq!(
            spectral.as_ref().map_or(0, |s| s.latency() as u32),
            spectral_latency
        );
        let before_sections = spectral_latency
            + if mode == PhaseMode::Linear {
                fir_latency
            } else {
                0
            };
        let mut p = Self {
            params,
            watching: Watching::new(sr as f32),
            sr,
            mode,
            bands: Box::new(std::array::from_fn(|_| BandDsp::default())),
            glide: (-(STEP as f64) / (GLIDE * sr)).exp(),
            fade_step: 1.0 / (FADE * sr),
            dyn_coeff: (-(STEP as f64) / (DYN_SMOOTH * sr)).exp(),
            out: Output {
                gain: 1.0,
                pan: [1.0, 1.0],
                mid_side: false,
                sign: 1.0,
                bypass: 0.0,
            },
            auto: 1.0,
            auto_target: 1.0,
            auto_for: None,
            auto_wait: 0,
            auto_scratch: Box::new([([Coefs::IDENTITY; MAX_SECTIONS], 0); BANDS]),
            listen: None,
            character: CharacterStage::new(sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            spectral,
            fir,
            keys: Delay::new(before_sections as usize),
            dry: Delay::new((spectral_latency + fir_latency) as usize),
            tap,
        };
        p.control(true);
        p
    }

    /// The latency the processor runs at.
    pub fn latency(&self) -> u32 {
        (self.dry.buf.len()) as u32
    }

    fn scale(&self) -> f64 {
        f64::from(self.params.get(global::GAIN_SCALE))
    }

    /// Read the parameters and move everything one control step on.
    fn control(&mut self, jump: bool) {
        let scale = self.scale();
        let interact = self.params.get(global::GAIN_Q) >= 0.5;
        let mut statics = [BandParams::read(&self.params, 0); BANDS];
        for (b, slot) in statics.iter_mut().enumerate().skip(1) {
            *slot = BandParams::read(&self.params, b);
        }
        let sr = self.sr;
        let mode = self.mode;
        let glide = if jump { 0.0 } else { self.glide };
        let dyn_k = if jump { 0.0 } else { self.dyn_coeff };
        let step_secs = STEP as f64 / sr;
        for (b, band) in self.bands.iter_mut().enumerate() {
            let p = statics[b];
            let wanted = structure(&p, mode);
            if band.fresh || (wanted != band.structure && band.mix <= 0.0) {
                // Out of the signal: change over and come back in.
                band.structure = wanted;
                band.reset_state();
                band.lf = p.freq.max(1.0).log2();
                band.gain = p.gain * scale;
                band.lq = p.q.max(0.001).log2();
                band.slope = p.kind.snap_slope(p.slope);
                band.designed = None;
                band.fresh = false;
            }
            band.mix_target = if p.enabled && wanted == band.structure && wanted.sections {
                1.0
            } else {
                0.0
            };
            if jump {
                band.mix = band.mix_target;
            }
            // Spectral and linear phase static bands are elsewhere; their
            // published values come from there.
            band.detecting = band.active() && p.dynamics_on() && wanted.sections;
            if !band.active() {
                band.dyn_db = 0.0;
                if !p.is_spectral() {
                    self.tap.set_value(value::DYN + b, 0.0);
                }
                continue;
            }
            // Glide towards the settings (a band not heard yet starts
            // where it is set).
            let glide = if band.mix <= 0.0 { 0.0 } else { glide };
            let g = |cur: f64, to: f64| to + (cur - to) * glide;
            band.lf = g(band.lf, p.freq.max(1.0).log2());
            band.gain = g(band.gain, p.gain * scale);
            band.lq = g(band.lq, p.q.max(0.001).log2());
            band.slope = g(band.slope, p.kind.snap_slope(p.slope));
            // The key level and how far the band has moved with it.
            let mut dyn_target = 0.0;
            if band.detecting {
                let freq = band.lf.exp2();
                band.detector.configure(&p, freq, band.lq.exp2(), sr);
                band.detector.control(step_secs);
                let level = band.detector.level_db();
                let threshold = if p.auto_threshold() {
                    band.detector.auto_threshold()
                } else {
                    p.threshold
                };
                dyn_target = dynamic_gain(p.range * scale, threshold, level);
                self.tap.set_value(value::KEY + b, level as f32);
                self.tap.set_value(value::THRESHOLD + b, threshold as f32);
            }
            band.dyn_db = dyn_target + (band.dyn_db - dyn_target) * dyn_k;
            if band.dyn_db.abs() < 1e-4 && dyn_target == 0.0 {
                band.dyn_db = 0.0;
            }
            self.tap.set_value(value::DYN + b, band.dyn_db as f32);
            let want = [
                band.lf,
                band.gain,
                band.lq,
                band.dyn_db,
                band.slope,
                f64::from(u8::from(interact)),
            ];
            let changed = band.designed.is_none_or(|d| {
                (d[0] - want[0]).abs() > 1e-5
                    || (d[1] - want[1]).abs() > 1e-4
                    || (d[2] - want[2]).abs() > 1e-5
                    || (d[3] - want[3]).abs() > 1e-3
                    || (d[4] - want[4]).abs() > 1e-3
                    || d[5] != want[5]
            });
            if changed {
                let mut shape = BandParams {
                    freq: band.lf.exp2(),
                    q: band.lq.exp2(),
                    ..p
                }
                .shape_with(band.gain + band.dyn_db, interact);
                shape.kind = band.structure.kind;
                shape.slope = band.slope;
                band.used = design::design(&shape, sr, &mut band.sections);
                band.designed = Some(want);
            }
        }
        // Output: gain, auto gain, pan, phase and bypass.
        let g = |cur: f64, to: f64| to + (cur - to) * glide;
        let out = 10f64.powf(f64::from(self.params.get(global::OUTPUT)) / 20.0);
        self.out.gain = g(self.out.gain, out);
        let pan = f64::from(self.params.get(global::PAN)).clamp(-1.0, 1.0);
        let pan_to = [1.0 - pan.max(0.0), 1.0 + pan.min(0.0)];
        let mid_side = self.params.get(global::PAN_MODE) >= 0.5;
        if mid_side != self.out.mid_side {
            // Switch over at the centre: no jump either way.
            self.out.pan = [1.0, 1.0];
            self.out.mid_side = mid_side;
        }
        self.out.pan = [g(self.out.pan[0], pan_to[0]), g(self.out.pan[1], pan_to[1])];
        let sign = if self.params.get(global::INVERT) >= 0.5 {
            -1.0
        } else {
            1.0
        };
        self.out.sign = g(self.out.sign, sign);
        let bypass = if self.params.get(global::BYPASS) >= 0.5 {
            1.0
        } else {
            0.0
        };
        // The bypass ramps (and ends exactly).
        let ramp = if jump {
            1.0
        } else {
            STEP as f64 / (BYPASS_RAMP * sr)
        };
        self.out.bypass += (bypass - self.out.bypass).clamp(-ramp, ramp);
        let auto_on = self.params.get(global::AUTO_GAIN) >= 0.5;
        if auto_on {
            let key = (statics, scale, interact);
            self.auto_wait = self.auto_wait.saturating_sub(1);
            if self.auto_for.as_ref() != Some(&key) && (jump || self.auto_wait == 0) {
                self.auto_wait = AUTO_EVERY;
                let change =
                    loudness_change_with(&statics, scale, interact, sr, &mut self.auto_scratch);
                self.auto_target = 10f64.powf(-change / 20.0);
                self.auto_for = Some(key);
            }
        } else {
            self.auto_target = 1.0;
            self.auto_for = None;
        }
        self.auto = g(self.auto, self.auto_target);
        self.character.set(Character::from_index(
            self.params.get(global::CHARACTER).round().max(0.0) as usize,
        ));
        // Listening to one band (or its trigger) on its own.
        self.update_listen(&statics);
    }

    fn update_listen(&mut self, statics: &[BandParams; BANDS]) {
        let want = self.tap.listen().filter(|s| *s < 2 * BANDS);
        let Some(setting) = want else {
            self.listen = None;
            return;
        };
        let (b, key) = if setting < BANDS {
            (setting, false)
        } else {
            (setting - BANDS, true)
        };
        let p = &statics[b];
        let band = &self.bands[b];
        let freq = if band.fresh { p.freq } else { band.lf.exp2() };
        let (filters, used) = if key {
            dynamics::trigger_filters(p, freq, p.q, self.sr)
        } else {
            region_filters(p, freq, self.sr)
        };
        match &mut self.listen {
            Some(l) if l.setting == setting => {
                l.filters = filters;
                l.used = used;
            }
            l => {
                *l = Some(Listen {
                    setting,
                    filters,
                    used,
                    state: [[State::default(); TRIGGER_SECTIONS]; 2],
                });
            }
        }
    }

    /// Process `left`/`right` in place; `side` is the sidechain.
    fn run(&mut self, left: &mut [f32], right: &mut [f32], side: Option<(&[f32], &[f32])>) {
        let n = left.len().min(right.len());
        let fade = self.fade_step;
        let mode = self.mode;
        for i in 0..n {
            let x = [f64::from(left[i]), f64::from(right[i])];
            let ext = side.map_or([0.0, 0.0], |(a, b)| [f64::from(a[i]), f64::from(b[i])]);
            let dry = self.dry.tick(x);
            let [mut l, mut r] = x;
            if let Some(s) = self.spectral.as_mut() {
                (l, r) = s.process(l, r, ext);
            }
            if mode == PhaseMode::Linear
                && let Some(f) = self.fir.as_mut()
            {
                f.process(&mut l, &mut r);
            }
            let [kl, kr, el, er] = self.keys.tick([x[0], x[1], ext[0], ext[1]]);
            for band in self.bands.iter_mut() {
                if !band.active() {
                    continue;
                }
                // Fade towards where the band is going.
                if band.mix < band.mix_target {
                    band.mix = (band.mix + fade).min(band.mix_target);
                } else if band.mix > band.mix_target {
                    band.mix = (band.mix - fade).max(band.mix_target);
                }
                if band.detecting {
                    if band.detector.external {
                        band.detector.feed(el, er, band.structure.placement);
                    } else {
                        band.detector.feed(kl, kr, band.structure.placement);
                    }
                }
                let mix = band.mix;
                let used = band.used;
                let sections = &band.sections;
                let cascade = |st: &mut [State; MAX_SECTIONS], x: f64| {
                    let mut y = x;
                    for (s, c) in st.iter_mut().zip(sections.iter()).take(used) {
                        y = s.run(c, y);
                    }
                    x + mix * (y - x)
                };
                match band.structure.placement {
                    Placement::Stereo => {
                        let [s0, s1] = &mut band.state;
                        l = cascade(s0, l);
                        r = cascade(s1, r);
                    }
                    Placement::Left => l = cascade(&mut band.state[0], l),
                    Placement::Right => r = cascade(&mut band.state[1], r),
                    Placement::Mid | Placement::Side => {
                        let (mut m, mut s) = (0.5 * (l + r), 0.5 * (l - r));
                        if band.structure.placement == Placement::Mid {
                            m = cascade(&mut band.state[0], m);
                        } else {
                            s = cascade(&mut band.state[0], s);
                        }
                        l = m + s;
                        r = m - s;
                    }
                }
            }
            if mode == PhaseMode::Natural
                && let Some(f) = self.fir.as_mut()
            {
                f.process(&mut l, &mut r);
            }
            if let Some(ls) = self.listen.as_mut() {
                // A band's region from the input; a trigger from where the
                // band hears it.
                let (a, b) = if ls.setting >= BANDS {
                    let band = &self.bands[ls.setting - BANDS];
                    if band.detector.external {
                        (el, er)
                    } else {
                        (kl, kr)
                    }
                } else {
                    (x[0], x[1])
                };
                let used = ls.used;
                let filters = &ls.filters;
                let run = |st: &mut [State; TRIGGER_SECTIONS], v: f64| {
                    let mut y = v;
                    for (s, c) in st.iter_mut().zip(filters.iter()).take(used) {
                        y = s.run(c, y);
                    }
                    y
                };
                l = run(&mut ls.state[0], a);
                r = run(&mut ls.state[1], b);
            }
            (l, r) = self.character.process(l, r);
            let o = &self.out;
            let gain = o.gain * self.auto * o.sign;
            if o.mid_side {
                let (m, s) = (0.5 * (l + r) * o.pan[0], 0.5 * (l - r) * o.pan[1]);
                l = m + s;
                r = m - s;
            } else {
                l *= o.pan[0];
                r *= o.pan[1];
            }
            l *= gain;
            r *= gain;
            if o.bypass > 0.0 {
                let b = o.bypass;
                l = dry[0] * b + l * (1.0 - b);
                r = dry[1] * b + r * (1.0 - b);
            }
            left[i] = l as f32;
            right[i] = r as f32;
        }
        for band in self.bands.iter_mut() {
            for st in band.state.iter_mut().flatten() {
                st.flush();
            }
            band.detector.flush();
        }
    }
}

/// The filters that let a band's region through (to listen to it): a band
/// pass round a bell, notch or band pass; what a cut takes away; the side
/// a shelf works on.
fn region_filters(p: &BandParams, freq: f64, sr: f64) -> ([Coefs; TRIGGER_SECTIONS], usize) {
    let (kind, q) = match p.kind {
        BandType::LowShelf | BandType::LowCut => (BandType::HighCut, 0.707),
        BandType::HighShelf | BandType::HighCut => (BandType::LowCut, 0.707),
        BandType::Bell | BandType::Notch | BandType::BandPass => (BandType::BandPass, p.q.max(0.3)),
        BandType::TiltShelf | BandType::FlatTilt | BandType::AllPass => (BandType::BandPass, 0.7),
    };
    let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
    let n = design::design(
        &BandShape {
            kind,
            freq,
            gain: 0.0,
            q,
            slope: 12.0,
        },
        sr,
        &mut s,
    );
    let mut out = [Coefs::IDENTITY; TRIGGER_SECTIONS];
    out[0] = s[0];
    (out, n.min(1))
}

impl PluginProcessor for EqProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let side = io.audio_in.get(1).filter(|k| k.num_channels() > 0);
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        let channels = out.num_channels();
        match io.audio_in.first() {
            Some(input) => out.copy_from(input),
            None => out.clear(),
        }
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        // The input as it arrived, for the meters and the analyser.
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let mut events = ctx.param_events.iter().peekable();
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                } else {
                    break;
                }
            }
            self.control(false);
            let side_slices = side.map(|k| {
                let a = &k.channel(0)[at..end];
                let b = &k.channel(1.min(k.num_channels() - 1))[at..end];
                (a, b)
            });
            if channels >= 2 {
                let (l, r) = out.channel_pair_mut(0, 1);
                self.run(&mut l[at..end], &mut r[at..end], side_slices);
            } else {
                // Mono: both sides of the stereo EQ see the one channel.
                let ch = out.channel_mut(0);
                let mut right = [0.0f32; STEP];
                let len = end - at;
                right[..len].copy_from_slice(&ch[at..end]);
                self.run(&mut ch[at..end], &mut right[..len], side_slices);
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        // Meters always, the analyser rings while an editor watches.
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
            if let Some(k) = side {
                let r = k.channel(1.min(k.num_channels() - 1));
                self.tap.sidechain.push(&k.channel(0)[..n], &r[..n]);
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for band in self.bands.iter_mut() {
            band.reset_state();
        }
        if let Some(f) = self.fir.as_mut() {
            f.reset();
        }
        if let Some(s) = self.spectral.as_mut() {
            s.reset();
        }
        if let Some(l) = self.listen.as_mut() {
            l.state = [[State::default(); TRIGGER_SECTIONS]; 2];
        }
        self.keys.reset();
        self.dry.reset();
        self.character.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
