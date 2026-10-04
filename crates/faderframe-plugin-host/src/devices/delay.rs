//! Delay (the built-in "Echo", grown up): stereo, ping-pong or mono
//! repeats, free or synced to the song, with the loop's sound shaped by a
//! low cut, damping, saturation, wow and flutter and a style; ducking,
//! width and freeze.
//!
//! The first five parameters are the Echo's and keep their meaning:
//! time, feedback, damping (the Echo's one-pole low pass in the loop: 0
//! off, 1 about 1.2 kHz), mix and ping-pong (now a mode: 0 stereo, 1
//! ping-pong, 2 mono). Time changes glide like a tape's heads moving.
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
use crate::dsp::delay::DelayLine;
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
    ]
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
        id::SYNC | id::FREEZE => on_off(v),
        id::DIVISION => division_name(v.round().max(0.0) as usize).into(),
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
    let base = if get(params, id::SYNC) >= 0.5 {
        division_seconds(get(params, id::DIVISION).round().max(0.0) as usize, tempo)
    } else {
        get(params, id::TIME) * 0.001
    };
    let base = base.clamp(0.001, MAX_SECONDS);
    let right = (base * (1.0 + get(params, id::OFFSET))).clamp(0.001, MAX_SECONDS);
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
            let ducking = self.get(id::DUCKING);
            let width = self.get(id::WIDTH);
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
                let live = 1.0 - frozen;
                let mut wet = [0.0; 2];
                for c in 0..2 {
                    self.delay[c] += (target[c] - self.delay[c]) * glide;
                    let ph = c as f64 * 0.25;
                    let m = wow_depth
                        * live
                        * (0.8 * (TAU * (self.wow + ph)).sin()
                            + 0.2 * (TAU * (self.flutter + ph)).sin());
                    // Read before this sample is written: one less.
                    wet[c] = self.lines[c].read(self.delay[c] + wow_depth + m - 1.0);
                }
                // The loop: what comes back, shaped (untouched when frozen).
                let mut back = wet;
                for c in 0..2 {
                    let mut v = back[c];
                    if self.damping {
                        v = self.damp[c].low(v);
                    }
                    v = self.low_cut.process(c, v);
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
        }
        self.low_cut.reset();
        self.bump.reset();
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
