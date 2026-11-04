//! Wavetable: two wavetable oscillators, a sub and noise per voice.
//!
//! An oscillator reads a table ([`table`]: a factory one made by code, or
//! a user table loaded into its slot) at a frame position (frames
//! interpolated, the position modulated like anything else), from the
//! band-limited copy its pitch needs (two crossfaded, so a glide never
//! steps), through a warp — Sync (the cycle read faster, restarting with
//! the period), Bend + / Bend − (the first or second half of the cycle
//! squeezed, a phase distortion), Mirror (the cycle forwards then back,
//! blended in), FM (the other oscillator moving its phase) — with up to
//! eight unison copies detuned across a spread of cents and fanned across
//! the stereo field. Then the shared parts ([`super::poly`]): the
//! Synth's filter or none, envelopes, LFOs, the matrix, the wheels,
//! per-note modulation, MPE and expressions, the effects.

pub mod table;
#[cfg(test)]
mod tests;

use super::poly::{
    self, Block, Chassis, Common, Defaults, Head, NoteStart, Output, Step, Steps, Voiced,
};
use super::samples::{SampleDoc, SampleSet, Shared};
use super::synth::{self, ChannelState, Fx};
use super::{param, pick, stepped};
use crate::tap::AnalysisTap;
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;
use table::{Frames, Mip, Table};

/// Stable parameter ids (`id == position`): the shared block, then per
/// oscillator [`OSC_FIELDS`] fields at [`osc`], then the sub and noise.
pub mod id {
    use super::poly::COMMON;
    pub use super::poly::id::*;

    /// An oscillator's fields.
    pub const TABLE: u32 = 0;
    pub const POSITION: u32 = 1;
    pub const WARP: u32 = 2;
    pub const WARP_AMOUNT: u32 = 3;
    pub const OCTAVE: u32 = 4;
    pub const SEMI: u32 = 5;
    /// Cents.
    pub const FINE: u32 = 6;
    pub const LEVEL: u32 = 7;
    pub const PAN: u32 = 8;
    pub const UNISON: u32 = 9;
    /// The unison copies' spread in cents, lowest to highest.
    pub const DETUNE: u32 = 10;
    /// How far the copies fan out across the stereo field.
    pub const SPREAD: u32 = 11;
    /// Where a note starts the cycle, and how much each copy's start is
    /// left to chance.
    pub const PHASE: u32 = 12;
    pub const RANDOM: u32 = 13;
    pub const OSC_FIELDS: u32 = 14;

    /// Oscillator `k`'s `field`.
    pub const fn osc(k: usize, field: u32) -> u32 {
        COMMON + OSC_FIELDS * k as u32 + field
    }

    pub const SUB: u32 = COMMON + 2 * OSC_FIELDS;
    /// One or two octaves below oscillator 1.
    pub const SUB_OCTAVE: u32 = SUB + 1;
    pub const SUB_WAVE: u32 = SUB + 2;
    pub const NOISE: u32 = SUB + 3;
    pub const NOISE_COLOUR: u32 = SUB + 4;
}

/// Matrix and wheel targets: the shared ones, then the oscillators'. At
/// full: positions and warps across their whole range, pitches ±12
/// semitones, levels ±100 %, detune ±100 cents, spread, sub and noise
/// ±100 %.
pub const TARGETS: [&str; 22] = [
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
    "Position 1",
    "Position 2",
    "Warp 1",
    "Warp 2",
    "Osc 1 Pitch",
    "Osc 2 Pitch",
    "Osc 1 Level",
    "Osc 2 Level",
    "Unison Detune",
    "Unison Spread",
    "Sub",
    "Noise",
];

/// [`TARGETS`] by name.
pub mod target {
    pub use super::poly::target::*;
    pub const POSITION: [usize; 2] = [10, 11];
    pub const WARP: [usize; 2] = [12, 13];
    pub const OSC_PITCH: [usize; 2] = [14, 15];
    pub const LEVEL: [usize; 2] = [16, 17];
    pub const DETUNE: usize = 18;
    pub const SPREAD: usize = 19;
    pub const SUB: usize = 20;
    pub const NOISE: usize = 21;
}

