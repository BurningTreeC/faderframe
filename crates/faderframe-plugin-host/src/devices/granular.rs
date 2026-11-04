//! Granular: clouds of short grains read from a sample.
//!
//! Each voice starts grains at a density (per second, evenly or jittered)
//! from a position in the sample (sprayed at random either side, and
//! scanning on through the sample at a speed unless frozen); each grain
//! has a size, a window (Hann, triangle, Tukey, Gauss, a decay or a swell),
//! a pitch (the key's from a root key, or fixed, plus a transpose and a
//! random spread), a place across the stereo field, a chance of playing
//! backwards and a random level. Grains are summed by their expected
//! overlap so the density does not change the level. The sample is the
//! one loaded into the device (the sampler's loading: `samples`,
//! `SampleHost`, read through a `TryCell`); with none it granulates a pad
//! of its own. Then the shared parts ([`super::poly`]).

#[cfg(test)]
mod tests;

use super::poly::{
    self, Block, Chassis, Common, Defaults, Head, NoteStart, Output, Step, Steps, Voiced,
};
use super::samples::{Sample, Shared};
use super::synth::{self, ChannelState, Fx};
use super::{on_off, param, pick, stepped};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::{Arc, OnceLock};

/// Stable parameter ids (`id == position`): the shared block, then the
/// grains'.
pub mod id {
    use super::poly::COMMON;
    pub use super::poly::id::*;

    /// Where in the sample grains start (0…1).
    pub const POSITION: u32 = COMMON;
    /// How far either side of it they may start (ms).
    pub const SPRAY: u32 = COMMON + 1;
    /// A grain's length (ms).
    pub const SIZE: u32 = COMMON + 2;
    /// Grains a second.
    pub const DENSITY: u32 = COMMON + 3;
    /// Semitones.
    pub const PITCH: u32 = COMMON + 4;
    /// Each grain's pitch spread at random (semitones either side).
    pub const PITCH_RANDOM: u32 = COMMON + 5;
    /// Grains follow the keys (from the root key); off: every key plays
    /// the sample's pitch.
    pub const FOLLOW_KEYS: u32 = COMMON + 6;
    pub const ROOT: u32 = COMMON + 7;
    pub const PAN_SPREAD: u32 = COMMON + 8;
    pub const WINDOW: u32 = COMMON + 9;
    /// The chance a grain plays backwards.
    pub const REVERSE: u32 = COMMON + 10;
    /// The position stays where it is set.
    pub const FREEZE: u32 = COMMON + 11;
    /// Unfrozen: how fast the position moves through the sample (1: as
    /// recorded; negative backwards).
    pub const SCAN: u32 = COMMON + 12;
    /// Grains' timing left to chance (0: evenly spaced).
    pub const JITTER: u32 = COMMON + 13;
    pub const LEVEL_RANDOM: u32 = COMMON + 14;
}

/// Matrix and wheel targets: the shared ones, then the grains'. At full:
/// the position across the whole sample, spray ±1 s, size and density
/// two and three octaves, pitch ±12 semitones, pitch spread ±12, pan
/// spread ±100 %, scan ±2.
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
    "Position",
    "Spray",
    "Size",
    "Density",
    "Grain Pitch",
    "Pitch Random",
    "Pan Spread",
    "Scan",
];

/// [`TARGETS`] by name.
pub mod target {
    pub use super::poly::target::*;
    pub const POSITION: usize = 10;
    pub const SPRAY: usize = 11;
    pub const SIZE: usize = 12;
    pub const DENSITY: usize = 13;
    pub const GRAIN_PITCH: usize = 14;
    pub const PITCH_RANDOM: usize = 15;
    pub const PAN_SPREAD: usize = 16;
    pub const SCAN: usize = 17;
}

/// Published: the shared values, then the newest voice's position (0…1)
/// and grains sounding, and where its first [`SHOWN_GRAINS`] grains are
/// reading (0…1, −1: none).
pub mod value {
    pub use super::poly::value::*;
    pub const POSITION: usize = 9;
    pub const GRAINS: usize = 10;
    pub const GRAIN: usize = 11;
}
pub const SHOWN_GRAINS: usize = 8;
pub const TAP_VALUES: usize = value::GRAIN + SHOWN_GRAINS;

