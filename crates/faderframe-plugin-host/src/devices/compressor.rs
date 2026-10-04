//! Compressor: a feed-forward or feedback compressor in five styles.
//!
//! * Clean: feed-forward, an RMS-leaning detector, no colour: transparent.
//! * Punch: feed-forward on the peaks, quick to let go: drums keep their
//!   snap.
//! * Opto: feedback, a slow-ish detector and a release that depends on the
//!   programme (fast for short peaks, slow for long ones), a wider knee: the
//!   smooth levelling of an optical cell.
//! * Vintage: feedback on the peaks, very fast, and saturation that grows
//!   with the reduction: the bite of a FET limiting amplifier.
//! * Bus: feed-forward RMS, stereo linked, auto release: glue for groups.
//!
//! Around the gain computer (threshold, ratio up to ∞:1, a soft knee of any
//! width, a range that caps the reduction) are the attack and release
//! (smoothed in dB; auto release rides a slow stage), lookahead (the audio
//! is delayed, the detector sees ahead), stereo link, parallel mix (the dry
//! path delayed to match), makeup (manual or automatic), colour (harmonic
//! saturation) and a sidechain: the external input or the input itself,
//! through high and low cuts, which can be heard on its own.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::dsp::delay::DelayLine;
use crate::dsp::env::{DualRelease, MeanSquare};
use crate::dsp::filter::{BandShape, BandType, Filter};
use crate::dsp::smooth::Smoothed;
use crate::dsp::{db, flush, gain};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

/// Parameter ids (0–4 are the first compressor's and keep their meaning).
pub mod id {
    pub const THRESHOLD: u32 = 0;
    pub const RATIO: u32 = 1;
    pub const ATTACK: u32 = 2;
    pub const RELEASE: u32 = 3;
    pub const MAKEUP: u32 = 4;
    pub const KNEE: u32 = 5;
    pub const STYLE: u32 = 6;
    pub const AUTO_RELEASE: u32 = 7;
    pub const AUTO_MAKEUP: u32 = 8;
    pub const MIX: u32 = 9;
    pub const LOOKAHEAD: u32 = 10;
    pub const DETECTOR: u32 = 11;
    pub const LINK: u32 = 12;
    pub const EXTERNAL: u32 = 13;
    pub const SC_LOW: u32 = 14;
    pub const SC_HIGH: u32 = 15;
    pub const LISTEN: u32 = 16;
    pub const RANGE: u32 = 17;
    pub const COLOR: u32 = 18;
}

/// Published values: the reduction now and its peak since the editor last
/// looked (dB), the input and output peaks since then (linear), the
/// detector's level now (dB).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const REDUCTION_PEAK: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const LEVEL: usize = 4;
}
pub const TAP_VALUES: usize = 5;

/// The ratio meaning "∞:1".
pub const INFINITE: f64 = 100.0;

