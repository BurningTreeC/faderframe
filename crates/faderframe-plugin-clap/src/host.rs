//! Host callbacks (what a CLAP plugin can ask of FaderFrame).
//!
//! Requests from the plugin (restart, latency/parameter changes, main
//! thread callbacks, state changes) set flags; the control thread polls
//! them ([`crate::ClapInstance`]'s `poll`) and reacts there — never inside
//! the callback, which may arrive on any thread.

use clack_extensions::audio_ports::{
    AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl, PluginAudioPorts,
};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl, PluginGui};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{
    HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags, PluginNotePorts,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
    PluginParams,
};
#[cfg(unix)]
use clack_extensions::posix_fd::{FdFlags, HostPosixFd, HostPosixFdImpl, PluginPosixFd};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::host::HostError;
use clack_host::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

/// Plugin-side extensions. CLAP forbids querying them before `init()`;
/// they are read after `init()` returned ([`query_extensions`]) and, for
/// plugins that call the host while initialising, already from there.
#[derive(Clone, Copy, Default)]
pub struct PluginExtensions {
    pub params: Option<PluginParams>,
    pub state: Option<PluginState>,
    pub latency: Option<PluginLatency>,
    pub audio_ports: Option<PluginAudioPorts>,
    pub note_ports: Option<PluginNotePorts>,
    pub gui: Option<PluginGui>,
    pub timer: Option<PluginTimer>,
    #[cfg(unix)]
    pub posix_fd: Option<PluginPosixFd>,
    pub gain_adjustment: Option<PluginGainAdjustment>,
}

/// Thread-safe host state of one instance.
pub struct FfShared {
    main_thread: ThreadId,
    extensions: std::sync::Mutex<PluginExtensions>,
    pub restart: AtomicBool,
    pub process: AtomicBool,
    pub callback: AtomicBool,
    pub latency_changed: AtomicBool,
    pub params_changed: AtomicBool,
    pub state_dirty: AtomicBool,
    pub flush: AtomicBool,
    // Editor requests (may arrive on any thread).
    pub gui_resize: std::sync::Mutex<Option<(u32, u32)>>,
    pub gui_show: AtomicBool,
    pub gui_hide: AtomicBool,
    pub gui_closed: AtomicBool,
}

impl FfShared {
    pub fn new() -> Self {
        Self {
            main_thread: std::thread::current().id(),
            extensions: std::sync::Mutex::new(PluginExtensions::default()),
            restart: AtomicBool::new(false),
            process: AtomicBool::new(false),
            callback: AtomicBool::new(false),
            latency_changed: AtomicBool::new(false),
            params_changed: AtomicBool::new(false),
            state_dirty: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            gui_resize: std::sync::Mutex::new(None),
            gui_show: AtomicBool::new(false),
            gui_hide: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
        }
    }

    pub fn ext(&self) -> PluginExtensions {
        self.extensions.lock().map(|e| *e).unwrap_or_default()
    }

    pub fn set_extensions(&self, ext: PluginExtensions) {
        if let Ok(mut e) = self.extensions.lock() {
            *e = ext;
        }
    }

    /// Read and clear a request flag.
    pub fn take(flag: &AtomicBool) -> bool {
        flag.swap(false, Ordering::AcqRel)
    }
}

impl Default for FfShared {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> SharedHandler<'a> for FfShared {
    fn initializing(&self, instance: InitializingPluginHandle<'a>) {
        self.set_extensions(PluginExtensions {
            params: instance.get_extension(),
            state: instance.get_extension(),
            latency: instance.get_extension(),
            audio_ports: instance.get_extension(),
            note_ports: instance.get_extension(),
            gui: instance.get_extension(),
            timer: instance.get_extension(),
            #[cfg(unix)]
            posix_fd: instance.get_extension(),
            gain_adjustment: instance.get_extension(),
        });
    }

    fn request_restart(&self) {
        self.restart.store(true, Ordering::Release);
    }

    fn request_process(&self) {
        self.process.store(true, Ordering::Release);
    }

    fn request_callback(&self) {
        self.callback.store(true, Ordering::Release);
    }
}