pub const WINDOWS: [&str; 6] = ["Hann", "Triangle", "Tukey", "Gauss", "Decay", "Swell"];
/// Grains a voice has at most at once.
pub const MAX_GRAINS: usize = 48;
/// Points of a window's table (and a guard).
const WINDOW_POINTS: usize = 512;

const DEFAULTS: Defaults = Defaults {
    volume: -6.0,
    filter: 0.0,
    cutoff: 10_000.0,
    attack: 80.0,
    decay: 800.0,
    sustain: 1.0,
    release: 900.0,
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
        param(POSITION, "Position", 0.0, 1.0, 0.3, Percent),
        param(SPRAY, "Spray", 0.0, 2_000.0, 40.0, Milliseconds),
        param(SIZE, "Grain Size", 5.0, 1_000.0, 90.0, Milliseconds),
        param(DENSITY, "Density", 1.0, 200.0, 25.0, Hertz),
        param(PITCH, "Grain Pitch", -24.0, 24.0, 0.0, None),
        param(PITCH_RANDOM, "Pitch Random", 0.0, 12.0, 0.0, None),
        stepped(FOLLOW_KEYS, "Follow Keys", 1.0, 1.0),
        ranged(ROOT, "Root Key", 0.0, 127.0, 60.0),
        param(PAN_SPREAD, "Pan Spread", 0.0, 1.0, 0.4, Percent),
        stepped(WINDOW, "Window", (WINDOWS.len() - 1) as f64, 0.0),
        param(REVERSE, "Reverse", 0.0, 1.0, 0.0, Percent),
        stepped(FREEZE, "Freeze", 1.0, 0.0),
        param(SCAN, "Scan", -2.0, 2.0, 0.1, None),
        param(JITTER, "Jitter", 0.0, 1.0, 0.3, Percent),
        param(LEVEL_RANDOM, "Level Random", 0.0, 1.0, 0.0, Percent),
    ]);
    p
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::format(pid, v, &TARGETS);
    }
    Some(match pid.0 {
        DENSITY => format!("{v:.1} /s"),
        PITCH => format!("{v:+.1} st").replace("+0.0 st", "0 st"),
        PITCH_RANDOM => format!("±{v:.1} st"),
        FOLLOW_KEYS | FREEZE => on_off(v),
        ROOT => super::note_name(v.round() as i32),
        WINDOW => pick(&WINDOWS, v),
        SCAN => format!("{v:+.2}×").replace("+0.00×", "0×"),
        _ => return None,
    })
}

/// Whether a parameter takes modulation per note (a voice's own).
pub fn per_voice(pid: ParameterId) -> bool {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::per_voice(pid);
    }
    !matches!(pid.0, FOLLOW_KEYS | ROOT | WINDOW | FREEZE)
}

/// The window shapes' tables, made once off the audio thread.
fn windows() -> &'static [[f32; WINDOW_POINTS + 1]; 6] {
    static TABLES: OnceLock<[[f32; WINDOW_POINTS + 1]; 6]> = OnceLock::new();
    TABLES.get_or_init(|| {
        std::array::from_fn(|shape| {
            std::array::from_fn(|i| {
                let t = i as f64 / WINDOW_POINTS as f64;
                let hann = |x: f64| 0.5 - 0.5 * (std::f64::consts::TAU * x).cos();
                (match shape {
                    1 => 1.0 - (2.0 * t - 1.0).abs(),
                    // Flat in the middle, Hann edges (a quarter each).
                    2 => {
                        if t < 0.25 {
                            hann(t * 2.0)
                        } else if t > 0.75 {
                            hann((t - 0.5) * 2.0)
                        } else {
                            1.0
                        }
                    }
                    3 => (-0.5 * ((t - 0.5) / 0.16).powi(2)).exp(),
                    // A fast rise and an exponential fall, and its mirror.
                    4 => (t / 0.03).min(1.0) * (-5.0 * t).exp(),
                    5 => ((1.0 - t) / 0.03).min(1.0) * (-5.0 * (1.0 - t)).exp(),
                    _ => hann(t),
                }) as f32
            })
        })
    })
}