/// Published: the shared values, then the newest voice's frame
/// positions (0…1).
pub mod value {
    pub use super::poly::value::*;
    pub const POSITION: [usize; 2] = [9, 10];
}
pub const TAP_VALUES: usize = 11;

pub const WARPS: [&str; 6] = ["Off", "Sync", "Bend +", "Bend −", "Mirror", "FM"];
/// [`WARPS`] by name.
pub mod warp {
    pub const OFF: usize = 0;
    pub const SYNC: usize = 1;
    pub const BEND_UP: usize = 2;
    pub const BEND_DOWN: usize = 3;
    pub const MIRROR: usize = 4;
    pub const FM: usize = 5;
}
pub const SUB_WAVES: [&str; 3] = ["Sine", "Square", "Triangle"];
pub const NOISE_COLOURS: [&str; 2] = ["White", "Pink"];

/// The Table parameter's value for an oscillator's own (loaded) table.
pub const USER: usize = table::FACTORY.len();
/// Unison copies at most.
pub const MAX_UNISON: usize = 8;
/// FM warp: cycles of phase a full-scale other oscillator moves at 100 %
/// (the amount is squared).
const FM_CYCLES: f32 = 1.5;

const DEFAULTS: Defaults = Defaults {
    volume: -8.0,
    filter: 0.0,
    cutoff: 8_000.0,
    attack: 3.0,
    decay: 600.0,
    sustain: 0.8,
    release: 300.0,
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
    for k in 0..2 {
        let n = |f: &str| format!("Osc {} {f}", k + 1);
        let first = k == 0;
        p.extend([
            stepped(osc(k, TABLE), &n("Table"), USER as f64, 0.0),
            param(
                osc(k, POSITION),
                &n("Position"),
                0.0,
                1.0,
                if first { 0.65 } else { 0.0 },
                Percent,
            ),
            stepped(osc(k, WARP), &n("Warp"), (WARPS.len() - 1) as f64, 0.0),
            param(
                osc(k, WARP_AMOUNT),
                &n("Warp Amount"),
                0.0,
                1.0,
                0.0,
                Percent,
            ),
            ranged(osc(k, OCTAVE), &n("Octave"), -3.0, 3.0, 0.0),
            ranged(osc(k, SEMI), &n("Semitone"), -12.0, 12.0, 0.0),
            param(osc(k, FINE), &n("Fine"), -100.0, 100.0, 0.0, None),
            param(
                osc(k, LEVEL),
                &n("Level"),
                0.0,
                1.0,
                if first { 0.8 } else { 0.0 },
                Percent,
            ),
            param(osc(k, PAN), &n("Pan"), -1.0, 1.0, 0.0, None),
            ranged(osc(k, UNISON), &n("Unison"), 1.0, MAX_UNISON as f64, 1.0),
            param(osc(k, DETUNE), &n("Detune"), 0.0, 100.0, 20.0, None),
            param(osc(k, SPREAD), &n("Spread"), 0.0, 1.0, 0.6, Percent),
            param(osc(k, PHASE), &n("Phase"), 0.0, 1.0, 0.0, Percent),
            param(osc(k, RANDOM), &n("Random Phase"), 0.0, 1.0, 1.0, Percent),
        ]);
    }
    p.extend([
        param(SUB, "Sub", 0.0, 1.0, 0.0, Percent),
        ranged(SUB_OCTAVE, "Sub Octave", -2.0, -1.0, -1.0),
        stepped(SUB_WAVE, "Sub Wave", (SUB_WAVES.len() - 1) as f64, 0.0),
        param(NOISE, "Noise", 0.0, 1.0, 0.0, Percent),
        stepped(NOISE_COLOUR, "Noise Colour", 1.0, 0.0),
    ]);
    p
}

