//! De-esser: sibilance turned down as it happens.
//!
//! The detector listens to the band where sibilance lives (above the
//! frequency, 24 dB/oct, or round it, a band pass of width Q). In absolute
//! mode the band's level is compared with the threshold; in relative mode
//! the threshold moves with the whole signal's level (its peaks smoothed
//! over 300 ms): it is where it reads for a take peaking at −18 dB, so a
//! quiet and a loud take de-ess alike. The reduction (5:1 through a 6 dB knee, at most the range) is
//! applied in split mode as a dynamic high shelf or bell at the frequency
//! (the EQ's matched designs: only the band moves, the voice keeps its
//! body), in wide mode to the whole signal. Listen plays what the detector
//! hears.

use super::{fixed, on_off, param, pass_through, pick, stepped};
use crate::devices::compressor::reduction;
use crate::dsp::env::DualRelease;
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
    pub const FREQUENCY: u32 = 2;
    pub const MODE: u32 = 3;
    pub const SHAPE: u32 = 4;
    pub const DETECTION: u32 = 5;
    pub const ATTACK: u32 = 6;
    pub const RELEASE: u32 = 7;
    pub const LINK: u32 = 8;
    pub const LISTEN: u32 = 9;
    pub const LOOKAHEAD: u32 = 10;
    pub const Q: u32 = 11;
}

/// Published: reduction now and its peak (dB), input and output peaks
/// (linear), the band's and the whole signal's levels (dB).
pub mod value {
    pub const REDUCTION: usize = 0;
    pub const REDUCTION_PEAK: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const BAND: usize = 4;
    pub const FULL: usize = 5;
}
pub const TAP_VALUES: usize = 6;

pub const MODES: [&str; 2] = ["Split", "Wide"];
pub const SHAPES: [&str; 2] = ["High", "Band"];
pub const DETECTIONS: [&str; 2] = ["Relative", "Absolute"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::THRESHOLD, "Threshold", -60.0, 0.0, -30.0, Decibels),
        param(id::RANGE, "Range", 0.0, 24.0, 10.0, Decibels),
        param(
            id::FREQUENCY,
            "Frequency",
            1_500.0,
            16_000.0,
            6_500.0,
            Hertz,
        ),
        stepped(id::MODE, "Mode", 1.0, 0.0),
        stepped(id::SHAPE, "Shape", 1.0, 0.0),
        stepped(id::DETECTION, "Detection", 1.0, 0.0),
        param(id::ATTACK, "Attack", 0.1, 20.0, 1.0, Milliseconds),
        param(id::RELEASE, "Release", 10.0, 500.0, 70.0, Milliseconds),
        param(id::LINK, "Stereo Link", 0.0, 1.0, 1.0, Percent),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
        fixed(param(
            id::LOOKAHEAD,
            "Lookahead",
            0.0,
            5.0,
            0.0,
            Milliseconds,
        )),
        param(id::Q, "Width", 0.5, 4.0, 1.4, None),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::MODE => pick(&MODES, v),
        id::SHAPE => pick(&SHAPES, v),
        id::DETECTION => pick(&DETECTIONS, v),
        id::LISTEN => on_off(v),
        id::Q => format!("Q {v:.2}"),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

pub fn latency(params: &ParamValues, rate: f64) -> u32 {
    (get(params, id::LOOKAHEAD) * 0.001 * rate).round() as u32
}

/// The detector's band for a shape.
pub fn detector_shape(high: bool, freq: f64, q: f64) -> BandShape {
    if high {
        BandShape {
            kind: BandType::LowCut,
            freq,
            gain: 0.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            slope: 24.0,
        }
    } else {
        BandShape {
            kind: BandType::BandPass,
            freq,
            gain: 0.0,
            q,
            slope: 12.0,
        }
    }
}

/// The split mode's dynamic band at a reduction.
pub fn cut_shape(high: bool, freq: f64, q: f64, gr: f64) -> BandShape {
    BandShape {
        kind: if high {
            BandType::HighShelf
        } else {
            BandType::Bell
        },
        freq: if high { freq * 0.85 } else { freq },
        gain: -gr,
        q: if high {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            q
        },
        slope: 12.0,
    }
}

const STEP: usize = 16;
/// Relative detection reads the threshold for a take peaking here (dB).
const REFERENCE: f64 = 18.0;

