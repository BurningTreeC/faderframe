//! Delay (the built-in "Echo", grown up): stereo, ping-pong or mono
//! repeats, free or synced to the song (each side its own note value, or
//! linked), with the loop's sound shaped by a low cut, damping, a tilt,
//! diffusion, saturation, wow and flutter, chorus modulation and a style;
//! ducking, width and freeze.
//!
//! The first five parameters are the Echo's and keep their meaning:
//! time, feedback, damping (the Echo's one-pole low pass in the loop: 0
//! off, 1 about 1.2 kHz), mix and ping-pong (now a mode: 0 stereo, 1
//! ping-pong, 2 mono). Time changes glide like a tape's heads moving.
//! Everything added since is neutral at its default: a project saved
//! with the Echo, or with the first Delay, sounds as it did.
//!
//! * Link (on by default) makes the right side follow the left (times the
//!   stereo offset); off, the right has its own time and note value.
//! * Tilt is a tilt shelf round 1 kHz in the loop: every pass gets
//!   brighter or darker by the amount.
//! * Diffusion smears each repeat through four allpasses a side (their
//!   length taken off the delay so the repeat still lands on time).
//! * Modulation is a chorus on the read heads (faster and deeper than the
//!   tape's wow, the right side a quarter turn behind).
//!
//! * Styles — Digital (clean); Tape (an arctangent saturation, a head
//!   bump and high frequencies falling with every pass); Analog (a bucket
//!   brigade's band limit, 2-pole at 4.5 kHz, and a `tanh` compander).
//! * The loop always has a safety curve (linear to full scale, then
//!   rounding off), so feedback over 100 % swells into saturation instead
//!   of exploding.
//! * Freeze closes the input and loops what is there untouched.
//! * Ducking turns the repeats down while the input plays (down by up to
//!   24 dB over 40 dB of input level).

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::{Allpass, DelayLine};
use crate::dsp::filter::{BandShape, BandType, Filter, OnePole, Svf};
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
use std::f64::consts::{FRAC_1_SQRT_2, TAU};
use std::sync::Arc;

pub mod id {
    pub const TIME: u32 = 0;
    pub const FEEDBACK: u32 = 1;
    pub const DAMPING: u32 = 2;
    pub const MIX: u32 = 3;
    pub const MODE: u32 = 4;
    pub const SYNC: u32 = 5;
    pub const DIVISION: u32 = 6;
    pub const OFFSET: u32 = 7;
    pub const LOW_CUT: u32 = 8;
    pub const SATURATION: u32 = 9;
    pub const WOW: u32 = 10;
    pub const WOW_RATE: u32 = 11;
    pub const DUCKING: u32 = 12;
    pub const WIDTH: u32 = 13;
    pub const FREEZE: u32 = 14;
    pub const STYLE: u32 = 15;
    /// The right side follows the left (default on).
    pub const LINK: u32 = 16;
    pub const TIME_R: u32 = 17;
    pub const DIVISION_R: u32 = 18;
    pub const TILT: u32 = 19;
    pub const DIFFUSION: u32 = 20;
    pub const MODULATION: u32 = 21;
    pub const MOD_RATE: u32 = 22;
}

/// Published: the left and right times (ms), input, output and repeats'
/// peaks (linear), the ducking (dB, ≥ 0).
pub mod value {
    pub const TIME_L: usize = 0;
    pub const TIME_R: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const WET_PEAK: usize = 4;
    pub const DUCK: usize = 5;
}
pub const TAP_VALUES: usize = 6;

