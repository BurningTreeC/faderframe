//! The control-thread side of a CLAP plugin instance.

use crate::host::{FfHost, FfMainThread, FfShared, host_info};
use crate::processor::{ClapProcessor, RtProc, RtState, SharedRt};
use crate::scan::ScannedPlugin;
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiError, GuiSize, Window};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags};
use clack_extensions::timer::TimerId;
use clack_host::events::Pckn;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use faderframe_core::ParameterId;
use faderframe_plugin_host::{
    ParameterInfo, ParameterUnit, ParentWindow, PluginDescriptor, PluginError, PluginFormat,
    PluginInstance as FfInstance, PluginProcessor, ProcessConfig, TailLength, WindowApi,
};
use faderframe_realtime::TryCell;
use std::ffi::{CString, c_void};
use std::sync::Arc;

pub struct ClapInstance {
    descriptor: PluginDescriptor,
    scanned: ScannedPlugin,
    params: Vec<ParameterInfo>,
    /// Parameters that take modulation, and how single voices are
    /// addressed (if they can be).
    modulation: Vec<(ParameterId, PerNote)>,
    rt: Option<SharedRt>,
    config: Option<ProcessConfig>,
    params_tx: Option<rtrb::Producer<(u32, f64)>>,
    latency: u32,
    /// The plugin asked for a restart: re-activate at the next opportunity.
    needs_restart: bool,
    /// Activations so far (see `PluginInstance::activation`).
    activations: u64,
    /// Parameter moves from the audio thread (the plugin's editor).
    edits_rx: Option<rtrb::Consumer<(u8, u32, f64)>>,
    gui_open: bool,
    /// Where the processor puts the gain reduction the plugin reports
    /// (CLAP's gain-adjustment metering); `None` while it has none.
    reduction: Option<Arc<faderframe_plugin_host::Reduction>>,
    // Declared last: dropped after the processor has been deactivated.
    instance: PluginInstance<FfHost>,
}

/// How a parameter's voices are addressed for modulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PerNote {
    No,
    /// By key and channel (`IS_MODULATABLE_PER_KEY`).
    ByKey,
    /// By note id only (`IS_MODULATABLE_PER_NOTE_ID`).
    ByNoteId,
}