impl HostLogImpl for FfShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        // Plugins rarely log from the audio thread; doing so is their bug.
        match severity {
            LogSeverity::Error
            | LogSeverity::Fatal
            | LogSeverity::HostMisbehaving
            | LogSeverity::PluginMisbehaving => {
                tracing::warn!("[plugin] {message}");
            }
            LogSeverity::Warning => tracing::info!("[plugin] {message}"),
            _ => tracing::debug!("[plugin] {message}"),
        }
    }
}

thread_local! {
    /// Set while this thread is inside a plugin's processing call. Offline
    /// rendering runs main-thread and audio-thread calls on one thread, so
    /// "not the main thread" is not enough to recognise the audio thread.
    static IN_AUDIO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current thread as the audio thread while alive.
pub(crate) struct AudioThreadScope(());

impl AudioThreadScope {
    #[inline]
    pub(crate) fn enter() -> Self {
        IN_AUDIO.with(|f| f.set(true));
        Self(())
    }
}

impl Drop for AudioThreadScope {
    #[inline]
    fn drop(&mut self) {
        IN_AUDIO.with(|f| f.set(false));
    }
}

impl HostThreadCheckImpl for FfShared {
    fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main_thread && !IN_AUDIO.with(|f| f.get())
    }

    fn is_audio_thread(&self) -> bool {
        IN_AUDIO.with(|f| f.get()) || std::thread::current().id() != self.main_thread
    }
}

impl HostGuiImpl for FfShared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        if let Ok(mut r) = self.gui_resize.lock() {
            *r = Some((new_size.width, new_size.height));
        }
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        self.gui_show.store(true, Ordering::Release);
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        self.gui_hide.store(true, Ordering::Release);
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.gui_closed.store(true, Ordering::Release);
    }
}

impl HostParamsImplShared for FfShared {
    fn request_flush(&self) {
        self.flush.store(true, Ordering::Release);
    }
}

/// Main-thread host state of one instance.
pub struct FfMainThread<'a> {
    pub shared: &'a FfShared,
    /// File descriptors the plugin registered (its GUI's event loop; only
    /// on Unix): (fd, read, write, error).
    pub fds: std::cell::RefCell<Vec<(i32, [bool; 3])>>,
    /// Timers: (id, period in ms).
    pub timers: std::cell::RefCell<Vec<(u32, u32)>>,
    next_timer: std::cell::Cell<u32>,
}

impl<'a> FfMainThread<'a> {
    pub fn new(shared: &'a FfShared) -> Self {
        Self {
            shared,
            fds: Default::default(),
            timers: Default::default(),
            next_timer: std::cell::Cell::new(1),
        }
    }
}

#[cfg(unix)]
fn interest(flags: FdFlags) -> [bool; 3] {
    [
        flags.contains(FdFlags::READ),
        flags.contains(FdFlags::WRITE),
        flags.contains(FdFlags::ERROR),
    ]
}

#[cfg(unix)]
impl HostPosixFdImpl for FfMainThread<'_> {
    fn register_fd(&self, fd: std::os::fd::RawFd, flags: FdFlags) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        fds.retain(|(f, _)| *f != fd);
        fds.push((fd, interest(flags)));
        Ok(())
    }

    fn modify_fd(&self, fd: std::os::fd::RawFd, flags: FdFlags) -> Result<(), HostError> {
        let mut fds = self.fds.borrow_mut();
        match fds.iter_mut().find(|(f, _)| *f == fd) {
            Some(e) => {
                e.1 = interest(flags);
                Ok(())
            }
            None => Err(HostError::Message("unknown fd")),
        }
    }

    fn unregister_fd(&self, fd: std::os::fd::RawFd) -> Result<(), HostError> {
        self.fds.borrow_mut().retain(|(f, _)| *f != fd);
        Ok(())
    }
}

impl HostTimerImpl for FfMainThread<'_> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let id = self.next_timer.get();
        self.next_timer.set(id + 1);
        // At least ~30 Hz is honoured; faster timers run at the UI rate.
        self.timers.borrow_mut().push((id, period_ms.max(1)));
        Ok(TimerId(id))
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.timers.borrow_mut().retain(|(id, _)| *id != timer_id.0);
        Ok(())
    }
}

impl<'a> MainThreadHandler<'a> for FfMainThread<'a> {}