pub const STYLES: [&str; 5] = ["Clean", "Punch", "Opto", "Vintage", "Bus"];
pub const DETECTORS: [&str; 2] = ["Peak", "RMS"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::THRESHOLD, "Threshold", -60.0, 0.0, -20.0, Decibels),
        param(id::RATIO, "Ratio", 1.0, INFINITE, 4.0, None),
        param(id::ATTACK, "Attack", 0.01, 250.0, 10.0, Milliseconds),
        param(id::RELEASE, "Release", 5.0, 2500.0, 120.0, Milliseconds),
        param(id::MAKEUP, "Makeup", -12.0, 30.0, 0.0, Decibels),
        param(id::KNEE, "Knee", 0.0, 24.0, 6.0, Decibels),
        stepped(id::STYLE, "Style", 4.0, 0.0),
        stepped(id::AUTO_RELEASE, "Auto Release", 1.0, 0.0),
        stepped(id::AUTO_MAKEUP, "Auto Makeup", 1.0, 0.0),
        param(id::MIX, "Mix", 0.0, 1.0, 1.0, Percent),
        fixed(param(
            id::LOOKAHEAD,
            "Lookahead",
            0.0,
            10.0,
            0.0,
            Milliseconds,
        )),
        stepped(id::DETECTOR, "Detector", 1.0, 0.0),
        param(id::LINK, "Stereo Link", 0.0, 1.0, 1.0, Percent),
        // On: a sidechain routed in keys the compressor.
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
        param(id::RANGE, "Range", 0.0, 60.0, 60.0, Decibels),
        param(id::COLOR, "Color", 0.0, 1.0, 0.0, Percent),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::RATIO if v >= INFINITE - 0.5 => "∞ : 1".into(),
        id::RATIO => format!("{v:.1} : 1"),
        id::STYLE => pick(&STYLES, v),
        id::DETECTOR => pick(&DETECTORS, v),
        id::AUTO_RELEASE | id::AUTO_MAKEUP | id::EXTERNAL | id::LISTEN => on_off(v),
        id::SC_LOW if v <= 10.5 => "Off".into(),
        id::SC_HIGH if v >= 29_500.0 => "Off".into(),
        id::RANGE if v >= 59.5 => "Full".into(),
        id::KNEE if v < 0.05 => "Hard".into(),
        _ => return None,
    })
}

/// The lookahead's latency at `rate` (samples).
pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    let ms = f64::from(params.get(index(id::LOOKAHEAD)));
    (ms * 0.001 * rate).round() as u32
}

/// Index of a parameter by id (ids are the indexes).
pub const fn index(pid: u32) -> usize {
    pid as usize
}

/// How much of the excess over the threshold a ratio takes away.
pub fn slope(ratio: f64) -> f64 {
    if ratio >= INFINITE - 0.5 {
        1.0
    } else {
        1.0 - 1.0 / ratio.max(1.0)
    }
}

/// The reduction (dB, ≥ 0) the gain computer asks for at `level` (dB).
pub fn reduction(level: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    reduction_by(level, threshold, slope(ratio), knee)
}