/// The table names an oscillator chooses from.
pub fn table_name(i: usize) -> &'static str {
    table::FACTORY.get(i).copied().unwrap_or("User")
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::format(pid, v, &TARGETS);
    }
    let field = (pid.0 - poly::COMMON) % OSC_FIELDS;
    Some(match pid.0 {
        x if x < SUB => match field {
            TABLE => table_name(v.round().max(0.0) as usize).into(),
            WARP => pick(&WARPS, v),
            OCTAVE | SEMI => format!("{:+.0}", v.round()).replace("+0", "0"),
            FINE | DETUNE => format!("{v:.0} ¢"),
            UNISON => format!("{:.0}", v.round()),
            PAN => pan_text(v),
            _ => return None,
        },
        SUB_OCTAVE => format!("{:.0} oct", v.round()),
        SUB_WAVE => pick(&SUB_WAVES, v),
        NOISE_COLOUR => pick(&NOISE_COLOURS, v),
        _ => return None,
    })
}

/// A pan position ("C", "L 40", "R 100").
pub(crate) fn pan_text(v: f64) -> String {
    let p = (v * 100.0).round();
    if p == 0.0 {
        "C".into()
    } else if p < 0.0 {
        format!("L {:.0}", -p)
    } else {
        format!("R {p:.0}")
    }
}

/// Whether a parameter takes modulation per note (a voice's own).
pub fn per_voice(pid: ParameterId) -> bool {
    use id::*;
    if pid.0 < poly::COMMON {
        return poly::per_voice(pid);
    }
    // The tables, warps and copies are the block's.
    let field = (pid.0 - poly::COMMON) % OSC_FIELDS;
    pid.0 >= SUB || !matches!(field, TABLE | WARP | UNISON | PHASE | RANDOM)
}

/// What the loaded files make: each slot's sample as a wavetable (its
/// frame size from the document, the file's `clm ` chunk, or guessed).
pub fn prepare(doc: &SampleDoc, set: &mut SampleSet) {
    set.tables = (0..set.slots.len())
        .map(|slot| {
            let sample = set.slot(slot)?;
            let size = match doc.cycle(slot) {
                0 => doc
                    .file(slot)
                    .and_then(|f| table::clm_frame_size(std::path::Path::new(f)))
                    .unwrap_or_else(|| table::guess_frame_size(sample.frames)),
                n => n as usize,
            };
            table::from_audio(&sample.name, &sample.data[0], &sample.data[1], size).map(Arc::new)
        })
        .collect();
}

#[derive(Clone, Copy, Debug)]
struct Osc {
    table: usize,
    position: f32,
    warp: usize,
    warp_amount: f32,
    /// Semitones from the key (octave, semitone, fine).
    transpose: f32,
    level: f32,
    pan: f32,
    unison: usize,
    detune: f32,
    spread: f32,
    phase: f32,
    random: f32,
}

#[derive(Clone, Copy)]
struct Settings {
    c: Common,
    osc: [Osc; 2],
    sub: f32,
    sub_octave: f32,
    sub_wave: usize,
    noise: f32,
    pink: bool,
}

impl Settings {
    fn read(get: &dyn Fn(u32) -> f32, sr: f32) -> Self {
        use id::*;
        let choice = |pid: u32, n: usize| (get(pid).round().max(0.0) as usize).min(n - 1);
        let osc = |k: usize| Osc {
            table: choice(id::osc(k, TABLE), USER + 1),
            position: get(id::osc(k, POSITION)).clamp(0.0, 1.0),
            warp: choice(id::osc(k, WARP), WARPS.len()),
            warp_amount: get(id::osc(k, WARP_AMOUNT)).clamp(0.0, 1.0),
            transpose: 12.0 * get(id::osc(k, OCTAVE)).round()
                + get(id::osc(k, SEMI)).round()
                + get(id::osc(k, FINE)) / 100.0,
            level: get(id::osc(k, LEVEL)).clamp(0.0, 1.0),
            pan: get(id::osc(k, PAN)).clamp(-1.0, 1.0),
            unison: (get(id::osc(k, UNISON)).round() as usize).clamp(1, MAX_UNISON),
            detune: get(id::osc(k, DETUNE)).max(0.0),
            spread: get(id::osc(k, SPREAD)).clamp(0.0, 1.0),
            phase: get(id::osc(k, PHASE)).clamp(0.0, 1.0),
            random: get(id::osc(k, RANDOM)).clamp(0.0, 1.0),
        };
        Settings {
            c: Common::read(get, sr, TARGETS.len()),
            osc: [osc(0), osc(1)],
            sub: get(SUB).clamp(0.0, 1.0),
            sub_octave: get(SUB_OCTAVE).round().clamp(-2.0, -1.0),
            sub_wave: choice(SUB_WAVE, SUB_WAVES.len()),
            noise: get(NOISE).clamp(0.0, 1.0),
            pink: get(NOISE_COLOUR) >= 0.5,
        }
    }
}

