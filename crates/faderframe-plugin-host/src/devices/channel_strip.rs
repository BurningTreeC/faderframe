//! Channel Strip: a console channel in one insert — input trim, high and
//! low pass filters, a gate/expander, a compressor, a four-band equaliser,
//! console drive and the output.
//!
//! * Filters: high pass 16–600 Hz at 12, 18 or 24 dB/oct, low pass
//!   3–22 kHz at 12 dB/oct; optionally only in the dynamics' key (the
//!   console's "filters to side-chain").
//! * Gate: opens over the threshold, closes 3 dB under it after the hold,
//!   down by the range; as an expander, 2:1 under the threshold.
//! * Compressor: the threshold, ratio and knee of the gain computer the
//!   stock compressor uses, peak or RMS detection, attack and release,
//!   make-up and a mix for parallel compression; the dynamics are keyed by
//!   the channel itself or the sidechain.
//! * EQ: low and high bands as shelves or bells, two parametric mids; the
//!   "Black" character narrows the bands as their gain grows (proportional
//!   Q), "Brown" keeps them as set.
//! * Order: the equaliser before the dynamics, or after them.
//! * Drive: a gentle asymmetric console saturation, unity for small
//!   signals.
//!
//! Zero latency; the dynamics are linked across both channels.

use super::compressor::reduction;
use super::{on_off, param, pass_through, pick, stepped};
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

/// Parameter ids (their positions too).
pub mod id {
    pub const INPUT: u32 = 0;
    pub const HPF: u32 = 1;
    pub const HPF_SLOPE: u32 = 2;
    pub const LPF: u32 = 3;
    pub const FILTERS_TO_SC: u32 = 4;
    pub const GATE: u32 = 5;
    pub const GATE_THRESHOLD: u32 = 6;
    pub const GATE_RANGE: u32 = 7;
    pub const GATE_MODE: u32 = 8;
    pub const GATE_ATTACK: u32 = 9;
    pub const GATE_HOLD: u32 = 10;
    pub const GATE_RELEASE: u32 = 11;
    pub const COMP: u32 = 12;
    pub const COMP_THRESHOLD: u32 = 13;
    pub const COMP_RATIO: u32 = 14;
    pub const COMP_ATTACK: u32 = 15;
    pub const COMP_RELEASE: u32 = 16;
    pub const COMP_KNEE: u32 = 17;
    pub const COMP_MAKEUP: u32 = 18;
    pub const COMP_PEAK: u32 = 19;
    pub const COMP_MIX: u32 = 20;
    pub const EXTERNAL: u32 = 21;
    pub const EQ: u32 = 22;
    pub const LF_GAIN: u32 = 23;
    pub const LF_FREQ: u32 = 24;
    pub const LF_BELL: u32 = 25;
    pub const LMF_GAIN: u32 = 26;
    pub const LMF_FREQ: u32 = 27;
    pub const LMF_Q: u32 = 28;
    pub const HMF_GAIN: u32 = 29;
    pub const HMF_FREQ: u32 = 30;
    pub const HMF_Q: u32 = 31;
    pub const HF_GAIN: u32 = 32;
    pub const HF_FREQ: u32 = 33;
    pub const HF_BELL: u32 = 34;
    pub const EQ_TYPE: u32 = 35;
    pub const ORDER: u32 = 36;
    pub const DRIVE: u32 = 37;
    pub const OUTPUT: u32 = 38;
}

/// Published: the compressor's reduction now and its peak, the gate's
/// (dB, ≥ 0), input and output peaks (linear), whether the gate is open.
pub mod value {
    pub const COMP_GR: usize = 0;
    pub const COMP_GR_PEAK: usize = 1;
    pub const GATE_GR: usize = 2;
    pub const IN_PEAK: usize = 3;
    pub const OUT_PEAK: usize = 4;
    pub const OPEN: usize = 5;
}
pub const TAP_VALUES: usize = 6;

