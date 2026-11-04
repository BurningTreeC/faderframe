//! De-reverb: a room's late tail taken down, from the decay predicted per
//! frequency ([`faderframe_restore::dereverb`]). Tail Length is the room's
//! RT60, or (Auto Tail) what the device estimates from the free decays it
//! hears; Early is where the late tail begins (what comes sooner — the
//! sound and its first reflections — is left); Damping shortens the highs'
//! tail. Frames of about 21 ms: a fixed latency of about 21 ms at 48 kHz.

use super::{on_off, param, pass_through, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_restore::dereverb::{DecayEstimator, Dereverb, ReverbSettings};
use faderframe_restore::stft::Streaming;
use std::sync::Arc;

pub mod id {
    pub const AMOUNT: u32 = 0;
    pub const TAIL: u32 = 1;
    pub const AUTO_TAIL: u32 = 2;
    pub const EARLY: u32 = 3;
    pub const DAMPING: u32 = 4;
    pub const SMOOTHING: u32 = 5;
    pub const LISTEN: u32 = 6;
    pub const LINK: u32 = 7;
}

/// Published: the tail length in use (ms; the estimate in Auto), the
/// reduction now (dB, positive) and whether the estimate has settled.
pub mod value {
    pub const TAIL: usize = 0;
    pub const REDUCTION: usize = 1;
    pub const ESTIMATED: usize = 2;
}
pub const TAP_VALUES: usize = 3;

pub const LISTENS: [&str; 2] = ["Output", "Reverb"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        param(id::AMOUNT, "Amount", 0.0, 1.0, 0.5, Percent),
        param(
            id::TAIL,
            "Tail Length",
            100.0,
            8_000.0,
            1_000.0,
            Milliseconds,
        ),
        stepped(id::AUTO_TAIL, "Auto Tail", 1.0, 1.0),
        param(id::EARLY, "Early", 10.0, 150.0, 50.0, Milliseconds),
        param(id::DAMPING, "Damping", 0.0, 1.0, 0.3, Percent),
        param(id::SMOOTHING, "Smoothing", 0.0, 1.0, 0.4, Percent),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
        stepped(id::LINK, "Stereo Link", 1.0, 1.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::LISTEN => pick(&LISTENS, v),
        id::AUTO_TAIL | id::LINK => on_off(v),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn frame(rate: f64) -> usize {
    faderframe_restore::frame_for(1024.0 / 48_000.0, rate)
}

pub fn latency(_params: &ParamValues, rate: f64) -> u32 {
    frame(rate) as u32
}

const STEP: usize = 16;

pub struct DereverbProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    stream: Streaming,
    dr: [Dereverb; 2],
    estimator: DecayEstimator,
    tail_ms: f64,
    estimated: bool,
    power: [Vec<f64>; 2],
    gains: [Vec<f64>; 2],
    reduction: f64,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DereverbProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let stream = Streaming::new(frame(sr), 2);
        let (bins, hop) = (stream.stft.bins, stream.stft.hop);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        Self {
            watching: Watching::new(sr as f32),
            dr: [Dereverb::new(bins, hop, sr), Dereverb::new(bins, hop, sr)],
            estimator: DecayEstimator::new(bins, hop, sr),
            tail_ms: get(&params, id::TAIL),
            estimated: false,
            power: [vec![0.0; bins], vec![0.0; bins]],
            gains: [vec![1.0; bins], vec![1.0; bins]],
            reduction: 0.0,
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            stream,
            params,
            tap,
        }
    }

    fn frame(&mut self, channels: usize) {
        let link = get(&self.params, id::LINK) >= 0.5 || channels == 1;
        let listen = get(&self.params, id::LISTEN) >= 0.5;
        let scale = self.stream.stft.power_scale();
        for c in 0..channels {
            for (p, x) in self.power[c].iter_mut().zip(&self.stream.spectra[c]) {
                *p = x.norm_sqr() / scale;
            }
        }
        if channels == 2 && link {
            let (a, b) = self.power.split_at_mut(1);
            for (x, y) in a[0].iter_mut().zip(b[0].iter()) {
                *x = 0.5 * (*x + *y);
            }
        }
        self.estimator.update(&self.power[0]);
        let estimate = self.estimator.estimate();
        self.estimated = estimate.is_some();
        self.tail_ms = if get(&self.params, id::AUTO_TAIL) >= 0.5 {
            estimate.map_or(get(&self.params, id::TAIL), |s| s * 1e3)
        } else {
            get(&self.params, id::TAIL)
        };
        let s = ReverbSettings {
            amount: get(&self.params, id::AMOUNT),
            tail_s: self.tail_ms * 1e-3,
            early_ms: get(&self.params, id::EARLY),
            damping: get(&self.params, id::DAMPING),
            smoothing: get(&self.params, id::SMOOTHING),
        };
        let lanes = if link { 1 } else { channels };
        for c in 0..lanes {
            self.dr[c].gains(&self.power[c], &s, &mut self.gains[c]);
        }
        if link && channels == 2 {
            let (a, b) = self.gains.split_at_mut(1);
            b[0].copy_from_slice(&a[0]);
        }
        let (mut before, mut after) = (0.0, 0.0);
        for (p, g) in self.power[0].iter().zip(&self.gains[0]) {
            before += p;
            after += p * g * g;
        }
        self.reduction = if before > 1e-20 {
            (10.0 * (before / after.max(1e-30)).log10()).max(0.0)
        } else {
            0.0
        };
        for c in 0..channels {
            for (x, &g) in self.stream.spectra[c].iter_mut().zip(&self.gains[c]) {
                *x *= if listen { 1.0 - g } else { g };
            }
        }
        self.stream.finish();
    }
}

impl PluginProcessor for DereverbProcessor {
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
        let lanes = channels.min(2);
        let n = frames.min(self.scratch[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.scratch[c][..n].copy_from_slice(&src[..n]);
        }
        let mut events = ctx.param_events.iter().peekable();
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
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                let mut y = [0.0; 2];
                if self.stream.push(&x[..lanes], &mut y[..lanes]) {
                    self.frame(lanes);
                }
                for (c, v) in y.iter().enumerate().take(lanes) {
                    out.channel_mut(c)[i] = *v as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.tap.set_value(value::TAIL, self.tail_ms as f32);
        self.tap.set_value(value::REDUCTION, self.reduction as f32);
        self.tap
            .set_value(value::ESTIMATED, if self.estimated { 1.0 } else { 0.0 });
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
        self.stream.reset();
        for d in &mut self.dr {
            d.reset();
        }
        self.estimator.reset();
        self.reduction = 0.0;
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, silence};
    use faderframe_restore::Noise;

    fn rig(set: &[(u32, f64)]) -> Rig<DereverbProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DereverbProcessor::new)
    }

    /// Short tone bursts twice a second in a room of RT60 0.7 s (a decaying
    /// noise tail, from 25 ms): the wet signal, and the dry one.
    fn room(seconds: f64) -> (Vec<f32>, Vec<f32>) {
        let len = (seconds * SR) as usize;
        let dry: Vec<f64> = (0..len)
            .map(|n| {
                let t = n as f64 / SR;
                let in_burst = t % 0.5 < 0.08;
                if in_burst {
                    0.3 * (std::f64::consts::TAU * 440.0 * t).sin()
                        + 0.2 * (std::f64::consts::TAU * 1_320.0 * t).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let rt = 0.7;
        let delta = 3.0 * std::f64::consts::LN_10 / rt;
        let ir_len = (rt * SR) as usize;
        let mut noise = Noise::new(31);
        // Sparse taps stand in for a dense tail (fast enough to convolve).
        let taps: Vec<(usize, f64)> = (0..2_000)
            .map(|i| {
                let k = (0.025 * SR) as usize + i * (ir_len / 2_000);
                let t = k as f64 / SR;
                (k, 0.08 * noise.next() * (-delta * t).exp())
            })
            .collect();
        let mut wet = dry.clone();
        for (n, &x) in dry.iter().enumerate() {
            if x == 0.0 {
                continue;
            }
            for &(k, h) in &taps {
                if n + k < len {
                    wet[n + k] += x * h;
                }
            }
        }
        (
            wet.iter().map(|v| *v as f32).collect(),
            dry.iter().map(|v| *v as f32).collect(),
        )
    }

    #[test]
    fn the_tail_between_bursts_is_taken_down() {
        let (wet, _) = room(8.0);
        // The room decays alike at every frequency: no damping.
        let mut r = rig(&[
            (id::AMOUNT, 1.0),
            (id::AUTO_TAIL, 0.0),
            (id::TAIL, 700.0),
            (id::DAMPING, 0.0),
        ]);
        let lat = latency(&r.params, SR) as usize;
        let w = wet.clone();
        let (l, _) = r.run(8.0, move |n| (w[n], w[n]), silence);
        // Energy in the gaps (0.2–0.45 s into each half second) and in the
        // bursts, before and after.
        let energy = |x: &[f32], a: f64, b: f64, shift: usize| -> f64 {
            (4..15)
                .map(|half| {
                    let s = ((half as f64 * 0.5 + a) * SR) as usize + shift;
                    let e = ((half as f64 * 0.5 + b) * SR) as usize + shift;
                    x[s..e].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>()
                })
                .sum()
        };
        let gap = 10.0 * (energy(&l, 0.2, 0.45, lat) / energy(&wet, 0.2, 0.45, 0)).log10();
        let burst = 10.0 * (energy(&l, 0.01, 0.07, lat) / energy(&wet, 0.01, 0.07, 0)).log10();
        // A sparse, fading tail (its level swings frame to frame): −10 dB
        // and better.
        assert!(gap < -8.0, "the tail {gap:.1} dB");
        assert!(burst > -2.0, "the bursts {burst:.1} dB");
        assert!(r.tap.value(value::REDUCTION) >= 0.0);
    }

    #[test]
    fn auto_tail_estimates_the_room() {
        let (wet, _) = room(12.0);
        let mut r = rig(&[(id::AMOUNT, 0.5)]);
        let w = wet.clone();
        r.run(12.0, move |n| (w[n], w[n]), silence);
        assert_eq!(r.tap.value(value::ESTIMATED), 1.0);
        let tail = f64::from(r.tap.value(value::TAIL));
        assert!((tail / 700.0 - 1.0).abs() < 0.35, "{tail:.0} ms");
    }

    #[test]
    fn no_amount_leaves_the_sound_alone() {
        let (wet, _) = room(2.0);
        let mut r = rig(&[(id::AMOUNT, 0.0)]);
        let lat = latency(&r.params, SR) as usize;
        let w = wet.clone();
        let (l, _) = r.run(2.0, move |n| (w[n], w[n]), silence);
        for i in lat + 4096..l.len() {
            assert!((l[i] - wet[i - lat]).abs() < 1e-5, "at {i}");
        }
    }
}
