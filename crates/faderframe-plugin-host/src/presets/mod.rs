//! Factory presets of the stock effects, the MIDI effects and the Synth:
//! starting points that
//! show what each device does best, named for what they are for.
//!
//! A preset lists only the values it moves; everything else is the
//! parameter's default, so [`FactoryPreset::values`] always gives the
//! device's complete settings and loading one leaves nothing of what was
//! there before. The samplers (their sound is the samples) and the tuner
//! have none.
//!
//! Effects that are usually fed from sends (reverb, delay) are voiced for
//! inserts, with a musical mix; [`send_return_mix`] names their mix so a
//! host can load them fully wet on an effect return.

mod dynamics;
mod effects;
mod equalisers;
mod guitar;
mod midi;
mod synth;
#[cfg(test)]
mod tests;

use crate::ParameterInfo;
use faderframe_core::{ParameterId, builtin};

/// One factory preset of a stock device.
#[derive(Clone, Debug, PartialEq)]
pub struct FactoryPreset {
    pub name: &'static str,
    /// The values it sets (parameter id, value); the rest are defaults.
    set: Vec<(u32, f64)>,
}

/// A preset from what it sets.
pub(crate) fn preset(name: &'static str, set: &[(u32, f64)]) -> FactoryPreset {
    FactoryPreset {
        name,
        set: set.to_vec(),
    }
}

impl FactoryPreset {
    /// Every parameter of `infos` with this preset's value (its default
    /// where the preset does not set it), clamped to the parameter's range.
    pub fn values(&self, infos: &[ParameterInfo]) -> Vec<(ParameterId, f64)> {
        infos
            .iter()
            .map(|p| {
                let v = self
                    .set
                    .iter()
                    .rev()
                    .find(|(id, _)| *id == p.id.0)
                    .map_or(p.default, |(_, v)| *v);
                (p.id, v.clamp(p.min, p.max))
            })
            .collect()
    }

    /// The values it sets itself.
    pub fn set(&self) -> &[(u32, f64)] {
        &self.set
    }
}

/// The factory presets of a built-in plugin (empty for those without).
pub fn factory_presets(plugin_id: &str) -> Vec<FactoryPreset> {
    match plugin_id {
        builtin::COMPRESSOR => dynamics::compressor(),
        builtin::LIMITER => dynamics::limiter(),
        builtin::GATE => dynamics::gate(),
        builtin::DEESSER => dynamics::deesser(),
        builtin::SATURATOR => effects::saturator(),
        builtin::ECHO => effects::delay(),
        builtin::REVERB => effects::reverb(),
        builtin::MODULATION => effects::modulation(),
        builtin::GAIN => effects::utility(),
        builtin::EQ => equalisers::eq(),
        builtin::PROGRAM_EQ => equalisers::program_eq(),
        builtin::SYNTH => synth::synth(),
        builtin::ARPEGGIATOR => midi::arpeggiator(),
        builtin::CHORD => midi::chord(),
        builtin::SCALE => midi::scale(),
        builtin::NOTE_ECHO => midi::note_echo(),
        builtin::GUITAR_STATION => guitar::guitar(),
        _ => Vec::new(),
    }
}

/// The parameters of a built-in plugin with factory presets.
pub fn parameters(plugin_id: &str) -> Vec<ParameterInfo> {
    use crate::devices::*;
    match plugin_id {
        builtin::COMPRESSOR => compressor::parameters(),
        builtin::LIMITER => limiter::parameters(),
        builtin::GATE => gate::parameters(),
        builtin::DEESSER => deesser::parameters(),
        builtin::SATURATOR => saturator::parameters(),
        builtin::ECHO => delay::parameters(),
        builtin::REVERB => reverb::parameters(),
        builtin::MODULATION => modulation::parameters(),
        builtin::GAIN => utility::parameters(),
        builtin::EQ => crate::eq::parameters(),
        builtin::PROGRAM_EQ => crate::program_eq::parameters(),
        builtin::SYNTH => synth::parameters(),
        builtin::ARPEGGIATOR => arpeggiator::parameters(),
        builtin::CHORD => chord::parameters(),
        builtin::SCALE => scale::parameters(),
        builtin::NOTE_ECHO => note_echo::parameters(),
        builtin::GUITAR_STATION => guitar::parameters(),
        _ => Vec::new(),
    }
}

/// Preset `index` of a built-in plugin, complete (see
/// [`FactoryPreset::values`]).
pub fn factory_preset_values(plugin_id: &str, index: usize) -> Option<Vec<(ParameterId, f64)>> {
    let preset = factory_presets(plugin_id).into_iter().nth(index)?;
    Some(preset.values(&parameters(plugin_id)))
}

/// The mix of a device usually fed from a send: on an effect return its
/// presets belong fully wet (the dry signal is on the sending tracks).
pub fn send_return_mix(plugin_id: &str) -> Option<ParameterId> {
    match plugin_id {
        builtin::REVERB => Some(ParameterId(crate::devices::reverb::id::MIX)),
        builtin::ECHO => Some(ParameterId(crate::devices::delay::id::MIX)),
        _ => None,
    }
}