pub const MODES: [&str; 3] = ["Stereo", "Ping-Pong", "Mono"];
pub const STYLES: [&str; 3] = ["Digital", "Tape", "Analog"];
/// The longest delay (seconds).
pub const MAX_SECONDS: f64 = 5.0;
/// The division a synced delay starts on (1/8 dotted).
const DEFAULT_DIVISION: f64 = 10.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::TIME, "Time", 1.0, 4_000.0, 401.0, Milliseconds),
        param(id::FEEDBACK, "Feedback", 0.0, 1.1, 0.38, Percent),
        param(id::DAMPING, "Damping", 0.0, 1.0, 0.35, Percent),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::MODE, "Mode", 2.0, 1.0),
        stepped(id::SYNC, "Sync", 1.0, 0.0),
        stepped(
            id::DIVISION,
            "Division",
            (DIVISIONS.len() - 1) as f64,
            DEFAULT_DIVISION,
        ),
        param(id::OFFSET, "Stereo Offset", -0.5, 0.5, 0.0, Percent),
        param(id::LOW_CUT, "Low Cut", 20.0, 2_000.0, 20.0, Hertz),
        param(id::SATURATION, "Saturation", 0.0, 1.0, 0.0, Percent),
        param(id::WOW, "Wow & Flutter", 0.0, 1.0, 0.0, Percent),
        param(id::WOW_RATE, "Wow Rate", 0.05, 8.0, 0.6, Hertz),
        param(id::DUCKING, "Ducking", 0.0, 1.0, 0.0, Percent),
        param(id::WIDTH, "Width", 0.0, 1.5, 1.0, Percent),
        stepped(id::FREEZE, "Freeze", 1.0, 0.0),
        stepped(id::STYLE, "Style", 2.0, 0.0),
        stepped(id::LINK, "Link", 1.0, 1.0),
        param(id::TIME_R, "Time R", 1.0, 4_000.0, 401.0, Milliseconds),
        stepped(
            id::DIVISION_R,
            "Division R",
            (DIVISIONS.len() - 1) as f64,
            DEFAULT_DIVISION,
        ),
        param(id::TILT, "Tilt", -12.0, 12.0, 0.0, Decibels),
        param(id::DIFFUSION, "Diffusion", 0.0, 1.0, 0.0, Percent),
        param(id::MODULATION, "Modulation", 0.0, 1.0, 0.0, Percent),
        param(id::MOD_RATE, "Modulation Rate", 0.1, 10.0, 1.5, Hertz),
    ]
}

/// The diffusers' lengths (ms, left; the right 9 % longer).
pub const DIFFUSER_MS: [f64; 4] = [1.73, 2.91, 4.37, 6.19];
/// The tilt's pivot (Hz).
pub const TILT_HZ: f64 = 1_000.0;
/// The chorus modulation's depth at 1 (seconds, peak).
pub const MOD_SECONDS: f64 = 0.004;

/// The diffusers' total length (seconds) a side.
pub fn diffuser_seconds(right: bool) -> f64 {
    DIFFUSER_MS.iter().sum::<f64>() * 0.001 * if right { 1.09 } else { 1.0 }
}

