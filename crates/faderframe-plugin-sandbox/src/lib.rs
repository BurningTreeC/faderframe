//! Sandboxed plugins: each third-party plugin instance runs in a helper
//! process of its own (`faderframe --plugin-sandbox`), so a plugin that
//! crashes or hangs takes only that process down — the session keeps
//! playing, the plugin's track carries on dry, and the plugin can be
//! reloaded.
//!
//! The engine sees ordinary [`PluginInstance`]s and [`PluginProcessor`]s
//! (`faderframe_plugin_host`): [`SandboxedFactory`] wraps a format's factory
//! and, while sandboxing is on, instantiates through a helper instead.
//!
//! * **Control** — instantiate, parameters, state, presets, the editor, a
//!   poll per UI tick — goes over a socket (Windows: a named pipe) as
//!   framed messages
//!   ([`wire`]). What the UI reads often (parameter values, editor requests,
//!   edits made in the plugin's editor) arrives with the poll and is read
//!   from a cache.
//! * **Audio** goes through shared memory, one block of it per activation
//!   (`shm`): the host's audio thread writes the block's inputs, MIDI and
//!   note expressions, parameter events and transport, wakes the helper's
//!   audio thread (a byte through a pipe; Windows: an event) and waits for
//!   its answer, then reads the outputs. Everything the helper writes is validated
//!   (counts, event kinds, finite samples) — the host never trusts it.
//! * **Failure**: a helper that dies is noticed at once (its pipe closes;
//!   Windows: its process handle is signalled); one that does not answer within [`BLOCK_TIMEOUT`] is given up. Either way
//!   the processor reports [`ProcessStatus::Error`] (the engine bypasses
//!   the plugin) and the instance answers from its cache until it is
//!   reloaded.
//! * **Editors** run in the helper. On X11 and Windows they embed into
//!   FaderFrame's own editor window across processes (window ids and
//!   handles are global); on macOS, where views cannot cross processes,
//!   the helper shows them in a window of its own. Resize and close
//!   requests come back with the poll.
//!
//! [`PluginInstance`]: faderframe_plugin_host::PluginInstance
//! [`PluginProcessor`]: faderframe_plugin_host::PluginProcessor
//! [`ProcessStatus::Error`]: faderframe_plugin_host::ProcessStatus::Error

pub mod wire;

pub mod child;
mod host;
#[cfg(target_os = "macos")]
mod mac;
mod shm;
mod sys;

use faderframe_plugin_host::{PluginDescriptor, PluginError, PluginFactory, PluginFormat};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Sandboxing works on this platform.
pub const AVAILABLE: bool = cfg!(any(target_os = "linux", target_os = "macos", windows));

/// How long the host's audio thread waits for a helper's block before it
/// gives the plugin up.
pub const BLOCK_TIMEOUT: Duration = Duration::from_millis(250);

/// How to start a helper process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub exe: PathBuf,
    /// Arguments that make `exe` a helper (the application's
    /// `--plugin-sandbox`).
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

static LAUNCHER: OnceLock<Launcher> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Set how helpers are started (first call wins).
pub fn set_launcher(launcher: Launcher) {
    let _ = LAUNCHER.set(launcher);
}

pub fn launcher() -> Option<&'static Launcher> {
    LAUNCHER.get()
}

/// Host newly instantiated plugins in helper processes (instances that
/// exist keep running where they are until they are reloaded).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Sandboxing is on, available and has a launcher.
pub fn enabled() -> bool {
    AVAILABLE && ENABLED.load(Ordering::Relaxed) && LAUNCHER.get().is_some()
}

/// A format's factory that instantiates through a helper process while
/// sandboxing is [`enabled`].
pub struct SandboxedFactory {
    inner: Box<dyn PluginFactory>,
}

impl SandboxedFactory {
    pub fn new(inner: Box<dyn PluginFactory>) -> Self {
        Self { inner }
    }
}

impl PluginFactory for SandboxedFactory {
    fn format(&self) -> PluginFormat {
        self.inner.format()
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        self.inner.scan()
    }

    fn instantiate(
        &self,
        id: &str,
    ) -> Result<Box<dyn faderframe_plugin_host::PluginInstance>, PluginError> {
        if let (true, Some(launcher)) = (enabled(), launcher()) {
            return Ok(Box::new(host::RemoteInstance::spawn(
                launcher,
                self.format(),
                id,
            )?));
        }
        self.inner.instantiate(id)
    }
}

/// Instantiate `id` of `format` in a helper started by `launcher`,
/// whatever the switch says (tests, tools).
pub fn instantiate_sandboxed(
    launcher: &Launcher,
    format: PluginFormat,
    id: &str,
) -> Result<Box<dyn faderframe_plugin_host::PluginInstance>, PluginError> {
    Ok(Box::new(host::RemoteInstance::spawn(launcher, format, id)?))
}
