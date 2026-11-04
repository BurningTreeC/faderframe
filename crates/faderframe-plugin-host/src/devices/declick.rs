//! De-click: clicks and crackle (vinyl, edits, digital errors, a
//! microphone's pops of static) found where an AR model of the sound
//! cannot predict it and filled by least-squares AR interpolation
//! ([`faderframe_restore::declick`]).
//!
//! Windows of about 21 ms are processed with about 11 ms of context either
//! side ([`faderframe_restore::blocks`]), so the latency is fixed (about
//! 32 ms at 48 kHz, the model's order and the context scaled with the
//! rate). Each window gets two passes: fit, find, fill; then the model
//! fitted again to the filled window, more found, and everything filled
//! again with the better model. Linked, a click found on one channel is
//! repaired on both (most are on both). Listen plays only what is taken.

use super::{on_off, param, pass_through, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_restore::blocks::Blocks;
use faderframe_restore::declick::{ClickSettings, Declicker, shape_runs};
use std::sync::Arc;

pub mod id {
    pub const SENSITIVITY: u32 = 0;
    pub const WIDTH: u32 = 1;
    pub const CRACKLE: u32 = 2;
    pub const LINK: u32 = 3;
    pub const LISTEN: u32 = 4;
}

/// Published: clicks repaired so far (a running count, for the editor's
/// marks), clicks per second, the input's and output's peaks and the peak
/// of what was taken (linear, raised).
pub mod value {
    pub const TOTAL: usize = 0;
    pub const RATE: usize = 1;
    pub const IN_PEAK: usize = 2;
    pub const OUT_PEAK: usize = 3;
    pub const REMOVED: usize = 4;
}
pub const TAP_VALUES: usize = 5;

pub const LISTENS: [&str; 2] = ["Output", "Clicks"];

/// The widest click (ms) the Width control reaches.
pub const MAX_WIDTH_MS: f64 = 4.0;

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::SENSITIVITY, "Sensitivity", 0.0, 1.0, 0.5, Percent),
        param(
            id::WIDTH,
            "Click Width",
            0.1,
            MAX_WIDTH_MS,
            1.0,
            Milliseconds,
        ),
        param(id::CRACKLE, "Crackle", 0.0, 1.0, 0.0, Percent),
        stepped(id::LINK, "Stereo Link", 1.0, 1.0),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::LISTEN => pick(&LISTENS, v),
        id::LINK => on_off(v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

/// The sizes at a rate: the model's order, the hop, the widening of a run
/// (each side), the widest gap filled and the context either side.
struct Sizes {
    order: usize,
    hop: usize,
    pad: usize,
    max_gap: usize,
    margin: usize,
    crackle: usize,
}

fn sizes(rate: f64) -> Sizes {
    let order = ((rate / 1_500.0).round() as usize).clamp(16, 96);
    let hop = faderframe_restore::frame_for(1024.0 / 48_000.0, rate);
    let pad = ((0.1e-3 * rate).round() as usize).max(1);
    // A click is found a little wider than it is (the model's response
    // round it): the widest run filled allows for that.
    let max_gap = (MAX_WIDTH_MS * 1e-3 * rate).ceil() as usize + 2 * pad + order;
    let margin = (max_gap + order + 2).next_power_of_two();
    Sizes {
        order,
        hop,
        pad,
        max_gap,
        margin,
        crackle: ((0.3e-3 * rate).round() as usize).max(2),
    }
}

pub fn latency(_params: &ParamValues, rate: f64) -> u32 {
    let s = sizes(rate);
    (s.hop + s.margin) as u32
}

pub struct DeclickProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    sizes: Sizes,
    blocks: Blocks,
    dec: [Declicker; 2],
    marks: [Vec<u8>; 2],
    fresh: [Vec<u8>; 2],
    total: f64,
    rate: f64,
    debug_windows: usize,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DeclickProcessor {
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
                Declicker::new(len, s.order, s.max_gap),
                Declicker::new(len, s.order, s.max_gap),
            ],
            marks: [vec![0; len], vec![0; len]],
            fresh: [vec![0; len], vec![0; len]],
            blocks,
            sizes: s,
            total: 0.0,
            rate: 0.0,
            debug_windows: 0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }

    /// A window is ready: find and fill its clicks (two passes).
    fn window(&mut self, lanes: usize) {
        let settings = ClickSettings {
            sensitivity: get(&self.params, id::SENSITIVITY),
            max_width: ((get(&self.params, id::WIDTH) * 1e-3 * self.sr).round() as usize).max(1),
            crackle: get(&self.params, id::CRACKLE),
            crackle_width: self.sizes.crackle,
        };
        let link = get(&self.params, id::LINK) >= 0.5 || lanes == 1;
        let (m, h, p) = (self.sizes.margin, self.sizes.hop, self.sizes.order);
        let len = self.blocks.len();
        let (from, to) = (m, len - p);
        let longest = settings.max_width + 2 * self.sizes.pad + p;
        // Detections closer than a click is wide are one click (a ramp's
        // start and end stand out, its middle is predictable).
        let join = (settings.max_width / 2).max(2);
        for marks in &mut self.marks {
            marks.fill(0);
        }
        let mut repaired = 0;
        for pass in 0..2 {
            for f in &mut self.fresh {
                f.fill(0);
            }
            for c in 0..lanes {
                let (work, _) = self.blocks.window(c);
                self.dec[c].analyse(work, m, m + h);
                let target = if link { 0 } else { c };
                self.dec[c].mark(&settings, &mut self.fresh[target], from, to);
            }
            let shared = if link { 1 } else { lanes };
            for c in 0..shared {
                shape_runs(
                    &mut self.fresh[c],
                    from,
                    to,
                    self.sizes.pad,
                    join,
                    self.sizes.crackle,
                );
                for (a, b) in self.marks[c].iter_mut().zip(&self.fresh[c]) {
                    *a = (*a).max(*b);
                }
            }
            repaired = 0;
            for c in 0..lanes {
                let marks = if link { 0 } else { c };
                let (work, _) = self.blocks.window(c);
                repaired = repaired.max(self.dec[c].repair(
                    work,
                    &mut self.marks[marks],
                    m,
                    m + h,
                    longest,
                    &settings,
                ));
            }
            if pass == 0
                && self
                    .marks
                    .iter()
                    .all(|mk| mk[from..to].iter().all(|&v| v == 0))
            {
                break;
            }
        }
        self.debug_windows += 1;
        self.total += repaired as f64;
        let window_s = h as f64 / self.sr;
        let k = 1.0 - (-window_s / 1.0).exp();
        self.rate += k * (repaired as f64 / window_s - self.rate);
        self.blocks.finish();
    }
}