impl HostLatencyImpl for FfMainThread<'_> {
    fn changed(&self) {
        self.shared.latency_changed.store(true, Ordering::Release);
    }
}

impl HostParamsImplMainThread for FfMainThread<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.shared.params_changed.store(true, Ordering::Release);
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for FfMainThread<'_> {
    fn mark_dirty(&self) {
        self.shared.state_dirty.store(true, Ordering::Release);
    }
}

impl HostAudioPortsImpl for FfMainThread<'_> {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        // Port changes are applied by a restart (graph rebuild).
        true
    }

    fn rescan(&self, _flags: AudioPortRescanFlags) {
        self.shared.restart.store(true, Ordering::Release);
    }
}

impl HostNotePortsImpl for FfMainThread<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {
        self.shared.restart.store(true, Ordering::Release);
    }
}

/// FaderFrame as a CLAP host.
pub struct FfHost;

impl HostHandlers for FfHost {
    type Shared<'a> = FfShared;
    type MainThread<'a> = FfMainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostThreadCheck>()
            .register::<HostLatency>()
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostGui>()
            .register::<HostTimer>();
        #[cfg(unix)]
        builder.register::<HostPosixFd>();
    }
}

pub fn host_info() -> Result<HostInfo, std::ffi::NulError> {
    HostInfo::new(
        "FaderFrame",
        "FaderFrame contributors",
        "https://github.com/BurningTreeC/faderframe",
        env!("CARGO_PKG_VERSION"),
    )
}

/// CLAP's (draft) gain-adjustment metering: the plugin reports the gain it
/// applies (dB; negative = reduction, before make-up), for a host's meter.
/// `get` is an audio-thread call, made after `process`.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_camel_case_types)]
pub struct clap_plugin_gain_adjustment_metering {
    pub get: Option<unsafe extern "C" fn(plugin: *const std::ffi::c_void) -> f64>,
}

#[derive(Copy, Clone)]
pub struct PluginGainAdjustment(
    clack_host::extensions::prelude::RawExtension<
        clack_host::extensions::prelude::PluginExtensionSide,
        clap_plugin_gain_adjustment_metering,
    >,
);

// SAFETY: the type is the extension's repr(C) struct (`get` takes the
// plugin pointer, ABI-identical to `*const clap_plugin`).
unsafe impl clack_host::extensions::prelude::Extension for PluginGainAdjustment {
    const IDENTIFIERS: &[&std::ffi::CStr] = &[c"clap.gain-adjustment-metering/0"];
    type ExtensionSide = clack_host::extensions::prelude::PluginExtensionSide;

    #[inline]
    unsafe fn from_raw(
        raw: clack_host::extensions::prelude::RawExtension<Self::ExtensionSide>,
    ) -> Self {
        // SAFETY: the caller guarantees the pointer is this extension's.
        Self(unsafe { raw.cast() })
    }
}

impl PluginGainAdjustment {
    /// The gain adjustment applied to the last sample of the last block
    /// (dB). Audio thread only.
    #[inline]
    pub fn get(
        &self,
        plugin: &clack_host::extensions::prelude::PluginAudioProcessorHandle<'_>,
    ) -> f64 {
        match plugin.use_extension(&self.0).get {
            // SAFETY: the plugin's own function for this instance, called on
            // the audio thread as the extension asks.
            Some(get) => unsafe { get(plugin.as_raw_ptr().cast()) },
            None => 0.0,
        }
    }
}

/// Every extension FaderFrame uses, queried once the plugin is initialised.
pub fn query_extensions(h: &PluginMainThreadHandle<'_>) -> PluginExtensions {
    PluginExtensions {
        params: h.get_extension(),
        state: h.get_extension(),
        latency: h.get_extension(),
        audio_ports: h.get_extension(),
        note_ports: h.get_extension(),
        gui: h.get_extension(),
        timer: h.get_extension(),
        #[cfg(unix)]
        posix_fd: h.get_extension(),
        gain_adjustment: h.get_extension(),
    }
}

/// Re-read the extensions after `init()` (some plugins, e.g. bridges, only
/// know them then).
pub fn refresh_extensions(instance: &mut PluginInstance<FfHost>) {
    let ext = query_extensions(&instance.plugin_handle());
    instance.access_shared_handler(|s| s.set_extensions(ext));
}