pub struct DeesserProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    detector: Filter<2>,
    /// The split mode's dynamic band, per channel (unlinked channels move
    /// apart).
    cut: [Filter<1>; 2],
    env: [f64; 2],
    env_fall: f64,
    full: [f64; 2],
    full_slow: f64,
    gr: [DualRelease; 2],
    delay: [Vec<f64>; 2],
    pos: usize,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DeesserProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let look = latency(&params, sr) as usize;
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        Self {
            watching: Watching::new(sr as f32),
            sr,
            detector: Filter::default(),
            cut: [Filter::default(), Filter::default()],
            env: [0.0; 2],
            env_fall: (-1.0 / (0.004 * sr)).exp(),
            full: [0.0; 2],
            full_slow: -60.0,
            gr: [DualRelease::default(); 2],
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

impl PluginProcessor for DeesserProcessor {
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
        let look = if self.get(id::LOOKAHEAD) > 0.0 {
            self.delay[0].len()
        } else {
            0
        };
        let slow_k = 1.0 - (-(STEP as f64) / (0.3 * sr)).exp();
        let mut events = ctx.param_events.iter().peekable();
        let (mut gr_peak, mut in_peak, mut out_peak) = (0.0f64, 0.0f64, 0.0f64);
        let (mut band_now, mut full_now) = (-150.0f64, -150.0f64);
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
            let freq = self.get(id::FREQUENCY).min(0.45 * sr);
            let q = self.get(id::Q);
            let high = self.get(id::SHAPE) < 0.5;
            let split = self.get(id::MODE) < 0.5;
            let relative = self.get(id::DETECTION) < 0.5;
            let threshold = self.get(id::THRESHOLD);
            let range = self.get(id::RANGE);
            let link = self.get(id::LINK).clamp(0.0, 1.0);
            let listen = self.get(id::LISTEN) >= 0.5;
            self.detector.set(detector_shape(high, freq, q), sr);
            let (attack, release) = (self.get(id::ATTACK), self.get(id::RELEASE));
            for g in &mut self.gr {
                g.set(attack, release, sr);
            }
            // The levels: the band and the whole signal (for relative).
            let full_db = db(self.full[0].max(self.full[1]));
            if full_db > -100.0 {
                self.full_slow += (full_db - self.full_slow) * slow_k;
            }
            let thr = if relative {
                threshold + self.full_slow + REFERENCE
            } else {
                threshold
            };
            // The dynamic band follows the reduction at the start of the
            // step (its coefficients every 16 samples).
            if split {
                for c in 0..2 {
                    let gr = self.gr[c].fast;
                    if gr > 0.01 {
                        self.cut[c].set(cut_shape(high, freq, q, gr), sr);
                    } else {
                        self.cut[c].clear();
                    }
                }
            }
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
                let mut band = [0.0; 2];
                let mut lv = [0.0; 2];
                for c in 0..2 {
                    band[c] = self.detector.process(c, x[c]);
                    let a = band[c].abs();
                    self.env[c] = if a > self.env[c] {
                        a
                    } else {
                        self.env[c] * self.env_fall
                    };
                    let f = x[c].abs();
                    self.full[c] = if f > self.full[c] {
                        f
                    } else {
                        self.full[c] * self.env_fall
                    };
                    lv[c] = db(self.env[c]);
                }
                let loudest = lv[0].max(lv[1]);
                band_now = band_now.max(loudest);
                full_now = full_now.max(db(self.full[0].max(self.full[1])));
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
                let mut y = [0.0; 2];
                for c in 0..2 {
                    let level = link * loudest + (1.0 - link) * lv[c];
                    let want = reduction(level, thr, 5.0, 6.0).min(range);
                    let gr = self.gr[c].process(want, false);
                    gr_peak = gr_peak.max(gr);
                    y[c] = if listen {
                        band[c]
                    } else if split {
                        self.cut[c].process(0, dry[c])
                    } else {
                        dry[c] * gain(-gr)
                    };
                }
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
        for c in 0..2 {
            self.env[c] = flush(self.env[c]);
            self.full[c] = flush(self.full[c]);
            self.cut[c].flush();
        }
        self.detector.flush();
        let gr_now = self.gr[0].fast.max(self.gr[1].fast);
        self.tap.set_value(value::REDUCTION, gr_now as f32);
        self.tap.raise_value(value::REDUCTION_PEAK, gr_peak as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.set_value(value::BAND, band_now as f32);
        self.tap.set_value(value::FULL, full_now as f32);
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
        self.env = [0.0; 2];
        self.full = [0.0; 2];
        for c in 0..2 {
            self.gr[c].reset();
            self.cut[c].reset();
            self.delay[c].fill(0.0);
        }
        self.detector.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, level, silence, tone};

    fn rig(set: &[(u32, f64)]) -> Rig<DeesserProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DeesserProcessor::new)
    }

    /// A voice-like low tone with a sibilant one on top.
    fn voice(sibilance: f64) -> impl Fn(usize) -> (f32, f32) {
        move |n| {
            let t = n as f64 / SR;
            let v = 0.3 * (std::f64::consts::TAU * 300.0 * t).sin()
                + sibilance * (std::f64::consts::TAU * 7_500.0 * t).sin();
            (v as f32, v as f32)
        }
    }

    #[test]
    fn split_mode_takes_the_sibilance_and_leaves_the_voice() {
        let mut r = rig(&[
            (id::DETECTION, 1.0),
            (id::THRESHOLD, -30.0),
            (id::RANGE, 10.0),
        ]);
        let (l, _) = r.run(1.0, voice(0.3), silence);
        let ess = bin_db(&l, 7_500.0);
        let body = bin_db(&l, 300.0);
        assert!(ess < 20.0 * 0.3f64.log10() - 7.0, "sibilance: {ess:.2}");
        assert!(
            (body - 20.0 * 0.3f64.log10()).abs() < 0.3,
            "body: {body:.2}"
        );
        assert!(r.tap.value(value::REDUCTION) > 7.0);
    }

    #[test]
    fn wide_mode_turns_everything_down_and_quiet_esses_pass() {
        let mut r = rig(&[
            (id::DETECTION, 1.0),
            (id::MODE, 1.0),
            (id::THRESHOLD, -30.0),
            (id::RANGE, 10.0),
        ]);
        let (l, _) = r.run(1.0, voice(0.3), silence);
        let body = bin_db(&l, 300.0);
        assert!(body < 20.0 * 0.3f64.log10() - 7.0, "body: {body:.2}");
        let mut r = rig(&[(id::DETECTION, 1.0), (id::THRESHOLD, -30.0)]);
        let (l, _) = r.run(1.0, voice(0.005), silence);
        assert!((bin_db(&l, 7_500.0) - 20.0 * 0.005f64.log10()).abs() < 0.3);
    }

    #[test]
    fn relative_detection_follows_the_level_of_the_take() {
        // The same voice 20 dB quieter de-esses as much.
        let gr_of = |scale: f64| {
            let mut r = rig(&[
                (id::DETECTION, 0.0),
                (id::THRESHOLD, -30.0),
                (id::RANGE, 24.0),
            ]);
            let s = voice(0.3);
            r.run(
                1.5,
                move |n| {
                    let (a, b) = s(n);
                    ((f64::from(a) * scale) as f32, (f64::from(b) * scale) as f32)
                },
                silence,
            );
            f64::from(r.tap.value(value::REDUCTION))
        };
        let (loud, quiet) = (gr_of(1.0), gr_of(0.1));
        assert!(
            loud > 2.0 && (loud - quiet).abs() < 1.5,
            "{loud:.2} vs {quiet:.2}"
        );
    }

    #[test]
    fn listening_plays_the_band() {
        let mut r = rig(&[(id::LISTEN, 1.0)]);
        let (l, _) = r.run(0.5, voice(0.3), silence);
        assert!(bin_db(&l, 300.0) < -40.0, "{}", bin_db(&l, 300.0));
        // 7.5 kHz is close over the 6.5 kHz edge (24 dB/oct).
        let ess = bin_db(&l, 7_500.0) - 20.0 * 0.3f64.log10();
        assert!(ess < 0.1 && ess > -4.0, "{ess}");
        let mut r = rig(&[
            (id::LISTEN, 1.0),
            (id::SHAPE, 1.0),
            (id::FREQUENCY, 7_500.0),
        ]);
        let (l, _) = r.run(0.5, voice(0.3), silence);
        assert!((bin_db(&l, 7_500.0) - 20.0 * 0.3f64.log10()).abs() < 0.3);
        let _ = (level(&l, 300.0), tone(1.0, 0.0));
    }
}