impl PluginProcessor for DeclickProcessor {
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
            // Read once a window: events need no finer timing.
            self.params.apply_event(e.parameter, e.value);
        }
        let lanes = channels.min(2);
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let listen = get(&self.params, id::LISTEN) >= 0.5;
        let (mut in_peak, mut out_peak, mut removed) = (0.0f64, 0.0f64, 0.0f64);
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
                let taken = dry[c] - y[c];
                removed = removed.max(taken.abs());
                let v = if listen { taken } else { y[c] };
                out_peak = out_peak.max(v.abs());
                out.channel_mut(c)[i] = v as f32;
            }
        }
        self.tap.set_value(value::TOTAL, self.total as f32);
        self.tap.set_value(value::RATE, self.rate as f32);
        self.tap.raise_value(value::IN_PEAK, in_peak as f32);
        self.tap.raise_value(value::OUT_PEAK, out_peak as f32);
        self.tap.raise_value(value::REMOVED, removed as f32);
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
        self.rate = 0.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, silence};
    use faderframe_restore::Noise;

    fn rig(set: &[(u32, f64)]) -> Rig<DeclickProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DeclickProcessor::new)
    }

    /// Music: a chord of three tones with a little noise.
    fn music(n: usize) -> f64 {
        let t = n as f64 / SR;
        let mut noise = Noise::new((n as u32).wrapping_mul(2_654_435_761) | 1);
        0.25 * (std::f64::consts::TAU * 196.0 * t).sin()
            + 0.15 * (std::f64::consts::TAU * 293.7 * t).sin()
            + 0.1 * (std::f64::consts::TAU * 440.0 * t).sin()
            + 0.003 * noise.next()
    }

    /// Clicks: every 37 ms one of up to 0.6 ms, on both channels.
    fn click(n: usize) -> f64 {
        let period = (0.037 * SR) as usize;
        let at = n % period;
        let width = 4 + (n / period) % 25;
        if (1000..1000 + width).contains(&at) {
            let s = if (n / period).is_multiple_of(2) {
                1.0
            } else {
                -1.0
            };
            s * 0.5 * (1.0 - (at - 1000) as f64 / width as f64)
        } else {
            0.0
        }
    }

    fn error_db(out: &[f32], latency: usize, damaged: bool) -> f64 {
        let (mut sig, mut err) = (0.0, 0.0);
        for (i, v) in out.iter().enumerate().skip(out.len() / 4) {
            let c = music(i - latency);
            sig += c * c;
            let got = if damaged {
                c + click(i - latency)
            } else {
                f64::from(*v)
            };
            err += (got - c).powi(2);
        }
        10.0 * (err / sig).log10()
    }

    #[test]
    fn clicks_are_repaired_and_the_music_kept() {
        let mut r = rig(&[]);
        let lat = latency(&r.params, SR) as usize;
        let input = |n: usize| {
            let v = (music(n) + click(n)) as f32;
            (v, v)
        };
        let (l, _) = r.run(3.0, input, silence);
        let before = error_db(&l, lat, true);
        let after = error_db(&l, lat, false);
        assert!(after < before - 15.0, "error {before:.1} → {after:.1} dB");
        // No click is left standing (they were 0.5 high).
        let worst = l
            .iter()
            .enumerate()
            .skip(l.len() / 4)
            .map(|(i, v)| (f64::from(*v) - music(i - lat)).abs())
            .fold(0.0f64, f64::max);
        assert!(worst < 0.15, "the worst left: {worst:.3}");
        let rate = r.tap.value(value::RATE);
        assert!((rate - 27.0).abs() < 6.0, "{rate} clicks/s");
    }

    #[test]
    fn clean_music_passes_untouched() {
        let mut r = rig(&[(id::SENSITIVITY, 0.5)]);
        let lat = latency(&r.params, SR) as usize;
        let (l, _) = r.run(2.0, |n| (music(n) as f32, music(n) as f32), silence);
        let err = error_db(&l, lat, false);
        assert!(err < -80.0, "error {err:.1} dB");
        // At most the music's start out of silence.
        assert!(
            r.tap.value(value::TOTAL) <= 1.0,
            "{}",
            r.tap.value(value::TOTAL)
        );
    }

    #[test]
    fn listening_plays_the_clicks_alone() {
        let mut r = rig(&[(id::LISTEN, 1.0)]);
        let lat = latency(&r.params, SR) as usize;
        let input = |n: usize| {
            let v = (music(n) + click(n)) as f32;
            (v, v)
        };
        let (l, _) = r.run(2.0, input, silence);
        let (mut a, mut b) = (0.0, 0.0);
        for (i, v) in l.iter().enumerate().skip(l.len() / 4) {
            let c = click(i - lat);
            a += c * c;
            b += (f64::from(*v) - c).powi(2);
        }
        assert!(
            10.0 * (b / a).log10() < -15.0,
            "the clicks heard: {}",
            10.0 * (b / a).log10()
        );
    }
}
