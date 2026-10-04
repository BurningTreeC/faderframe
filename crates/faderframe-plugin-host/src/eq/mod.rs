//! FaderFrame EQ: a 24 band parametric equaliser.
//!
//! * Bells, low and high shelves, low and high cuts (6 to 96 dB/oct),
//!   notches, band passes and tilt shelves, each designed to keep the
//!   analog shape up to Nyquist ([`design`]).
//! * Per band stereo placement (both channels, left, right, mid, side) and
//!   a dynamic range: the band moves by up to that many dB as the level in
//!   its own frequency region rises over its threshold — keyed by the
//!   signal itself or by the sidechain input.
//! * Changes are click free: frequency, gain and Q glide, and a band that
//!   is switched on or off or changes its type, slope or placement fades
//!   out and back in.
//! * Auto gain keeps the loudness of pink noise where it was; a gain scale
//!   turns every band up or down at once; any band can be heard on its own.
//!
//! The processor publishes each band's live dynamic gain through the
//! [`AnalysisTap`] and fills its audio rings for the editor's analyser.

pub mod design;
pub mod linear;

use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use design::{BandShape, BandType, Coefs, MAX_SECTIONS, SLOPES};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

/// Bands the EQ has.
pub const BANDS: usize = 24;
/// Parameters before the first band's.
pub const GLOBALS: usize = 8;
/// Parameters per band.
pub const FIELDS: usize = 9;
/// Parameter ids of band `b` start at `BAND_BASE + b * BAND_STRIDE`.
pub const BAND_BASE: u32 = 100;
pub const BAND_STRIDE: u32 = 16;

/// Global parameters (ids and indexes).
pub mod global {
    pub const OUTPUT: usize = 0;
    pub const AUTO_GAIN: usize = 1;
    pub const GAIN_SCALE: usize = 2;
    pub const PHASE: usize = 3;
    pub const QUALITY: usize = 4;
    pub const ATTACK: usize = 5;
    pub const RELEASE: usize = 6;
    pub const SIDECHAIN: usize = 7;
}

/// A band's parameters, in order. `Enabled` is the band's state:
/// 0 unused (not shown), 1 on, 2 bypassed (shown, not heard).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Enabled = 0,
    Type = 1,
    Freq = 2,
    Gain = 3,
    Q = 4,
    Slope = 5,
    Placement = 6,
    Range = 7,
    Threshold = 8,
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
    ];
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
    ParameterId(BAND_BASE + band as u32 * BAND_STRIDE + field as u32)
}

/// Id of a global parameter.
pub fn global_id(g: usize) -> ParameterId {
    ParameterId(g as u32)
}

/// Where a new band's frequency sits by default.
pub fn default_freq(band: usize) -> f64 {
    let t = band as f64 / (BANDS - 1) as f64;
    40.0 * (12_000.0f64 / 40.0).powf(t)
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
        ParameterInfo {
            automatable: false,
            ..g(global::PHASE, "Phase Mode", 0.0, 1.0, 0.0, None, true)
        },
        ParameterInfo {
            automatable: false,
            ..g(
                global::QUALITY,
                "Linear Phase Quality",
                0.0,
                3.0,
                1.0,
                None,
                true,
            )
        },
        g(
            global::ATTACK,
            "Dynamic Attack",
            0.5,
            500.0,
            10.0,
            Milliseconds,
            false,
        ),
        g(
            global::RELEASE,
            "Dynamic Release",
            5.0,
            5000.0,
            150.0,
            Milliseconds,
            false,
        ),
        g(
            global::SIDECHAIN,
            "External Sidechain",
            0.0,
            1.0,
            0.0,
            None,
            true,
        ),
    ];
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
                "Type",
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
                (SLOPES.len() - 1) as f64,
                1.0,
                None,
                true,
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
                -30.0,
                Decibels,
                false,
            ),
        ]);
    }
    out
}

