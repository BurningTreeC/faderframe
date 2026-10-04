//! Modulation: chorus, ensemble, flanger, phaser and vibrato.
//!
//! * Chorus — up to four voices a side reading a delay line round the
//!   delay time, swept by the LFO at evenly spread phases (alternating
//!   sides), summed at equal power.
//! * Ensemble — the string machine's: three voices a side at 120° apart,
//!   each swept by a slow chorus LFO and a fast vibrato one (10.5 × the
//!   rate, a fifth of the depth).
//! * Flanger — one voice a side swept from almost nothing up to the delay
//!   time, with feedback (negative feedback hollows instead of rings).
//! * Phaser — 2 to 12 first-order allpass stages whose corners sweep
//!   exponentially round the centre (up to ±3 octaves at full depth), with
//!   feedback; mixed with the dry signal they make the notches.
//! * Vibrato — one voice, all wet (the mix does not apply).
//!
//! The LFO runs free in hertz or locked to the song; the right side runs
//! the spread ahead (degrees). The wet signal has a high cut and a width.

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::filter::{BandShape, BandType, Filter};
use crate::dsp::flush;
use crate::dsp::lfo::{DIVISIONS, Lfo, Shape, division_name};
use crate::dsp::smooth::Smoothed;
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::f64::consts::{FRAC_1_SQRT_2, PI};
use std::sync::Arc;

pub mod id {
    pub const MODE: u32 = 0;
    pub const RATE: u32 = 1;
    pub const SYNC: u32 = 2;
    pub const DIVISION: u32 = 3;
    pub const DEPTH: u32 = 4;
    pub const FEEDBACK: u32 = 5;
    pub const DELAY: u32 = 6;
    pub const VOICES: u32 = 7;
    pub const STAGES: u32 = 8;
    pub const CENTER: u32 = 9;
    pub const SPREAD: u32 = 10;
    pub const SHAPE: u32 = 11;
    pub const MIX: u32 = 12;
    pub const WIDTH: u32 = 13;
    pub const HIGH_CUT: u32 = 14;
}

/// Published: input and output peaks (linear), the LFO now (−1…1, left),
/// and its phase (0…1).
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const LFO: usize = 2;
    pub const PHASE: usize = 3;
}
pub const TAP_VALUES: usize = 4;

pub const MODES: [&str; 5] = ["Chorus", "Ensemble", "Flanger", "Phaser", "Vibrato"];
pub const SHAPES: [&str; 6] = ["Sine", "Triangle", "Saw", "Square", "S&H", "Drift"];
/// The phaser's stage counts.
pub const STAGES: [usize; 6] = [2, 4, 6, 8, 10, 12];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::MODE, "Mode", 4.0, 0.0),
        param(id::RATE, "Rate", 0.01, 10.0, 0.5, Hertz),
        stepped(id::SYNC, "Sync", 1.0, 0.0),
        stepped(id::DIVISION, "Division", (DIVISIONS.len() - 1) as f64, 17.0),
        param(id::DEPTH, "Depth", 0.0, 1.0, 0.5, Percent),
        param(id::FEEDBACK, "Feedback", -0.95, 0.95, 0.0, Percent),
        param(id::DELAY, "Delay", 0.1, 30.0, 7.0, Milliseconds),
        stepped(id::VOICES, "Voices", 3.0, 1.0),
        stepped(id::STAGES, "Stages", 5.0, 3.0),
        param(id::CENTER, "Centre", 100.0, 8_000.0, 800.0, Hertz),
        param(id::SPREAD, "Stereo Spread", 0.0, 180.0, 90.0, None),
        stepped(id::SHAPE, "Shape", 5.0, 0.0),
        param(id::MIX, "Mix", 0.0, 1.0, 0.5, Percent),
        param(id::WIDTH, "Width", 0.0, 1.5, 1.0, Percent),
        param(id::HIGH_CUT, "High Cut", 1_000.0, 20_000.0, 20_000.0, Hertz),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        id::SHAPE => pick(&SHAPES, v),
        id::SYNC => on_off(v),
        id::DIVISION => division_name(v.round().max(0.0) as usize).into(),
        id::VOICES => format!("{:.0}", v.round() + 1.0),
        id::STAGES => format!("{}", STAGES[v.round().clamp(0.0, 5.0) as usize]),
        id::SPREAD => format!("{v:.0}°"),
        id::HIGH_CUT if v >= 19_500.0 => "Off".into(),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The longest delay any mode reads (seconds).
