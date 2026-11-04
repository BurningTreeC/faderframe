//! De-clip: peaks that hit a converter's or a plug-in's ceiling rebuilt by
//! constrained least-squares AR interpolation ([`faderframe_restore::declip`]):
//! each clipped run is filled with what the sound round it predicts, kept
//! at or beyond the clip level on its side.
//!
//! The clip level is found (Auto: the loudest the input has been lately,
//! falling a decibel a second) or set; samples within Tolerance of it, two
//! or more in a row on one side, are clipped. Runs up to Max Length are
//! rebuilt (longer ones are left: too little round them to go by). Best
//! quality fits the model twice. Rebuilt peaks go over the old ceiling, so
//! Output trims the level; Listen plays only what the repair adds.
//! Windows of about 21 ms with context either side: a fixed latency of
//! about 32 ms at 48 kHz.

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::gain;
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_restore::blocks::Blocks;
use faderframe_restore::declip::{Declipper, mark};
use std::sync::Arc;

pub mod id {
    pub const AUTO: u32 = 0;
    pub const LEVEL: u32 = 1;
    pub const TOLERANCE: u32 = 2;
    pub const MAX_LENGTH: u32 = 3;
    pub const QUALITY: u32 = 4;
    pub const OUTPUT: u32 = 5;
    pub const LISTEN: u32 = 6;
}

/// Published: the clip level in use (dBFS), the share of samples found
/// clipped (smoothed, 0…1), runs rebuilt per second, input and output
/// peaks (linear, raised).
pub mod value {
    pub const LEVEL: usize = 0;
    pub const CLIPPED: usize = 1;
    pub const RATE: usize = 2;
    pub const IN_PEAK: usize = 3;
    pub const OUT_PEAK: usize = 4;
}
pub const TAP_VALUES: usize = 5;

pub const QUALITIES: [&str; 2] = ["Fast", "Best"];
pub const LISTENS: [&str; 2] = ["Output", "Repairs"];

/// The longest run (ms) Max Length reaches.
pub const MAX_LENGTH_MS: f64 = 5.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::AUTO, "Auto Level", 1.0, 1.0),
        param(id::LEVEL, "Clip Level", -24.0, 0.0, -0.1, Decibels),
        param(id::TOLERANCE, "Tolerance", 0.0, 3.0, 0.1, Decibels),
        param(
            id::MAX_LENGTH,
            "Max Length",
            0.2,
            MAX_LENGTH_MS,
            2.0,
            Milliseconds,
        ),
        stepped(id::QUALITY, "Quality", 1.0, 1.0),
        param(id::OUTPUT, "Output", -12.0, 6.0, 0.0, Decibels),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::AUTO => on_off(v),
        id::QUALITY => pick(&QUALITIES, v),
        id::LISTEN => pick(&LISTENS, v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

struct Sizes {
    order: usize,
    hop: usize,
    max_len: usize,
    margin: usize,
}

fn sizes(rate: f64) -> Sizes {
    let order = ((rate / 1_500.0).round() as usize).clamp(16, 96);
    let hop = faderframe_restore::frame_for(1024.0 / 48_000.0, rate);
    let max_len = (MAX_LENGTH_MS * 1e-3 * rate).ceil() as usize;
    let margin = (max_len + order + 2).next_power_of_two();
    Sizes {
        order,
        hop,
        max_len,
        margin,
    }
}

pub fn latency(_params: &ParamValues, rate: f64) -> u32 {
    let s = sizes(rate);
    (s.hop + s.margin) as u32
}

pub struct DeclipProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    sizes: Sizes,
    blocks: Blocks,
    dec: [Declipper; 2],
    marks: Vec<u8>,
    /// The automatic clip level (linear).
    peak: f64,
    level_db: f64,
    clipped: f64,
    rate: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DeclipProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let s = sizes(sr);
        let blocks = Blocks::new(s.hop, s.margin, 2);
        let len = blocks.len();
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        Self {
            watching: Watching::new(sr as f32),
            sr,
            dec: [
                Declipper::new(len, s.order, s.max_len),
                Declipper::new(len, s.order, s.max_len),
            ],
            marks: vec![0; len],
            blocks,
            sizes: s,
            peak: 0.0,
            level_db: 0.0,
            clipped: 0.0,
            rate: 0.0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }

    fn window(&mut self, lanes: usize) {
        let (m, h) = (self.sizes.margin, self.sizes.hop);
        let len = self.blocks.len();
        // The level: the loudest lately (Auto), or as set.
        let window_s = h as f64 / self.sr;
        let mut loudest = 0.0f64;
        for c in 0..lanes {
            let (_, raw) = self.blocks.window(c);
            for v in &raw[len - h..] {
                loudest = loudest.max(v.abs());
            }
        }
        self.peak = loudest.max(self.peak * crate::dsp::gain(-window_s));
        let level = if get(&self.params, id::AUTO) >= 0.5 {
            self.peak
        } else {
            gain(get(&self.params, id::LEVEL))
        };
        self.level_db = crate::dsp::db(level);
        let threshold = level * gain(-get(&self.params, id::TOLERANCE));
        let max_len = ((get(&self.params, id::MAX_LENGTH) * 1e-3 * self.sr).round() as usize)
            .clamp(2, self.sizes.max_len);
        let best = get(&self.params, id::QUALITY) >= 0.5;
        let mut marked = 0;
        let mut runs = 0;
        if level > 1e-6 {
            for c in 0..lanes {
                self.marks.fill(0);
                let (work, raw) = self.blocks.window(c);
                marked += mark(raw, threshold, m, m + h, max_len, &mut self.marks);
                if self.marks[m..].iter().all(|&v| v == 0) {
                    continue;
                }
                self.dec[c].fit(work);
                let n = self.dec[c].repair(work, raw, &self.marks, m, m + h);
                if best {
                    // Again with a model of the rebuilt window, from the
                    // clipped samples as they came.
                    self.dec[c].fit(work);
                    for (i, &mk) in self.marks.iter().enumerate().skip(m) {
                        if mk != 0 {
                            work[i] = raw[i];
                        }
                    }
                    let n2 = self.dec[c].repair(work, raw, &self.marks, m, m + h);
                    runs += n2.max(n);
                } else {
                    runs += n;
                }
            }
        }
        let k = 1.0 - (-window_s / 1.0).exp();
        self.clipped += k * (marked as f64 / (h * lanes) as f64 - self.clipped);
        self.rate += k * (runs as f64 / window_s - self.rate);
        self.blocks.finish();
    }
}