/// What a voice's samples read between control ticks.
#[derive(Clone, Copy, Debug)]
struct Tick {
    inc: [[f32; MAX_UNISON]; 2],
    mip: [Mip; 2],
    frames: [Frames; 2],
    position: [f32; 2],
    warp: [f32; 2],
    /// Per copy its share of each side (its level, pan and the copies'
    /// normalisation).
    sides: [[(f32, f32); MAX_UNISON]; 2],
    sub_inc: f32,
    sub: f32,
    noise: f32,
    /// The voice's level without its envelope.
    gain: f32,
    pans: (f32, f32),
    drive: f32,
}

impl Tick {
    const IDLE: Tick = Tick {
        inc: [[0.0; MAX_UNISON]; 2],
        mip: [Mip {
            a: 0,
            b: 0,
            mix: 0.0,
        }; 2],
        frames: [Frames {
            f0: 0,
            f1: 0,
            mix: 0.0,
        }; 2],
        position: [0.0; 2],
        warp: [0.0; 2],
        sides: [[(0.0, 0.0); MAX_UNISON]; 2],
        sub_inc: 0.0,
        sub: 0.0,
        noise: 0.0,
        gain: 0.0,
        pans: (1.0, 1.0),
        drive: 0.0,
    };
}

#[derive(Clone, Copy)]
struct Voice {
    head: Head,
    phase: [[f32; MAX_UNISON]; 2],
    sub_phase: f32,
    noise: u32,
    pink: [f32; 3],
    /// The oscillators' last outputs (the FM warp's source).
    last: [f32; 2],
    tick: Tick,
    /// The voice's own settings while it has per-note modulation.
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
        phase: [[0.0; MAX_UNISON]; 2],
        sub_phase: 0.0,
        noise: 0x1234_5678,
        pink: [0.0; 3],
        last: [0.0; 2],
        tick: Tick::IDLE,
        own: None,
    };

    /// A new note's own parts (phases unless it was sounding).
    fn start(&mut self, ns: NoteStart, s: &Settings) {
        if !ns.retrigger {
            return;
        }
        let n = self.head.serial;
        self.noise = 0x9e37_79b9 ^ n.wrapping_mul(2_654_435_761);
        self.own = None;
        if ns.kept {
            return;
        }
        for (o, os) in s.osc.iter().enumerate() {
            for (u, p) in self.phase[o].iter_mut().enumerate() {
                let r = 0.5
                    + 0.5
                        * synth::spread_random(
                            n.wrapping_mul(747_796_405)
                                ^ ((o * 8 + u) as u32).wrapping_mul(2_891_336_453),
                        );
                *p = (os.phase + os.random * r).fract();
            }
        }
        self.sub_phase = 0.0;
        self.pink = [0.0; 3];
        self.last = [0.0; 2];
    }
}

/// How much faster than its pitch a warp reads the cycle at most (the
/// band-limited copy is picked for that).
fn warp_speed(warp: usize, a: f32) -> f32 {
    match warp {
        warp::SYNC => 1.0 + 7.0 * a,
        warp::BEND_UP | warp::BEND_DOWN => 0.5 / (0.5 - 0.45 * a),
        warp::MIRROR if a > 0.0 => 2.0,
        _ => 1.0,
    }
}

/// One sample of an oscillator through its warp.
#[inline]
fn warped(t: &Table, kind: usize, a: f32, m: Mip, f: Frames, p: f32, other: f32) -> f32 {
    match kind {
        warp::SYNC => {
            let q = p * (1.0 + 7.0 * a);
            t.read(m, f, q - q.floor())
        }
        warp::BEND_UP | warp::BEND_DOWN => {
            let mid = if kind == warp::BEND_UP {
                0.5 - 0.45 * a
            } else {
                0.5 + 0.45 * a
            };
            let q = if p < mid {
                0.5 * p / mid
            } else {
                0.5 + 0.5 * (p - mid) / (1.0 - mid)
            };
            t.read(m, f, q)
        }
        warp::MIRROR => {
            let x = t.read(m, f, p);
            if a <= 0.0 {
                return x;
            }
            let q = if p < 0.5 { 2.0 * p } else { 2.0 - 2.0 * p };
            x + (t.read(m, f, q) - x) * a
        }
        warp::FM => {
            let q = p + FM_CYCLES * a * a * other;
            t.read(m, f, q - q.floor())
        }
        _ => t.read(m, f, p),
    }
}