pub const SLOPES: [&str; 3] = ["12 dB", "18 dB", "24 dB"];
pub const GATE_MODES: [&str; 2] = ["Gate", "Expander"];
pub const EQ_TYPES: [&str; 2] = ["Brown", "Black"];
pub const ORDERS: [&str; 2] = ["EQ → Dynamics", "Dynamics → EQ"];

/// The high pass is off at its lowest frequency, the low pass at its
/// highest.
pub const HPF_OFF: f64 = 16.0;
pub const LPF_OFF: f64 = 22_000.0;
pub const GATE_FLOOR: f64 = -80.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::INPUT, "Input", -24.0, 24.0, 0.0, Decibels),
        param(id::HPF, "High Pass", HPF_OFF, 600.0, HPF_OFF, Hertz),
        stepped(id::HPF_SLOPE, "High Pass Slope", 2.0, 1.0),
        param(id::LPF, "Low Pass", 3_000.0, LPF_OFF, LPF_OFF, Hertz),
        stepped(id::FILTERS_TO_SC, "Filters to Side-chain", 1.0, 0.0),
        stepped(id::GATE, "Gate", 1.0, 0.0),
        param(
            id::GATE_THRESHOLD,
            "Gate Threshold",
            -80.0,
            0.0,
            -50.0,
            Decibels,
        ),
        param(
            id::GATE_RANGE,
            "Gate Range",
            GATE_FLOOR,
            0.0,
            -40.0,
            Decibels,
        ),
        stepped(id::GATE_MODE, "Gate Mode", 1.0, 0.0),
        param(id::GATE_ATTACK, "Gate Attack", 0.1, 50.0, 0.5, Milliseconds),
        param(id::GATE_HOLD, "Gate Hold", 0.0, 1_000.0, 20.0, Milliseconds),
        param(
            id::GATE_RELEASE,
            "Gate Release",
            5.0,
            4_000.0,
            120.0,
            Milliseconds,
        ),
        stepped(id::COMP, "Compressor", 1.0, 1.0),
        param(id::COMP_THRESHOLD, "Threshold", -40.0, 10.0, 0.0, Decibels),
        param(id::COMP_RATIO, "Ratio", 1.0, 20.0, 2.0, None),
        param(id::COMP_ATTACK, "Attack", 0.1, 100.0, 10.0, Milliseconds),
        param(
            id::COMP_RELEASE,
            "Release",
            10.0,
            4_000.0,
            200.0,
            Milliseconds,
        ),
        param(id::COMP_KNEE, "Knee", 0.0, 12.0, 3.0, Decibels),
        param(id::COMP_MAKEUP, "Make-up", 0.0, 24.0, 0.0, Decibels),
        stepped(id::COMP_PEAK, "Peak Detection", 1.0, 0.0),
        param(id::COMP_MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        stepped(id::EXTERNAL, "External Sidechain", 1.0, 0.0),
        stepped(id::EQ, "EQ", 1.0, 1.0),
        param(id::LF_GAIN, "LF Gain", -16.0, 16.0, 0.0, Decibels),
        param(id::LF_FREQ, "LF Frequency", 30.0, 450.0, 100.0, Hertz),
        stepped(id::LF_BELL, "LF Bell", 1.0, 0.0),
        param(id::LMF_GAIN, "LMF Gain", -16.0, 16.0, 0.0, Decibels),
        param(id::LMF_FREQ, "LMF Frequency", 200.0, 2_500.0, 600.0, Hertz),
        param(id::LMF_Q, "LMF Q", 0.4, 3.0, 0.8, None),
        param(id::HMF_GAIN, "HMF Gain", -16.0, 16.0, 0.0, Decibels),
        param(
            id::HMF_FREQ,
            "HMF Frequency",
            600.0,
            7_000.0,
            2_500.0,
            Hertz,
        ),
        param(id::HMF_Q, "HMF Q", 0.4, 3.0, 0.8, None),
        param(id::HF_GAIN, "HF Gain", -16.0, 16.0, 0.0, Decibels),
        param(
            id::HF_FREQ,
            "HF Frequency",
            1_500.0,
            16_000.0,
            10_000.0,
            Hertz,
        ),
        stepped(id::HF_BELL, "HF Bell", 1.0, 0.0),
        stepped(id::EQ_TYPE, "EQ Type", 1.0, 0.0),
        stepped(id::ORDER, "Order", 1.0, 0.0),
        param(id::DRIVE, "Drive", 0.0, 1.0, 0.0, Percent),
        param(id::OUTPUT, "Output", -24.0, 24.0, 0.0, Decibels),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::HPF_SLOPE => pick(&SLOPES, v),
        id::GATE_MODE => pick(&GATE_MODES, v),
        id::EQ_TYPE => pick(&EQ_TYPES, v),
        id::ORDER => pick(&ORDERS, v),
        id::FILTERS_TO_SC
        | id::GATE
        | id::COMP
        | id::COMP_PEAK
        | id::EXTERNAL
        | id::EQ
        | id::LF_BELL
        | id::HF_BELL => on_off(v),
        id::HPF if v <= HPF_OFF + 0.5 => "Off".into(),
        id::LPF if v >= LPF_OFF - 50.0 => "Off".into(),
        id::GATE_RANGE if v <= GATE_FLOOR + 0.5 => "−∞ dB".into(),
        id::COMP_RATIO if v >= 19.9 => "∞ : 1".into(),
        id::COMP_RATIO => format!("{v:.1} : 1"),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The bands as designed for the current settings: high pass, low pass,
/// then LF, LMF, HMF, HF (`None`: off). Shared with the editor's curve.
pub fn bands(params: &ParamValues) -> [Option<BandShape>; 6] {
    let g = |pid| get(params, pid);
    let butter = std::f64::consts::FRAC_1_SQRT_2;
    let hpf = g(id::HPF);
    let lpf = g(id::LPF);
    let slope = 12.0 + 6.0 * g(id::HPF_SLOPE).round().clamp(0.0, 2.0);
    let eq = g(id::EQ) >= 0.5;
    let black = g(id::EQ_TYPE) >= 0.5;
    // Proportional Q: narrower as the gain grows (a band at ±15 dB about
    // twice as narrow as one at ±1).
    let q = |q: f64, gain: f64| {
        if black {
            q * (1.0 + gain.abs() / 15.0)
        } else {
            q
        }
    };
    let shape = |kind, freq, gain: f64, qv| {
        (eq && gain.abs() > 0.01).then_some(BandShape {
            kind,
            freq,
            gain,
            q: qv,
            slope: 12.0,
        })
    };
    let lf = g(id::LF_GAIN);
    let hf = g(id::HF_GAIN);
    let (lmf, hmf) = (g(id::LMF_GAIN), g(id::HMF_GAIN));
    [
        (hpf > HPF_OFF + 0.5).then_some(BandShape {
            kind: BandType::LowCut,
            freq: hpf,
            gain: 0.0,
            q: butter,
            slope,
        }),
        (lpf < LPF_OFF - 50.0).then_some(BandShape {
            kind: BandType::HighCut,
            freq: lpf,
            gain: 0.0,
            q: butter,
            slope: 12.0,
        }),
        shape(
            if g(id::LF_BELL) >= 0.5 {
                BandType::Bell
            } else {
                BandType::LowShelf
            },
            g(id::LF_FREQ),
            lf,
            if g(id::LF_BELL) >= 0.5 {
                q(0.7, lf)
            } else {
                butter
            },
        ),
        shape(BandType::Bell, g(id::LMF_FREQ), lmf, q(g(id::LMF_Q), lmf)),
        shape(BandType::Bell, g(id::HMF_FREQ), hmf, q(g(id::HMF_Q), hmf)),
        shape(
            if g(id::HF_BELL) >= 0.5 {
                BandType::Bell
            } else {
                BandType::HighShelf
            },
            g(id::HF_FREQ),
            hf,
            if g(id::HF_BELL) >= 0.5 {
                q(0.7, hf)
            } else {
                butter
            },
        ),
    ]
}

/// Console drive: unity slope at zero, rounding peaks with a little
/// asymmetry (even harmonics); `drive` 0…1.
#[inline]
fn drive(x: f64, drive: f64) -> f64 {
    if drive <= 1e-6 {
        return x;
    }
    let k = 1.0 + 4.0 * drive;
    let b = 0.12 * drive;
    let tb = (k * b).tanh();
    ((k * (x + b)).tanh() - tb) / (k * (1.0 - tb * tb))
}

/// Sub-blocks the controls are updated in.
const STEP: usize = 32;

pub struct ChannelStripProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    /// High pass, low pass, LF, LMF, HMF, HF.
    filters: [Filter<2>; 6],
    designed: [Option<BandShape>; 6],
    // Gate.
    gate_env: f64,
    gate_fall: f64,
    open: bool,
    hold: usize,
    gate_db: f64,
    // Compressor.
    comp_env: f64,
    comp_ms: f64,
    comp_gr: f64,
    // Gains, smoothed (linear).
    input: f64,
    output: f64,
    makeup: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl ChannelStripProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let g = |pid| gain(get(&params, pid));
        Self {
            watching: Watching::new(sr as f32),
            sr,
            filters: Default::default(),
            designed: [None; 6],
            gate_env: 0.0,
            gate_fall: (-1.0 / (0.015 * sr)).exp(),
            open: false,
            hold: 0,
            gate_db: GATE_FLOOR,
            comp_env: 0.0,
            comp_ms: 0.0,
            comp_gr: 0.0,
            input: g(id::INPUT),
            output: g(id::OUTPUT),
            makeup: g(id::COMP_MAKEUP),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    /// Redesign the bands that changed.
    fn design(&mut self) {
        let want = bands(&self.params);
        let nyquist = 0.45 * self.sr;
        for (i, shape) in want.iter().enumerate() {
            if self.designed[i] == *shape {
                continue;
            }
            match shape {
                Some(s) if s.freq < nyquist => self.filters[i].set(*s, self.sr),
                _ => self.filters[i].clear(),
            }
            self.designed[i] = *shape;
        }
    }
}

impl PluginProcessor for ChannelStripProcessor {
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
        let mut events = ctx.param_events.iter().peekable();
        let (mut comp_peak, mut in_peak, mut out_peak) = (0.0f64, 0.0f64, 0.0f64);
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
            self.design();
            let smooth = 1.0 - (-1.0 / (0.005 * sr)).exp();
            let (in_t, out_t) = (gain(self.get(id::INPUT)), gain(self.get(id::OUTPUT)));
            let makeup_t = gain(self.get(id::COMP_MAKEUP));
            let to_sc = self.get(id::FILTERS_TO_SC) >= 0.5;
            let external = self.get(id::EXTERNAL) >= 0.5 && side.is_some();
            let eq_first = self.get(id::ORDER) < 0.5;
            // Gate.
            let gate_on = self.get(id::GATE) >= 0.5;
            let g_thr = self.get(id::GATE_THRESHOLD);
            let g_range = self.get(id::GATE_RANGE);
            let expander = self.get(id::GATE_MODE) >= 0.5;
            let span = (-g_range).max(1.0);
            let rise = span / (self.get(id::GATE_ATTACK).max(0.1) * 0.001 * sr);
            let fall = span / (self.get(id::GATE_RELEASE).max(1.0) * 0.001 * sr);
            let k_open = 1.0 - (-1.0 / (self.get(id::GATE_ATTACK).max(0.1) * 0.001 * sr)).exp();
            let k_close = 1.0 - (-1.0 / (self.get(id::GATE_RELEASE).max(1.0) * 0.001 * sr)).exp();
            let hold = (self.get(id::GATE_HOLD) * 0.001 * sr) as usize;
            // Compressor.
            let comp_on = self.get(id::COMP) >= 0.5;
            let c_thr = self.get(id::COMP_THRESHOLD);
            let ratio = self.get(id::COMP_RATIO);
            let ratio = if ratio >= 19.9 {
                super::compressor::INFINITE
            } else {
                ratio
            };
            let knee = self.get(id::COMP_KNEE);
            let peak = self.get(id::COMP_PEAK) >= 0.5;
            let mix = self.get(id::COMP_MIX).clamp(0.0, 1.0);
            let k_att = 1.0 - (-1.0 / (self.get(id::COMP_ATTACK).max(0.05) * 0.001 * sr)).exp();
            let k_rel = 1.0 - (-1.0 / (self.get(id::COMP_RELEASE).max(1.0) * 0.001 * sr)).exp();
            let k_rms = 1.0 - (-1.0 / (0.01 * sr)).exp();
            let amount = self.get(id::DRIVE).clamp(0.0, 1.0);
            for i in at..end {
                self.input += (in_t - self.input) * smooth;
                self.output += (out_t - self.output) * smooth;
                self.makeup += (makeup_t - self.makeup) * smooth;
                let raw = [
                    f64::from(self.scratch[0][i]) * self.input,
                    f64::from(self.scratch[1][i]) * self.input,
                ];
                in_peak = in_peak.max(raw[0].abs()).max(raw[1].abs());
                // The filters (or only the key's).
                let mut filtered = raw;
                for (c, v) in filtered.iter_mut().enumerate() {
                    let hp = self.filters[0].process(c, *v);
                    *v = self.filters[1].process(c, hp);
                }
                let mut x = if to_sc { raw } else { filtered };
                let eq = |s: &mut Self, x: &mut [f64; 2]| {
                    for (c, v) in x.iter_mut().enumerate() {
                        let mut y = *v;
                        for f in &mut s.filters[2..] {
                            y = f.process(c, y);
                        }
                        *v = y;
                    }
                };
                if eq_first {
                    eq(self, &mut x);
                }
                // The key: the sidechain, the filtered channel, or what
                // reaches the dynamics.
                let key = match side {
                    Some(k) if external => [
                        f64::from(k.channel(0)[i]),
                        f64::from(k.channel(1.min(k.num_channels() - 1))[i]),
                    ],
                    _ if to_sc => filtered,
                    _ => x,
                };
                let key_peak = key[0].abs().max(key[1].abs());
                // Gate / expander.
                if gate_on {
                    self.gate_env = if key_peak > self.gate_env {
                        key_peak
                    } else {
                        self.gate_env * self.gate_fall
                    };
                    let level = db(self.gate_env);
                    let target = if expander {
                        if level >= g_thr {
                            0.0
                        } else {
                            (level - g_thr).max(g_range)
                        }
                    } else {
                        if level > g_thr {
                            self.open = true;
                            self.hold = hold;
                        } else if level < g_thr - 3.0 {
                            if self.hold > 0 {
                                self.hold -= 1;
                            } else {
                                self.open = false;
                            }
                        }
                        if self.open { 0.0 } else { g_range }
                    };
                    if expander {
                        let k = if target > self.gate_db {
                            k_open
                        } else {
                            k_close
                        };
                        self.gate_db += (target - self.gate_db) * k;
                    } else if target > self.gate_db {
                        self.gate_db = (self.gate_db + rise).min(target);
                    } else {
                        self.gate_db = (self.gate_db - fall).max(target);
                    }
                    let g = if self.gate_db <= GATE_FLOOR + 0.01 {
                        0.0
                    } else {
                        gain(self.gate_db)
                    };
                    x = [x[0] * g, x[1] * g];
                } else {
                    self.gate_db = 0.0;
                    self.open = true;
                }
                // Compressor.
                if comp_on {
                    let detect = if peak {
                        self.comp_env = key_peak;
                        key_peak
                    } else {
                        let sq = 0.5 * (key[0] * key[0] + key[1] * key[1]);
                        self.comp_ms += (sq - self.comp_ms) * k_rms;
                        // RMS of a sine reads as its peak (like a VU).
                        (2.0 * self.comp_ms).sqrt()
                    };
                    let target = reduction(db(detect), c_thr, ratio, knee);
                    let k = if target > self.comp_gr { k_att } else { k_rel };
                    self.comp_gr += (target - self.comp_gr) * k;
                    comp_peak = comp_peak.max(self.comp_gr);
                    let g = gain(-self.comp_gr) * self.makeup;
                    let wet = [x[0] * g, x[1] * g];
                    x = [x[0] + (wet[0] - x[0]) * mix, x[1] + (wet[1] - x[1]) * mix];
                } else {
                    self.comp_gr = 0.0;
                }
                if !eq_first {
                    eq(self, &mut x);
                }
                let y = [
                    drive(x[0], amount) * self.output,
                    drive(x[1], amount) * self.output,
                ];
                out_peak = out_peak.max(y[0].abs()).max(y[1].abs());
                out.channel_mut(0)[i] = y[0] as f32;
                if channels >= 2 {
                    out.channel_mut(1)[i] = y[1] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.gate_env = flush(self.gate_env);
        self.comp_ms = flush(self.comp_ms);
        for f in &mut self.filters {
            f.flush();
        }
        self.tap
            .set_value(value::COMP_GR, self.comp_gr.clamp(0.0, 60.0) as f32);
        self.tap
            .raise_value(value::COMP_GR_PEAK, comp_peak.clamp(0.0, 60.0) as f32);
        // The gate as it is now (how far down).
        let gate_now = if self.get(id::GATE) >= 0.5 {
            -self.gate_db
        } else {
            0.0
        };
        self.tap
            .set_value(value::GATE_GR, gate_now.clamp(0.0, 80.0) as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
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
        self.gate_env = 0.0;
        self.comp_env = 0.0;
        self.comp_ms = 0.0;
        self.comp_gr = 0.0;
        self.hold = 0;
        self.gate_db = GATE_FLOOR;
        for f in &mut self.filters {
            f.reset();
        }
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, level, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<ChannelStripProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, ChannelStripProcessor::new)
    }

    #[test]
    fn ids_are_positions() {
        for (i, p) in parameters().iter().enumerate() {
            assert_eq!(p.id.0 as usize, i, "{}", p.name);
        }
    }

    #[test]
    fn as_it_comes_it_passes_the_signal() {
        let mut r = rig(&[]);
        r.run(0.2, tone(1_000.0, gain(-20.0)), silence);
        let (l, _) = r.run(0.3, tone(1_000.0, gain(-20.0)), silence);
        assert!(
            (level(&l, 1_000.0) + 20.0).abs() < 0.05,
            "{}",
            level(&l, 1_000.0)
        );
    }

    #[test]
    fn the_high_pass_cuts_the_lows() {
        let mut r = rig(&[(id::HPF, 200.0)]);
        r.run(0.2, tone(50.0, 0.5), silence);
        let (l, _) = r.run(0.5, tone(50.0, 0.5), silence);
        // Two octaves under 200 Hz at 18 dB/oct.
        assert!(level(&l, 50.0) < -6.0 - 30.0, "{}", level(&l, 50.0));
        let (l, _) = r.run(0.3, tone(2_000.0, 0.5), silence);
        assert!((level(&l, 2_000.0) + 6.02).abs() < 0.2);
    }

    #[test]
    fn the_low_shelf_lifts_the_lows() {
        let mut r = rig(&[(id::LF_GAIN, 6.0), (id::LF_FREQ, 100.0)]);
        r.run(0.2, tone(30.0, gain(-20.0)), silence);
        let (l, _) = r.run(0.5, tone(30.0, gain(-20.0)), silence);
        assert!((level(&l, 30.0) + 14.0).abs() < 0.6, "{}", level(&l, 30.0));
        let (l, _) = r.run(0.3, tone(5_000.0, gain(-20.0)), silence);
        assert!((level(&l, 5_000.0) + 20.0).abs() < 0.2);
    }

    #[test]
    fn the_compressor_takes_its_ratio_over_the_threshold() {
        let mut r = rig(&[
            (id::COMP_THRESHOLD, -20.0),
            (id::COMP_RATIO, 4.0),
            (id::COMP_KNEE, 0.0),
            (id::COMP_PEAK, 1.0),
            (id::COMP_ATTACK, 1.0),
            (id::COMP_RELEASE, 50.0),
        ]);
        r.run(0.5, tone(500.0, gain(-8.0)), silence);
        let (l, _) = r.run(0.5, tone(500.0, gain(-8.0)), silence);
        // Twelve over at 4:1: three over, nine taken.
        let out = level(&l, 500.0);
        assert!((out + 17.0).abs() < 1.0, "{out}");
        assert!((r.tap.value(value::COMP_GR) - 9.0).abs() < 1.0);
    }

    #[test]
    fn the_gate_closes_under_its_threshold() {
        let mut r = rig(&[
            (id::GATE, 1.0),
            (id::GATE_THRESHOLD, -40.0),
            (id::GATE_RANGE, GATE_FLOOR),
        ]);
        let (l, _) = r.run(0.5, tone(500.0, gain(-60.0)), silence);
        assert!(level(&l, 500.0) < -120.0, "{}", level(&l, 500.0));
        let (l, _) = r.run(0.5, tone(500.0, gain(-20.0)), silence);
        assert!(
            (level(&l, 500.0) + 20.0).abs() < 0.2,
            "{}",
            level(&l, 500.0)
        );
    }

    #[test]
    fn the_order_decides_what_the_compressor_hears() {
        let set = |order: f64| {
            let mut r = rig(&[
                (id::ORDER, order),
                (id::HMF_GAIN, 12.0),
                (id::HMF_FREQ, 1_000.0),
                (id::COMP_THRESHOLD, -20.0),
                (id::COMP_RATIO, 10.0),
                (id::COMP_KNEE, 0.0),
                (id::COMP_PEAK, 1.0),
                (id::COMP_ATTACK, 1.0),
            ]);
            r.run(0.5, tone(1_000.0, gain(-20.0)), silence);
            let (l, _) = r.run(0.5, tone(1_000.0, gain(-20.0)), silence);
            level(&l, 1_000.0)
        };
        // EQ first: the boost reaches the compressor (12 over → 1.2).
        let eq_first = set(0.0);
        assert!((eq_first + 18.8).abs() < 1.0, "{eq_first}");
        // Dynamics first: nothing over, then boosted.
        let dyn_first = set(1.0);
        assert!((dyn_first + 8.0).abs() < 0.5, "{dyn_first}");
    }

    #[test]
    fn the_sidechain_can_key_the_dynamics() {
        let mut r = rig(&[
            (id::EXTERNAL, 1.0),
            (id::COMP_THRESHOLD, -30.0),
            (id::COMP_RATIO, 20.0),
            (id::COMP_KNEE, 0.0),
            (id::COMP_PEAK, 1.0),
            (id::COMP_ATTACK, 1.0),
        ]);
        let (l, _) = r.run(0.5, tone(500.0, gain(-20.0)), silence);
        assert!(
            (level(&l, 500.0) + 20.0).abs() < 0.2,
            "quiet key: untouched"
        );
        let (l, _) = r.run(0.5, tone(500.0, gain(-20.0)), tone(80.0, 0.5));
        assert!(level(&l, 500.0) < -40.0, "keyed: {}", level(&l, 500.0));
    }

    #[test]
    fn drive_adds_harmonics_and_leaves_small_signals_alone() {
        let mut r = rig(&[(id::DRIVE, 1.0)]);
        r.run(0.2, tone(500.0, 0.5), silence);
        let (l, _) = r.run(0.5, tone(500.0, 0.5), silence);
        assert!(level(&l, 1_500.0) > -60.0, "third: {}", level(&l, 1_500.0));
        assert!(level(&l, 1_000.0) > -70.0, "second: {}", level(&l, 1_000.0));
        let (l, _) = r.run(0.5, tone(500.0, gain(-50.0)), silence);
        assert!(
            (level(&l, 500.0) + 50.0).abs() < 0.3,
            "{}",
            level(&l, 500.0)
        );
    }
}