/// A parameter value as the EQ shows it (`None`: the generic formatting).
pub fn format(id: ParameterId, value: f64) -> Option<String> {
    let raw = id.0;
    if raw < BAND_BASE {
        return match raw as usize {
            global::AUTO_GAIN | global::SIDECHAIN => {
                Some(if value >= 0.5 { "On" } else { "Off" }.into())
            }
            global::PHASE => Some(
                if value >= 0.5 {
                    "Linear Phase"
                } else {
                    "Zero Latency"
                }
                .into(),
            ),
            global::QUALITY => Some(
                ["Low", "Medium", "High", "Maximum"]
                    .get(value.round().max(0.0) as usize)
                    .unwrap_or(&"High")
                    .to_string(),
            ),
            global::GAIN_SCALE => Some(format!("{:.0} %", value * 100.0)),
            _ => None,
        };
    }
    let field = ((raw - BAND_BASE) % BAND_STRIDE) as usize;
    let i = value.round().max(0.0) as usize;
    match Field::ALL.get(field)? {
        Field::Enabled => Some(
            match value.round() as i64 {
                1 => "On",
                2 => "Bypassed",
                _ => "Unused",
            }
            .into(),
        ),
        Field::Type => Some(BandType::from_index(i).name().into()),
        Field::Slope => Some(format!("{} dB/oct", SLOPES[i.min(SLOPES.len() - 1)])),
        Field::Placement => Some(Placement::from_index(i).name().into()),
        Field::Freq => Some(format_hz(value)),
        Field::Q => Some(format!("{value:.2}")),
        Field::Range if value.abs() < 0.05 => Some("Off".into()),
        _ => None,
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
    pub slope: u32,
    pub placement: Placement,
    pub range: f64,
    pub threshold: f64,
}

impl BandParams {
    pub fn read(params: &ParamValues, band: usize) -> Self {
        let v = |f: Field| f64::from(params.get(band_index(band, f)));
        let state = v(Field::Enabled).round();
        Self {
            enabled: state == 1.0,
            used: state >= 1.0,
            kind: BandType::from_index(v(Field::Type).round().max(0.0) as usize),
            freq: v(Field::Freq),
            gain: v(Field::Gain),
            q: v(Field::Q),
            slope: SLOPES[(v(Field::Slope).round().max(0.0) as usize).min(SLOPES.len() - 1)],
            placement: Placement::from_index(v(Field::Placement).round().max(0.0) as usize),
            range: v(Field::Range),
            threshold: v(Field::Threshold),
        }
    }

    /// The band's static shape with the gain scaled by `scale`.
    pub fn shape(&self, scale: f64) -> BandShape {
        BandShape {
            kind: self.kind,
            freq: self.freq,
            gain: self.gain * scale,
            q: self.q,
            slope: self.slope,
        }
    }

    /// Whether the band moves with the level.
    pub fn dynamic(&self) -> bool {
        self.kind.has_gain() && self.range.abs() >= 0.05
    }
}

/// The level the dynamic range is fully applied at, over the threshold, is
/// as many dB as the range itself (at least this many).
const DYN_SPAN_MIN: f64 = 3.0;

/// How far a dynamic band has moved for a key level `env_db`.
pub fn dynamic_gain(range: f64, threshold: f64, env_db: f64) -> f64 {
    let over = (env_db - threshold).max(0.0);
    let span = range.abs().max(DYN_SPAN_MIN);
    range * (over / span).min(1.0)
}

/// The loudness change the bands make to pink noise, in dB.
pub fn loudness_change(bands: &[BandParams], scale: f64, sample_rate: f64) -> f64 {
    let mut scratch = [([Coefs::IDENTITY; MAX_SECTIONS], 0usize); BANDS];
    loudness_change_with(bands, scale, sample_rate, &mut scratch)
}

/// [`loudness_change`] with the band designs in `scratch` (allocation
/// free, for the audio thread).
fn loudness_change_with(
    bands: &[BandParams],
    scale: f64,
    sample_rate: f64,
    scratch: &mut [([Coefs; MAX_SECTIONS], usize); BANDS],
) -> f64 {
    const POINTS: usize = 48;
    let mut used = 0;
    for b in bands.iter().filter(|b| b.enabled).take(BANDS) {
        let (s, n) = &mut scratch[used];
        *n = design::design(&b.shape(scale), sample_rate, s);
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

#[derive(Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    fn run(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    fn flush(&mut self) {
        if self.z1.abs() < 1e-25 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-25 {
            self.z2 = 0.0;
        }
    }
}

/// The live part of one band.
struct BandDsp {
    sections: [Coefs; MAX_SECTIONS],
    used: usize,
    state: [[State; MAX_SECTIONS]; 2],
    /// The structure running (what changes by fading).
    kind: BandType,
    slope: u32,
    placement: Placement,
    /// Gliding values: log2 frequency, gain (dB), log2 Q.
    lf: f64,
    gain: f64,
    lq: f64,
    /// What the coefficients were designed for.
    designed: Option<(f64, f64, f64, f64)>,
    /// 0 (out) to 1 (in), and where it is going.
    mix: f64,
    mix_target: f64,
    /// The key detector: a band pass at the band's frequency.
    detector: Coefs,
    det_state: [State; 2],
    env: f64,
    dyn_db: f64,
    fresh: bool,
}

impl Default for BandDsp {
    fn default() -> Self {
        Self {
            sections: [Coefs::IDENTITY; MAX_SECTIONS],
            used: 0,
            state: [[State::default(); MAX_SECTIONS]; 2],
            kind: BandType::Bell,
            slope: 12,
            placement: Placement::Stereo,
            lf: 10.0,
            gain: 0.0,
            lq: -0.5,
            designed: None,
            mix: 0.0,
            mix_target: 0.0,
            detector: Coefs::IDENTITY,
            det_state: [State::default(); 2],
            env: 0.0,
            dyn_db: 0.0,
            fresh: true,
        }
    }
}

impl BandDsp {
    fn reset_state(&mut self) {
        self.state = [[State::default(); MAX_SECTIONS]; 2];
        self.det_state = [State::default(); 2];
        self.env = 0.0;
        self.dyn_db = 0.0;
    }

    fn active(&self) -> bool {
        self.mix > 0.0 || self.mix_target > 0.0
    }
}

pub struct EqProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    bands: Box<[BandDsp; BANDS]>,
    glide: f64,
    fade_step: f64,
    output: f64,
    auto: f64,
    auto_target: f64,
    /// The static settings the auto gain was computed for.
    auto_for: Option<([BandParams; BANDS], f64)>,
    /// Steps until the next loudness estimate may run, and its scratch.
    auto_wait: u32,
    auto_scratch: Box<[([Coefs; MAX_SECTIONS], usize); BANDS]>,
    listen: Option<(usize, Coefs, [State; 2])>,
    meters: [[MeterTap; 2]; 2],
    /// Inputs copied for the rings when the output buffer is the input.
    scratch: [Vec<f32>; 2],
    /// Linear phase: the static bands as FIRs (set when the processor is
    /// made; a change of mode rebuilds it, since the latency changes).
    linear: Option<Box<linear::LinearPhase>>,
}