impl PluginProcessor for DeclipProcessor {
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
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
        let lanes = channels.min(2);
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let listen = get(&self.params, id::LISTEN) >= 0.5;
        let trim = gain(get(&self.params, id::OUTPUT));
        let (mut in_peak, mut out_peak) = (0.0f64, 0.0f64);
        for i in 0..n {
            let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
            in_peak = in_peak.max(x[0].abs()).max(x[1].abs());
            let (mut y, mut dry) = ([0.0; 2], [0.0; 2]);
            if self
                .blocks
                .push(&x[..lanes], &mut y[..lanes], &mut dry[..lanes])
            {
                self.window(lanes);
            }
            for c in 0..lanes {
                let v = if listen { y[c] - dry[c] } else { y[c] * trim };
                out_peak = out_peak.max(v.abs());
                out.channel_mut(c)[i] = v as f32;
            }
        }
        self.tap.set_value(value::LEVEL, self.level_db as f32);
        self.tap.set_value(value::CLIPPED, self.clipped as f32);
        self.tap.set_value(value::RATE, self.rate as f32);
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
        self.blocks.reset();
        self.peak = 0.0;
        self.clipped = 0.0;
        self.rate = 0.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, silence};

    fn rig(set: &[(u32, f64)]) -> Rig<DeclipProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DeclipProcessor::new)
    }

    fn clean(n: usize) -> f64 {
        let t = n as f64 / SR;
        let env = 0.7 + 0.3 * (std::f64::consts::TAU * 0.5 * t).sin();
        env * (0.8 * (std::f64::consts::TAU * 180.0 * t).sin()
            + 0.25 * (std::f64::consts::TAU * 610.0 * t + 1.0).sin()
            + 0.1 * (std::f64::consts::TAU * 1_270.0 * t).sin())
    }

    const CEILING: f64 = 0.75;

    fn clipped(n: usize) -> (f32, f32) {
        let v = clean(n).clamp(-CEILING, CEILING) as f32;
        (v, v)
    }

    fn error_db(out: &[f32], latency: usize, trim: f64, damaged: bool) -> f64 {
        let (mut sig, mut err) = (0.0, 0.0);
        for (i, v) in out.iter().enumerate().skip(out.len() / 4) {
            let c = clean(i - latency);
            sig += c * c;
            let got = if damaged {
                f64::from(clipped(i - latency).0)
            } else {
                f64::from(*v) / trim
            };
            err += (got - c).powi(2);
        }
        10.0 * (err / sig).log10()
    }

    #[test]
    fn clipped_peaks_are_rebuilt() {
        for quality in [0.0, 1.0] {
            let mut r = rig(&[(id::QUALITY, quality), (id::OUTPUT, -3.0)]);
            let lat = latency(&r.params, SR) as usize;
            let (l, _) = r.run(3.0, clipped, silence);
            let before = error_db(&l, lat, gain(-3.0), true);
            let after = error_db(&l, lat, gain(-3.0), false);
            assert!(
                after < before - 8.0,
                "quality {quality}: {before:.1} → {after:.1} dB"
            );
            assert!((r.tap.value(value::LEVEL) - (20.0 * CEILING.log10()) as f32).abs() < 0.05);
            assert!(r.tap.value(value::CLIPPED) > 0.02);
        }
    }

    #[test]
    fn unclipped_audio_under_a_set_level_is_untouched() {
        let mut r = rig(&[(id::AUTO, 0.0), (id::LEVEL, 0.0)]);
        let lat = latency(&r.params, SR) as usize;
        let (l, _) = r.run(
            2.0,
            |n| (clean(n) as f32 * 0.8, clean(n) as f32 * 0.8),
            silence,
        );
        for (i, v) in l.iter().enumerate().skip(lat) {
            assert_eq!(*v, (clean(i - lat) as f32) * 0.8, "at {i}");
        }
    }
}
