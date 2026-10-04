//! The stock devices: dynamics (compressor, limiter, gate, de-esser),
//! effects (saturator, reverb, delay, modulation), utilities (utility,
//! tuner). Each module has its parameters (ids stable once released), how
//! it shows their values, what it publishes through the [`AnalysisTap`],
//! its latency and its processor.
//!
//! [`AnalysisTap`]: crate::tap::AnalysisTap

pub mod compressor;
pub mod deesser;
pub mod gate;
pub mod limiter;
#[cfg(test)]
pub(crate) mod rig;

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

/// Copy a processor's main input to its output (or silence it).
pub(crate) fn pass_through(io: &mut faderframe_audio_graph::NodeIo<'_>) -> Option<usize> {
    let out = io.audio_out.first_mut()?;
    match io.audio_in.first() {
        Some(input) => out.copy_from(input),
        None => out.clear(),
    }
    Some(out.num_channels())
}
