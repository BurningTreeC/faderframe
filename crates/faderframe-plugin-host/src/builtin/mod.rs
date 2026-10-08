//! Plugins shipped with FaderFrame, implemented on the same
//! [`PluginInstance`]/[`PluginProcessor`] API that external formats use.

mod latency;

use crate::tap::AnalysisTap;
use crate::{
    AudioPortInfo, ParamValues, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor,
    PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessContext,
    PluginProcessor, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, builtin};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Preamp(usize),
    Compressor,
    Gain,
    Echo,
    Synth,
    LatencyProbe,
    Eq,
    ProgramEq,
    Limiter,
    Drums,
    Sampler,
    Tuner,
    Modulation,
    Reverb,
    Saturator,
    Deesser,
    Gate,
    ChannelStrip,
    Guitar,
    Arpeggiator,
    Chord,
    Scale,
    NoteEcho,
    /// Parallel chains: the graph builds them; the instance does nothing.
    Container,
    /// Outboard gear (the graph builds its send and return).
    HardwareInsert,
}

impl Kind {
    const ALL: [Kind; 30] = [
        Kind::Preamp(0),
        Kind::Preamp(1),
        Kind::Preamp(2),
        Kind::Preamp(3),
        Kind::Preamp(4),
        Kind::Preamp(5),
        Kind::Eq,
        Kind::ProgramEq,
        Kind::Limiter,
        Kind::Drums,
        Kind::Sampler,
        Kind::Tuner,
        Kind::Modulation,
        Kind::Reverb,
        Kind::Saturator,
        Kind::Deesser,
        Kind::Gate,
        Kind::ChannelStrip,
        Kind::Guitar,
        Kind::Synth,
        Kind::Echo,
        Kind::Compressor,
        Kind::Gain,
        Kind::Arpeggiator,
        Kind::Chord,
        Kind::Scale,
        Kind::NoteEcho,
        Kind::Container,
        Kind::HardwareInsert,
        Kind::LatencyProbe,
    ];