/// A voice's control tick: its modulations, then everything its samples
/// read until the next.
#[allow(clippy::too_many_arguments)]
fn control(
    v: &mut Voice,
    s: &Settings,
    ch: &ChannelState,
    b: &Block,
    sr: f32,
    tables: [&Table; 2],
    vibrato: f32,
) {
    let m = poly::control_tick(&mut v.head, &s.c, ch, b, sr);
    let semis = poly::semitones(&v.head, &s.c, ch, &m, vibrato);
    let t = &mut v.tick;
    for (o, os) in s.osc.iter().enumerate() {
        let level = (os.level + m.get(target::LEVEL[o])).clamp(0.0, 1.0);
        let n = os.unison;
        let detune = (os.detune + 100.0 * m.get(target::DETUNE)).max(0.0);
        let spread = (os.spread + m.get(target::SPREAD)).clamp(0.0, 1.0);
        let pitch = semis + os.transpose + 12.0 * m.get(target::OSC_PITCH[o]);
        let base = 440.0 * 2f32.powf(pitch / 12.0) / sr;
        let mut top = 0.0f32;
        for u in 0..n {
            let at = if n > 1 {
                u as f32 / (n - 1) as f32 - 0.5
            } else {
                0.0
            };
            let inc = (base * 2f32.powf(detune * at / 1200.0)).min(0.5);
            t.inc[o][u] = inc;
            top = top.max(inc);
        }
        let a = (os.warp_amount + m.get(target::WARP[o])).clamp(0.0, 1.0);
        t.warp[o] = a;
        t.mip[o] = Mip::for_increment(top * warp_speed(os.warp, a));
        let position = (os.position + m.get(target::POSITION[o])).clamp(0.0, 1.0);
        t.position[o] = position;
        t.frames[o] = tables[o].frames_at(position);
        let norm = level / (n as f32).sqrt();
        for u in 0..n {
            let q = if n > 1 {
                os.pan + spread * (2.0 * u as f32 / (n - 1) as f32 - 1.0)
            } else {
                os.pan
            }
            .clamp(-1.0, 1.0);
            t.sides[o][u] = (norm * 0.5 * (1.0 - q), norm * 0.5 * (1.0 + q));
        }
        if o == 0 {
            t.sub_inc = (base * 2f32.powf(s.sub_octave)).min(0.45);
        }
    }
    t.sub = (s.sub + m.get(target::SUB)).clamp(0.0, 1.0);
    t.noise = (s.noise + m.get(target::NOISE)).clamp(0.0, 1.0);
    t.gain = poly::level(&s.c, &v.head, ch, &m, 1.0);
    t.pans = poly::pan_gains(&v.head, &m);
    t.drive = (s.c.drive + m.get(target::DRIVE)).clamp(0.0, 1.0);
}

pub struct WavetableProcessor {
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    out: Output,
    sr: f32,
    chassis: Chassis<Voice>,
    fx: Fx,
    /// The loaded user tables (slot `k`: oscillator `k`'s).
    shared: Shared,
}