pub(crate) fn descriptor_of(p: &ScannedPlugin) -> PluginDescriptor {
    p.descriptor(PluginFormat::Clap)
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

impl ClapInstance {
    pub fn new(entry: &PluginEntry, scanned: &ScannedPlugin) -> Result<Self, PluginError> {
        let info = host_info().map_err(|e| PluginError::Failed(e.to_string()))?;
        let id =
            CString::new(scanned.id.clone()).map_err(|e| PluginError::Failed(e.to_string()))?;
        let mut instance = PluginInstance::<FfHost>::new(
            |_| FfShared::new(),
            |shared| FfMainThread::new(shared),
            entry,
            &id,
            &info,
        )
        .map_err(|e| PluginError::Failed(format!("{}: {e}", scanned.name)))?;
        crate::host::refresh_extensions(&mut instance);
        let mut s = Self {
            descriptor: descriptor_of(scanned),
            scanned: scanned.clone(),
            params: Vec::new(),
            modulation: Vec::new(),
            rt: None,
            config: None,
            params_tx: None,
            latency: 0,
            needs_restart: false,
            activations: 0,
            edits_rx: None,
            gui_open: false,
            reduction: None,
            instance,
        };
        s.query_params();
        Ok(s)
    }

    fn ext(&self) -> crate::host::PluginExtensions {
        self.instance.access_shared_handler(|s| s.ext())
    }

    /// Every audio port takes 64-bit buffers.
    fn supports_64bit(&mut self) -> bool {
        use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer};
        let Some(ports) = self.ext().audio_ports else {
            return false;
        };
        let handle = self.instance.plugin_handle();
        let mut buffer = AudioPortInfoBuffer::new();
        let mut any = false;
        for is_input in [true, false] {
            for i in 0..ports.count(&handle, is_input) {
                match ports.get(&handle, i, is_input, &mut buffer) {
                    Some(info) if info.flags.contains(AudioPortFlags::SUPPORTS_64BITS) => {
                        any = true;
                    }
                    _ => return false,
                }
            }
        }
        any
    }

    /// The output ports' names.
    fn port_names(&mut self) -> Vec<String> {
        use clack_extensions::audio_ports::AudioPortInfoBuffer;
        let Some(ports) = self.ext().audio_ports else {
            return Vec::new();
        };
        let handle = self.instance.plugin_handle();
        let mut buffer = AudioPortInfoBuffer::new();
        (0..ports.count(&handle, false))
            .map(|i| {
                ports
                    .get(&handle, i, false, &mut buffer)
                    .map(|info| text(info.name))
                    .unwrap_or_default()
            })
            .collect()
    }

    fn query_params(&mut self) {
        let Some(params) = self.ext().params else {
            self.params.clear();
            self.modulation.clear();
            return;
        };
        let mut modulation = Vec::new();
        let handle = self.instance.plugin_handle();
        let mut buffer = ParamInfoBuffer::new();
        let mut out = Vec::new();
        for i in 0..params.count(&handle) {
            let Some(info) = params.get_info(&handle, i, &mut buffer) else {
                continue;
            };
            if info.flags.contains(ParamInfoFlags::IS_HIDDEN) {
                continue;
            }
            if info.flags.contains(ParamInfoFlags::IS_MODULATABLE) {
                let per_note = if info.flags.contains(ParamInfoFlags::IS_MODULATABLE_PER_KEY) {
                    PerNote::ByKey
                } else if info
                    .flags
                    .contains(ParamInfoFlags::IS_MODULATABLE_PER_NOTE_ID)
                {
                    PerNote::ByNoteId
                } else {
                    PerNote::No
                };
                modulation.push((ParameterId(info.id.get()), per_note));
            }
            let module = text(info.module);
            let name = text(info.name);
            out.push(ParameterInfo {
                id: ParameterId(info.id.get()),
                name: if module.is_empty() {
                    name
                } else {
                    format!("{module}/{name}")
                },
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                unit: ParameterUnit::None,
                automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE)
                    && !info.flags.contains(ParamInfoFlags::IS_READONLY),
                stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
            });
        }
        self.params = out;
        self.modulation = modulation;
    }

    /// Handle what the plugin asked for (main-thread callbacks run here).
    fn poll_requests(&mut self) -> faderframe_plugin_host::PluginPoll {
        let (callback, restart, latency, params, dirty, flush) =
            self.instance.access_shared_handler(|s| {
                (
                    FfShared::take(&s.callback),
                    FfShared::take(&s.restart),
                    FfShared::take(&s.latency_changed),
                    FfShared::take(&s.params_changed),
                    FfShared::take(&s.state_dirty),
                    FfShared::take(&s.flush),
                )
            });
        if callback {
            self.instance.call_on_main_thread_callback();
        }
        if flush && self.rt.is_none() {
            self.flush_inactive(&[]);
        }
        if params {
            self.query_params();
        }
        let mut latency_changed = false;
        if latency && self.rt.is_some() {
            let new = self.read_latency();
            latency_changed = new != self.latency;
        }
        if restart || latency_changed {
            self.needs_restart = true;
        }
        faderframe_plugin_host::PluginPoll {
            restart: restart || latency_changed,
            params_changed: params,
            state_dirty: dirty,
        }
    }

    fn read_latency(&mut self) -> u32 {
        let ext = self.ext();
        let handle = self.instance.plugin_handle();
        ext.latency.map_or(0, |l| l.get(&handle))
    }

    /// Parameter changes while inactive go through `flush`.
    fn flush_inactive(&mut self, changes: &[(u32, f64)]) {
        let Some(params) = self.ext().params else {
            return;
        };
        let mut input = EventBuffer::with_capacity(changes.len().max(1));
        for &(id, value) in changes {
            input.push(&ParamValueEvent::new(
                0,
                ClapId::new(id),
                Pckn::match_all(),
                value,
            ));
        }
        let mut output = EventBuffer::new();
        if let Some(mut handle) = self.instance.inactive_plugin_handle() {
            params.flush(&mut handle, &input.as_input(), &mut output.as_output());
        }
    }

    /// Stop and deactivate the processor (graphs still holding the shared
    /// cell go silent until they get a fresh processor).
    pub fn deactivate(&mut self) {
        let Some(cell) = self.rt.take() else { return };
        self.params_tx = None;
        self.config = None;
        // The audio thread holds the cell for at most one block.
        let Some(mut guard) = cell.lock_blocking(10_000) else {
            tracing::error!(
                "{}: cannot reclaim the processor; leaking it",
                self.scanned.name
            );
            std::mem::forget(cell);
            return;
        };
        if let Some(proc) = guard.proc.take() {
            let stopped = match proc {
                // stop_processing is an audio-thread call. Holding the cell,
                // this thread has exclusive access to the processor, so it
                // may act as the audio thread (CLAP lets that role move
                // between threads as long as calls never overlap).
                RtProc::Started(s) => {
                    let _audio = crate::host::AudioThreadScope::enter();
                    s.stop_processing()
                }
                RtProc::Stopped(s) => s,
            };
            self.instance.deactivate(stopped);
        }
    }

    pub fn scanned(&self) -> &ScannedPlugin {
        &self.scanned
    }
}

