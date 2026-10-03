//! Plugins shipped with FaderFrame, implemented on the same
//! [`PluginInstance`]/[`PluginProcessor`] API that external formats use.

mod compressor;
mod echo;
mod gain;
mod latency;
mod synth;

use crate::{
    AudioPortInfo, ParamValues, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor,
    PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessor, ProcessConfig,
    TailLength,
};
use faderframe_core::{ParameterId, builtin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Compressor,
    Gain,
    Echo,
    Synth,
    LatencyProbe,
}

impl Kind {
    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            builtin::GAIN => Kind::Gain,
            builtin::COMPRESSOR => Kind::Compressor,
            builtin::ECHO => Kind::Echo,
            builtin::SYNTH => Kind::Synth,
            builtin::LATENCY_PROBE => Kind::LatencyProbe,
            _ => return None,
        })
    }

    fn descriptor(self) -> PluginDescriptor {
        let stereo = AudioPortInfo {
            channels: 2,
            is_main: true,
        };
        let sidechain = AudioPortInfo {
            channels: 2,
            is_main: false,
        };
        let (id, name, category, inputs, notes) = match self {
            Kind::Compressor => (
                builtin::COMPRESSOR,
                "Compressor",
                PluginCategory::Effect,
                vec![stereo, sidechain],
                0,
            ),
            Kind::Gain => (
                builtin::GAIN,
                "Gain",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
            Kind::Echo => (
                builtin::ECHO,
                "Echo",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::Synth => (
                builtin::SYNTH,
                "Synth",
                PluginCategory::Instrument,
                vec![],
                1,
            ),
            Kind::LatencyProbe => (
                builtin::LATENCY_PROBE,
                "Latency Probe",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
        };
        PluginDescriptor {
            format: PluginFormat::Builtin,
            id: id.into(),
            name: format!("FaderFrame {name}"),
            vendor: "FaderFrame".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            category,
            audio_inputs: inputs,
            audio_outputs: vec![stereo],
            note_inputs: notes,
            note_outputs: 0,
        }
    }

    fn parameters(self) -> Vec<ParameterInfo> {
        let p = |id: u32, name: &str, min: f64, max: f64, default: f64, unit| ParameterInfo {
            id: ParameterId(id),
            name: name.into(),
            min,
            max,
            default,
            unit,
            automatable: true,
            stepped: false,
        };
        use ParameterUnit::*;
        match self {
            Kind::Gain => vec![p(0, "Gain", -60.0, 24.0, 0.0, Decibels)],
            Kind::Compressor => vec![
                p(0, "Threshold", -60.0, 0.0, -20.0, Decibels),
                p(1, "Ratio", 1.0, 20.0, 4.0, None),
                p(2, "Attack", 0.1, 200.0, 10.0, Milliseconds),
                p(3, "Release", 5.0, 2000.0, 150.0, Milliseconds),
                p(4, "Makeup", 0.0, 24.0, 0.0, Decibels),
            ],
            Kind::Echo => vec![
                p(0, "Time", 10.0, 2000.0, 401.0, Milliseconds),
                p(1, "Feedback", 0.0, 0.95, 0.38, Percent),
                p(2, "Damping", 0.0, 1.0, 0.35, Percent),
                p(3, "Mix", 0.0, 1.0, 1.0, Percent),
                ParameterInfo {
                    stepped: true,
                    ..p(4, "Ping-Pong", 0.0, 1.0, 1.0, None)
                },
            ],
            Kind::Synth => vec![
                p(0, "Volume", -48.0, 6.0, -6.0, Decibels),
                p(1, "Cutoff", 40.0, 16_000.0, 2_400.0, Hertz),
                p(2, "Resonance", 0.0, 1.0, 0.25, Percent),
                p(3, "Env Amount", 0.0, 1.0, 0.5, Percent),
                p(4, "Attack", 0.5, 2_000.0, 5.0, Milliseconds),
                p(5, "Decay", 5.0, 4_000.0, 300.0, Milliseconds),
                p(6, "Sustain", 0.0, 1.0, 0.6, Percent),
                p(7, "Release", 5.0, 5_000.0, 350.0, Milliseconds),
                p(8, "Detune", 0.0, 50.0, 9.0, None),
            ],
            Kind::LatencyProbe => vec![ParameterInfo {
                stepped: true,
                automatable: false,
                ..p(0, "Latency", 0.0, 48_000.0, 256.0, Samples)
            }],
        }
    }
}

/// Control-side instance shared by all built-ins.
pub struct BuiltinInstance {
    kind: Kind,
    descriptor: PluginDescriptor,
    params: ParamValues,
}

impl PluginInstance for BuiltinInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        self.params.infos()
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        self.params.get_by_id(id)
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        self.params.set_by_id(id, value)
    }

    fn latency_samples(&self) -> u32 {
        match self.kind {
            Kind::LatencyProbe => self.params.get(0).max(0.0) as u32,
            _ => 0,
        }
    }

    fn tail(&self) -> TailLength {
        match self.kind {
            Kind::Echo => TailLength::Infinite,
            Kind::Synth => TailLength::Samples(48_000 * 5),
            Kind::Gain | Kind::Compressor => TailLength::None,
            Kind::LatencyProbe => TailLength::Samples(self.latency_samples()),
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(self.params.save())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        self.params.load(data)
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        let params = self.params.clone();
        Ok(match self.kind {
            Kind::Gain => Box::new(gain::GainProcessor::new(params)),
            Kind::Compressor => Box::new(compressor::CompressorProcessor::new(params, config)),
            Kind::Echo => Box::new(echo::EchoProcessor::new(params, config)),
            Kind::Synth => Box::new(synth::SynthProcessor::new(params, config)),
            Kind::LatencyProbe => Box::new(latency::LatencyProcessor::new(self.latency_samples())),
        })
    }
}

/// Factory for [`PluginFormat::Builtin`].
pub struct BuiltinFactory;

impl PluginFactory for BuiltinFactory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Builtin
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        [
            Kind::Synth,
            Kind::Echo,
            Kind::Compressor,
            Kind::Gain,
            Kind::LatencyProbe,
        ]
        .iter()
        .map(|k| k.descriptor())
        .collect()
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        let kind = Kind::from_id(id).ok_or_else(|| PluginError::NotFound(id.into()))?;
        Ok(Box::new(BuiltinInstance {
            kind,
            descriptor: kind.descriptor(),
            params: ParamValues::new(kind.parameters()),
        }))
    }
}

#[cfg(test)]
mod tests;