impl WavetableProcessor {
    pub fn new(
        params: ParamValues,
        tap: Option<Arc<AnalysisTap>>,
        config: &ProcessConfig,
        shared: Shared,
    ) -> Self {
        // Made here (the control thread), once per process.
        let _ = table::factory();
        let sr = config.sample_rate as f32;
        Self {
            params,
            tap,
            out: Output::new(sr),
            sr,
            chassis: Chassis::new(Voice::IDLE),
            fx: Fx::new(config.sample_rate),
            shared,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        s: &Settings,
        b: &Block,
        tables: [&Table; 2],
        out_l: &mut [f32],
        mut out_r: Option<&mut [f32]>,
        start: usize,
        end: usize,
    ) {
        let sr = self.sr;
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
            v.head.begin_segment(b);
            for i in start..end {
                let (amp_env, _) = v.head.tick(&s.c);
                if !v.head.sounding() {
                    break;
                }
                if v.head.control == 0 {
                    control(v, s, &ch, b, sr, tables, vibrato);
                }
                v.head.control = (v.head.control + 1) % synth::CONTROL_INTERVAL;
                let t = &v.tick;
                let mut side = [0.0f32; 2];
                // Oscillator 2 first: it moves oscillator 1's phase in FM.
                let mut out = [0.0f32; 2];
                for o in [1usize, 0] {
                    let os = &s.osc[o];
                    let other = if o == 0 { out[1] } else { v.last[0] };
                    // Silent, it still runs as the other's FM source.
                    let audible = t.sides[o][0] != (0.0, 0.0);
                    if !audible && s.osc[1 - o].warp != warp::FM {
                        continue;
                    }
                    let copies = if audible { os.unison } else { 1 };
                    let tbl = tables[o];
                    for u in 0..copies {
                        let p = v.phase[o][u];
                        let x = warped(tbl, os.warp, t.warp[o], t.mip[o], t.frames[o], p, other);
                        if u == 0 {
                            out[o] = x;
                        }
                        let (l, r) = t.sides[o][u];
                        side[0] += x * l;
                        side[1] += x * r;
                        let mut q = p + t.inc[o][u];
                        if q >= 1.0 {
                            q -= 1.0;
                        }
                        v.phase[o][u] = q;
                    }
                }
                v.last = out;
                if t.sub > 0.0 {
                    let x = match s.sub_wave {
                        1 => synth::wave(1, v.sub_phase, t.sub_inc, 0.5),
                        2 => synth::wave(2, v.sub_phase, t.sub_inc, 0.5),
                        _ => synth::wave(3, v.sub_phase, t.sub_inc, 0.5),
                    } * t.sub
                        * 0.5;
                    side[0] += x;
                    side[1] += x;
                }
                v.sub_phase += t.sub_inc;
                if v.sub_phase >= 1.0 {
                    v.sub_phase -= 1.0;
                }
                if t.noise > 0.0 {
                    let n = &mut v.noise;
                    *n ^= *n << 13;
                    *n ^= *n >> 17;
                    *n ^= *n << 5;
                    let w = *n as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    let w = if s.pink {
                        let p = &mut v.pink;
                        p[0] = 0.997_65 * p[0] + w * 0.099_046;
                        p[1] = 0.963 * p[1] + w * 0.296_516_4;
                        p[2] = 0.57 * p[2] + w * 1.052_691_3;
                        (p[0] + p[1] + p[2] + w * 0.1848) * 0.25
                    } else {
                        w
                    };
                    let x = w * t.noise * 0.5;
                    side[0] += x;
                    side[1] += x;
                }
                let (gain, pans, drive) = (t.gain * amp_env, t.pans, t.drive);
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

/// An oscillator's table: a factory one, or its slot's loaded table
/// (the first factory table while none is).
fn table_for(index: usize, slot: usize, set: Option<&SampleSet>) -> &Table {
    let factory = table::factory();
    if index < factory.len() {
        return &factory[index];
    }
    set.and_then(|s| s.tables.get(slot))
        .and_then(|t| t.as_deref())
        .unwrap_or(&factory[0])
}

impl PluginProcessor for WavetableProcessor {
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
        // The user tables, if the control side is not swapping them now.
        let shared = Arc::clone(&self.shared);
        let guard = shared.try_lock();
        let set = guard.as_ref().and_then(|g| g.set.as_deref());
        let tables = [
            table_for(settings.osc[0].table, 0, set),
            table_for(settings.osc[1].table, 1, set),
        ];
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
                        self.render(&settings, &block, tables, l, Some(r), a, b);
                    } else {
                        self.render(&settings, &block, tables, out.channel_mut(0), None, a, b);
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
            for o in 0..2 {
                tap.set_value(
                    value::POSITION[o],
                    newest.map_or(settings.osc[o].position, |v| v.tick.position[o]),
                );
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