    fn from_id(id: &str) -> Option<Self> {
        if let Some(i) = builtin::preamp_index(id) {
            return Some(Self::Preamp(i));
        }
        Some(match id {
            builtin::GAIN => Kind::Gain,
            builtin::COMPRESSOR => Kind::Compressor,
            builtin::ECHO => Kind::Echo,
            builtin::SYNTH => Kind::Synth,
            builtin::LATENCY_PROBE => Kind::LatencyProbe,
            builtin::EQ => Kind::Eq,
            builtin::PROGRAM_EQ => Kind::ProgramEq,
            builtin::LIMITER => Kind::Limiter,
            builtin::DRUMS => Kind::Drums,
            builtin::SAMPLER => Kind::Sampler,
            builtin::TUNER => Kind::Tuner,
            builtin::ARPEGGIATOR => Kind::Arpeggiator,
            builtin::CHORD => Kind::Chord,
            builtin::SCALE => Kind::Scale,
            builtin::NOTE_ECHO => Kind::NoteEcho,
            builtin::MODULATION => Kind::Modulation,
            builtin::REVERB => Kind::Reverb,
            builtin::SATURATOR => Kind::Saturator,
            builtin::DEESSER => Kind::Deesser,
            builtin::GATE => Kind::Gate,
            builtin::CHANNEL_STRIP => Kind::ChannelStrip,
            builtin::GUITAR_STATION => Kind::Guitar,
            builtin::CONTAINER => Kind::Container,
            builtin::HARDWARE_INSERT => Kind::HardwareInsert,
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
            Kind::Preamp(i) => (
                builtin::PREAMPS[i].0,
                builtin::PREAMPS[i].1,
                PluginCategory::Preamp,
                vec![stereo],
                0,
            ),
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
            Kind::ChannelStrip => (
                builtin::CHANNEL_STRIP,
                "Channel Strip",
                PluginCategory::Effect,
                vec![stereo, sidechain],
                0,
            ),
            Kind::Guitar => (
                builtin::GUITAR_STATION,
                "Guitar Station",
                PluginCategory::Effect,
                vec![stereo],
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
            Kind::Arpeggiator => (
                builtin::ARPEGGIATOR,
                "Arpeggiator",
                PluginCategory::MidiEffect,
                vec![],
                1,
            ),
            Kind::Chord => (
                builtin::CHORD,
                "Chord",
                PluginCategory::MidiEffect,
                vec![],
                1,
            ),
            Kind::Scale => (
                builtin::SCALE,
                "Scale",
                PluginCategory::MidiEffect,
                vec![],
                1,
            ),
            Kind::NoteEcho => (
                builtin::NOTE_ECHO,
                "Note Echo",
                PluginCategory::MidiEffect,
                vec![],
                1,
            ),
            Kind::Tuner => (
                builtin::TUNER,
                "Tuner",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
            Kind::Container => (
                builtin::CONTAINER,
                "Container",
                PluginCategory::Utility,
                vec![stereo],
                0,
            ),
            Kind::HardwareInsert => (
                builtin::HARDWARE_INSERT,
                "Hardware Insert",
                PluginCategory::Utility,
                vec![stereo, sidechain],
                0,
            ),
            Kind::Sampler => (
                builtin::SAMPLER,
                "Sampler",
                PluginCategory::Instrument,
                vec![],
                1,
            ),
            Kind::Drums => (
                builtin::DRUMS,
                "Drum Sampler",
                PluginCategory::Instrument,
                vec![],
                1,
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
            // A MIDI effect: notes in, notes out, no audio. The Drum
            // Sampler's pads can play into extra outputs, the Guitar
            // Station's DI into a second one.
            audio_outputs: if category == PluginCategory::MidiEffect {
                vec![]
            } else if self == Kind::Drums {
                vec![stereo; 1 + crate::devices::drums::AUX]
            } else if self == Kind::Guitar {
                vec![stereo; 2]
            } else {
                vec![stereo]
            },
            note_inputs: notes,
            note_outputs: if category == PluginCategory::MidiEffect {
                1
            } else {
                0
            },
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
            Kind::Preamp(_) => crate::devices::preamp::parameters(),
            Kind::Gain => crate::devices::utility::parameters(),
            Kind::Compressor => crate::devices::compressor::parameters(),
            Kind::Limiter => crate::devices::limiter::parameters(),
            Kind::Drums => crate::devices::drums::parameters(),
            Kind::Sampler => crate::devices::sampler::parameters(),
            Kind::Tuner => crate::devices::tuner::parameters(),
            Kind::Arpeggiator => crate::devices::arpeggiator::parameters(),
            Kind::Chord => crate::devices::chord::parameters(),
            Kind::Scale => crate::devices::scale::parameters(),
            Kind::NoteEcho => crate::devices::note_echo::parameters(),
            Kind::Modulation => crate::devices::modulation::parameters(),
            Kind::Reverb => crate::devices::reverb::parameters(),
            Kind::Saturator => crate::devices::saturator::parameters(),
            Kind::Deesser => crate::devices::deesser::parameters(),
            Kind::Gate => crate::devices::gate::parameters(),
            Kind::ChannelStrip => crate::devices::channel_strip::parameters(),
            Kind::Guitar => crate::devices::guitar::parameters(),
            Kind::Echo => crate::devices::delay::parameters(),
            Kind::Synth => crate::devices::synth::parameters(),
            Kind::LatencyProbe => vec![ParameterInfo {
                stepped: true,
                automatable: false,
                ..p(0, "Latency", 0.0, 48_000.0, 256.0, Samples)
            }],
            Kind::Eq => crate::eq::parameters(),
            Kind::ProgramEq => crate::program_eq::parameters(),
            Kind::Container => Vec::new(),
            Kind::HardwareInsert => crate::devices::hardware_insert::parameters(),
        }
    }

    /// Values the processor publishes through the tap.
    fn tap_values(self) -> Option<usize> {
        match self {
            Kind::Eq => Some(crate::eq::TAP_VALUES),
            Kind::Compressor => Some(crate::devices::compressor::TAP_VALUES),
            Kind::Limiter => Some(crate::devices::limiter::TAP_VALUES),
            Kind::Drums => Some(crate::devices::drums::TAP_VALUES),
            Kind::Sampler => Some(crate::devices::sampler::TAP_VALUES),
            Kind::Synth => Some(crate::devices::synth::TAP_VALUES),
            Kind::Tuner => Some(crate::devices::tuner::TAP_VALUES),
            Kind::Arpeggiator => Some(crate::devices::arpeggiator::TAP_VALUES),
            Kind::Chord => Some(crate::devices::chord::TAP_VALUES),
            Kind::Scale => Some(crate::devices::scale::TAP_VALUES),
            Kind::NoteEcho => Some(crate::devices::note_echo::TAP_VALUES),
            Kind::Modulation => Some(crate::devices::modulation::TAP_VALUES),
            Kind::Reverb => Some(crate::devices::reverb::TAP_VALUES),
            Kind::Echo => Some(crate::devices::delay::TAP_VALUES),
            Kind::Gain => Some(crate::devices::utility::TAP_VALUES),
            Kind::Saturator => Some(crate::devices::saturator::TAP_VALUES),
            Kind::Deesser => Some(crate::devices::deesser::TAP_VALUES),
            Kind::Gate => Some(crate::devices::gate::TAP_VALUES),
            Kind::ChannelStrip => Some(crate::devices::channel_strip::TAP_VALUES),
            Kind::Guitar => Some(crate::devices::guitar::TAP_VALUES),
            Kind::ProgramEq => Some(0),
            // A tap for its live parameters (the engine's send reads them).
            Kind::HardwareInsert => Some(0),
            _ => None,
        }
    }
}

/// How long a new device block must hold before the buffered devices size
/// their buffers by it (see `BuiltinInstance::sized_block`).
const BLOCK_SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

/// Control-side instance shared by all built-ins.
pub struct BuiltinInstance {
    kind: Kind,
    channels: usize,
    realtime: bool,
    /// Frames per device callback (0: unknown).
    device_block: usize,
    /// The device block the buffered devices (preamps, the Guitar Station)
    /// size their buffers by: it follows `device_block` once that has held
    /// for [`BLOCK_SETTLE`], so a cycle that changes for a moment (another
    /// client asking the audio server for a longer one) does not change
    /// their latency and restart them mid-song. Seen differing since.
    sized_block: usize,
    block_differs: Option<std::time::Instant>,
    /// When the Guitar Station last wrote its trace.
    traced: Option<std::time::Instant>,
    descriptor: PluginDescriptor,
    params: ParamValues,
    tap: Option<Arc<AnalysisTap>>,
    /// The latency and processor shape last reported (a change asks for a
    /// restart).
    reported: Option<(u32, u64)>,
    /// Restarts for a new processor shape so far: the instance's
    /// `activation`, so the engine keeps the new processor instead of
    /// adopting the old one into the rebuilt graph (the latency, the
    /// node's other identity, does not change with the shape).
    reshaped: u64,
    /// The program last selected.
    program: Option<usize>,
    /// The sample rate of the last processor (latencies in samples depend
    /// on it).
    rate: f64,
    /// The samplers' samples.
    samples: Option<crate::devices::samples::SampleHost>,
}

impl PluginInstance for BuiltinInstance {
    fn configure_channels(&mut self, channels: usize) {
        self.channels = channels;
    }
    fn configure_realtime(&mut self, realtime: bool) {
        self.realtime = realtime;
    }
    fn configure_device_block(&mut self, frames: usize) {
        self.device_block = frames;
        if self.sized_block == 0 {
            // Known now: no change to wait out.
            self.sized_block = frames;
        }
    }
    fn output_bus_names(&mut self) -> Vec<String> {
        match self.kind {
            Kind::Drums => crate::devices::drums::output_bus_names(),
            Kind::Guitar => crate::devices::guitar::output_bus_names(),
            _ => Vec::new(),
        }
    }

    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        self.params.infos()
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        self.params.get_by_id(id)
    }

    fn modulatable(&self, id: ParameterId) -> bool {
        // Continuous parameters (a mode or a switch does not glide).
        self.params
            .infos()
            .iter()
            .any(|p| p.id == id && p.automatable && !p.stepped)
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
            Kind::Drums => crate::devices::drums::format(id, value),
            Kind::Sampler => crate::devices::sampler::format(id, value),
            Kind::Synth => crate::devices::synth::format(id, value),
            Kind::Tuner => crate::devices::tuner::format(id, value),
            Kind::Arpeggiator => crate::devices::arpeggiator::format(id, value),
            Kind::Chord => crate::devices::chord::format(id, value),
            Kind::Scale => crate::devices::scale::format(id, value),
            Kind::NoteEcho => crate::devices::note_echo::format(id, value),
            Kind::Modulation => crate::devices::modulation::format(id, value),
            Kind::Reverb => crate::devices::reverb::format(id, value),
            Kind::Echo => crate::devices::delay::format(id, value),
            Kind::Gain => crate::devices::utility::format(id, value),
            Kind::HardwareInsert => crate::devices::hardware_insert::format(id, value),
            Kind::Saturator => crate::devices::saturator::format(id, value),
            Kind::Deesser => crate::devices::deesser::format(id, value),
            Kind::Gate => crate::devices::gate::format(id, value),
            Kind::ChannelStrip => crate::devices::channel_strip::format(id, value),
            Kind::Guitar => crate::devices::guitar::format(id, value),
            _ => None,
        }
    }

    fn tap(&self) -> Option<Arc<AnalysisTap>> {
        self.tap.clone()
    }

    fn activation(&self) -> u64 {
        self.reshaped
    }

    fn programs(&self) -> Vec<String> {
        crate::presets::factory_presets(&self.descriptor.id)
            .into_iter()
            .map(|p| p.name.to_string())
            .collect()
    }

    fn program_groups(&self) -> Vec<String> {
        let presets = crate::presets::factory_presets(&self.descriptor.id);
        if presets.iter().all(|p| p.group.is_none()) {
            return Vec::new();
        }
        presets
            .into_iter()
            .map(|p| p.group.unwrap_or_default().to_string())
            .collect()
    }

    fn current_program(&self) -> Option<usize> {
        self.program
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        let values = crate::presets::factory_preset_values(&self.descriptor.id, index)
            .ok_or_else(|| PluginError::Failed(format!("no program {index}")))?;
        for (param, value) in values {
            self.params.set_by_id(param, value)?;
        }
        self.program = Some(index);
        Ok(())
    }

    fn poll(&mut self) -> crate::PluginPoll {
        // The EQ's phase mode, resolution and spectral bands change its
        // latency: the graph must be rebuilt (with a processor for the new
        // mode).
        if let Some(h) = &mut self.samples {
            // Samples the processor kept us from swapping in.
            h.flush();
        }
        // The samplers' Keep Length needs a processor with stretchers.
        let shape = match self.kind {
            Kind::Sampler => u64::from(
                self.params
                    .get(crate::devices::sampler::id::PITCH_MODE as usize)
                    >= 0.5,
            ),
            Kind::Drums => u64::from(crate::devices::drums::keeps_length(&self.params)),
            // Its channels and round trip are the graph's (send, return and
            // the return's latency): built again when they change.
            Kind::HardwareInsert => {
                let (send, ret, _, trip) = crate::devices::hardware_insert::routing(&self.params);
                u64::from(send) | (u64::from(ret) << 8) | (u64::from(trip) << 16)
            }
            _ => 0,
        };
        if self.device_block == self.sized_block {
            self.block_differs = None;
        } else {
            let since = *self
                .block_differs
                .get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= BLOCK_SETTLE {
                self.sized_block = self.device_block;
                self.block_differs = None;
            }
        }
        let now = (self.latency_samples(), shape);
        let restart = self.reported.is_some_and(|r| r != now);
        if self.reported.is_some_and(|r| r.1 != shape) {
            self.reshaped += 1;
        }
        if self.kind == Kind::Guitar && crate::devices::guitar::tracing_on() {
            if restart {
                tracing::info!(
                    "guitar: restart: latency {:?} -> {} (device block {}, sized {})",
                    self.reported.map(|r| r.0),
                    now.0,
                    self.device_block,
                    self.sized_block
                );
            }
            if self
                .traced
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1))
            {
                self.traced = Some(std::time::Instant::now());
                if let Some(tap) = &self.tap {
                    let pedals = crate::devices::guitar::pedal_count(&self.params);
                    tracing::info!(
                        "guitar: block {} (sized {}) {}",
                        self.device_block,
                        self.sized_block,
                        crate::devices::guitar::trace_line(tap, pedals)
                    );
                }
            }
        }
        self.reported = Some(now);
        crate::PluginPoll {
            restart,
            ..crate::PluginPoll::default()
        }
    }

