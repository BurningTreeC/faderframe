//! FaderFrame Program EQ: a circuit modelled passive program equaliser with
//! a tube make-up stage.
//!
//! This is PultEQFx by Simon Huber, used in FaderFrame under the MIT
//! licence. It models the passive LC/RC network of the classic 1950s tube
//! program equaliser by nodal analysis rather than with a bank of shelves,
//! so boosting and attenuating the same low frequency gives the famous
//! bump-and-dip; the make-up amplifier adds mostly odd harmonics as it runs
//! out of headroom, harder with DRIVE. The knobs read 0 to 10 like the
//! hardware's; the frequency selectors switch the network's capacitors and
//! inductors. The latency is a fixed 74 samples at every oversampling
//! setting, and with the power off the dry signal comes out as late.

mod channel;
pub mod network;
mod nodal;
mod tube;

pub use channel::{CROSSFADE, Channel, LATENCY, WARM_UP};
pub use network::{Controls, HIGH_ATTEN_FREQS, HIGH_BOOST_FREQS, LOW_FREQS, PassiveNetwork};

use crate::tap::{AnalysisTap, MeterTap};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use std::sync::Arc;

/// Parameter indexes (and ids).
pub mod param {
    pub const LOW_FREQ: usize = 0;
    pub const LOW_BOOST: usize = 1;
    pub const LOW_ATTEN: usize = 2;
    pub const BANDWIDTH: usize = 3;
    pub const HIGH_BOOST_FREQ: usize = 4;
    pub const HIGH_BOOST: usize = 5;
    pub const HIGH_ATTEN_FREQ: usize = 6;
    pub const HIGH_ATTEN: usize = 7;
    pub const POWER: usize = 8;
    pub const EQ_IN: usize = 9;
    pub const DRIVE: usize = 10;
    pub const OUTPUT: usize = 11;
    pub const OVERSAMPLING: usize = 12;
}

/// Labels engraved round the selectors.
pub const LOW_FREQ_LABELS: [&str; 4] = ["20", "30", "60", "100"];
pub const HIGH_BOOST_LABELS: [&str; 7] = ["3", "4", "5", "8", "10", "12", "16"];
pub const HIGH_ATTEN_LABELS: [&str; 3] = ["5", "10", "20"];
pub const OVERSAMPLING_LABELS: [&str; 4] = ["Off", "2x", "4x", "8x"];

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    let p = |i: usize, name: &str, min: f64, max: f64, default: f64, unit, stepped| ParameterInfo {
        id: ParameterId(i as u32),
        name: name.into(),
        min,
        max,
        default,
        unit,
        automatable: true,
        stepped,
    };
    vec![
        p(param::LOW_FREQ, "Low Frequency", 0.0, 3.0, 3.0, None, true),
        p(param::LOW_BOOST, "Low Boost", 0.0, 10.0, 0.0, None, false),
        p(param::LOW_ATTEN, "Low Atten", 0.0, 10.0, 0.0, None, false),
        p(param::BANDWIDTH, "Bandwidth", 0.0, 10.0, 5.0, None, false),
        p(
            param::HIGH_BOOST_FREQ,
            "High Boost Frequency",
            0.0,
            6.0,
            4.0,
            None,
            true,
        ),
        p(param::HIGH_BOOST, "High Boost", 0.0, 10.0, 0.0, None, false),
        p(
            param::HIGH_ATTEN_FREQ,
            "High Atten Frequency",
            0.0,
            2.0,
            1.0,
            None,
            true,
        ),
        p(param::HIGH_ATTEN, "High Atten", 0.0, 10.0, 0.0, None, false),
        p(param::POWER, "Power", 0.0, 1.0, 1.0, None, true),
        p(param::EQ_IN, "EQ In", 0.0, 1.0, 1.0, None, true),
        p(param::DRIVE, "Drive", 0.0, 18.0, 0.0, Decibels, false),
        p(param::OUTPUT, "Output", -24.0, 24.0, 0.0, Decibels, false),
        ParameterInfo {
            automatable: false,
            ..p(
                param::OVERSAMPLING,
                "Oversampling",
                0.0,
                3.0,
                2.0,
                None,
                true,
            )
        },
    ]
}

