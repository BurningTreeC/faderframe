//! Gate: a gate, a downward expander or a ducker.
//!
//! * Gate: opens when the key rises over the threshold, closes (down by the
//!   range) once it has fallen under the threshold less the hysteresis and
//!   the hold time has passed; the two thresholds keep it from chattering.
//! * Expander: below the threshold the level falls by the ratio (2:1: ten
//!   dB under, twenty dB down), no further than the range.
//! * Ducker: turned down by the range while the key is over the threshold
//!   (a voice over music, a kick over a bass), back after the hold.
//!
//! The key is the input or the sidechain, through high and low cuts, and
//! can be heard. Opening takes the attack time, closing the release,
//! smoothed so neither clicks; lookahead opens the gate before the attack
//! that triggers it arrives.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::filter::{BandShape, BandType, Filter};
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

pub mod id {
    pub const THRESHOLD: u32 = 0;
    pub const RANGE: u32 = 1;
    pub const MODE: u32 = 2;
    pub const RATIO: u32 = 3;
    pub const ATTACK: u32 = 4;
    pub const HOLD: u32 = 5;
    pub const RELEASE: u32 = 6;
    pub const HYSTERESIS: u32 = 7;
    pub const LOOKAHEAD: u32 = 8;
    pub const EXTERNAL: u32 = 9;
    pub const SC_LOW: u32 = 10;
    pub const SC_HIGH: u32 = 11;
    pub const LISTEN: u32 = 12;
}

/// Published: attenuation now and its peak (dB, ≥ 0), input and output
/// peaks (linear), the key's level (dB), whether open (0/1).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const REDUCTION_PEAK: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const LEVEL: usize = 4;
    pub const OPEN: usize = 5;
}
pub const TAP_VALUES: usize = 6;

pub const MODES: [&str; 3] = ["Gate", "Expander", "Ducker"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::THRESHOLD, "Threshold", -80.0, 0.0, -40.0, Decibels),
        param(id::RANGE, "Range", -90.0, 0.0, -90.0, Decibels),
        stepped(id::MODE, "Mode", 2.0, 0.0),
        param(id::RATIO, "Ratio", 1.0, 20.0, 2.0, None),
        param(id::ATTACK, "Attack", 0.01, 100.0, 0.5, Milliseconds),
        param(id::HOLD, "Hold", 0.0, 1_000.0, 25.0, Milliseconds),
        param(id::RELEASE, "Release", 5.0, 4_000.0, 150.0, Milliseconds),
        param(id::HYSTERESIS, "Hysteresis", 0.0, 20.0, 4.0, Decibels),
        fixed(param(
            id::LOOKAHEAD,
            "Lookahead",
            0.0,
            10.0,
            0.0,
            Milliseconds,
        )),
        stepped(id::EXTERNAL, "External Sidechain", 1.0, 1.0),
        param(id::SC_LOW, "Sidechain Low Cut", 10.0, 2_000.0, 10.0, Hertz),
        param(
            id::SC_HIGH,
            "Sidechain High Cut",
            500.0,
            30_000.0,
            30_000.0,
            Hertz,
        ),
        stepped(id::LISTEN, "Sidechain Listen", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        id::EXTERNAL | id::LISTEN => on_off(v),
        id::RATIO => format!("{v:.1} : 1"),
        id::RANGE if v <= -89.5 => "−∞ dB".into(),
        id::SC_LOW if v <= 10.5 => "Off".into(),
        id::SC_HIGH if v >= 29_500.0 => "Off".into(),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    (get(params, id::LOOKAHEAD) * 0.001 * rate).round() as u32
}

fn cut(kind: BandType, freq: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain: 0.0,
        q: std::f64::consts::FRAC_1_SQRT_2,
        slope: 12.0,
    }
}

const STEP: usize = 32;

pub struct GateProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    sc_low: Filter<2>,
    sc_high: Filter<2>,
    /// The key's peak envelope.
    env: f64,
    env_fall: f64,
    open: bool,
    /// Samples left to hold open.
    hold: usize,
    /// The gain applied (linear) and the expander's smoothed gain (dB).
    gain: f64,
    exp_db: f64,
    delay: [Vec<f64>; 2],
    pos: usize,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl GateProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let look = latency(&params, sr) as usize;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mode = get(&params, id::MODE).round() as i64;
        Self {
            watching: Watching::new(sr as f32),
            sr,
            sc_low: Filter::default(),
            sc_high: Filter::default(),
            env: 0.0,
            // Long enough not to ripple through the hysteresis on a bass
            // note, short enough to follow a drum.
            env_fall: (-1.0 / (0.015 * sr)).exp(),
            // A ducker starts open (nothing to duck), a gate closed.
            open: mode == 2,
            hold: 0,
            gain: if mode == 2 { 1.0 } else { 0.0 },
            exp_db: 0.0,
            delay: [vec![0.0; look.max(1)], vec![0.0; look.max(1)]],
            pos: 0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }
}