/// Window `shape` at `t` (0…1), as grains read it (for drawing).
pub fn window_at(shape: usize, t: f32) -> f32 {
    let w = &windows()[shape.min(WINDOWS.len() - 1)];
    let x = t.clamp(0.0, 1.0) * WINDOW_POINTS as f32;
    let k = (x as usize).min(WINDOW_POINTS - 1);
    w[k] + (w[k + 1] - w[k]) * (x - k as f32)
}

/// What it plays with no sample loaded: three seconds of a slowly opening
/// pad (an A minor chord of saws through a sweeping low pass), made once.
pub fn builtin_source() -> &'static Sample {
    static SOURCE: OnceLock<Sample> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let sr = 48_000.0f64;
        let n = (3.0 * sr) as usize;
        let notes = [110.0, 130.81, 164.81, 220.0];
        let mut ch = vec![vec![0.0f32; n]; 2];
        for (c, out) in ch.iter_mut().enumerate() {
            let mut phases = [0.0f64; 4];
            let (mut ic1, mut ic2) = (0.0f64, 0.0f64);
            for (i, o) in out.iter_mut().enumerate() {
                let t = i as f64 / sr;
                let mut x = 0.0;
                for (k, f) in notes.iter().enumerate() {
                    let detune = 1.0 + 0.002 * (k as f64 + 1.0) * if c == 0 { 1.0 } else { -1.0 };
                    phases[k] = (phases[k] + f * detune / sr).fract();
                    x += 2.0 * phases[k] - 1.0;
                }
                // A state-variable low pass from 300 Hz up to 4 kHz and back.
                let fc = 300.0 * (4_000f64 / 300.0).powf((std::f64::consts::PI * t / 3.0).sin());
                let g = (std::f64::consts::PI * fc / sr).tan();
                let k = 1.2;
                let a1 = 1.0 / (1.0 + g * (g + k));
                let v3 = x - ic2;
                let v1 = a1 * ic1 + g * a1 * v3;
                let v2 = ic2 + g * v1;
                ic1 = 2.0 * v1 - ic1;
                ic2 = 2.0 * v2 - ic2;
                *o = (v2 * 0.18) as f32;
            }
        }
        Sample::from_channels("Pad".into(), sr, ch)
    })
}

/// A sample of nothing (read while the samples are being swapped).
fn silence() -> &'static Sample {
    static SILENCE: OnceLock<Sample> = OnceLock::new();
    SILENCE.get_or_init(|| Sample::from_channels(String::new(), 48_000.0, vec![Vec::new()]))
}

#[derive(Clone, Copy)]
struct Settings {
    c: Common,
    position: f32,
    spray: f32,
    size: f32,
    density: f32,
    pitch: f32,
    pitch_random: f32,
    key_track: bool,
    root: f32,
    pan_spread: f32,
    window: usize,
    reverse: f32,
    freeze: bool,
    scan: f32,
    jitter: f32,
    level_random: f32,
}