fn pick<'a>(labels: &[&'a str], value: f64) -> &'a str {
    labels[(value.round().max(0.0) as usize).min(labels.len() - 1)]
}

/// A parameter value as the panel letters it.
pub fn format(id: ParameterId, value: f64) -> Option<String> {
    Some(match id.0 as usize {
        param::LOW_FREQ => format!("{} Hz", pick(&LOW_FREQ_LABELS, value)),
        param::HIGH_BOOST_FREQ => format!("{} kHz", pick(&HIGH_BOOST_LABELS, value)),
        param::HIGH_ATTEN_FREQ => format!("{} kHz", pick(&HIGH_ATTEN_LABELS, value)),
        param::POWER => if value >= 0.5 { "ON" } else { "OFF" }.into(),
        param::EQ_IN => if value >= 0.5 { "IN" } else { "OUT" }.into(),
        param::OVERSAMPLING => pick(&OVERSAMPLING_LABELS, value).into(),
        param::LOW_BOOST
        | param::LOW_ATTEN
        | param::BANDWIDTH
        | param::HIGH_BOOST
        | param::HIGH_ATTEN => format!("{value:.1}"),
        _ => return None,
    })
}

/// Built-in presets: name and panel values `(parameter, value)`.
pub const PRESETS: &[(&str, &[(usize, f64)])] = &[(
    // The low end trick at 100 cps with a little 10 kc air, and a touch of
    // the amplifier (5.5 dB more level into it, taken back at the output).
    "Low End Punch",
    &[
        (param::POWER, 1.0),
        (param::EQ_IN, 1.0),
        (param::LOW_FREQ, 3.0),
        (param::LOW_BOOST, 6.0),
        (param::LOW_ATTEN, 7.0),
        (param::BANDWIDTH, 5.0),
        (param::HIGH_BOOST_FREQ, 4.0),
        (param::HIGH_BOOST, 3.0),
        (param::HIGH_ATTEN_FREQ, 1.0),
        (param::HIGH_ATTEN, 0.0),
        (param::DRIVE, 5.5),
        (param::OUTPUT, -5.5),
    ],
)];

/// The circuit's controls from the panel values.
pub fn controls(params: &ParamValues, smoothed: &[f64; 5]) -> Controls {
    let sel = |i: usize, table: &[f32]| {
        let k = (params.get(i).round().max(0.0) as usize).min(table.len() - 1);
        f64::from(table[k])
    };
    Controls {
        low_boost: smoothed[0] / 10.0,
        low_atten: smoothed[1] / 10.0,
        high_boost: smoothed[2] / 10.0,
        high_atten: smoothed[3] / 10.0,
        bandwidth: smoothed[4] / 10.0,
        low_freq: sel(param::LOW_FREQ, &LOW_FREQS),
        high_boost_freq: sel(param::HIGH_BOOST_FREQ, &HIGH_BOOST_FREQS),
        high_atten_freq: sel(param::HIGH_ATTEN_FREQ, &HIGH_ATTEN_FREQS),
    }
}

/// Controls are refreshed every this many samples.
const CONTROL_BLOCK: usize = 32;
/// Knobs glide to a new setting over this long (seconds).
const GLIDE: f64 = 0.03;

/// A linear glide, one control block at a time.
#[derive(Clone, Copy, Debug)]
struct Glide {
    current: f64,
    target: f64,
    delta: f64,
    left: u32,
    steps: u32,
}

impl Glide {
    fn new(value: f64, steps: u32) -> Self {
        Self {
            current: value,
            target: value,
            delta: 0.0,
            left: 0,
            steps: steps.max(1),
        }
    }

    fn next(&mut self, target: f64) -> f64 {
        if target != self.target {
            self.target = target;
            self.left = self.steps;
            self.delta = (target - self.current) / f64::from(self.steps);
        }
        if self.left > 0 {
            self.left -= 1;
            self.current = if self.left == 0 {
                self.target
            } else {
                self.current + self.delta
            };
        }
        self.current
    }
}