const MAX_DELAY: f64 = 0.07;
const STEP: usize = 16;

/// A first-order allpass (its corner set by `a`).
#[derive(Clone, Copy, Default)]
struct Allpass1 {
    s: f64,
}

impl Allpass1 {
    #[inline]
    fn process(&mut self, x: f64, a: f64) -> f64 {
        let y = a * x + self.s;
        self.s = x - a * y;
        y
    }
}

/// The allpass coefficient for a corner at `f`.
#[inline]
pub fn coefficient(f: f64, rate: f64) -> f64 {
    let g = (PI * f.clamp(10.0, 0.45 * rate) / rate).tan();
    (g - 1.0) / (g + 1.0)
}

pub struct ModulationProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    lines: [DelayLine; 2],
    lfo: Lfo,
    fast: Lfo,
    stages: [[Allpass1; 12]; 2],
    /// The last wet sample per side (feedback).
    last: [f64; 2],
    high_cut: Filter<2>,
    mix: Smoothed,
    depth: Smoothed,
    delay: Smoothed,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl ModulationProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let cap = (MAX_DELAY * sr) as usize + 8;
        let mt = MeterTap::new(sr as f32);
        let mut s = Self {
            watching: Watching::new(sr as f32),
            sr,
            lines: [DelayLine::new(cap), DelayLine::new(cap)],
            lfo: Lfo::new(0x5eed),
            fast: Lfo::new(0xfa57),
            stages: [[Allpass1::default(); 12]; 2],
            last: [0.0; 2],
            high_cut: Filter::default(),
            mix: Smoothed::new(0.5, 20.0, sr),
            depth: Smoothed::new(0.5, 30.0, sr),
            delay: Smoothed::new(7.0, 60.0, sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        s.control();
        s.mix.snap();
        s.depth.snap();
        s.delay.snap();
        s
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn control(&mut self) {
        let vibrato = self.get(id::MODE).round() as i64 == 4;
        self.mix.set(if vibrato {
            1.0
        } else {
            self.get(id::MIX).clamp(0.0, 1.0)
        });
        self.depth.set(self.get(id::DEPTH).clamp(0.0, 1.0));
        self.delay.set(self.get(id::DELAY).clamp(0.1, 30.0));
        let high = self.get(id::HIGH_CUT);
        if high < 19_500.0 && high < 0.45 * self.sr {
            self.high_cut.set(
                BandShape {
                    kind: BandType::HighCut,
                    freq: high,
                    gain: 0.0,
                    q: FRAC_1_SQRT_2,
                    slope: 12.0,
                },
                self.sr,
            );
        } else {
            self.high_cut.clear();
        }
    }
}

impl PluginProcessor for ModulationProcessor {
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
        let t = ctx.transport;
        let tempo = if t.tempo > 0.0 { t.tempo } else { 120.0 };
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak) = (0.0f64, 0.0f64);
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
            let mode = self.get(id::MODE).round() as i64;
            let shape = Shape::from_index(self.get(id::SHAPE).round().max(0.0) as usize);
            let synced = self.get(id::SYNC) >= 0.5;
            let period_q = DIVISIONS
                [(self.get(id::DIVISION).round().max(0.0) as usize).min(DIVISIONS.len() - 1)]
            .1;
            let cycles = if synced {
                tempo / 60.0 / period_q / sr
            } else {
                self.get(id::RATE) / sr
            };
            if synced && t.playing {
                // Locked to the song at the start of this step.
                let q = t.quarter_position + (at as f64) * tempo / 60.0 / sr;
                self.lfo.sync(q, period_q);
            }
            let spread = self.get(id::SPREAD) / 360.0;
            let feedback = self.get(id::FEEDBACK).clamp(-0.95, 0.95);
            let voices = self.get(id::VOICES).round().clamp(0.0, 3.0) as usize + 1;
            let stages = STAGES[self.get(id::STAGES).round().clamp(0.0, 5.0) as usize];
            let center = self.get(id::CENTER);
            let width = self.get(id::WIDTH);
            // The phaser's corners follow the LFO every step.
            let mut coef = [0.0; 2];
            if mode == 3 {
                let depth = self.depth.value;
                for (c, k) in coef.iter_mut().enumerate() {
                    let v = self.lfo.value(shape, c as f64 * spread);
                    *k = coefficient(center * 2f64.powf(3.0 * depth * v), sr);
                }
            }
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let mix = self.mix.tick();
                let depth = self.depth.tick();
                let delay = self.delay.tick() * 0.001 * sr;
                let mut wet = [0.0; 2];
                match mode {
                    3 => {
                        for c in 0..2 {
                            let mut s = x[c] + feedback * self.last[c];
                            for st in self.stages[c].iter_mut().take(stages) {
                                s = st.process(s, coef[c]);
                            }
                            self.last[c] = flush(s);
                            wet[c] = s;
                        }
                    }
                    _ => {
                        for (c, xc) in x.iter().enumerate() {
                            // Write first so a delay of under one sample works.
                            let fb = if mode == 2 {
                                feedback * self.last[c]
                            } else {
                                0.0
                            };
                            self.lines[c].push(xc + fb);
                        }
                        let off = |c: usize| c as f64 * spread;
                        match mode {
                            // Chorus: voices at spread phases, alternate sides.
                            0 => {
                                let norm = (voices as f64).sqrt().recip();
                                for (c, w) in wet.iter_mut().enumerate() {
                                    let mut sum = 0.0;
                                    for v in 0..voices {
                                        let ph = off(c) + v as f64 / voices as f64;
                                        let d =
                                            delay * (1.0 + 0.8 * depth * self.lfo.value(shape, ph));
                                        sum += self.lines[c].read(d.max(1.0));
                                    }
                                    *w = sum * norm;
                                }
                            }
                            1 => {
                                let norm = 3f64.sqrt().recip();
                                for (c, w) in wet.iter_mut().enumerate() {
                                    let mut sum = 0.0;
                                    for v in 0..3 {
                                        let ph = off(c) + v as f64 / 3.0;
                                        let m = 0.8 * self.lfo.value(Shape::Sine, ph)
                                            + 0.2 * self.fast.value(Shape::Sine, ph);
                                        let d = delay * (1.0 + 0.8 * depth * m);
                                        sum += self.lines[c].read(d.max(1.0));
                                    }
                                    *w = sum * norm;
                                }
                            }
                            // Flanger: from almost nothing up to the delay.
                            2 => {
                                for (c, w) in wet.iter_mut().enumerate() {
                                    let v = 0.5 + 0.5 * self.lfo.value(shape, off(c));
                                    let d =
                                        1.0 + (delay - 1.0).max(0.0) * (1.0 - depth + depth * v);
                                    *w = self.lines[c].read(d);
                                    self.last[c] = flush(*w);
                                }
                            }
                            _ => {
                                for (c, w) in wet.iter_mut().enumerate() {
                                    let d =
                                        delay * (1.0 + 0.9 * depth * self.lfo.value(shape, off(c)));
                                    *w = self.lines[c].read(d.max(1.0));
                                }
                            }
                        }
                    }
                }
                self.lfo.advance(cycles);
                self.fast.advance(cycles * 10.5);
                for (c, w) in wet.iter_mut().enumerate() {
                    *w = self.high_cut.process(c, *w);
                }
                let mid = 0.5 * (wet[0] + wet[1]);
                let side = 0.5 * (wet[0] - wet[1]) * width;
                let w = [mid + side, mid - side];
                let y = [x[0] + mix * (w[0] - x[0]), x[1] + mix * (w[1] - x[1])];
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
        for st in self.stages.iter_mut().flatten() {
            st.s = flush(st.s);
        }
        self.high_cut.flush();
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        let shape = Shape::from_index(self.get(id::SHAPE).round().max(0.0) as usize);
        self.tap
            .set_value(value::LFO, self.lfo.value(shape, 0.0) as f32);
        self.tap.set_value(value::PHASE, self.lfo.phase as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
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
        for l in &mut self.lines {
            l.reset();
        }
        self.stages = [[Allpass1::default(); 12]; 2];
        self.last = [0.0; 2];
        self.high_cut.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, bin_db, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<ModulationProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, ModulationProcessor::new)
    }

    fn noise(n: usize) -> (f32, f32) {
        let mut x = (n as u32)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(0x9e37_79b9);
        x ^= x >> 15;
        x = x.wrapping_mul(0x2c1b_3c6d);
        x ^= x >> 12;
        let v = (f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0) as f32 * 0.3;
        (v, v)
    }

    /// Power at `f` of a long noise run.
    fn band(x: &[f32], f: f64) -> f64 {
        // Average a few neighbouring bins against the noise's spread.
        (0..5)
            .map(|k| bin_db(x, f + (k as f64 - 2.0) * 4.0))
            .sum::<f64>()
            / 5.0
    }

    #[test]
    fn a_phaser_with_no_movement_cuts_notches() {
        // Depth 0: the corners stay at the centre. Each stage turns the
        // phase by 90° at its corner, so two make 180° there: mixed half
        // and half with the dry signal, a notch at the centre.
        let mut r = rig(&[
            (id::MODE, 3.0),
            (id::DEPTH, 0.0),
            (id::STAGES, 0.0),
            (id::CENTER, 1_000.0),
            (id::MIX, 0.5),
        ]);
        let (l, _) = r.run(2.0, noise, silence);
        let (notch, away) = (band(&l, 1_000.0), band(&l, 100.0));
        assert!(notch < away - 20.0, "{notch:.1} vs {away:.1}");
    }

    #[test]
    fn a_flanger_combs_and_chorus_keeps_the_level() {
        // A still flanger at 1 ms: notches at 500 Hz, 1500 Hz…
        let mut r = rig(&[
            (id::MODE, 2.0),
            (id::DEPTH, 0.0),
            (id::DELAY, 1.0),
            (id::MIX, 0.5),
            (id::FEEDBACK, 0.0),
        ]);
        let (l, _) = r.run(2.0, noise, silence);
        assert!(band(&l, 500.0) < band(&l, 1_000.0) - 15.0);
        // A chorus leaves a tone's level about where it was.
        let mut r = rig(&[(id::MODE, 0.0), (id::MIX, 1.0), (id::VOICES, 3.0)]);
        let (l, _) = r.run(1.0, tone(440.0, 0.3), silence);
        let tail = &l[l.len() / 2..];
        let rms =
            (tail.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        let lv = 20.0 * (rms * 2f64.sqrt()).log10();
        assert!(lv > -14.0 && lv < -7.0, "{lv:.1}");
    }

    #[test]
    fn vibrato_bends_the_pitch_and_sync_locks_the_rate() {
        // Vibrato: a tone's energy spreads to sidebands at the rate.
        let mut r = rig(&[
            (id::MODE, 4.0),
            (id::RATE, 5.0),
            (id::DEPTH, 1.0),
            (id::DELAY, 5.0),
        ]);
        let (l, _) = r.run(2.0, tone(1_000.0, 0.3), silence);
        assert!(bin_db(&l, 1_000.0) < 20.0 * 0.3f64.log10() - 3.0, "carrier");
        assert!(
            bin_db(&l, 1_005.0) > -50.0,
            "sideband {}",
            bin_db(&l, 1_005.0)
        );
        // Synced to a quarter at 120 BPM: two cycles a second.
        let mut r = rig(&[(id::SYNC, 1.0), (id::DIVISION, 11.0), (id::SHAPE, 2.0)]);
        r.transport.tempo = 120.0;
        r.transport.playing = true;
        let mut phases = Vec::new();
        for _ in 0..6 {
            r.run(0.25, silence, silence);
            r.transport.quarter_position += 0.5;
            phases.push(r.tap.value(value::PHASE));
        }
        // Every quarter second is half a cycle on.
        for w in phases.windows(2) {
            let d = (f64::from(w[1]) - f64::from(w[0])).rem_euclid(1.0);
            assert!((d - 0.5).abs() < 0.05, "{phases:?}");
        }
    }
}