impl EqProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            params,
            tap,
            watching: Watching::new(sr as f32),
            sr,
            bands: Box::new(std::array::from_fn(|_| BandDsp::default())),
            glide: (-(STEP as f64) / (GLIDE * sr)).exp(),
            fade_step: 1.0 / (FADE * sr),
            output: 1.0,
            auto: 1.0,
            auto_target: 1.0,
            auto_for: None,
            auto_wait: 0,
            auto_scratch: Box::new([([Coefs::IDENTITY; MAX_SECTIONS], 0); BANDS]),
            listen: None,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            linear: None,
        };
        if linear::wanted(&p.params) {
            p.linear = Some(Box::new(linear::LinearPhase::new(&p.params, sr)));
        }
        p.control(true);
        p
    }

    fn scale(&self) -> f64 {
        f64::from(self.params.get(global::GAIN_SCALE))
    }

    /// Read the parameters and move everything one control step on.
    fn control(&mut self, jump: bool) {
        let scale = self.scale();
        let attack = f64::from(self.params.get(global::ATTACK));
        let release = f64::from(self.params.get(global::RELEASE));
        let mut statics = [BandParams::read(&self.params, 0); BANDS];
        for (b, slot) in statics.iter_mut().enumerate() {
            *slot = BandParams::read(&self.params, b);
        }
        let sr = self.sr;
        let glide = if jump { 0.0 } else { self.glide };
        for (b, band) in self.bands.iter_mut().enumerate() {
            let p = statics[b];
            let structure = (p.kind, p.slope, p.placement);
            let running = (band.kind, band.slope, band.placement);
            if band.fresh || (structure != running && band.mix <= 0.0) {
                // Out of the signal: change over and come back in.
                band.kind = p.kind;
                band.slope = p.slope;
                band.placement = p.placement;
                band.reset_state();
                band.lf = p.freq.log2();
                band.gain = p.gain * scale;
                band.lq = p.q.log2();
                band.designed = None;
                band.fresh = false;
            }
            band.mix_target = if p.enabled && structure == (band.kind, band.slope, band.placement) {
                1.0
            } else {
                0.0
            };
            if jump {
                band.mix = band.mix_target;
            }
            if !band.active() {
                continue;
            }
            // Glide towards the settings.
            let g = |cur: f64, to: f64| to + (cur - to) * glide;
            band.lf = g(band.lf, p.freq.max(1.0).log2());
            band.gain = g(band.gain, p.gain * scale);
            band.lq = g(band.lq, p.q.max(0.001).log2());
            // The key level and how far the band has moved with it.
            let dyn_target = if p.dynamic() {
                let env_db = 20.0 * band.env.max(1e-9).log10();
                dynamic_gain(p.range * scale, p.threshold, env_db)
            } else {
                0.0
            };
            let coeff = if dyn_target.abs() > band.dyn_db.abs() {
                attack
            } else {
                release
            };
            let k = (-(STEP as f64) / (coeff.max(0.1) * 0.001 * sr)).exp();
            band.dyn_db = dyn_target + (band.dyn_db - dyn_target) * if jump { 0.0 } else { k };
            if band.dyn_db.abs() < 1e-4 && dyn_target == 0.0 {
                band.dyn_db = 0.0;
            }
            let want = (band.lf, band.gain, band.lq, band.dyn_db);
            let changed = band.designed.is_none_or(|d| {
                (d.0 - want.0).abs() > 1e-5
                    || (d.1 - want.1).abs() > 1e-4
                    || (d.2 - want.2).abs() > 1e-5
                    || (d.3 - want.3).abs() > 1e-3
            });
            if changed {
                let shape = BandShape {
                    kind: band.kind,
                    freq: band.lf.exp2(),
                    gain: band.gain + band.dyn_db,
                    q: band.lq.exp2(),
                    slope: band.slope,
                };
                band.used = design::design(&shape, sr, &mut band.sections);
                // The detector listens where the band works.
                let dq = if band.kind == BandType::Bell {
                    shape.q.max(0.3)
                } else {
                    0.7
                };
                let mut det = [Coefs::IDENTITY; MAX_SECTIONS];
                design::design(
                    &BandShape {
                        kind: BandType::BandPass,
                        freq: shape.freq,
                        gain: 0.0,
                        q: dq,
                        slope: 12,
                    },
                    sr,
                    &mut det,
                );
                band.detector = det[0];
                band.designed = Some(want);
            }
        }
        for (b, band) in self.bands.iter().enumerate() {
            self.tap.set_value(b, band.dyn_db as f32);
        }
        // Output and auto gain.
        let out = 10f64.powf(f64::from(self.params.get(global::OUTPUT)) / 20.0);
        self.output = if jump {
            out
        } else {
            out + (self.output - out) * self.glide
        };
        let auto_on = self.params.get(global::AUTO_GAIN) >= 0.5;
        if auto_on {
            let key = (statics, scale);
            self.auto_wait = self.auto_wait.saturating_sub(1);
            if self.auto_for.as_ref() != Some(&key) && (jump || self.auto_wait == 0) {
                self.auto_wait = AUTO_EVERY;
                let change = loudness_change_with(&statics, scale, sr, &mut self.auto_scratch);
                self.auto_target = 10f64.powf(-change / 20.0);
                self.auto_for = Some(key);
            }
        } else {
            self.auto_target = 1.0;
            self.auto_for = None;
        }
        self.auto = if jump {
            self.auto_target
        } else {
            self.auto_target + (self.auto - self.auto_target) * self.glide
        };
        // Listening to one band on its own.
        let want = self.tap.listen().filter(|b| *b < BANDS);
        match (want, &mut self.listen) {
            (None, l) => *l = None,
            (Some(b), l) => {
                let band = &self.bands[b];
                let freq = band.lf.exp2();
                let (kind, q) = match band.kind {
                    BandType::LowShelf | BandType::LowCut => (BandType::HighCut, 0.707),
                    BandType::HighShelf | BandType::HighCut => (BandType::LowCut, 0.707),
                    BandType::Bell | BandType::Notch | BandType::BandPass => {
                        (BandType::BandPass, band.lq.exp2().max(0.3))
                    }
                    BandType::TiltShelf => (BandType::BandPass, 0.7),
                };
                let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
                design::design(
                    &BandShape {
                        kind,
                        freq,
                        gain: 0.0,
                        q,
                        slope: 12,
                    },
                    sr,
                    &mut s,
                );
                match l {
                    Some((lb, c, _)) if *lb == b => *c = s[0],
                    _ => *l = Some((b, s[0], [State::default(); 2])),
                }
            }
        }
    }

    /// Process frames `from..to` of `left`/`right` in place; `key` is the
    /// sidechain (or the input).
    fn run(&mut self, left: &mut [f32], right: &mut [f32], key: Option<(&[f32], &[f32])>) {
        let attack = f64::from(self.params.get(global::ATTACK));
        let release = f64::from(self.params.get(global::RELEASE));
        let ka = (-1.0 / (attack.max(0.1) * 0.001 * self.sr)).exp();
        let kr = (-1.0 / (release.max(1.0) * 0.001 * self.sr)).exp();
        let n = left.len().min(right.len());
        let gain = self.output * self.auto;
        let fade = self.fade_step;
        let listen = &mut self.listen;
        let linear = &mut self.linear;
        // In linear phase the static bands are in the FIR.
        let fir = linear.is_some();
        let mut dynamic = [false; BANDS];
        if fir {
            for (b, d) in dynamic.iter_mut().enumerate() {
                *d = BandParams::read(&self.params, b).dynamic();
            }
        }
        for i in 0..n {
            let (in_l, in_r) = (f64::from(left[i]), f64::from(right[i]));
            let (mut l, mut r) = (in_l, in_r);
            if let Some(lp) = linear.as_mut() {
                lp.process(&mut l, &mut r);
            }
            let (kl, kr_) = key.map_or((l, r), |(a, b)| (f64::from(a[i]), f64::from(b[i])));
            for (b, band) in self.bands.iter_mut().enumerate() {
                if !band.active() || (fir && !dynamic[b]) {
                    continue;
                }
                // Fade towards where the band is going.
                if band.mix < band.mix_target {
                    band.mix = (band.mix + fade).min(band.mix_target);
                } else if band.mix > band.mix_target {
                    band.mix = (band.mix - fade).max(band.mix_target);
                }
                let mix = band.mix;
                // Key level in the band's region.
                let det_in = match band.placement {
                    Placement::Stereo => {
                        let a = band.det_state[0].run(&band.detector, kl).abs();
                        let b = band.det_state[1].run(&band.detector, kr_).abs();
                        a.max(b)
                    }
                    Placement::Left => band.det_state[0].run(&band.detector, kl).abs(),
                    Placement::Right => band.det_state[0].run(&band.detector, kr_).abs(),
                    Placement::Mid => band.det_state[0]
                        .run(&band.detector, 0.5 * (kl + kr_))
                        .abs(),
                    Placement::Side => band.det_state[0]
                        .run(&band.detector, 0.5 * (kl - kr_))
                        .abs(),
                };
                let k = if det_in > band.env { ka } else { kr };
                band.env = det_in + (band.env - det_in) * k;
                let used = band.used;
                let sections = &band.sections;
                let cascade = |st: &mut [State; MAX_SECTIONS], x: f64| {
                    let mut y = x;
                    for (s, c) in st.iter_mut().zip(sections.iter()).take(used) {
                        y = s.run(c, y);
                    }
                    x + mix * (y - x)
                };
                match band.placement {
                    Placement::Stereo => {
                        let [s0, s1] = &mut band.state;
                        l = cascade(s0, l);
                        r = cascade(s1, r);
                    }
                    Placement::Left => l = cascade(&mut band.state[0], l),
                    Placement::Right => r = cascade(&mut band.state[1], r),
                    Placement::Mid | Placement::Side => {
                        let (mut m, mut s) = (0.5 * (l + r), 0.5 * (l - r));
                        if band.placement == Placement::Mid {
                            m = cascade(&mut band.state[0], m);
                        } else {
                            s = cascade(&mut band.state[0], s);
                        }
                        l = m + s;
                        r = m - s;
                    }
                }
            }
            if let Some((_, c, st)) = listen.as_mut() {
                l = st[0].run(c, in_l);
                r = st[1].run(c, in_r);
            }
            left[i] = (l * gain) as f32;
            right[i] = (r * gain) as f32;
        }
        for band in self.bands.iter_mut() {
            for st in band
                .state
                .iter_mut()
                .flatten()
                .chain(band.det_state.iter_mut())
            {
                st.flush();
            }
            if band.env < 1e-12 {
                band.env = 0.0;
            }
        }
    }
}

impl PluginProcessor for EqProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
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
        let sidechain = self.params.get(global::SIDECHAIN) >= 0.5;
        let key = io
            .audio_in
            .get(1)
            .filter(|k| sidechain && k.num_channels() > 0);
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
            let key_slices = key.map(|k| {
                let a = &k.channel(0)[at..end];
                let b = &k.channel(1.min(k.num_channels() - 1))[at..end];
                (a, b)
            });
            if channels >= 2 {
                let (l, r) = out.channel_pair_mut(0, 1);
                self.run(&mut l[at..end], &mut r[at..end], key_slices);
            } else {
                // Mono: both sides of the stereo EQ see the one channel.
                let ch = out.channel_mut(0);
                let mut right = [0.0f32; STEP];
                let len = end - at;
                right[..len].copy_from_slice(&ch[at..end]);
                self.run(&mut ch[at..end], &mut right[..len], key_slices);
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
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        for band in self.bands.iter_mut() {
            band.reset_state();
        }
        if let Some(lp) = self.linear.as_mut() {
            lp.reset();
        }
        if let Some((_, _, st)) = self.listen.as_mut() {
            *st = [State::default(); 2];
        }
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