/// [`reduction`] for a slope (a feedback compressor's may exceed 1).
fn reduction_by(level: f64, threshold: f64, slope: f64, knee: f64) -> f64 {
    let over = level - threshold;
    if knee > 0.0 && over.abs() <= knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// How a style works.
#[derive(Clone, Copy)]
struct Style {
    feedback: bool,
    /// RMS time (ms) of the detector, 0 for peaks.
    rms: f64,
    /// Attack and release time scales.
    attack: f64,
    release: f64,
    /// Knee widened by this much (dB).
    knee: f64,
    /// Saturation that comes with it and with reduction.
    color: f64,
    gr_color: f64,
    always_auto: bool,
}

fn style(i: usize) -> Style {
    match i {
        1 => Style {
            feedback: false,
            rms: 0.0,
            attack: 1.0,
            release: 0.6,
            knee: 0.0,
            color: 0.0,
            gr_color: 0.0,
            always_auto: false,
        },
        2 => Style {
            feedback: true,
            rms: 8.0,
            attack: 2.0,
            release: 1.0,
            knee: 6.0,
            color: 0.15,
            gr_color: 0.01,
            always_auto: true,
        },
        3 => Style {
            feedback: true,
            rms: 0.0,
            attack: 0.25,
            release: 0.8,
            knee: 0.0,
            color: 0.3,
            gr_color: 0.04,
            always_auto: false,
        },
        4 => Style {
            feedback: false,
            rms: 12.0,
            attack: 1.0,
            release: 1.0,
            knee: 2.0,
            color: 0.05,
            gr_color: 0.0,
            always_auto: false,
        },
        _ => Style {
            feedback: false,
            rms: 3.0,
            attack: 1.0,
            release: 1.0,
            knee: 0.0,
            color: 0.0,
            gr_color: 0.0,
            always_auto: false,
        },
    }
}

/// A gentle asymmetric saturation, unit slope at zero.
#[inline]
fn saturate(x: f64, drive: f64) -> f64 {
    if drive <= 1e-6 {
        return x;
    }
    let d = 1.0 + 4.0 * drive;
    let b = 0.15;
    let tb = (d * b).tanh();
    let y = ((d * (x + b)).tanh() - tb) / (d * (1.0 - tb * tb));
    x + drive.min(1.0) * (y - x)
}

/// Sub-blocks the controls are updated in.
const STEP: usize = 32;

pub struct CompressorProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    sc_low: Filter<2>,
    sc_high: Filter<2>,
    rms: [MeanSquare; 2],
    /// The peak detector ahead of the gain computer (instant attack, a
    /// short release) for the peak styles.
    peak: [f64; 2],
    peak_fall: f64,
    gr: [DualRelease; 2],
    /// The last output (feedback styles' detector).
    last: [f64; 2],
    delay: [DelayLine; 2],
    look: usize,
    makeup: Smoothed,
    mix: Smoothed,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl CompressorProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let look = latency(&params, sr) as usize;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            sc_low: Filter::default(),
            sc_high: Filter::default(),
            rms: [MeanSquare::new(3.0, sr); 2],
            peak: [0.0; 2],
            peak_fall: (-1.0 / (0.010 * sr)).exp(),
            gr: [DualRelease::default(); 2],
            last: [0.0; 2],
            delay: [DelayLine::new(look + 1), DelayLine::new(look + 1)],
            look,
            makeup: Smoothed::new(1.0, 20.0, sr),
            mix: Smoothed::new(1.0, 20.0, sr),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        // Start where the controls are, not gliding there.
        p.control();
        p.makeup.snap();
        p.mix.snap();
        p
    }

    fn get(&self, pid: u32) -> f64 {
        f64::from(self.params.get(index(pid)))
    }

    /// Read the controls (every sub-block): the style, threshold, slope,
    /// knee, whether RMS, auto release, listen.
    fn control(&mut self) -> (Style, f64, f64, f64, f64, bool, bool) {
        let st = style(self.get(id::STYLE).round().max(0.0) as usize);
        let sr = self.sr;
        let attack = self.get(id::ATTACK) * st.attack;
        let release = self.get(id::RELEASE) * st.release;
        for g in &mut self.gr {
            g.set(attack, release, sr);
        }
        let rms_ms = if self.get(id::DETECTOR) >= 0.5 {
            st.rms.max(10.0)
        } else {
            st.rms
        };
        if rms_ms > 0.0 {
            for r in &mut self.rms {
                r.set_time(rms_ms, sr);
            }
        }
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
        let threshold = self.get(id::THRESHOLD);
        let ratio = self.get(id::RATIO);
        let knee = self.get(id::KNEE) + st.knee;
        let mut makeup = self.get(id::MAKEUP);
        if self.get(id::AUTO_MAKEUP) >= 0.5 {
            // Half of what a full scale signal loses.
            makeup += 0.5 * reduction(0.0, threshold, ratio, knee);
        }
        // A feedback compressor hears its own output: a steeper curve makes
        // its effective ratio the one set (s / (1 − s) gives s in the
        // loop), up to what the loop takes stably.
        let s = slope(ratio);
        let slope = if st.feedback {
            (s / (1.0 - s).max(1e-3)).min(8.0)
        } else {
            s
        };
        self.makeup.set(gain(makeup));
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        let auto = st.always_auto || self.get(id::AUTO_RELEASE) >= 0.5;
        let listen = self.get(id::LISTEN) >= 0.5;
        (
            st,
            threshold,
            slope,
            knee,
            if rms_ms > 0.0 { 1.0 } else { 0.0 },
            auto,
            listen,
        )
    }
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

