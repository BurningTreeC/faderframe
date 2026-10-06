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
//! Formats: [`builtin`] here; CLAP (`faderframe-plugin-clap`) and VST3
//! (`faderframe-plugin-vst3`) in their own crates. Third-party plugins are
//! untrusted: scanning runs in helper processes ([`scan`]).

#![forbid(unsafe_code)]

pub mod builtin;
pub mod devices;
pub mod dsp;
pub mod emulated;
pub mod eq;
pub mod harmony;
mod params;
pub mod presets;
pub mod program_eq;
pub mod scan;
pub mod tap;

pub use harmony::{Harmony, NO_HARMONY};
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
    /// Channel input stage, selectable only through the dedicated mixer slot.
    Preamp,
    Effect,
    Instrument,
    Analyzer,
    Utility,
    /// Notes in, notes out (no audio): an arpeggiator, a chord or scale
    /// device; it plays before an instrument.
    MidiEffect,
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
    /// The host feeds the plugin's second audio input (its sidechain) as
    /// the processor's second graph input.
    pub sidechain: bool,
    /// Process in 64-bit floating point where the plugin can (the graph's
    /// audio stays 32-bit; the format layer converts around the plugin).
    pub double_precision: bool,
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
    /// The project's key and chord track (built-in MIDI effects follow
    /// them; [`NO_HARMONY`] outside a project).
    pub harmony: &'a Harmony,
    /// Modulation for this block: offsets on parameters' values that
    /// leave the values themselves as they are. A parameter not listed has
    /// none.
    pub param_mods: &'a [ParamMod],
    /// Modulation of single voices (CLAP's polyphonic modulation), for
    /// parameters that take it per note: addressed by the note's channel
    /// and key, in time order (a new note's after its note-on).
    pub note_mods: &'a [NoteParamMod],
}

/// One voice's modulation of a parameter (plain units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteParamMod {
    pub parameter: ParameterId,
    pub channel: u8,
    pub key: u8,
    pub amount: f32,
    pub sample_offset: u32,
}

/// A parameter's modulation for a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamMod {
    pub parameter: ParameterId,
    /// Share of the parameter's whole range (−1..1). Built-ins apply it in
    /// their own scale (a frequency's is logarithmic).
    pub share: f32,
    /// The same in plain units (`share × (max − min)`), for plugins that
    /// add it to the value (CLAP).
    pub amount: f32,
}

/// A parameter moved in the plugin's own editor (plain units), with the
/// gesture around it when the plugin reports one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditorEdit {
    Begin(ParameterId),
    Value(ParameterId, f64),
    End(ParameterId),
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

/// The windowing system an editor runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowApi {
    /// X11 (Linux/BSD; through XWayland on Wayland desktops).
    X11,
    /// Win32 `HWND`s.
    Win32,
    /// Cocoa `NSView`s (macOS).
    Cocoa,
}

impl WindowApi {
    /// The one this platform's editors use.
    pub const NATIVE: WindowApi = if cfg!(windows) {
        WindowApi::Win32
    } else if cfg!(target_os = "macos") {
        WindowApi::Cocoa
    } else {
        WindowApi::X11
    };
}

/// A host window an editor embeds into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentWindow {
    pub api: WindowApi,
    /// The X11 window id, the `HWND` or the `NSView` pointer.
    pub handle: u64,
}

/// A plugin's own editor GUI (control/UI thread).
pub trait PluginEditor {
    /// Can embed into a host window of `api`.
    fn can_embed(&mut self, api: WindowApi) -> bool;
    /// Can open its own top-level window of `api`.
    fn can_float(&mut self, api: WindowApi) -> bool;
    /// Create the embedded editor; returns its size in pixels (points on
    /// macOS). `scale` is the display's scale factor (Windows editors draw
    /// at it; X11 and Cocoa editors take it from the system). The host then
    /// creates a parent window of that size and calls
    /// [`attach`](Self::attach).
    fn open_embedded(&mut self, api: WindowApi, scale: f64) -> Result<(u32, u32), PluginError>;
    /// Put the created editor into `parent` and show it.
    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError>;
    /// Open as the plugin's own window.
    fn open_floating(&mut self, api: WindowApi, title: &str) -> Result<(), PluginError>;
    fn close(&mut self);
    /// Bring a floating editor's window to the front.
    fn raise(&mut self) {}
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
    /// Actual graph output width, supplied off the audio thread before activation.
    fn configure_channels(&mut self, _channels: usize) {}

