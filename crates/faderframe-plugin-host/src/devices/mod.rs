//! The stock devices: dynamics (compressor, limiter, gate, de-esser),
//! effects (saturator, reverb, delay, modulation), utilities (utility,
//! tuner), MIDI effects (arpeggiator, chord, scale, note echo) and
//! instruments (synth, sampler, drum sampler). Each module has its parameters (ids stable once released), how
//! it shows their values, what it publishes through the [`AnalysisTap`],
//! its latency and its processor.
//!
//! [`AnalysisTap`]: crate::tap::AnalysisTap

pub mod arpeggiator;
pub mod channel_strip;
pub mod chord;
pub mod compressor;
pub mod deesser;
pub mod delay;
pub mod drums;
pub mod gate;
pub mod guitar;
pub mod keep_length;
pub mod limiter;
pub(crate) mod midi_fx;
pub mod modulation;
pub mod note_echo;
pub mod preamp;
pub mod reverb;
#[cfg(test)]
pub(crate) mod rig;
pub mod sampler;
pub mod samples;
pub mod saturator;
pub mod scale;
pub mod sfz;
pub mod synth;
pub mod tuner;
pub mod utility;

use crate::{ParameterInfo, ParameterUnit};
use faderframe_core::ParameterId;

/// A parameter of a device.
pub(crate) fn param(
    id: u32,
    name: &str,
    min: f64,
    max: f64,
    default: f64,
    unit: ParameterUnit,
) -> ParameterInfo {
    ParameterInfo {
        id: ParameterId(id),
        name: name.into(),
        min,
        max,
        default,
        unit,
        automatable: true,
        stepped: false,
    }
}

/// A stepped (switch or choice) parameter of a device.
pub(crate) fn stepped(id: u32, name: &str, max: f64, default: f64) -> ParameterInfo {
    ParameterInfo {
        stepped: true,
        ..param(id, name, 0.0, max, default, ParameterUnit::None)
    }
}

/// A parameter that changes the latency (not automatable).
pub(crate) fn fixed(info: ParameterInfo) -> ParameterInfo {
    ParameterInfo {
        automatable: false,
        ..info
    }
}

/// "On"/"Off".
pub(crate) fn on_off(v: f64) -> String {
    if v >= 0.5 { "On" } else { "Off" }.into()
}

/// The `i`th of `names` (clamped).
pub(crate) fn pick(names: &[&str], v: f64) -> String {
    names[(v.round().max(0.0) as usize).min(names.len() - 1)].into()
}

/// A MIDI note's name (`60` = "C4").
pub fn note_name(n: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[n.rem_euclid(12) as usize],
        n.div_euclid(12) - 1
    )
}

/// Copy a processor's main input to its output (or silence it).
/// How many periods after a segment starts its circuits' solvers may run
/// before they abandon the samples that will not settle (GainStageFx's
/// `REALTIME_CUTOFF_PERIODS`). Late on purpose: an abandoned sample is a
/// click, and the circuit can take from milliseconds to over a second to
/// find its way back, so the cutoff only fires once the audio is certainly
/// late.
pub(crate) const CUTOFF_PERIODS: f64 = 1.5;

/// The live cutoff for the solves of a reservoir segment of `frames` at
/// `rate` (upstream's rule): from when the worker starts it -- not from the
/// reservoir's pace, which runs up to 5 ms behind real time by design and
/// put the cutoff in the past -- `CUTOFF_PERIODS` periods, plus the buffer
/// beyond one period. `None` offline.
pub(crate) fn solve_deadline(
    timing: &faderframe_realtime::reservoir::Timing,
    frames: usize,
    rate: f64,
) -> Option<std::time::Instant> {
    if !timing.realtime || frames == 0 {
        return None;
    }
    let period = std::time::Duration::from_secs_f64(frames as f64 / rate.max(1.0));
    let start = if timing.delay.is_zero() {
        timing.due
    } else {
        std::time::Instant::now().max(timing.due)
    };
    Some(start + period.mul_f64(CUTOFF_PERIODS) + timing.delay.saturating_sub(period))
}

pub(crate) fn pass_through(io: &mut faderframe_audio_graph::NodeIo<'_>) -> Option<usize> {
    let out = io.audio_out.first_mut()?;
    match io.audio_in.first() {
        Some(input) => out.copy_from(input),
        None => out.clear(),
    }
    Some(out.num_channels())
}