impl PluginProcessor for CompressorProcessor {
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
        let mut events = ctx.param_events.iter().peekable();
        let link = self.get(id::LINK).clamp(0.0, 1.0);
        let range = self.get(id::RANGE);
        let range = if range >= 59.5 { f64::INFINITY } else { range };
        let color = self.get(id::COLOR).clamp(0.0, 1.0);
        let external = self.get(id::EXTERNAL) >= 0.5 && side.is_some();
        let mut gr_peak = 0.0f64;
        let mut level_now = -150.0f64;
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
            let (st, threshold, slope, knee, rms, auto, listen) = self.control();
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                let key = match side {
                    Some(k) if external => [
                        f64::from(k.channel(0)[i]),
                        f64::from(k.channel(1.min(k.num_channels() - 1))[i]),
                    ],
                    _ => x,
                };
                // The detector: the key (or, feedback, the last output),
                // through the sidechain filters.
                let mut levels = [0.0f64; 2];
                let mut heard = [0.0f64; 2];
                for c in 0..2 {
                    let src = if st.feedback && !external {
                        self.last[c]
                    } else {
                        key[c]
                    };
                    let f = self.sc_high.process(c, self.sc_low.process(c, src));
                    heard[c] = f;
                    levels[c] = if rms > 0.0 {
                        10.0 * (2.0 * self.rms[c].process(f)).max(1e-24).log10()
                    } else {
                        let a = f.abs();
                        let pk = &mut self.peak[c];
                        *pk = if a > *pk { a } else { *pk * self.peak_fall };
                        db(*pk)
                    };
                }
                let loudest = levels[0].max(levels[1]);
                level_now = level_now.max(loudest);
                let makeup = self.makeup.tick();
                let mix = self.mix.tick();
                let mut y = [0.0; 2];
                for c in 0..2 {
                    let level = link * loudest + (1.0 - link) * levels[c];
                    let want = reduction_by(level, threshold, slope, knee).min(range);
                    let gr = self.gr[c].process(want, auto);
                    gr_peak = gr_peak.max(gr);
                    self.delay[c].push(x[c]);
                    let dry = if self.look > 0 {
                        self.delay[c].tap(self.look)
                    } else {
                        x[c]
                    };
                    let drive = color * (0.2 + st.color) + st.gr_color * gr;
                    let wet = saturate(dry * gain(-gr), drive) * makeup;
                    self.last[c] = dry * gain(-gr);
                    y[c] = if listen {
                        heard[c]
                    } else {
                        dry + (wet - dry) * mix
                    };
                }
                if channels >= 2 {
                    out.channel_mut(0)[i] = y[0] as f32;
                    out.channel_mut(1)[i] = y[1] as f32;
                } else {
                    out.channel_mut(0)[i] = (0.5 * (y[0] + y[1])) as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        for c in 0..2 {
            self.last[c] = flush(self.last[c]);
            self.peak[c] = flush(self.peak[c]);
            self.rms[c].flush();
        }
        self.sc_low.flush();
        self.sc_high.flush();
        // Meters and published values.
        let gr_now = self.gr[0].fast.max(self.gr[1].fast);
        self.tap.set_value(value::REDUCTION, gr_now as f32);
        self.tap.raise_value(value::REDUCTION_PEAK, gr_peak as f32);
        self.tap.set_value(value::LEVEL, level_now as f32);
        let mut peaks = [0.0f32; 2];
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.scratch[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
                peaks[0] = peaks[0].max(x.abs());
                peaks[1] = peaks[1].max(y.abs());
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        self.tap.raise_value(value::IN_PEAK, peaks[0]);
        self.tap.raise_value(value::OUT_PEAK, peaks[1]);
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
        for c in 0..2 {
            self.gr[c].reset();
            self.rms[c].reset();
            self.delay[c].reset();
            self.last[c] = 0.0;
            self.peak[c] = 0.0;
        }
        self.sc_low.reset();
        self.sc_high.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
