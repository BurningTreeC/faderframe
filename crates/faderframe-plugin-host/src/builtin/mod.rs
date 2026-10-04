//! Plugins shipped with FaderFrame, implemented on the same
//! [`PluginInstance`]/[`PluginProcessor`] API that external formats use.

mod latency;
mod synth;

use crate::tap::AnalysisTap;
use crate::{
    AudioPortInfo, ParamValues, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor,
    PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessor, ProcessConfig,
    TailLength,
};
use faderframe_core::{ParameterId, builtin};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Compressor,
    Gain,
    Echo,
    Synth,
    LatencyProbe,
    Eq,
    ProgramEq,
    Limiter,
    Tuner,
    Modulation,
    Reverb,
    Saturator,
    Deesser,
    Gate,
}

impl Kind {
    const ALL: [Kind; 14] = [
        Kind::Eq,
        Kind::ProgramEq,
        Kind::Limiter,
        Kind::Tuner,
        Kind::Modulation,
        Kind::Reverb,
        Kind::Saturator,
        Kind::Deesser,
        Kind::Gate,
        Kind::Synth,
        Kind::Echo,
        Kind::Compressor,
        Kind::Gain,
        Kind::LatencyProbe,
    ];

    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            builtin::GAIN => Kind::Gain,
            builtin::COMPRESSOR => Kind::Compressor,
            builtin::ECHO => Kind::Echo,
            builtin::SYNTH => Kind::Synth,
            builtin::LATENCY_PROBE => Kind::LatencyProbe,
            builtin::EQ => Kind::Eq,
            builtin::PROGRAM_EQ => Kind::ProgramEq,
            builtin::LIMITER => Kind::Limiter,
            builtin::TUNER => Kind::Tuner,
            builtin::MODULATION => Kind::Modulation,
            builtin::REVERB => Kind::Reverb,
            builtin::SATURATOR => Kind::Saturator,
            builtin::DEESSER => Kind::Deesser,
            builtin::GATE => Kind::Gate,
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
                "Utility",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
            Kind::Echo => (
                builtin::ECHO,
                "Delay",
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
            Kind::Eq => (
                builtin::EQ,
                "EQ",
                PluginCategory::Effect,
                vec![stereo, sidechain],
                0,
            ),
            Kind::Gate => (
                builtin::GATE,
                "Gate",
                PluginCategory::Effect,
                vec![stereo, sidechain],
                0,
            ),
            Kind::Deesser => (
                builtin::DEESSER,
                "De-esser",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::Saturator => (
                builtin::SATURATOR,
                "Saturator",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::Reverb => (
                builtin::REVERB,
                "Reverb",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::Modulation => (
                builtin::MODULATION,
                "Modulation",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::Tuner => (
                builtin::TUNER,
                "Tuner",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
            Kind::Limiter => (
                builtin::LIMITER,
                "Limiter",
                PluginCategory::Effect,
                vec![stereo],
                0,
            ),
            Kind::ProgramEq => (
                builtin::PROGRAM_EQ,
                "Program EQ",
                PluginCategory::Effect,
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
            Kind::Gain => crate::devices::utility::parameters(),
            Kind::Compressor => crate::devices::compressor::parameters(),
            Kind::Limiter => crate::devices::limiter::parameters(),
            Kind::Tuner => crate::devices::tuner::parameters(),
            Kind::Modulation => crate::devices::modulation::parameters(),
            Kind::Reverb => crate::devices::reverb::parameters(),
            Kind::Saturator => crate::devices::saturator::parameters(),
            Kind::Deesser => crate::devices::deesser::parameters(),
            Kind::Gate => crate::devices::gate::parameters(),
            Kind::Echo => crate::devices::delay::parameters(),
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
            Kind::Eq => crate::eq::parameters(),
            Kind::ProgramEq => crate::program_eq::parameters(),
        }
    }

    /// Values the processor publishes through the tap.
    fn tap_values(self) -> Option<usize> {
        match self {
            Kind::Eq => Some(crate::eq::TAP_VALUES),
            Kind::Compressor => Some(crate::devices::compressor::TAP_VALUES),
            Kind::Limiter => Some(crate::devices::limiter::TAP_VALUES),
            Kind::Tuner => Some(crate::devices::tuner::TAP_VALUES),
            Kind::Modulation => Some(crate::devices::modulation::TAP_VALUES),
            Kind::Reverb => Some(crate::devices::reverb::TAP_VALUES),
            Kind::Echo => Some(crate::devices::delay::TAP_VALUES),
            Kind::Gain => Some(crate::devices::utility::TAP_VALUES),
            Kind::Saturator => Some(crate::devices::saturator::TAP_VALUES),
            Kind::Deesser => Some(crate::devices::deesser::TAP_VALUES),
            Kind::Gate => Some(crate::devices::gate::TAP_VALUES),
            Kind::ProgramEq => Some(0),
            _ => None,
        }
    }
}

/// Control-side instance shared by all built-ins.
pub struct BuiltinInstance {
    kind: Kind,
    descriptor: PluginDescriptor,
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    /// The latency last reported (a change asks for a restart).
    reported: Option<u32>,
    /// The program last selected.
    program: Option<usize>,
    /// The sample rate of the last processor (latencies in samples depend
    /// on it).
    rate: f64,
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

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        match self.kind {
            Kind::Eq => crate::eq::format(id, value),
            Kind::ProgramEq => crate::program_eq::format(id, value),
            Kind::Compressor => crate::devices::compressor::format(id, value),
            Kind::Limiter => crate::devices::limiter::format(id, value),
            Kind::Tuner => crate::devices::tuner::format(id, value),
            Kind::Modulation => crate::devices::modulation::format(id, value),
            Kind::Reverb => crate::devices::reverb::format(id, value),
            Kind::Echo => crate::devices::delay::format(id, value),
            Kind::Gain => crate::devices::utility::format(id, value),
            Kind::Saturator => crate::devices::saturator::format(id, value),
            Kind::Deesser => crate::devices::deesser::format(id, value),
            Kind::Gate => crate::devices::gate::format(id, value),
            _ => None,
        }
    }

    fn tap(&self) -> Option<Arc<AnalysisTap>> {
        self.tap.clone()
    }

    fn programs(&self) -> Vec<String> {
        match self.kind {
            Kind::ProgramEq => crate::program_eq::PRESETS
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn current_program(&self) -> Option<usize> {
        self.program
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        let presets = match self.kind {
            Kind::ProgramEq => crate::program_eq::PRESETS,
            _ => return Err(PluginError::Failed("no programs".into())),
        };
        let (_, values) = presets
            .get(index)
            .ok_or_else(|| PluginError::Failed(format!("no program {index}")))?;
        for (param, value) in *values {
            self.params.set_by_id(ParameterId(*param as u32), *value)?;
        }
        self.program = Some(index);
        Ok(())
    }

    fn poll(&mut self) -> crate::PluginPoll {
        // The EQ's phase mode, resolution and spectral bands change its
        // latency: the graph must be rebuilt (with a processor for the new
        // mode).
        let now = self.latency_samples();
        let restart = self.reported.is_some_and(|r| r != now);
        self.reported = Some(now);
        crate::PluginPoll {
            restart,
            ..crate::PluginPoll::default()
        }
    }

    fn latency_samples(&self) -> u32 {
        match self.kind {
            Kind::LatencyProbe => self.params.get(0).max(0.0) as u32,
            Kind::ProgramEq => crate::program_eq::LATENCY,
            Kind::Eq => crate::eq::latency(&self.params),
            Kind::Compressor => crate::devices::compressor::latency(&self.params, self.rate),
            Kind::Limiter => crate::devices::limiter::latency(&self.params, self.rate),
            Kind::Tuner => crate::devices::tuner::latency(&self.params, self.rate),
            Kind::Modulation => crate::devices::modulation::latency(&self.params, self.rate),
            Kind::Reverb => crate::devices::reverb::latency(&self.params, self.rate),
            Kind::Saturator => crate::devices::saturator::latency(&self.params, self.rate),
            Kind::Deesser => crate::devices::deesser::latency(&self.params, self.rate),
            Kind::Gate => crate::devices::gate::latency(&self.params, self.rate),
            _ => 0,
        }
    }

    fn note_expressions(&self) -> Option<Vec<faderframe_midi::NoteExpressionKind>> {
        match self.kind {
            Kind::Synth => Some(faderframe_midi::NoteExpressionKind::ALL.to_vec()),
            _ => Some(Vec::new()),
        }
    }

    fn tail(&self) -> TailLength {
        match self.kind {
            Kind::Echo | Kind::Reverb => TailLength::Infinite,
            Kind::Modulation => TailLength::Samples(4_096),
            Kind::Synth => TailLength::Samples(48_000 * 5),
            Kind::Gain
            | Kind::Compressor
            | Kind::Limiter
            | Kind::Gate
            | Kind::Deesser
            | Kind::Saturator
            | Kind::Tuner => TailLength::None,
            // The longest ring of a resonant cut near 10 Hz.
            Kind::Eq => TailLength::Samples(48_000),
            Kind::ProgramEq => TailLength::Samples(24_000),
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
        self.rate = config.sample_rate.max(1.0);
        let tap = || {
            self.tap
                .clone()
                .ok_or_else(|| PluginError::Failed("no tap".into()))
        };
        Ok(match self.kind {
            Kind::Gain => Box::new(crate::devices::utility::UtilityProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Compressor => Box::new(crate::devices::compressor::CompressorProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Limiter => Box::new(crate::devices::limiter::LimiterProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Tuner => Box::new(crate::devices::tuner::TunerProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Modulation => Box::new(crate::devices::modulation::ModulationProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Reverb => Box::new(crate::devices::reverb::ReverbProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Saturator => Box::new(crate::devices::saturator::SaturatorProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Deesser => Box::new(crate::devices::deesser::DeesserProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Gate => Box::new(crate::devices::gate::GateProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Echo => Box::new(crate::devices::delay::DelayProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Synth => Box::new(synth::SynthProcessor::new(params, config)),
            Kind::LatencyProbe => Box::new(latency::LatencyProcessor::new(self.latency_samples())),
            Kind::Eq => {
                let tap = self
                    .tap
                    .clone()
                    .ok_or_else(|| PluginError::Failed("no tap".into()))?;
                Box::new(crate::eq::EqProcessor::new(params, tap, config))
            }
            Kind::ProgramEq => {
                let tap = self
                    .tap
                    .clone()
                    .ok_or_else(|| PluginError::Failed("no tap".into()))?;
                Box::new(crate::program_eq::ProgramEqProcessor::new(
                    params, tap, config,
                ))
            }
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
        Kind::ALL.iter().map(|k| k.descriptor()).collect()
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        let kind = Kind::from_id(id).ok_or_else(|| PluginError::NotFound(id.into()))?;
        let params = ParamValues::new(kind.parameters());
        let tap = kind
            .tap_values()
            .map(|n| Arc::new(AnalysisTap::new(params.clone(), n)));
        Ok(Box::new(BuiltinInstance {
            kind,
            descriptor: kind.descriptor(),
            params,
            tap,
            reported: None,
            program: None,
            rate: 48_000.0,
        }))
    }
}

#[cfg(test)]
mod tests;