    fn latency_samples(&self) -> u32 {
        match self.kind {
            Kind::Preamp(_) => {
                faderframe_circuit::preamp::Preamp::latency()
                    + crate::devices::preamp::buffer_delay(self.sized_block) as u32
            }
            Kind::Guitar => crate::devices::guitar::latency(&self.params, self.sized_block),
            Kind::LatencyProbe => self.params.get(0).max(0.0) as u32,
            Kind::ProgramEq => crate::program_eq::LATENCY,
            Kind::Eq => crate::eq::latency(&self.params),
            Kind::Compressor => crate::devices::compressor::latency(&self.params, self.rate),
            Kind::Limiter => crate::devices::limiter::latency(&self.params, self.rate),
            Kind::Tuner => crate::devices::tuner::latency(&self.params, self.rate),
            Kind::Arpeggiator => crate::devices::arpeggiator::latency(&self.params, self.rate),
            Kind::Chord => crate::devices::chord::latency(&self.params, self.rate),
            Kind::Scale => crate::devices::scale::latency(&self.params, self.rate),
            Kind::NoteEcho => crate::devices::note_echo::latency(&self.params, self.rate),
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
            Kind::Preamp(_) => TailLength::Samples(48_000),
            // A spring tank's tail and a power stage's recovery.
            Kind::Guitar => TailLength::Samples(96_000),
            Kind::Echo | Kind::Reverb => TailLength::Infinite,
            Kind::Modulation => TailLength::Samples(4_096),
            Kind::Synth => TailLength::Samples(48_000 * 5),
            Kind::Sampler | Kind::Drums => TailLength::Samples(48_000 * 20),
            Kind::Gain
            | Kind::Compressor
            | Kind::Limiter
            | Kind::Gate
            | Kind::ChannelStrip
            | Kind::Deesser
            | Kind::Saturator
            | Kind::Tuner
            | Kind::Container
            | Kind::HardwareInsert => TailLength::None,
            // Notes still due: a held arpeggio's last steps, strums, echoes.
            Kind::Arpeggiator | Kind::Chord | Kind::Scale => TailLength::Samples(48_000 * 2),
            Kind::NoteEcho => TailLength::Samples(48_000 * 40),
            // The longest ring of a resonant cut near 10 Hz.
            Kind::Eq => TailLength::Samples(48_000),
            Kind::ProgramEq => TailLength::Samples(24_000),
            Kind::LatencyProbe => TailLength::Samples(self.latency_samples()),
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(match &self.samples {
            Some(h) => crate::devices::samples::pack(&self.params.save(), &h.doc),
            None => self.params.save(),
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        match crate::devices::samples::unpack(data) {
            Some((params, doc)) => {
                self.params.load(params)?;
                if let Some(h) = &mut self.samples {
                    h.set_doc(doc, self.tap.as_deref());
                }
                Ok(())
            }
            None => self.params.load(data),
        }?;
        // State files store the sound, not a program index. An undo or
        // user preset must not keep the checkmark of a later selection.
        self.program = None;
        Ok(())
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
        let inner: Box<dyn PluginProcessor> = match self.kind {
            Kind::Preamp(i) => Box::new(crate::devices::preamp::BufferedPreampProcessor::new(
                i,
                params,
                config,
                self.channels,
                self.realtime,
                crate::devices::preamp::buffer_delay(self.sized_block),
            )?),
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
            Kind::Drums => Box::new(crate::devices::drums::DrumsProcessor::new(
                params,
                self.tap.clone(),
                config,
                self.samples
                    .as_ref()
                    .map_or_else(crate::devices::samples::empty, |h| Arc::clone(&h.shared)),
            )),
            Kind::Sampler => Box::new(crate::devices::sampler::SamplerProcessor::new(
                params,
                self.tap.clone(),
                config,
                self.samples
                    .as_ref()
                    .map_or_else(crate::devices::samples::empty, |h| Arc::clone(&h.shared)),
            )),
            Kind::Tuner => Box::new(crate::devices::tuner::TunerProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Arpeggiator => Box::new(crate::devices::arpeggiator::ArpeggiatorProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Chord => Box::new(crate::devices::chord::ChordProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Scale => Box::new(crate::devices::scale::ScaleProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::NoteEcho => Box::new(crate::devices::note_echo::NoteEchoProcessor::new(
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
            Kind::ChannelStrip => Box::new(
                crate::devices::channel_strip::ChannelStripProcessor::new(params, tap()?, config),
            ),
            Kind::Guitar => {
                let started = std::time::Instant::now();
                let p = crate::devices::guitar::GuitarProcessor::new(
                    params,
                    self.tap.clone(),
                    config,
                    self.channels,
                    self.realtime,
                    self.sized_block,
                )?;
                if crate::devices::guitar::tracing_on() {
                    tracing::info!(
                        "guitar: built a processor in {:.1} ms: {} channels, {} pedals, {}, block {} (sized {})",
                        started.elapsed().as_secs_f64() * 1e3,
                        self.channels,
                        p.pedals(),
                        if self.realtime {
                            "live"
                        } else {
                            "inline (rendered ahead or offline)"
                        },
                        self.device_block,
                        self.sized_block
                    );
                }
                Box::new(p)
            }
            Kind::Echo => Box::new(crate::devices::delay::DelayProcessor::new(
                params,
                tap()?,
                config,
            )),
            Kind::Synth => Box::new(crate::devices::synth::SynthProcessor::new(
                params,
                self.tap.clone(),
                config,
            )),
            Kind::LatencyProbe => Box::new(latency::LatencyProcessor::new(self.latency_samples())),
            // Never in a graph (it is built from the chains): passes audio.
            Kind::Container => Box::new(latency::LatencyProcessor::new(0)),
            Kind::HardwareInsert => Box::new(
                crate::devices::hardware_insert::HardwareInsertProcessor::new(params, config),
            ),
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
        };
        Ok(Box::new(Modulated {
            params: self.params.clone(),
            inner,
            set: [usize::MAX; MAX_MODULATED],
            count: 0,
        }))
    }
}

/// Most parameters of a built-in modulated at once.
const MAX_MODULATED: usize = 64;

/// A built-in processor with modulation: the block's offsets go into the
/// shared values' modulation (cleared when a parameter's goes away), so
/// the processor reads them as it reads any value.
struct Modulated {
    params: ParamValues,
    inner: Box<dyn PluginProcessor>,
    /// Indices given an offset last block.
    set: [usize; MAX_MODULATED],
    count: usize,
}

impl PluginProcessor for Modulated {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if self.count > 0 || !ctx.param_mods.is_empty() {
            for &i in &self.set[..self.count] {
                self.params.set_mod(i, 0.0);
            }
            self.count = 0;
            for m in ctx.param_mods {
                if self.count == MAX_MODULATED {
                    break;
                }
                if let Some(i) = self.params.index(m.parameter) {
                    self.params.set_mod(i, m.share);
                    self.set[self.count] = i;
                    self.count += 1;
                }
            }
        }
        self.inner.process(ctx, io)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn take_underruns(&mut self) -> u64 {
        self.inner.take_underruns()
    }

    fn preferred_block_size(&self) -> usize {
        self.inner.preferred_block_size()
    }

    fn set_callback_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.inner.set_callback_deadline(deadline);
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
        let samples = matches!(kind, Kind::Sampler | Kind::Drums).then(|| {
            let mut host = crate::devices::samples::SampleHost::default();
            host.set_doc(
                crate::devices::samples::SampleDoc::default(),
                tap.as_deref(),
            );
            host
        });
        Ok(Box::new(BuiltinInstance {
            channels: 2,
            realtime: false,
            device_block: 0,
            sized_block: 0,
            block_differs: None,
            traced: None,
            kind,
            descriptor: kind.descriptor(),
            params,
            tap,
            reported: None,
            reshaped: 0,
            program: None,
            rate: 48_000.0,
            samples,
        }))
    }
}

#[cfg(test)]
mod tests;