/// The damping's cutoff (Hz) at a rate, as the Echo's one-pole had it.
pub fn damping_hz(damping: f64, rate: f64) -> Option<f64> {
    (damping > 0.001).then(|| {
        let a = 1.0 - damping.clamp(0.0, 1.0) * 0.85;
        -(1.0 - a).ln() * rate / TAU
    })
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        id::STYLE => pick(&STYLES, v),
        id::SYNC | id::FREEZE | id::LINK => on_off(v),
        id::DIVISION | id::DIVISION_R => division_name(v.round().max(0.0) as usize).into(),
        id::TILT if v.abs() < 0.05 => "Flat".into(),
        id::TILT => format!("{v:+.1} dB"),
        id::DIFFUSION | id::MODULATION if v < 0.005 => "Off".into(),
        id::DAMPING => match damping_hz(v, 48_000.0) {
            Some(hz) => crate::eq::format_hz(hz),
            None => "Off".into(),
        },
        id::LOW_CUT if v <= 20.5 => "Off".into(),
        id::OFFSET if v.abs() < 0.005 => "0 %".into(),
        id::OFFSET => format!("{:+.0} %", v * 100.0),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The left and right delay times (seconds) for the parameters at a tempo.
pub fn times(params: &ParamValues, tempo: f64) -> (f64, f64) {
    let synced = get(params, id::SYNC) >= 0.5;
    let of = |division: u32, time: u32| {
        if synced {
            division_seconds(get(params, division).round().max(0.0) as usize, tempo)
        } else {
            get(params, time) * 0.001
        }
    };
    let base = of(id::DIVISION, id::TIME).clamp(0.001, MAX_SECONDS);
    let own = if get(params, id::LINK) >= 0.5 {
        base
    } else {
        of(id::DIVISION_R, id::TIME_R).clamp(0.001, MAX_SECONDS)
    };
    let right = (own * (1.0 + get(params, id::OFFSET))).clamp(0.001, MAX_SECONDS);
    if get(params, id::MODE).round() as i64 == 2 {
        (base, base)
    } else {
        (base, right)
    }
}

const STEP: usize = 32;

pub struct DelayProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    lines: [DelayLine; 2],
    /// The delays read now (samples), gliding to the set times.
    delay: [f64; 2],
    damp: [OnePole; 2],
    damping: bool,
    low_cut: Filter<2>,
    /// Tape: head loss and bump; analog: the band limit.
    head: [OnePole; 2],
    bump: Filter<2>,
    bbd: [[Svf; 2]; 2],
    /// Wow (slow) and flutter (fast) phases.
    wow: f64,
    flutter: f64,
    /// The chorus modulation's phase.
    chorus: f64,
    tilt: Filter<2>,
    diffusers: [[Allpass; 4]; 2],
    /// What the diffusers delay a side (samples), taken off the read.
    diffuser_len: [f64; 2],
    diffusion: Smoothed,
    duck_env: f64,
    duck: f64,
    freeze: Smoothed,
    mix: Smoothed,
    feedback: Smoothed,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DelayProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let cap = (MAX_SECONDS * sr * 1.02) as usize + 64;
        let (l, r) = times(&params, 120.0);
        let mt = MeterTap::new(sr as f32);
        let mut bbd = [[Svf::default(); 2]; 2];
        for s in bbd.iter_mut().flatten() {
            s.set(4_500.0, FRAC_1_SQRT_2, sr);
        }
        let ap_cap = (DIFFUSER_MS[3] * 1.09 * 0.001 * sr) as usize + 8;
        let mut diffusers: [[Allpass; 4]; 2] =
            std::array::from_fn(|_| std::array::from_fn(|_| Allpass::new(ap_cap)));
        let mut diffuser_len = [0.0; 2];
        for (c, side) in diffusers.iter_mut().enumerate() {
            for (ap, ms) in side.iter_mut().zip(DIFFUSER_MS) {
                let n = (ms * 0.001 * sr * if c == 1 { 1.09 } else { 1.0 }).round() as usize;
                ap.set_len(n.max(1));
                diffuser_len[c] += ap.delay() as f64;
            }
        }
        let mut s = Self {
            watching: Watching::new(sr as f32),
            sr,
            lines: [DelayLine::new(cap), DelayLine::new(cap)],
            delay: [l * sr, r * sr],
            damp: [OnePole::default(); 2],
            damping: false,
            low_cut: Filter::default(),
            head: [OnePole::new(9_000.0, sr); 2],
            bump: Filter::default(),
            bbd,
            wow: 0.0,
            flutter: 0.0,
            chorus: 0.0,
            tilt: Filter::default(),
            diffusers,
            diffuser_len,
            diffusion: Smoothed::new(0.0, 30.0, sr),
            duck_env: 0.0,
            duck: 1.0,
            freeze: Smoothed::new(0.0, 15.0, sr),
            mix: Smoothed::new(1.0, 15.0, sr),
            feedback: Smoothed::new(0.0, 15.0, sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        s.control();
        s.freeze.snap();
        s.mix.snap();
        s.feedback.snap();
        s.diffusion.snap();
        s
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn control(&mut self) {
        let sr = self.sr;
        let freeze = self.get(id::FREEZE) >= 0.5;
        self.freeze.set(if freeze { 1.0 } else { 0.0 });
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        self.feedback.set(self.get(id::FEEDBACK).clamp(0.0, 1.1));
        match damping_hz(self.get(id::DAMPING), sr) {
            Some(hz) => {
                self.damping = true;
                for d in &mut self.damp {
                    d.set(hz, sr);
                }
            }
            None => self.damping = false,
        }
        let low = self.get(id::LOW_CUT);
        if low > 20.5 {
            self.low_cut.set(
                BandShape {
                    kind: BandType::LowCut,
                    freq: low,
                    gain: 0.0,
                    q: FRAC_1_SQRT_2,
                    slope: 12.0,
                },
                sr,
            );
        } else {
            self.low_cut.clear();
        }
        let tilt = self.get(id::TILT);
        if tilt.abs() >= 0.05 {
            self.tilt.set(
                BandShape {
                    kind: BandType::TiltShelf,
                    freq: TILT_HZ,
                    gain: tilt,
                    q: FRAC_1_SQRT_2,
                    slope: 12.0,
                },
                sr,
            );
        } else {
            self.tilt.clear();
        }
        self.diffusion.set(self.get(id::DIFFUSION).clamp(0.0, 1.0));
        if self.get(id::STYLE).round() as i64 == 1 {
            self.bump.set(
                BandShape {
                    kind: BandType::Bell,
                    freq: 110.0,
                    gain: 1.0,
                    q: 0.9,
                    slope: 12.0,
                },
                sr,
            );
        } else {
            self.bump.clear();
        }
    }
}

/// The loop's saturation for a style and amount (unity for small signals).
#[inline]
fn saturate(style: i64, amount: f64, x: f64) -> f64 {
    if amount <= 0.0 {
        // Untouched up to full scale, then rounding off towards 2.
        return if x.abs() <= 1.0 {
            x
        } else {
            x.signum() * (1.0 + (x.abs() - 1.0).tanh())
        };
    }
    let g = 1.0 + 7.0 * amount;
    match style {
        1 => (g * x * std::f64::consts::FRAC_PI_2).atan() / (g * std::f64::consts::FRAC_PI_2),
        _ => (g * x).tanh() / g,
    }
}

impl PluginProcessor for DelayProcessor {
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
        let glide = 1.0 - (-1.0 / (0.015 * sr)).exp();
        let duck_attack = 1.0 - (-1.0 / (0.005 * sr)).exp();
        let duck_release = (-1.0 / (0.25 * sr)).exp();
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
            let (tl, tr) = times(&self.params, tempo);
            let target = [tl * sr, tr * sr];
            let mode = self.get(id::MODE).round() as i64;
            let style = self.get(id::STYLE).round() as i64;
            let sat = self.get(id::SATURATION);
            let wow_depth =
                self.get(id::WOW) * 0.0025 * sr + if style == 1 { 0.0003 * sr } else { 0.0 };
            let wow_step = self.get(id::WOW_RATE) / sr;
            let mod_depth = self.get(id::MODULATION) * MOD_SECONDS * sr;
            let mod_step = self.get(id::MOD_RATE) / sr;
            let ducking = self.get(id::DUCKING);
            let width = self.get(id::WIDTH);
            let tilted = self.tilt.is_active();
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let frozen = self.freeze.tick();
                let feedback = self.feedback.tick() * (1.0 - frozen) + frozen;
                let mix = self.mix.tick();
                // Wow and flutter move both heads (the right a quarter
                // turn later).
                self.wow = (self.wow + wow_step).fract();
                self.flutter = (self.flutter + wow_step * 9.7).fract();
                self.chorus = (self.chorus + mod_step).fract();
                let live = 1.0 - frozen;
                let diffusion = self.diffusion.tick();
                let mut wet = [0.0; 2];
                for c in 0..2 {
                    self.delay[c] += (target[c] - self.delay[c]) * glide;
                    let ph = c as f64 * 0.25;
                    let m = wow_depth
                        * live
                        * (0.8 * (TAU * (self.wow + ph)).sin()
                            + 0.2 * (TAU * (self.flutter + ph)).sin());
                    let chorus = mod_depth * live * (TAU * (self.chorus + ph)).sin();
                    // Read before this sample is written: one less; the
                    // diffusers' own delay comes off the read.
                    let mut d = self.delay[c] + wow_depth + m + chorus - 1.0;
                    if diffusion > 0.0 {
                        d -= self.diffuser_len[c];
                    }
                    let mut v = self.lines[c].read(d);
                    if diffusion > 0.0 {
                        let g = 0.5 * diffusion;
                        for ap in &mut self.diffusers[c] {
                            v = ap.process(v, g);
                        }
                    }
                    wet[c] = v;
                }
                // The loop: what comes back, shaped (untouched when frozen).
                let mut back = wet;
                for c in 0..2 {
                    let mut v = back[c];
                    if self.damping {
                        v = self.damp[c].low(v);
                    }
                    v = self.low_cut.process(c, v);
                    if tilted {
                        v = self.tilt.process(c, v);
                    }
                    match style {
                        1 => v = self.head[c].low(self.bump.process(c, v)),
                        2 => {
                            let s = self.bbd[c][0].process(v).low;
                            v = self.bbd[c][1].process(s).low;
                        }
                        _ => {}
                    }
                    v = saturate(style, sat, v);
                    back[c] = wet[c] + live * (v - wet[c]);
                }
                let into = [x[0] * live, x[1] * live];
                let (w0, w1) = match mode {
                    // Ping-pong: the input enters on the left, repeats cross.
                    1 => (
                        0.5 * (into[0] + into[1]) + feedback * back[1],
                        feedback * back[0],
                    ),
                    2 => {
                        let m = 0.5 * (into[0] + into[1]) + feedback * 0.5 * (back[0] + back[1]);
                        (m, m)
                    }
                    _ => (into[0] + feedback * back[0], into[1] + feedback * back[1]),
                };
                self.lines[0].push(flush(w0));
                self.lines[1].push(flush(w1));
                // Ducking by the input's level.
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
                // Width of the repeats.
                let mid = 0.5 * (wet[0] + wet[1]);
                let side = 0.5 * (wet[0] - wet[1]) * width;
                let w = [(mid + side) * self.duck, (mid - side) * self.duck];
                wet_peak = wet_peak.max(w[0].abs()).max(w[1].abs());
                let y = [
                    x[0] * (1.0 - mix) + w[0] * mix,
                    x[1] * (1.0 - mix) + w[1] * mix,
                ];
                out_peak = out_peak.max(y[0].abs()).max(y[1].abs());
                if channels == 1 {
                    out.channel_mut(0)[i] = (0.5 * (y[0] + y[1])) as f32;
                } else {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for c in 0..2 {
            self.damp[c].flush();
            self.head[c].flush();
            for s in &mut self.bbd[c] {
                s.flush();
            }
        }
        self.low_cut.flush();
        self.bump.flush();
        self.tilt.flush();
        self.duck_env = flush(self.duck_env);
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        self.tap
            .set_value(value::TIME_L, (self.delay[0] / sr * 1000.0) as f32);
        self.tap
            .set_value(value::TIME_R, (self.delay[1] / sr * 1000.0) as f32);
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
        for c in 0..2 {
            self.lines[c].reset();
            self.damp[c].reset();
            self.head[c].reset();
            for s in &mut self.bbd[c] {
                s.reset();
            }
            for ap in &mut self.diffusers[c] {
                ap.reset();
            }
        }
        self.low_cut.reset();
        self.bump.reset();
        self.tilt.reset();
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

    fn rig(set: &[(u32, f64)]) -> Rig<DelayProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DelayProcessor::new)
    }

    fn click(at: usize) -> impl Fn(usize) -> (f32, f32) {
        move |n| if n == at { (1.0, 1.0) } else { (0.0, 0.0) }
    }

    /// Where the signal is loudest within `±w` of `at`, and how loud.
    fn hit(x: &[f32], at: usize, w: usize) -> (usize, f32) {
        (at.saturating_sub(w)..(at + w).min(x.len()))
            .map(|i| (i, x[i].abs()))
            .fold((0, 0.0), |m, v| if v.1 > m.1 { v } else { m })
    }

    #[test]
    fn repeats_come_back_after_the_time_each_one_quieter() {
        let mut r = rig(&[
            (id::MODE, 0.0),
            (id::DAMPING, 0.0),
            (id::FEEDBACK, 0.5),
            (id::TIME, 100.0),
        ]);
        let (l, _) = r.run(0.5, click(1_000), silence);
        let d = 4_800;
        let (a, la) = hit(&l, 1_000 + d, 4);
        let (b, lb) = hit(&l, 1_000 + 2 * d, 4);
        assert!(
            a.abs_diff(1_000 + d) <= 1 && b.abs_diff(1_000 + 2 * d) <= 1,
            "{a} {b}"
        );
        assert!((la - 1.0).abs() < 0.05, "{la}");
        assert!((lb / la - 0.5).abs() < 0.03, "{}", lb / la);
    }

    #[test]
    fn ping_pong_alternates_and_sync_follows_the_tempo() {
        let mut r = rig(&[
            (id::MODE, 1.0),
            (id::DAMPING, 0.0),
            (id::SYNC, 1.0),
            (id::DIVISION, 8.0),
            (id::FEEDBACK, 0.5),
        ]);
        r.transport.tempo = 120.0;
        // 1/8 at 120 BPM: 250 ms.
        let (l, rr) = r.run(1.0, click(1_000), silence);
        let d = 12_000;
        assert!(hit(&l, 1_000 + d, 4).1 > 0.4, "first repeat on the left");
        assert!(hit(&rr, 1_000 + d, 4).1 < 0.01);
        assert!(hit(&rr, 1_000 + 2 * d, 4).1 > 0.2, "second on the right");
        assert!(hit(&l, 1_000 + 2 * d, 4).1 < 0.01);
    }

    #[test]
    fn freeze_holds_the_loop() {
        let mut r = rig(&[
            (id::MODE, 0.0),
            (id::TIME, 50.0),
            (id::FEEDBACK, 0.3),
            (id::DAMPING, 0.5),
        ]);
        r.run(0.2, tone(440.0, 0.3), silence);
        r.set(id::FREEZE, 1.0);
        let (l, _) = r.run(0.3, silence, silence);
        let early = bin_db(&l, 440.0);
        let (l, _) = r.run(1.0, silence, silence);
        let quiet = bin_db(&l, 1_010.0);
        let (l, _) = r.run(1.0, tone(1_010.0, 0.5), silence);
        // Still there seconds later, at the same level, and the new input
        // does not get in.
        assert!(
            (bin_db(&l, 440.0) - early).abs() < 1.0,
            "{} vs {early}",
            bin_db(&l, 440.0)
        );
        assert!(
            bin_db(&l, 1_010.0) < quiet + 1.0,
            "{} vs {quiet}",
            bin_db(&l, 1_010.0)
        );
    }

    #[test]
    fn ducking_turns_the_repeats_down_while_the_input_plays() {
        let level = |duck: f64| {
            let mut r = rig(&[
                (id::MODE, 0.0),
                (id::TIME, 100.0),
                (id::FEEDBACK, 0.6),
                (id::DUCKING, duck),
                (id::MIX, 1.0),
            ]);
            let (l, _) = r.run(1.0, tone(300.0, 0.5), silence);
            bin_db(&l, 300.0)
        };
        let (open, ducked) = (level(0.0), level(1.0));
        assert!(ducked < open - 18.0, "{open:.1} → {ducked:.1}");
    }

    #[test]
    fn the_loop_filters_take_lows_and_highs_from_every_pass() {
        let after = |set: &[(u32, f64)], f: f64| {
            let mut all = vec![
                (id::MODE, 0.0),
                (id::TIME, 20.0),
                (id::FEEDBACK, 0.9),
                (id::DAMPING, 0.0),
            ];
            all.extend_from_slice(set);
            let mut r = rig(&all);
            r.run(0.05, tone(f, 0.3), silence);
            let (l, _) = r.run(0.4, silence, silence);
            bin_db(&l, f)
        };
        let clean_low = after(&[], 80.0);
        assert!(after(&[(id::LOW_CUT, 400.0)], 80.0) < clean_low - 20.0);
        let clean_high = after(&[], 8_000.0);
        assert!(after(&[(id::DAMPING, 0.6)], 8_000.0) < clean_high - 20.0);
        assert!(
            after(&[(id::STYLE, 2.0)], 8_000.0) < clean_high - 20.0,
            "analog is dark"
        );
        let _ = SR;
    }

    #[test]
    fn unlinked_sides_have_their_own_times_and_linked_ones_ignore_them() {
        let base = [(id::MODE, 0.0), (id::DAMPING, 0.0), (id::FEEDBACK, 0.0)];
        // Free: 100 ms left, 150 ms right.
        let mut set = base.to_vec();
        set.extend([(id::LINK, 0.0), (id::TIME, 100.0), (id::TIME_R, 150.0)]);
        let mut r = rig(&set);
        let (l, rr) = r.run(0.5, click(1_000), silence);
        assert_eq!(hit(&l, 1_000 + 4_800, 8).0, 5_800);
        assert_eq!(hit(&rr, 1_000 + 7_200, 8).0, 8_200);
        assert!(hit(&rr, 5_800, 8).1 < 0.01 && hit(&l, 8_200, 8).1 < 0.01);
        // Synced: 1/8 left (250 ms at 120), 1/8 triplet right (166.7 ms).
        let mut set = base.to_vec();
        set.extend([
            (id::LINK, 0.0),
            (id::SYNC, 1.0),
            (id::DIVISION, 8.0),
            (id::DIVISION_R, 6.0),
        ]);
        let mut r = rig(&set);
        r.transport.tempo = 120.0;
        let (l, rr) = r.run(0.5, click(1_000), silence);
        assert_eq!(hit(&l, 1_000 + 12_000, 8).0, 13_000);
        assert_eq!(hit(&rr, 1_000 + 8_000, 8).0, 9_000);
        // Linked (the default): the right side's own settings do nothing.
        let mut set = base.to_vec();
        set.push((id::TIME, 100.0));
        let mut plain = rig(&set);
        set.extend([(id::TIME_R, 999.0), (id::DIVISION_R, 3.0)]);
        let mut other = rig(&set);
        let (a, _) = plain.run(0.5, click(1_000), silence);
        let (b, _) = other.run(0.5, click(1_000), silence);
        assert_eq!(a, b);
    }

    #[test]
    fn diffusion_smears_the_repeat_and_keeps_its_time() {
        let spread = |diffusion: f64| {
            let mut r = rig(&[
                (id::MODE, 0.0),
                (id::DAMPING, 0.0),
                (id::FEEDBACK, 0.0),
                (id::TIME, 100.0),
                (id::DIFFUSION, diffusion),
            ]);
            let (l, _) = r.run(0.5, click(1_000), silence);
            let (at, peak) = hit(&l, 5_800, 1_200);
            let energy: f64 = l[4_000..8_000].iter().map(|v| f64::from(*v).powi(2)).sum();
            (at, f64::from(peak).powi(2) / energy)
        };
        let (at0, focus0) = spread(0.0);
        let (at1, focus1) = spread(1.0);
        assert_eq!(at0, 5_800);
        assert_eq!(at1, 5_800, "the smeared repeat still lands on time");
        assert!(focus0 > 0.99, "{focus0}");
        assert!(focus1 < 0.5, "spread over the diffusers: {focus1}");
    }

    #[test]
    fn the_tilt_brightens_or_darkens_every_pass() {
        let after = |tilt: f64, f: f64| {
            let mut r = rig(&[
                (id::MODE, 0.0),
                (id::TIME, 20.0),
                (id::FEEDBACK, 0.9),
                (id::DAMPING, 0.0),
                (id::TILT, tilt),
            ]);
            r.run(0.05, tone(f, 0.3), silence);
            let (l, _) = r.run(0.4, silence, silence);
            bin_db(&l, f)
        };
        let (flat_low, flat_high) = (after(0.0, 100.0), after(0.0, 8_000.0));
        assert!(after(12.0, 8_000.0) > flat_high + 6.0, "brighter");
        assert!(after(12.0, 100.0) < flat_low - 6.0);
        assert!(after(-12.0, 8_000.0) < flat_high - 6.0, "darker");
        assert!(after(-12.0, 100.0) > flat_low + 6.0);
    }

    #[test]
    fn modulation_moves_the_repeats_in_pitch() {
        let steady = |depth: f64| {
            let mut r = rig(&[
                (id::MODE, 0.0),
                (id::TIME, 100.0),
                (id::FEEDBACK, 0.0),
                (id::DAMPING, 0.0),
                (id::MODULATION, depth),
                (id::MOD_RATE, 2.0),
            ]);
            let (l, _) = r.run(1.0, tone(1_000.0, 0.5), silence);
            bin_db(&l, 1_000.0)
        };
        let (still, moving) = (steady(0.0), steady(1.0));
        // ±4 ms at 2 Hz: ±5 % of pitch — the tone is no longer at 1 kHz.
        assert!(moving < still - 6.0, "{still:.1} → {moving:.1}");
    }

    #[test]
    fn feedback_over_one_saturates_instead_of_exploding() {
        let mut r = rig(&[
            (id::MODE, 0.0),
            (id::TIME, 10.0),
            (id::FEEDBACK, 1.1),
            (id::DAMPING, 0.0),
        ]);
        let (l, _) = r.run(3.0, click(100), silence);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() < 3.0));
    }
}
