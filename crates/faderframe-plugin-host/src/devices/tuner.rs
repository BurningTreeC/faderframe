//! Tuner: passes the audio (or mutes it while tuning) and lets its editor
//! hear the input. The pitch is found in the editor (McLeod's normalised
//! square difference on the tap's ring, `view-devices::tuner`), so the
//! audio thread does nothing but copy.

use super::{on_off, param, pass_through, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

pub mod id {
    pub const REFERENCE: u32 = 0;
    pub const MUTE: u32 = 1;
    pub const DISPLAY: u32 = 2;
}

/// Published: the input's peak (linear).
pub mod value {
    pub const IN_PEAK: usize = 0;
}
pub const TAP_VALUES: usize = 1;

pub const DISPLAYS: [&str; 2] = ["Needle", "Strobe"];

pub fn parameters() -> Vec<ParameterInfo> {
    vec![
        param(
            id::REFERENCE,
            "Reference A4",
            400.0,
            480.0,
            440.0,
            ParameterUnit::Hertz,
        ),
        stepped(id::MUTE, "Mute", 1.0, 0.0),
        stepped(id::DISPLAY, "Display", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::REFERENCE => format!("{v:.1} Hz"),
        id::MUTE => on_off(v),
        id::DISPLAY => pick(&DISPLAYS, v),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

pub struct TunerProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    /// The output's gain, ramped per block.
    gain: f32,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl TunerProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0) as f32;
        let block = config.max_block_size.max(1) as usize;
        let gain = if params.get(id::MUTE as usize) >= 0.5 {
            0.0
        } else {
            1.0
        };
        Self {
            watching: Watching::new(sr),
            gain,
            meters: [[MeterTap::new(sr); 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }
}

impl PluginProcessor for TunerProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for e in ctx.param_events {
            self.params.apply_event(e.parameter, e.value);
        }
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
        let target = if self.params.get(id::MUTE as usize) >= 0.5 {
            0.0
        } else {
            1.0
        };
        if target != 1.0 || self.gain != 1.0 {
            let step = (target - self.gain) / frames.max(1) as f32;
            for c in 0..channels {
                let mut g = self.gain;
                for s in out.channel_mut(c).iter_mut() {
                    g += step;
                    *s *= g;
                }
            }
        }
        self.gain = target;
        let peak = self.scratch[0][..n]
            .iter()
            .chain(&self.scratch[1][..n])
            .fold(0.0f32, |m, v| m.max(v.abs()));
        self.tap.raise_value(value::IN_PEAK, peak);
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
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, silence, tone};

    #[test]
    fn it_passes_or_mutes_and_feeds_the_editor() {
        let mut r = Rig::with(parameters(), TAP_VALUES, &[], TunerProcessor::new);
        let input = tone(440.0, 0.5);
        r.tap.watch();
        let (l, _) = r.run(0.05, &input, silence);
        assert!(l.iter().enumerate().all(|(i, v)| *v == input(i).0));
        assert!(r.tap.input.written() > 0);
        r.set(id::MUTE, 1.0);
        r.run(0.01, &input, silence);
        let (l, _) = r.run(0.05, &input, silence);
        assert!(l.iter().all(|v| *v == 0.0));
    }
}