/// The knobs that glide: low boost, low atten, high boost, high atten,
/// bandwidth, then drive and output.
const GLIDING: [usize; 7] = [
    param::LOW_BOOST,
    param::LOW_ATTEN,
    param::HIGH_BOOST,
    param::HIGH_ATTEN,
    param::BANDWIDTH,
    param::DRIVE,
    param::OUTPUT,
];

pub struct ProgramEqProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    channels: [Channel; 2],
    oversampling: usize,
    glides: [Glide; 7],
    meters: [[MeterTap; 2]; 2],
}

impl ProgramEqProcessor {
    pub fn new(params: ParamValues, tap: Arc<AnalysisTap>, config: &ProcessConfig) -> Self {
        let sr = config.sample_rate.max(1.0);
        let factor = 1usize << (params.get(param::OVERSAMPLING).round().clamp(0.0, 3.0) as u32);
        let steps = ((GLIDE * sr) / CONTROL_BLOCK as f64).ceil() as u32;
        let glides = GLIDING.map(|i| Glide::new(f64::from(params.get(i)), steps));
        let mt = MeterTap::new(sr as f32);
        let mut p = Self {
            channels: [Channel::new(sr, factor), Channel::new(sr, factor)],
            oversampling: factor,
            params,
            tap,
            glides,
            meters: [[mt; 2]; 2],
        };
        p.apply_controls();
        p
    }

    /// Glide the knobs one control block on and hand the circuit its settings.
    fn apply_controls(&mut self) -> f64 {
        let mut v = [0.0; 7];
        for (k, i) in GLIDING.iter().enumerate() {
            v[k] = self.glides[k].next(f64::from(self.params.get(*i)));
        }
        let c = controls(&self.params, &[v[0], v[1], v[2], v[3], v[4]]);
        for ch in &mut self.channels {
            ch.set_controls(c);
            ch.set_drive(v[5]);
        }
        10f64.powf(v[6] / 20.0)
    }
}

impl PluginProcessor for ProgramEqProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        match io.audio_in.first() {
            Some(input) => out.copy_from(input),
            None => out.clear(),
        }
        let channels = out.num_channels().min(2);
        // The oversampling can change while running without a gap (the
        // latency does not change with it).
        let factor =
            1usize << (self.params.get(param::OVERSAMPLING).round().clamp(0.0, 3.0) as u32);
        if factor != self.oversampling {
            self.oversampling = factor;
            for ch in &mut self.channels {
                ch.set_oversampling(factor);
            }
        }
        let mut events = ctx.param_events.iter().peekable();
        let mut at = 0;
        while at < frames {
            let end = (at + CONTROL_BLOCK).min(frames);
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                } else {
                    break;
                }
            }
            let powered = self.params.get(param::POWER) >= 0.5;
            let eq_in = self.params.get(param::EQ_IN) >= 0.5;
            let output = self.apply_controls();
            // The output trim is the amplifier's: it leaves with the power.
            let gain = if powered { output as f32 } else { 1.0 };
            for c in 0..channels {
                let samples = &mut out.channel_mut(c)[at..end];
                let (input_tap, output_tap) = {
                    let [i, o] = &mut self.meters;
                    (&mut i[c], &mut o[c])
                };
                let ch = &mut self.channels[c];
                for s in samples.iter_mut() {
                    input_tap.add(*s);
                    *s = ch.process(*s, eq_in, powered) * gain;
                    output_tap.add(*s);
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        // The panel has two meter bars even on a mono track. Mirror the
        // measurement before publishing (which clears the accumulated peak).
        if channels == 1 {
            self.meters[0][1] = self.meters[0][0];
            self.meters[1][1] = self.meters[1][0];
        }
        for c in 0..if channels == 1 { 2 } else { channels } {
            self.meters[0][c].publish(&self.tap.meter_in, c, frames);
            self.meters[1][c].publish(&self.tap.meter_out, c, frames);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