    /// Whether processing has a live deadline. Offline/ahead graphs can run
    /// buffered built-ins synchronously, with identical reported latency.
    fn configure_realtime(&mut self, _realtime: bool) {}
    fn descriptor(&self) -> &PluginDescriptor;
    fn parameters(&self) -> &[ParameterInfo];
    fn parameter(&mut self, id: ParameterId) -> Option<f64>;
    /// Whether the parameter takes modulation that leaves its value as it
    /// is ([`PluginProcessContext::param_mods`]).
    fn modulatable(&self, _id: ParameterId) -> bool {
        false
    }
    /// Whether the parameter takes modulation per note (a voice's own;
    /// CLAP's polyphonic modulation).
    fn modulatable_per_note(&self, _id: ParameterId) -> bool {
        false
    }
    /// Set a parameter from the UI; reaches the processor without blocking.
    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError>;
    fn latency_samples(&self) -> u32;
    /// Preset files in the plugin format's own preset folders (e.g. VST3
    /// `.vstpreset`).
    fn preset_files(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
    /// The state ([`Self::load_state`] format) of one of those files.
    fn state_from_preset_file(&self, _data: &[u8]) -> Result<Vec<u8>, PluginError> {
        Err(PluginError::InvalidState(
            "this plugin has no preset files".into(),
        ))
    }
    /// The plugin's own programs (a VST3 program list), by name. Selecting
    /// one makes the plugin load its settings.
    fn programs(&self) -> Vec<String> {
        Vec::new()
    }
    /// The program selected now, as the plugin reports it.
    fn current_program(&self) -> Option<usize> {
        None
    }
    /// Switch to program `index`; it takes effect with the processor's next
    /// block (see [`Self::changes_pending`]).
    fn select_program(&mut self, _index: usize) -> Result<(), PluginError> {
        Err(PluginError::Failed("the plugin has no programs".into()))
    }
    /// Changes sent to the processor that it has not taken yet (its state
    /// does not show them).
    fn changes_pending(&self) -> bool {
        false
    }
    /// Parameter moves made in the plugin's own editor since the last call
    /// (for automation writing).
    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        Vec::new()
    }
    /// The per-note expressions the plugin accepts (`None`: it does not say
    /// — CLAP plugins ignore what they do not support).
    fn note_expressions(&self) -> Option<Vec<faderframe_midi::NoteExpressionKind>> {
        None
    }
    /// Runs in a helper process (`faderframe-plugin-sandbox`): a crash
    /// costs only the instance, and its state is worth saving often.
    fn sandboxed(&self) -> bool {
        false
    }
    /// What a built-in plugin's own editor reads (live parameters,
    /// analyser audio, meters); `None` for other plugins.
    fn tap(&self) -> Option<std::sync::Arc<tap::AnalysisTap>> {
        None
    }
    /// Counts (re)activations. Processors of different activations are not
    /// interchangeable: after a restart the old one is dead, so the engine
    /// must not keep it in place of the new one (it is part of the node's
    /// identity). Instances whose processors are independent keep 0.
    fn activation(&self) -> u64 {
        0
    }
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
    /// Newly concealed blocks from an asynchronous processor. The engine
    /// includes these in its xrun counter even when the callback returned fast.
    fn take_underruns(&mut self) -> u64 {
        0
    }
    /// Advisory graph quantum for asynchronous processors; not added latency.
    fn preferred_block_size(&self) -> usize {
        usize::MAX
    }
    /// Whole device callback's bounded work deadline, shared by all chunks.
    fn set_callback_deadline(&mut self, _deadline: Option<std::time::Instant>) {}
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
