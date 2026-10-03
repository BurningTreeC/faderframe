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
    /// A fraction (0–1) shown as a percentage.
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

/// What a plugin asked the host for since the last poll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PluginPoll {
    /// The plugin must be re-activated (latency or ports changed): rebuild
    /// the graph.
    pub restart: bool,
    pub params_changed: bool,
    pub state_dirty: bool,
}

impl PluginPoll {
    pub fn merge(self, o: PluginPoll) -> PluginPoll {
        PluginPoll {
            restart: self.restart || o.restart,
            params_changed: self.params_changed || o.params_changed,
            state_dirty: self.state_dirty || o.state_dirty,
        }
    }
}

/// A file descriptor a plugin wants watched (its GUI's event loop).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginFd {
    pub fd: i32,
    pub read: bool,
    pub write: bool,
    pub error: bool,
}

/// Event sources a plugin registered: file descriptors and timers
/// (`(id, period in ms)`), serviced by the host's main loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginEventSources {
    pub fds: Vec<PluginFd>,
    pub timers: Vec<(u32, u32)>,
}

/// What a plugin's editor asked the host for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorRequests {
    pub resize: Option<(u32, u32)>,
    pub show: bool,
    pub hide: bool,
    /// The editor window was closed by the plugin (or its connection lost).
    pub closed: bool,
}

/// A plugin's own editor GUI (control/UI thread).
pub trait PluginEditor {
    /// Can embed into an X11 window (Linux/BSD).
    fn can_embed_x11(&mut self) -> bool;
    /// Can open its own top-level window.
    fn can_float(&mut self) -> bool;
    /// Create the embedded (X11) editor; returns its size in pixels. The
    /// host then creates a parent window of that size and calls
    /// [`attach_x11`](Self::attach_x11).
    fn open_embedded(&mut self) -> Result<(u32, u32), PluginError>;
    /// Put the created editor into X11 window `parent` and show it.
    fn attach_x11(&mut self, parent: u64) -> Result<(), PluginError>;
    /// Open as the plugin's own window.
    fn open_floating(&mut self, title: &str) -> Result<(), PluginError>;
    fn close(&mut self);
    fn is_open(&self) -> bool;
    fn can_resize(&mut self) -> bool;
    /// Ask for a new size; returns the size actually applied.
    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)>;
    fn take_requests(&mut self) -> EditorRequests;
}

/// Control-thread side of a plugin instance.
///
/// Not `Send`: formats such as CLAP require every main-thread call of an
/// instance to come from the thread that created it, so instances stay on
/// the thread that owns the engine controller (the UI thread, or a render
/// thread for its own instances).
pub trait PluginInstance {
    fn descriptor(&self) -> &PluginDescriptor;
    fn parameters(&self) -> &[ParameterInfo];
    fn parameter(&mut self, id: ParameterId) -> Option<f64>;
    /// Set a parameter from the UI; reaches the processor without blocking.
    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError>;
    fn latency_samples(&self) -> u32;
    fn tail(&self) -> TailLength;
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError>;
    /// Handle plugin requests (main-thread callbacks, restarts); call
    /// regularly on the control thread.
    fn poll(&mut self) -> PluginPoll {
        PluginPoll::default()
    }

    /// The plugin's own editor, if it has one.
    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        None
    }

    /// The plugin's own text for a parameter value (e.g. "1.2 kHz").
    fn format_parameter(&mut self, _id: ParameterId, _value: f64) -> Option<String> {
        None
    }

    /// File descriptors and timers the plugin registered.
    fn event_sources(&self) -> PluginEventSources {
        PluginEventSources::default()
    }

    /// A registered file descriptor is ready.
    fn on_fd(&mut self, _fd: PluginFd) {}

    /// A registered timer fired.
    fn on_timer(&mut self, _id: u32) {}

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
pub trait PluginFactory {
    fn format(&self) -> PluginFormat;
    fn scan(&self) -> Vec<PluginDescriptor>;
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError>;
}

/// All available plugin formats.
pub struct PluginRegistry {
    factories: Vec<Box<dyn PluginFactory>>,
}

/// Builds the process-wide default registry (set once by the application
/// to add format hosts such as CLAP; engines use it for their plugin host).
static DEFAULT_REGISTRY: std::sync::OnceLock<fn() -> PluginRegistry> = std::sync::OnceLock::new();

/// Install the builder of [`PluginRegistry::default`] (first call wins).
pub fn set_default_registry(builder: fn() -> PluginRegistry) {
    let _ = DEFAULT_REGISTRY.set(builder);
}

impl Default for PluginRegistry {
    fn default() -> Self {
        match DEFAULT_REGISTRY.get() {
            Some(build) => build(),
            None => Self::with_builtins(),
        }
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