impl Settings {
    fn read(get: &dyn Fn(u32) -> f32, sr: f32) -> Self {
        use id::*;
        Settings {
            c: Common::read(get, sr, TARGETS.len()),
            position: get(POSITION).clamp(0.0, 1.0),
            spray: get(SPRAY).max(0.0),
            size: get(SIZE).max(1.0),
            density: get(DENSITY).max(0.1),
            pitch: get(PITCH),
            pitch_random: get(PITCH_RANDOM).max(0.0),
            key_track: get(FOLLOW_KEYS) >= 0.5,
            root: get(ROOT).round(),
            pan_spread: get(PAN_SPREAD).clamp(0.0, 1.0),
            window: (get(WINDOW).round().max(0.0) as usize).min(WINDOWS.len() - 1),
            reverse: get(REVERSE).clamp(0.0, 1.0),
            freeze: get(FREEZE) >= 0.5,
            scan: get(SCAN),
            jitter: get(JITTER).clamp(0.0, 1.0),
            level_random: get(LEVEL_RANDOM).clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Grain {
    /// Where it reads (frames of the sample) and how far a sample moves
    /// it (negative: backwards).
    pos: f64,
    step: f64,
    /// Its window's phase (0…1) and step.
    t: f32,
    dt: f32,
    left: f32,
    right: f32,
    on: bool,
}

impl Grain {
    const OFF: Grain = Grain {
        pos: 0.0,
        step: 0.0,
        t: 0.0,
        dt: 0.0,
        left: 0.0,
        right: 0.0,
        on: false,
    };
}

/// What a voice's samples read between control ticks.
#[derive(Clone, Copy, Debug)]
struct Tick {
    /// Samples between grains.
    interval: f32,
    /// A grain's length in samples.
    length: f32,
    /// Where grains start (0…1) and their spray (a share of the sample).
    position: f32,
    spray: f32,
    /// The grains' transpose (semitones) and spread.
    semitones: f32,
    pitch_random: f32,
    pan_spread: f32,
    /// The scan's step per sample (a share of the sample).
    scan: f32,
    /// What a grain's level is (the overlap's normalisation).
    grain_gain: f32,
    gain: f32,
    pans: (f32, f32),
    drive: f32,
}

impl Tick {
    const IDLE: Tick = Tick {
        interval: 1.0e9,
        length: 1.0,
        position: 0.0,
        spray: 0.0,
        semitones: 0.0,
        pitch_random: 0.0,
        pan_spread: 0.0,
        scan: 0.0,
        grain_gain: 0.0,
        gain: 0.0,
        pans: (1.0, 1.0),
        drive: 0.0,
    };
}

#[derive(Clone, Copy)]
struct Voice {
    head: Head,
    grains: [Grain; MAX_GRAINS],
    /// Samples to the next grain.
    until: f32,
    /// How far the scan has moved the position (a share of the sample).
    scan: f64,
    rng: u32,
    tick: Tick,
    /// Grains started (for tests and the editor).
    started: u64,
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
        grains: [Grain::OFF; MAX_GRAINS],
        until: 0.0,
        scan: 0.0,
        rng: 0x2545_f491,
        tick: Tick::IDLE,
        started: 0,
        own: None,
    };

    fn start(&mut self, ns: NoteStart) {
        if !ns.retrigger {
            return;
        }
        self.own = None;
        self.rng = (0x9e37_79b9 ^ self.head.serial.wrapping_mul(2_654_435_761)) | 1;
        if !ns.kept {
            self.grains = [Grain::OFF; MAX_GRAINS];
        }
        self.until = 0.0;
        self.scan = 0.0;
        self.started = 0;
    }

    /// A random number in 0…1.
    #[inline]
    fn random(&mut self) -> f32 {
        let r = &mut self.rng;
        *r ^= *r << 13;
        *r ^= *r >> 17;
        *r ^= *r << 5;
        *r as f32 / u32::MAX as f32
    }

    /// Start a grain (or none, when every slot is busy).
    fn spawn(&mut self, s: &Settings, sample: &Sample, sr: f32) {
        let Some(slot) = self.grains.iter().position(|g| !g.on) else {
            return;
        };
        let t = self.tick;
        let len = sample.frames.max(1) as f64;
        let spray = (2.0 * self.random() - 1.0) * t.spray;
        let at = (f64::from(t.position) + self.scan + f64::from(spray)).rem_euclid(1.0) * len;
        let semis = t.semitones + t.pitch_random * (2.0 * self.random() - 1.0);
        let mut step = f64::from(2f32.powf(semis / 12.0)) * sample.rate / f64::from(sr);
        if self.random() < s.reverse {
            step = -step;
        }
        let pan = t.pan_spread * (2.0 * self.random() - 1.0);
        let level = t.grain_gain * (1.0 - s.level_random * self.random());
        self.grains[slot] = Grain {
            pos: at,
            step,
            t: 0.0,
            dt: 1.0 / t.length.max(1.0),
            left: level * (1.0 - pan).min(1.0),
            right: level * (1.0 + pan).min(1.0),
            on: true,
        };
        self.started += 1;
    }
}

/// A voice's control tick.
#[allow(clippy::too_many_arguments)]
fn control(
    v: &mut Voice,
    s: &Settings,
    ch: &ChannelState,
    b: &Block,
    sr: f32,
    vibrato: f32,
    sample: &Sample,
) {
    let m = poly::control_tick(&mut v.head, &s.c, ch, b, sr);
    let semis = poly::semitones(&v.head, &s.c, ch, &m, vibrato);
    let len = sample.frames.max(1) as f32;
    let t = &mut v.tick;
    let density = (s.density * 2f32.powf(3.0 * m.get(target::DENSITY))).clamp(0.1, 1_000.0);
    let size_ms = (s.size * 2f32.powf(2.0 * m.get(target::SIZE))).clamp(1.0, 4_000.0);
    t.interval = sr / density;
    t.length = size_ms * 0.001 * sr;
    t.position = (s.position + m.get(target::POSITION)).clamp(0.0, 1.0);
    let spray_ms = (s.spray + 1_000.0 * m.get(target::SPRAY)).max(0.0);
    t.spray = spray_ms * 0.001 * sample.rate as f32 / len;
    // From the key (by the root) or the sample's own pitch; the wheel's
    // bend, vibrato and glide either way.
    let from_key = if s.key_track {
        semis + 69.0 - s.root
    } else {
        semis - v.head.pitch
    };
    t.semitones = from_key + s.pitch + 12.0 * m.get(target::GRAIN_PITCH);
    t.pitch_random = (s.pitch_random + 12.0 * m.get(target::PITCH_RANDOM)).max(0.0);
    t.pan_spread = (s.pan_spread + m.get(target::PAN_SPREAD)).clamp(0.0, 1.0);
    let scan = s.scan + 2.0 * m.get(target::SCAN);
    t.scan = if s.freeze {
        0.0
    } else {
        scan * sample.rate as f32 / sr / len
    };
    // Grains overlapping by n sum like n uncorrelated sources.
    let overlap = density * size_ms * 0.001;
    t.grain_gain = 1.0 / overlap.max(1.0).sqrt();
    t.gain = poly::level(&s.c, &v.head, ch, &m, 1.0);
    t.pans = poly::pan_gains(&v.head, &m);
    t.drive = (s.c.drive + m.get(target::DRIVE)).clamp(0.0, 1.0);
}

/// The sample at a fractional frame (kept within it by the caller), both
/// sides.
#[inline]
fn read(sample: &Sample, p: f64) -> (f32, f32) {
    let n = sample.frames;
    if n == 0 {
        return (0.0, 0.0);
    }
    let i = (p.max(0.0) as usize).min(n - 1);
    let j = if i + 1 >= n { 0 } else { i + 1 };
    let t = (p - i as f64) as f32;
    let [l, r] = &sample.data;
    (l[i] + (l[j] - l[i]) * t, r[i] + (r[j] - r[i]) * t)
}

pub struct GranularProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    out: Output,
    sr: f32,
    chassis: Chassis<Voice>,
    fx: Fx,
    shared: Shared,
    windows: &'static [[f32; WINDOW_POINTS + 1]; 6],
    builtin: &'static Sample,
}

impl GranularProcessor {
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        shared: Shared,
    ) -> Self {
        let sr = config.sample_rate as f32;
        Self {
            params,
            tap,
            out: Output::new(sr),
            sr,
            chassis: Chassis::new(Voice::IDLE),
            fx: Fx::new(config.sample_rate),
            shared,
            windows: windows(),
            builtin: {
                let _ = silence();
                builtin_source()
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        s: &Settings,
        b: &Block,
        sample: &Sample,
        out_l: &mut [f32],
        mut out_r: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        let sr = self.sr;
        let vibrato = self.chassis.vibrato(end - start, sr);
        let channels = self.chassis.channels;
        let params = &self.params;
        let windows = self.windows;
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
            let window = &windows[s.window];
            v.head.begin_segment(b);
            for i in start..end {
                let (amp_env, _) = v.head.tick(&s.c);
                if !v.head.sounding() {
                    break;
                }
                if v.head.control == 0 {
                    control(v, s, &ch, b, sr, vibrato, sample);
                }
                v.head.control = (v.head.control + 1) % synth::CONTROL_INTERVAL;
                v.until -= 1.0;
                if v.until <= 0.0 {
                    v.spawn(s, sample, sr);
                    let jitter = s.jitter * (2.0 * v.random() - 1.0);
                    v.until += v.tick.interval * (1.0 + 0.9 * jitter);
                }
                let mut side = [0.0f32; 2];
                let len = sample.frames as f64;
                for g in v.grains.iter_mut().filter(|g| g.on) {
                    let x = g.t * WINDOW_POINTS as f32;
                    let k = (x as usize).min(WINDOW_POINTS - 1);
                    let w = window[k] + (window[k + 1] - window[k]) * (x - k as f32);
                    let (l, r) = read(sample, g.pos);
                    side[0] += l * w * g.left;
                    side[1] += r * w * g.right;
                    // On round the sample's ends.
                    g.pos += g.step;
                    if g.pos >= len {
                        g.pos -= len;
                    } else if g.pos < 0.0 {
                        g.pos += len;
                    }
                    g.t += g.dt;
                    if g.t >= 1.0 {
                        g.on = false;
                    }
                }
                v.scan = (v.scan + f64::from(v.tick.scan)).rem_euclid(1.0);
                let t = v.tick;
                poly::finish(
                    &mut v.head,
                    &s.c,
                    side,
                    t.gain * amp_env,
                    t.pans,
                    t.drive,
                    out_l,
                    out_r.as_deref_mut(),
                    i,
                );
            }
            v.head.filter.settle();
        }
    }

    /// Grains started by the voices sounding (tests).
    #[cfg(test)]
    fn grains_started(&self) -> u64 {
        self.chassis
            .voices
            .iter()
            .filter(|v| v.head.sounding())
            .map(|v| v.started)
            .sum()
    }
}

impl PluginProcessor for GranularProcessor {
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
        // The loaded sample, if the control side is not swapping it now;
        // else the built-in pad.
        let shared = Arc::clone(&self.shared);
        let guard = shared.try_lock();
        let loaded = guard
            .as_ref()
            .and_then(|g| g.set.as_deref())
            .and_then(|set| set.slot(0));
        // Swapping samples: a block of silence rather than the pad.
        let sample = match (&guard, loaded) {
            (_, Some(s)) => s,
            (Some(_), None) => self.builtin,
            (None, None) => silence(),
        };
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
                        self.render(&settings, &block, sample, l, Some(r), a, b);
                    } else {
                        self.render(&settings, &block, sample, out.channel_mut(0), None, a, b);
                    }
                }
                Step::Midi(e) => {
                    self.chassis
                        .handle(e, &settings.c, &mut |v: &mut Voice, ns| v.start(ns));
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
            let len = sample.frames.max(1) as f64;
            tap.set_value(
                value::POSITION,
                newest.map_or(settings.position, |v| {
                    ((f64::from(v.tick.position) + v.scan).rem_euclid(1.0)) as f32
                }),
            );
            let mut shown = 0;
            if let Some(v) = newest {
                tap.set_value(
                    value::GRAINS,
                    v.grains.iter().filter(|g| g.on).count() as f32,
                );
                for g in v.grains.iter().filter(|g| g.on).take(SHOWN_GRAINS) {
                    tap.set_value(value::GRAIN + shown, (g.pos.rem_euclid(len) / len) as f32);
                    shown += 1;
                }
            } else {
                tap.set_value(value::GRAINS, 0.0);
            }
            for k in shown..SHOWN_GRAINS {
                tap.set_value(value::GRAIN + k, -1.0);
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