impl PluginProcessor for GateProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        let side = io.audio_in.get(1).filter(|k| k.num_channels() > 0);
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
        let look = if self.get(id::LOOKAHEAD) > 0.0 {
            self.delay[0].len()
        } else {
            0
        };
        let mut events = ctx.param_events.iter().peekable();
        let (mut gr_peak, mut in_peak, mut out_peak, mut level_now) =
            (0.0f64, 0.0f64, 0.0f64, -150.0f64);
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
            let threshold = self.get(id::THRESHOLD);
            let range = self.get(id::RANGE);
            let floor = if range <= -89.5 { 0.0 } else { gain(range) };
            let mode = self.get(id::MODE).round() as i64;
            let ratio = self.get(id::RATIO).max(1.0);
            let hyst = self.get(id::HYSTERESIS);
            let k_open = 1.0 - (-1.0 / (self.get(id::ATTACK).max(0.01) * 0.001 * sr)).exp();
            let k_close = 1.0 - (-1.0 / (self.get(id::RELEASE).max(1.0) * 0.001 * sr)).exp();
            // Gate and ducker move in dB at a steady rate: the attack is
            // the time to open fully (a ducker: to duck), the release the
            // time to close (to come back).
            let span = (-range).max(1.0);
            let per = |ms: f64| span / (ms.max(0.01) * 0.001 * sr);
            let (rise, fall) = if mode == 2 {
                (per(self.get(id::RELEASE)), per(self.get(id::ATTACK)))
            } else {
                (per(self.get(id::ATTACK)), per(self.get(id::RELEASE)))
            };
            let hold = (self.get(id::HOLD) * 0.001 * sr) as usize;
            let external = self.get(id::EXTERNAL) >= 0.5 && side.is_some();
            let listen = self.get(id::LISTEN) >= 0.5;
            let low = self.get(id::SC_LOW);
            if low > 10.5 {
                self.sc_low.set(cut(BandType::LowCut, low), sr);
            } else {
                self.sc_low.clear();
            }
            let high = self.get(id::SC_HIGH);
            if high < 29_500.0 && high < 0.45 * sr {
                self.sc_high.set(cut(BandType::HighCut, high), sr);
            } else {
                self.sc_high.clear();
            }
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let key = match side {
                    Some(k) if external => [
                        f64::from(k.channel(0)[i]),
                        f64::from(k.channel(1.min(k.num_channels() - 1))[i]),
                    ],
                    _ => x,
                };
                let mut heard = [0.0; 2];
                let mut peak = 0.0f64;
                for c in 0..2 {
                    heard[c] = self.sc_high.process(c, self.sc_low.process(c, key[c]));
                    peak = peak.max(heard[c].abs());
                }
                self.env = if peak > self.env {
                    peak
                } else {
                    self.env * self.env_fall
                };
                let level = db(self.env);
                level_now = level_now.max(level);
                // Open or closed (a ducker is "open" while ducking).
                let over = level > threshold;
                let under = level < threshold - hyst;
                let target = match mode {
                    1 => {
                        // Expander: the gain follows the level below the
                        // threshold, with the hold before letting it fall.
                        let want = if over || level >= threshold {
                            0.0
                        } else {
                            ((level - threshold) * (ratio - 1.0)).max(range)
                        };
                        if want >= self.exp_db {
                            self.hold = hold;
                            self.exp_db += (want - self.exp_db) * k_open;
                        } else if self.hold > 0 {
                            self.hold -= 1;
                        } else {
                            self.exp_db += (want - self.exp_db) * k_close;
                        }
                        self.open = want > range + 0.5;
                        gain(self.exp_db)
                    }
                    _ => {
                        if over {
                            self.open = true;
                            self.hold = hold;
                        } else if under {
                            if self.hold > 0 {
                                self.hold -= 1;
                            } else {
                                self.open = false;
                            }
                        }
                        let passing = if mode == 2 { !self.open } else { self.open };
                        let to = if passing { 0.0 } else { range.max(-90.0) };
                        let now = db(self.gain.max(1e-9)).max(-90.0);
                        let next = if to > now {
                            (now + rise).min(to)
                        } else {
                            (now - fall).max(to)
                        };
                        self.gain = if next <= -89.99 && floor == 0.0 {
                            0.0
                        } else {
                            gain(next)
                        };
                        self.gain
                    }
                };
                let g = if mode == 1 { target } else { self.gain };
                gr_peak = gr_peak.max(-db(g.max(1e-9)));
                let dry = if look > 0 {
                    let p = self.pos;
                    let d = [self.delay[0][p], self.delay[1][p]];
                    self.delay[0][p] = x[0];
                    self.delay[1][p] = x[1];
                    self.pos = (p + 1) % look;
                    d
                } else {
                    x
                };
                let y = if listen {
                    heard
                } else {
                    [dry[0] * g, dry[1] * g]
                };
                out_peak = out_peak.max(y[0].abs()).max(y[1].abs());
                if channels >= 2 {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                } else {
                    out.channel_mut(0)[i] = y[0] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.env = flush(self.env);
        self.sc_low.flush();
        self.sc_high.flush();
        let mode = self.get(id::MODE).round() as i64;
        let g_now = if mode == 1 {
            gain(self.exp_db)
        } else {
            self.gain
        };
        self.tap
            .set_value(value::REDUCTION, (-db(g_now.max(1e-9))).min(90.0) as f32);
        self.tap
            .raise_value(value::REDUCTION_PEAK, gr_peak.min(90.0) as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.set_value(value::LEVEL, level_now as f32);
        self.tap
            .set_value(value::OPEN, if self.open { 1.0 } else { 0.0 });
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
        self.env = 0.0;
        self.hold = 0;
        self.exp_db = 0.0;
        for d in &mut self.delay {
            d.fill(0.0);
        }
        self.sc_low.reset();
        self.sc_high.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, level, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<GateProcessor> {
        let mut all = vec![(id::EXTERNAL, 0.0)];
        all.extend_from_slice(set);
        Rig::with(parameters(), TAP_VALUES, &all, GateProcessor::new)
    }

    #[test]
    fn a_gate_closes_under_the_threshold_and_opens_over_it() {
        let mut r = rig(&[(id::THRESHOLD, -30.0), (id::RANGE, -60.0)]);
        let (l, _) = r.run(0.5, tone(500.0, gain(-50.0)), silence);
        assert!(level(&l, 500.0) < -105.0, "closed: {}", level(&l, 500.0));
        let (l, _) = r.run(0.5, tone(500.0, gain(-10.0)), silence);
        assert!(
            (level(&l, 500.0) + 10.0).abs() < 0.1,
            "open: {}",
            level(&l, 500.0)
        );
        // In between the two thresholds it stays as it was (open).
        let (l, _) = r.run(0.5, tone(500.0, gain(-32.0)), silence);
        assert!(
            (level(&l, 500.0) + 32.0).abs() < 0.1,
            "held open: {}",
            level(&l, 500.0)
        );
        assert_eq!(r.tap.value(value::OPEN), 1.0);
        // Under the lower one it closes.
        let (l, _) = r.run(0.5, tone(500.0, gain(-40.0)), silence);
        assert!(
            level(&l, 500.0) < -95.0,
            "closed again: {}",
            level(&l, 500.0)
        );
    }

    #[test]
    fn the_hold_keeps_it_open() {
        let mut r = rig(&[
            (id::THRESHOLD, -30.0),
            (id::HOLD, 200.0),
            (id::RELEASE, 5.0),
        ]);
        r.run(0.2, tone(500.0, 0.5), silence);
        let (l, _) = r.run(0.3, tone(500.0, gain(-50.0)), silence);
        let at = |ms: f64| {
            let i = (ms * 0.001 * SR) as usize;
            level(&l[i..i + 960], 500.0)
        };
        assert!((at(100.0) + 50.0).abs() < 0.5, "still open: {}", at(100.0));
        assert!(at(260.0) < -100.0, "closed after: {}", at(260.0));
    }

    #[test]
    fn an_expander_turns_down_by_its_ratio() {
        let mut r = rig(&[
            (id::MODE, 1.0),
            (id::THRESHOLD, -20.0),
            (id::RATIO, 2.0),
            (id::RANGE, -60.0),
        ]);
        let (l, _) = r.run(1.0, tone(500.0, gain(-30.0)), silence);
        // Ten under at 2:1: ten more down.
        assert!(
            (level(&l, 500.0) + 40.0).abs() < 0.6,
            "{}",
            level(&l, 500.0)
        );
        let (l, _) = r.run(0.5, tone(500.0, gain(-10.0)), silence);
        assert!((level(&l, 500.0) + 10.0).abs() < 0.2);
    }

    #[test]
    fn a_ducker_turns_down_while_the_key_plays() {
        let mut r = rig(&[
            (id::MODE, 2.0),
            (id::EXTERNAL, 1.0),
            (id::THRESHOLD, -30.0),
            (id::RANGE, -12.0),
        ]);
        let (l, _) = r.run(0.5, tone(500.0, 0.25), silence);
        assert!(
            (level(&l, 500.0) + 12.04).abs() < 0.2,
            "untouched without a key"
        );
        let (l, _) = r.run(0.5, tone(500.0, 0.25), tone(80.0, 0.5));
        assert!(
            (level(&l, 500.0) + 24.04).abs() < 0.3,
            "ducked: {}",
            level(&l, 500.0)
        );
    }

    #[test]
    fn lookahead_opens_ahead_of_the_attack() {
        let mut r = rig(&[
            (id::LOOKAHEAD, 5.0),
            (id::THRESHOLD, -30.0),
            (id::ATTACK, 1.0),
        ]);
        assert_eq!(latency(&r.params, SR), 240);
        r.run(0.1, silence, silence);
        let start = r.at;
        let (l, _) = r.run(
            0.1,
            move |n| {
                if n >= start + 1_000 {
                    (0.5, 0.5)
                } else {
                    (0.0, 0.0)
                }
            },
            silence,
        );
        // The step comes out 240 later, already at full level.
        assert!(l[1_000 + 240 + 2] > 0.45, "{}", l[1_000 + 242]);
    }
}
