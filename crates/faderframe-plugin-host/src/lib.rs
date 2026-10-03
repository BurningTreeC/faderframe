//! Format-independent plugin hosting.
//!
//! The DAW core never knows whether a plugin is CLAP, VST3, an Audio Unit
//! or built in. Plugins are split the way the real formats split them:
//!
//! * [`PluginInstance`] — the *control-thread* side: descriptor,
//!   parameters, state save/load, latency/tail reporting and activation.
//! * [`PluginProcessor`] — the *audio-thread* side created by activation;
//!   processes one block with transport info and sample-accurate parameter
//!   events.
//!
//! Neither trait assumes the plugin lives in this process. A sandboxed
//! plugin is represented by proxy implementations that forward control calls
//! over IPC and exchange audio through shared memory with bounded,
//! lock-free queues; the engine is unaffected.
//!
//! Formats: [`builtin`] is implemented. CLAP (via `clack-host`) is the next
//! format; VST3 and AU follow. Third-party plugins are untrusted: scanning
//! and (optionally) processing are meant to run in helper processes.

#![forbid(unsafe_code)]

pub mod builtin;
mod params;

pub use params::ParamValues;

use faderframe_audio_graph::NodeIo;
use faderframe_automation::ParameterEvent;
use faderframe_core::ParameterId;
use faderframe_transport::TransportInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginFormat {
    Builtin,
    Clap,
    Vst3,
    AudioUnit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginCategory {
    Effect,
    Instrument,
    Analyzer,
    Utility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPortInfo {
    pub channels: u16,
    pub is_main: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginDescriptor {
    pub format: PluginFormat,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub category: PluginCategory,
    pub audio_inputs: Vec<AudioPortInfo>,
    pub audio_outputs: Vec<AudioPortInfo>,
    pub note_inputs: u16,
    pub note_outputs: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterUnit {
    None,
    Decibels,
    Milliseconds,
    Hertz,
    Percent,
    Samples,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParameterInfo {
    pub id: ParameterId,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: ParameterUnit,
    pub automatable: bool,
    pub stepped: bool,
}

impl ParameterInfo {
    pub fn clamp(&self, v: f64) -> f64 {
        if v.is_nan() {
            self.default
        } else {
            v.clamp(self.min, self.max)
        }
    }
}

/// How long a plugin keeps producing output after its input goes silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailLength {
    None,
    Samples(u32),
    Infinite,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessConfig {
    pub sample_rate: f64,
    pub max_block_size: u32,
}

/// Outcome of one `process` call. Kept `Copy` and allocation-free; detailed
/// errors are reported out of band (control side) by the format layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Output is valid; keep calling.
    Continue,
    /// Output is valid and silent from now on until new input arrives.
    Sleep,
    /// The plugin failed; the host bypasses it and reports on the control side.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("plugin format not supported yet: {0:?}")]
    UnsupportedFormat(PluginFormat),
    #[error("unknown parameter {0:?}")]
    UnknownParameter(ParameterId),
    #[error("invalid plugin state: {0}")]
    InvalidState(String),
    #[error("plugin failed: {0}")]
    Failed(String),
}

/// Per-block data handed to a processor.
pub struct PluginProcessContext<'a> {
    pub transport: &'a TransportInfo,
    /// Sample-accurate parameter changes for this block, sorted by offset.
    pub param_events: &'a [ParameterEvent],
}

/// Control-thread side of a plugin instance.
pub trait PluginInstance: Send {
    fn descriptor(&self) -> &PluginDescriptor;
    fn parameters(&self) -> &[ParameterInfo];
    fn parameter(&self, id: ParameterId) -> Option<f64>;
    /// Set a parameter from the UI; reaches the processor without blocking.
    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError>;
    fn latency_samples(&self) -> u32;
    fn tail(&self) -> TailLength;
    fn save_state(&self) -> Result<Vec<u8>, PluginError>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError>;
    /// Activate for processing and return the audio-thread half.
    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError>;
}

/// Audio-thread side of a plugin instance. Must be realtime-safe.
pub trait PluginProcessor: Send {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus;
    /// Clear tails, voices and delay lines.
    fn reset(&mut self);
}

/// Something that can list and instantiate plugins of one format.
pub trait PluginFactory: Send {
    fn format(&self) -> PluginFormat;
    fn scan(&self) -> Vec<PluginDescriptor>;
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError>;
}

/// All available plugin formats.
pub struct PluginRegistry {
    factories: Vec<Box<dyn PluginFactory>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl PluginRegistry {
    pub fn empty() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    pub fn with_builtins() -> Self {
        let mut r = Self::empty();
        r.add_factory(Box::new(builtin::BuiltinFactory));
        r
    }

    pub fn add_factory(&mut self, factory: Box<dyn PluginFactory>) {
        self.factories.push(factory);
    }

    pub fn scan(&self) -> Vec<PluginDescriptor> {
        self.factories.iter().flat_map(|f| f.scan()).collect()
    }

    pub fn instantiate(
        &self,
        format: PluginFormat,
        id: &str,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        self.factories
            .iter()
            .find(|f| f.format() == format)
            .ok_or(PluginError::UnsupportedFormat(format))?
            .instantiate(id)
    }
}