impl FfInstance for ClapInstance {
    fn output_bus_names(&mut self) -> Vec<String> {
        self.port_names()
    }

    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }

    fn modulatable(&self, id: ParameterId) -> bool {
        self.modulation.iter().any(|(p, _)| *p == id)
    }

    fn modulatable_per_note(&self, id: ParameterId) -> bool {
        self.modulation
            .iter()
            .any(|(p, n)| *p == id && *n != PerNote::No)
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        let params = self.ext().params?;
        let handle = self.instance.plugin_handle();
        params.get_value(&handle, ClapId::new(id.0))
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        if !self.params.iter().any(|p| p.id == id) {
            return Err(PluginError::UnknownParameter(id));
        }
        match self.params_tx.as_mut() {
            Some(tx) => tx
                .push((id.0, value))
                .map_err(|_| PluginError::Failed("parameter queue full".into())),
            None => {
                self.flush_inactive(&[(id.0, value)]);
                Ok(())
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn reduction(&self) -> Option<Arc<faderframe_plugin_host::Reduction>> {
        self.reduction.clone()
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn take_editor_edits(&mut self) -> Vec<faderframe_plugin_host::EditorEdit> {
        use faderframe_plugin_host::EditorEdit as E;
        let mut out = Vec::new();
        if let Some(rx) = self.edits_rx.as_mut() {
            while let Ok((kind, id, value)) = rx.pop() {
                let id = ParameterId(id);
                out.push(match kind {
                    0 => E::Begin(id),
                    2 => E::End(id),
                    _ => E::Value(id, value),
                });
            }
        }
        out
    }

    fn tail(&self) -> TailLength {
        TailLength::Infinite
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let Some(state) = self.ext().state else {
            return Ok(Vec::new());
        };
        let handle = self.instance.plugin_handle();
        let mut out = Vec::new();
        state
            .save(&handle, &mut out)
            .map_err(|e| PluginError::Failed(e.to_string()))?;
        Ok(out)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let Some(state) = self.ext().state else {
            return Err(PluginError::InvalidState(
                "the plugin has no state extension".into(),
            ));
        };
        let handle = self.instance.plugin_handle();
        state
            .load(&handle, &mut &data[..])
            .map_err(|e| PluginError::InvalidState(e.to_string()))?;
        self.query_params();
        Ok(())
    }

    fn poll(&mut self) -> faderframe_plugin_host::PluginPoll {
        self.poll_requests()
    }

    fn editor(&mut self) -> Option<&mut dyn faderframe_plugin_host::PluginEditor> {
        self.ext().gui?;
        Some(self)
    }

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        let params = self.ext().params?;
        let h = self.instance.plugin_handle();
        let mut buf = [0u8; 128];
        let text = params
            .value_to_text(&h, ClapId::new(id.0), value, &mut buf)
            .ok()?;
        let end = text.iter().position(|b| *b == 0).unwrap_or(text.len());
        let s = String::from_utf8_lossy(&text[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn event_sources(&self) -> faderframe_plugin_host::PluginEventSources {
        self.instance
            .access_handler(|m| faderframe_plugin_host::PluginEventSources {
                fds: m
                    .fds
                    .borrow()
                    .iter()
                    .map(
                        |&(fd, [read, write, error])| faderframe_plugin_host::PluginFd {
                            fd,
                            read,
                            write,
                            error,
                        },
                    )
                    .collect(),
                timers: m.timers.borrow().clone(),
            })
    }

    #[cfg(not(unix))]
    fn on_fd(&mut self, _fd: faderframe_plugin_host::PluginFd) {}

    #[cfg(unix)]
    fn on_fd(&mut self, fd: faderframe_plugin_host::PluginFd) {
        use clack_extensions::posix_fd::FdFlags;
        let Some(posix) = self.ext().posix_fd else {
            return;
        };
        let mut flags = FdFlags::empty();
        flags.set(FdFlags::READ, fd.read);
        flags.set(FdFlags::WRITE, fd.write);
        flags.set(FdFlags::ERROR, fd.error);
        let h = self.instance.plugin_handle();
        posix.on_fd(&h, fd.fd, flags);
    }

    fn on_timer(&mut self, id: u32) {
        let Some(timer) = self.ext().timer else {
            return;
        };
        let h = self.instance.plugin_handle();
        timer.on_timer(&h, TimerId(id));
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if self.config.as_ref() != Some(config) || self.rt.is_none() || self.needs_restart {
            self.needs_restart = false;
            self.deactivate();
            let stopped = self
                .instance
                .activate(
                    |_, _| (),
                    PluginAudioConfiguration {
                        sample_rate: config.sample_rate,
                        min_frames_count: 1,
                        max_frames_count: config.max_block_size.max(1),
                    },
                )
                .map_err(|e| PluginError::Failed(format!("{}: {e}", self.scanned.name)))?;
            let (tx, rx) = rtrb::RingBuffer::new(1024);
            let (edits_tx, edits_rx) = rtrb::RingBuffer::new(1024);
            let double = config.double_precision && self.supports_64bit();
            tracing::debug!(
                "{}: processing in {}-bit floating point",
                self.scanned.name,
                if double { 64 } else { 32 }
            );
            // Gain-adjustment metering: a cell kept across activations.
            let metering = self.ext().gain_adjustment.map(|m| {
                let cell = Arc::clone(
                    self.reduction
                        .get_or_insert_with(faderframe_plugin_host::Reduction::new),
                );
                (m, cell)
            });
            let state = RtState::new(
                stopped,
                &self.scanned.audio_inputs,
                &self.scanned.audio_outputs,
                config.max_block_size.max(1) as usize,
                double,
                rx,
                edits_tx,
                {
                    let mut ids: Vec<u32> = self
                        .modulation
                        .iter()
                        .filter(|(_, n)| *n == PerNote::ByNoteId)
                        .map(|(p, _)| p.0)
                        .collect();
                    ids.sort_unstable();
                    ids.into()
                },
                metering,
            );
            self.edits_rx = Some(edits_rx);
            self.rt = Some(Arc::new(TryCell::new(state)));
            self.params_tx = Some(tx);
            self.config = Some(*config);
            self.latency = self.read_latency();
            self.activations += 1;
        }
        let cell = self
            .rt
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| PluginError::Failed("not active".into()))?;
        Ok(Box::new(ClapProcessor { cell }))
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        faderframe_plugin_host::PluginEditor::close(self);
        self.deactivate();
    }
}

fn gui_config(api: WindowApi, floating: bool) -> GuiConfiguration<'static> {
    GuiConfiguration {
        api_type: match api {
            WindowApi::X11 => GuiApiType::X11,
            WindowApi::Win32 => GuiApiType::WIN32,
            WindowApi::Cocoa => GuiApiType::COCOA,
        },
        is_floating: floating,
    }
}

impl faderframe_plugin_host::PluginEditor for ClapInstance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        let Some(gui) = self.ext().gui else {
            return false;
        };
        let h = self.instance.plugin_handle();
        gui.is_api_supported(&h, gui_config(api, false))
    }

    fn can_float(&mut self, api: WindowApi) -> bool {
        let Some(gui) = self.ext().gui else {
            return false;
        };
        let h = self.instance.plugin_handle();
        gui.is_api_supported(&h, gui_config(api, true))
    }

    fn open_embedded(&mut self, api: WindowApi, scale: f64) -> Result<(u32, u32), PluginError> {
        self.close();
        let gui = self
            .ext()
            .gui
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        let h = self.instance.plugin_handle();
        gui.create(&h, gui_config(api, false))
            .map_err(|e| PluginError::Failed(format!("editor: {e:?}")))?;
        // Cocoa sizes are in points (the system scales); X11 plugins take
        // the scale from the system too, so it is only a hint there.
        if api != WindowApi::Cocoa {
            let _ = gui.set_scale(&h, scale);
        }
        self.gui_open = true;
        let size = gui.get_size(&h).unwrap_or(GuiSize {
            width: 640,
            height: 420,
        });
        Ok((size.width.max(1), size.height.max(1)))
    }

    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        let gui = self
            .ext()
            .gui
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        let fail = |e: GuiError| PluginError::Failed(format!("editor: {e:?}"));
        let h = self.instance.plugin_handle();
        let window = match parent.api {
            WindowApi::X11 => Window::from_x11_handle(parent.handle as std::ffi::c_ulong),
            // SAFETY: the handle is a live HWND / NSView of the host.
            WindowApi::Win32 => unsafe {
                Window::from_win32_hwnd(parent.handle as usize as *mut c_void)
            },
            // SAFETY: as above.
            WindowApi::Cocoa => unsafe {
                Window::from_cocoa_nsview(parent.handle as usize as *mut c_void)
            },
        };
        // SAFETY: the parent window outlives the editor: the host closes the
        // editor (destroy) before destroying its window.
        let attached = unsafe { gui.set_parent(&h, window) };
        if let Err(e) = attached.and_then(|()| gui.show(&h)) {
            self.close();
            return Err(fail(e));
        }
        Ok(())
    }

    fn open_floating(&mut self, api: WindowApi, title: &str) -> Result<(), PluginError> {
        self.close();
        let gui = self
            .ext()
            .gui
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        let fail = |e: GuiError| PluginError::Failed(format!("editor: {e:?}"));
        let h = self.instance.plugin_handle();
        gui.create(&h, gui_config(api, true)).map_err(fail)?;
        if let Ok(t) = CString::new(title) {
            gui.suggest_title(&h, &t);
        }
        gui.show(&h).map_err(fail)?;
        self.gui_open = true;
        Ok(())
    }

    fn close(&mut self) {
        if !self.gui_open {
            return;
        }
        self.gui_open = false;
        if let Some(gui) = self.ext().gui {
            let h = self.instance.plugin_handle();
            let _ = gui.hide(&h);
            gui.destroy(&h);
        }
    }

    fn is_open(&self) -> bool {
        self.gui_open
    }

    fn can_resize(&mut self) -> bool {
        let Some(gui) = self.ext().gui else {
            return false;
        };
        let h = self.instance.plugin_handle();
        gui.can_resize(&h)
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let gui = self.ext().gui?;
        let h = self.instance.plugin_handle();
        let size = gui
            .adjust_size(&h, GuiSize { width, height })
            .unwrap_or(GuiSize { width, height });
        gui.set_size(&h, size).ok()?;
        Some((size.width, size.height))
    }

    fn take_requests(&mut self) -> faderframe_plugin_host::EditorRequests {
        self.instance
            .access_shared_handler(|s| faderframe_plugin_host::EditorRequests {
                resize: s.gui_resize.lock().ok().and_then(|mut r| r.take()),
                show: FfShared::take(&s.gui_show),
                hide: FfShared::take(&s.gui_hide),
                closed: FfShared::take(&s.gui_closed),
            })
    }
}
