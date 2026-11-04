//! De-hum: mains hum and buzz notched out at the fundamental and its
//! harmonics, the notches following the mains as it drifts
//! ([`faderframe_restore::dehum`]). Auto decides between 50 and 60 Hz by
//! listening; Custom takes any fundamental (a generator, a tape running
//! off speed). No latency.

use super::{on_off, param, pass_through, pick, stepped};
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_restore::dehum::{Dehum, HumSettings, MAX_HARMONICS};
use std::sync::Arc;

pub mod id {
    pub const FUNDAMENTAL: u32 = 0;
    pub const FREQUENCY: u32 = 1;
    pub const HARMONICS: u32 = 2;
    pub const WIDTH: u32 = 3;
    pub const DEPTH: u32 = 4;
    pub const TRACK: u32 = 5;
    pub const LISTEN: u32 = 6;
}

/// Published: the fundamental the notches stand at now (Hz) and the mains
/// it follows (50 or 60 in Auto, else the fundamental chosen).
pub mod value {
    pub const FREQUENCY: usize = 0;
    pub const NOMINAL: usize = 1;
}
pub const TAP_VALUES: usize = 2;

pub const FUNDAMENTALS: [&str; 4] = ["Auto", "50 Hz", "60 Hz", "Custom"];
pub const LISTENS: [&str; 2] = ["Output", "Hum"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        stepped(id::FUNDAMENTAL, "Fundamental", 3.0, 0.0),
        param(id::FREQUENCY, "Custom Frequency", 20.0, 500.0, 50.0, Hertz),
        ParameterInfo {
            stepped: true,
            ..param(
                id::HARMONICS,
                "Harmonics",
                1.0,
                MAX_HARMONICS as f64,
                8.0,
                None,
            )
        },
        param(id::WIDTH, "Notch Width", 0.5, 20.0, 3.0, Hertz),
        param(id::DEPTH, "Depth", 0.0, 1.0, 1.0, Percent),
        stepped(id::TRACK, "Track Drift", 1.0, 1.0),
        stepped(id::LISTEN, "Listen", 1.0, 0.0),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::FUNDAMENTAL => pick(&FUNDAMENTALS, v),
        id::LISTEN => pick(&LISTENS, v),
        id::TRACK => on_off(v),
        id::HARMONICS => format!("{}", v.round() as i64),
        _ => return None,
    })
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

/// The settings of the parameters now.
pub fn settings(params: &ParamValues) -> HumSettings {
    let fundamental = match get(params, id::FUNDAMENTAL).round() as i64 {
        1 => Some(50.0),
        2 => Some(60.0),
        3 => Some(get(params, id::FREQUENCY)),
        _ => None,
    };
    HumSettings {
        fundamental,
        harmonics: (get(params, id::HARMONICS).round().max(1.0) as usize).min(MAX_HARMONICS),
        width_hz: get(params, id::WIDTH),
        depth: get(params, id::DEPTH),
        track: get(params, id::TRACK) >= 0.5,
    }
}

const STEP: usize = 32;

pub struct DehumProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    dehum: Dehum,
    meters: [[MeterTap; 2]; 2],
    scratch: [Vec<f32>; 2],
}

impl DehumProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let mt = MeterTap::new(sr as f32);
        Self {
            watching: Watching::new(sr as f32),
            dehum: Dehum::new(sr, 2),
            meters: [[mt; 2]; 2],
            scratch: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        }
    }
}

impl PluginProcessor for DehumProcessor {
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
            let s = settings(&self.params);
            let listen = get(&self.params, id::LISTEN) >= 0.5;
            for i in at..end {
                let x = [f64::from(self.scratch[0][i]), f64::from(self.scratch[1][i])];
                let mut y = x;
                self.dehum.process(&mut y[..lanes], &s);
                for c in 0..lanes {
                    let v = if listen { x[c] - y[c] } else { y[c] };
                    out.channel_mut(c)[i] = v as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.dehum.flush();
        self.tap
            .set_value(value::FREQUENCY, self.dehum.frequency() as f32);
        self.tap
            .set_value(value::NOMINAL, self.dehum.nominal() as f32);
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
        self.dehum.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::devices::rig::{Rig, SR, bin_db, silence};

    fn rig(set: &[(u32, f64)]) -> Rig<DehumProcessor> {
        Rig::with(parameters(), TAP_VALUES, set, DehumProcessor::new)
    }

    /// 50 Hz buzz (0.4 % slow) with seven harmonics under a 700 Hz tone.
    fn buzzing(n: usize) -> (f32, f32) {
        let t = n as f64 / SR;
        let f0 = 49.8;
        let hum: f64 = (1..=7)
            .map(|h| 0.04 / (h as f64).sqrt() * (std::f64::consts::TAU * f0 * h as f64 * t).sin())
            .sum();
        let v = (hum + 0.2 * (std::f64::consts::TAU * 700.0 * t).sin()) as f32;
        (v, v)
    }

    #[test]
    fn mains_buzz_is_found_followed_and_taken() {
        let mut r = rig(&[(id::HARMONICS, 10.0), (id::WIDTH, 2.0)]);
        let (l, _) = r.run(8.0, buzzing, silence);
        assert_eq!(r.tap.value(value::NOMINAL), 50.0);
        assert!((r.tap.value(value::FREQUENCY) - 49.8).abs() < 0.01);
        for h in 1..=7 {
            let was = 20.0 * (0.04 / (h as f64).sqrt()).log10();
            let now = bin_db(&l, 49.8 * h as f64);
            assert!(now < was - 30.0, "harmonic {h}: {was:.1} → {now:.1} dB");
        }
        let tone = bin_db(&l, 700.0) - 20.0 * 0.2f64.log10();
        assert!(tone.abs() < 0.3, "the tone {tone:.2} dB");
    }

    #[test]
    fn half_depth_takes_half_and_listen_plays_the_hum() {
        let mut r = rig(&[(id::DEPTH, 0.5), (id::FUNDAMENTAL, 1.0)]);
        let (l, _) = r.run(6.0, buzzing, silence);
        let was = 20.0 * 0.04f64.log10();
        let now = bin_db(&l, 49.8);
        assert!((now - (was - 6.0)).abs() < 1.0, "{now:.1}");
        let mut r = rig(&[(id::LISTEN, 1.0), (id::FUNDAMENTAL, 1.0)]);
        let (l, _) = r.run(6.0, buzzing, silence);
        // The notches' skirts let a little of the tone through.
        let tone = bin_db(&l, 700.0) - 20.0 * 0.2f64.log10();
        assert!(tone < -25.0, "the tone {tone:.1} dB");
        assert!((bin_db(&l, 49.8) - was).abs() < 1.0);
    }
}
