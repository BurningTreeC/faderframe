//! The control side of an LV2 plugin: parameters (its input control
//! ports), state, presets, activation and the editor.
//!
//! LV2 plugins are instantiated with a sample rate and block sizes, which
//! the host learns only when it activates them: an instance starts at
//! 48 kHz and is made again (its state carried over) when the engine runs
//! at another rate or larger blocks.

use crate::core::{self, Core, Links, Shared};
use crate::scan::{Lv2Plugin, PortKind};
use crate::state::{self, State};
use crate::sys;
use crate::ttl::{Graph, Node};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_plugin_host::{
    EditorEdit, ParameterInfo, ParameterUnit, PluginDescriptor, PluginError, PluginEventSources,
    PluginFormat, PluginInstance, PluginPoll, PluginProcessContext, PluginProcessor, ProcessConfig,
    ProcessStatus, TailLength,
};
use faderframe_realtime::TryCell;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The rate and block size an instance starts with.
const START_RATE: f64 = 48_000.0;
const START_BLOCK: usize = 8192;
/// The editor's timer (idle calls, port updates).
pub(crate) const UI_TIMER: u32 = 1;
const UI_PERIOD_MS: u32 = 30;

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

pub struct Lv2Instance {
    pub(crate) model: Arc<Lv2Plugin>,
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
    binary: core::Binary,
    pub(crate) shared: Arc<Shared>,
    cell: Arc<TryCell<Core>>,
    pub(crate) links: Links,
    /// The handle and its state interface (null: none), for `save`, which
    /// may run alongside the audio thread.
    handle: sys::LV2_Handle,
    state_iface: *const sys::LV2_State_Interface,
    max_frames: usize,
    /// Rate and block size of the last activation.
    active: Option<(f64, u32)>,
    latency: u32,
    activations: u64,
    pub(crate) editor: Option<crate::ui::Editor>,
    pub(crate) ui_scale: f64,
    pub(crate) rate: f64,
    pub(crate) edits: Vec<EditorEdit>,
    program: Option<usize>,
    state_dirty: bool,
}

impl Lv2Instance {
    pub fn new(model: Arc<Lv2Plugin>) -> Result<Self, PluginError> {
        let missing = crate::features::missing(&model.required_features);
        if !missing.is_empty() {
            return Err(failed(format!(
                "{} needs host features FaderFrame lacks: {}",
                model.name,
                missing.join(", ")
            )));
        }
        if model
            .ports
            .iter()
            .any(|p| p.kind == PortKind::Other && !p.optional)
        {
            return Err(failed(format!(
                "{} has ports of an unknown type",
                model.name
            )));
        }
        // SAFETY: loading the plugin's library runs its initialisers; the
        // user chose the plugin (its metadata was read before).
        let library = unsafe { libloading::Library::new(&model.binary) }
            .map_err(|e| failed(format!("{}: {e}", model.binary.display())))?;
        Self::with_binary(model, core::Binary::Library(Arc::new(library)))
    }

    /// An instance of a plugin whose entry point is linked into this
    /// program (`model` describes it as its bundle would).
    pub fn with_entry(
        model: Arc<Lv2Plugin>,
        entry: sys::LV2_Descriptor_Function,
    ) -> Result<Self, PluginError> {
        Self::with_binary(model, core::Binary::Static(entry))
    }

    fn with_binary(model: Arc<Lv2Plugin>, binary: core::Binary) -> Result<Self, PluginError> {
        let shared = Arc::new(Shared::new(&model));
        let (core, links) = Core::new(
            &model,
            binary.clone(),
            START_RATE,
            START_BLOCK,
            Arc::clone(&shared),
        )
        .map_err(failed)?;
        let mut s = Lv2Instance {
            descriptor: model.scanned().descriptor(PluginFormat::Lv2),
            params: parameters(&model),
            handle: core.handle,
            state_iface: core
                .extension(sys::uri::STATE_INTERFACE)
                .cast::<sys::LV2_State_Interface>(),
            cell: Arc::new(TryCell::new(core)),
            links,
            binary,
            shared,
            rate: START_RATE,
            max_frames: START_BLOCK,
            active: None,
            latency: 0,
            activations: 0,
            editor: None,
            ui_scale: 1.0,
            edits: Vec::new(),
            program: None,
            state_dirty: false,
            model,
        };
        s.load_default_state();
        Ok(s)
    }

    /// The bundle's metadata (presets, default state).
    fn graph(&self) -> Graph {
        crate::scan::bundle_graph(&self.model.bundle).unwrap_or_default()
    }

    /// The `state:state` the plugin's data lists, loaded by the host.
    fn load_default_state(&mut self) {
        let g = self.graph();
        let node = Node::Iri(self.model.uri.clone());
        if let Some(s) = g.object(&node, &format!("{}state", crate::ttl::STATE)) {
            let properties = state::properties_of(&g, s);
            if let Err(e) = self.restore(&properties) {
                tracing::warn!("{}: default state: {e}", self.model.name);
            }
        }
    }

    /// Hand `properties` to the plugin, nothing else running on it.
    fn restore(&mut self, properties: &[state::Property]) -> Result<(), String> {
        if self.state_iface.is_null() || properties.is_empty() {
            return Ok(());
        }
        let mut guard = self
            .cell
            .lock_blocking(10_000)
            .ok_or("the plugin is busy")?;
        let core = &mut *guard;
        let _paused = core.pause_worker();
        // SAFETY: the instance's own interface; holding the cell and the
        // worker's gate, nothing else runs on it.
        unsafe { state::restore(core.handle, self.state_iface, properties, core.schedule()) }
    }

    /// Port values by symbol: shared, and to the plugin.
    fn apply_ports(&mut self, ports: &[(String, f32)]) {
        for (symbol, value) in ports {
            if let Some(p) = self
                .model
                .ports
                .iter()
                .find(|p| p.input && p.kind == PortKind::Control && &p.symbol == symbol)
            {
                let v = clamp(p, *value);
                self.shared.set(p.index, v);
                let _ = self.links.edits.push((p.index, v));
            }
        }
    }

    fn current_state(&mut self) -> State {
        let ports = self
            .model
            .ports
            .iter()
            .filter(|p| p.input && p.kind == PortKind::Control)
            .filter_map(|p| Some((p.symbol.clone(), self.shared.get(p.index)?)))
            .collect();
        let properties = if self.state_iface.is_null() {
            Vec::new()
        } else {
            // SAFETY: `save` may run alongside `run` (its own threading
            // class); the handle lives while the cell does.
            unsafe { state::save(self.handle, self.state_iface) }
        };
        State { ports, properties }
    }

    fn apply_state(&mut self, s: &State) -> Result<(), PluginError> {
        self.apply_ports(&s.ports);
        self.restore(&s.properties)
            .map_err(PluginError::InvalidState)?;
        self.push_controls_to_ui();
        Ok(())
    }

    /// Make the plugin again for `rate` and blocks of `frames`.
    fn reinstantiate(&mut self, rate: f64, frames: usize) -> Result<(), PluginError> {
        let saved = self.current_state();
        if let Some(mut old) = self.cell.lock_blocking(10_000) {
            old.live = false;
        }
        let (core, links) = Core::new(
            &self.model,
            self.binary.clone(),
            rate,
            frames,
            Arc::clone(&self.shared),
        )
        .map_err(failed)?;
        self.handle = core.handle;
        self.state_iface = core
            .extension(sys::uri::STATE_INTERFACE)
            .cast::<sys::LV2_State_Interface>();
        self.cell = Arc::new(TryCell::new(core));
        self.links = links;
        self.rate = rate;
        self.max_frames = frames;
        self.active = None;
        self.restore(&saved.properties).map_err(failed)?;
        Ok(())
    }

    /// The UI's writes and the plugin's messages for it.
    pub(crate) fn pump_ui(&mut self) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        for e in ed.take_events() {
            match e {
                crate::ui::UiEvent::Control(port, value) => {
                    if self.params.iter().any(|p| p.id.0 == port) {
                        let v = f64::from(value);
                        self.edits.push(EditorEdit::Value(ParameterId(port), v));
                    }
                    self.shared.set(port, value);
                    let _ = self.links.edits.push((port, value));
                    self.program = None;
                }
                crate::ui::UiEvent::Touch(port, grabbed) => {
                    if self.params.iter().any(|p| p.id.0 == port) {
                        self.edits.push(if grabbed {
                            EditorEdit::Begin(ParameterId(port))
                        } else {
                            EditorEdit::End(ParameterId(port))
                        });
                    }
                }
                crate::ui::UiEvent::Atom(port, type_, body) => {
                    core::push_message(&mut self.links.to_plugin, port, type_, &body);
                    self.state_dirty = true;
                }
            }
        }
    }

    /// Tell the UI every control value anew (after a state or preset).
    fn push_controls_to_ui(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            ed.forget_sent();
        }
    }

    pub(crate) fn idle_ui(&mut self) {
        self.pump_ui();
        if let Some(ed) = self.editor.as_mut() {
            ed.idle(&self.model, &self.shared, &mut self.links.from_plugin);
        }
        self.pump_ui();
    }
}

fn clamp(p: &crate::scan::Port, v: f32) -> f32 {
    match (p.minimum, p.maximum) {
        (Some(lo), Some(hi)) if lo <= hi => (v as f64).clamp(lo, hi) as f32,
        _ => v,
    }
}

/// The host parameters: input control ports the user sets.
fn parameters(model: &Lv2Plugin) -> Vec<ParameterInfo> {
    model
        .ports
        .iter()
        .filter(|p| {
            p.kind == PortKind::Control
                && p.input
                && !p.latency
                && !p.enabled
                && !p.free_wheeling
                && !p.hidden
        })
        .map(|p| {
            let min = p.minimum.unwrap_or(0.0);
            let max = p.maximum.unwrap_or(1.0).max(min);
            ParameterInfo {
                id: ParameterId(p.index),
                name: p.name.clone(),
                min,
                max,
                default: f64::from(core::initial_value(p)).clamp(min, max),
                unit: match p.unit.as_deref() {
                    Some("db") => ParameterUnit::Decibels,
                    Some("ms") => ParameterUnit::Milliseconds,
                    Some("hz") => ParameterUnit::Hertz,
                    Some("frame") => ParameterUnit::Samples,
                    _ => ParameterUnit::None,
                },
                automatable: true,
                stepped: p.integer || p.toggled || p.enumeration,
            }
        })
        .collect()
}

pub struct Lv2Processor {
    cell: Arc<TryCell<Core>>,
}

impl PluginProcessor for Lv2Processor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        match self.cell.try_lock() {
            Some(mut core) => core.process(ctx, io),
            None => {
                for out in io.audio_out.iter_mut() {
                    out.clear();
                }
                ProcessStatus::Continue
            }
        }
    }

    fn reset(&mut self) {
        if let Some(mut core) = self.cell.try_lock() {
            core.reset();
        }
    }
}

impl PluginInstance for Lv2Instance {
    fn configure_realtime(&mut self, realtime: bool) {
        self.shared.freewheel.store(!realtime, Ordering::Relaxed);
    }

    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        self.params.iter().find(|p| p.id == id)?;
        self.shared.get(id.0).map(f64::from)
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        let info = self
            .params
            .iter()
            .find(|p| p.id == id)
            .ok_or(PluginError::UnknownParameter(id))?;
        let v = info.clamp(value) as f32;
        self.shared.set(id.0, v);
        let _ = self.links.edits.push((id.0, v));
        Ok(())
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn programs(&self) -> Vec<String> {
        self.model.presets.iter().map(|p| p.label.clone()).collect()
    }

    fn current_program(&self) -> Option<usize> {
        self.program
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        let preset = self
            .model
            .presets
            .get(index)
            .ok_or_else(|| PluginError::NotFound(format!("preset {index}")))?;
        let mut g = crate::scan::bundle_graph(&preset.bundle).map_err(PluginError::InvalidState)?;
        let node = Node::Iri(preset.uri.clone());
        // A preset's own file (a user preset bundle).
        let files: Vec<_> = g
            .objects(&node, crate::ttl::RDFS_SEE_ALSO)
            .filter_map(|o| o.iri().and_then(crate::ttl::url_path))
            .collect();
        for f in files {
            g.load(&f).map_err(PluginError::InvalidState)?;
        }
        let s = state::preset(&g, &node);
        self.apply_state(&s)?;
        self.program = Some(index);
        self.state_dirty = true;
        Ok(())
    }

    fn changes_pending(&self) -> bool {
        false
    }

    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        self.pump_ui();
        std::mem::take(&mut self.edits)
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn tail(&self) -> TailLength {
        // LV2 does not say: keep processing.
        TailLength::Infinite
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(self.current_state().to_bytes())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let s = State::from_bytes(data).map_err(PluginError::InvalidState)?;
        self.apply_state(&s)
    }

    fn poll(&mut self) -> PluginPoll {
        self.pump_ui();
        let restart = self.shared.latency_changed.swap(false, Ordering::Relaxed);
        if restart {
            // Measured again at the next activation.
            self.active = None;
        }
        PluginPoll {
            restart,
            params_changed: false,
            state_dirty: std::mem::take(&mut self.state_dirty),
        }
    }

    fn editor(&mut self) -> Option<&mut dyn faderframe_plugin_host::PluginEditor> {
        if crate::ui::find(&self.model).is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        let p = self.model.ports.get(id.0 as usize)?;
        if p.toggled {
            return Some(if value >= 0.5 { "On" } else { "Off" }.into());
        }
        p.scale_points
            .iter()
            .find(|(v, _)| (v - value).abs() < 1e-6)
            .map(|(_, label)| label.clone())
    }

    fn event_sources(&self) -> PluginEventSources {
        PluginEventSources {
            fds: Vec::new(),
            timers: if self.editor.is_some() {
                vec![(UI_TIMER, UI_PERIOD_MS)]
            } else {
                Vec::new()
            },
        }
    }

    fn on_timer(&mut self, id: u32) {
        if id == UI_TIMER {
            self.idle_ui();
        }
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        let frames = (config.max_block_size.max(1)) as usize;
        if config.sample_rate != self.rate || frames > self.max_frames {
            self.reinstantiate(config.sample_rate, frames.max(START_BLOCK))?;
        }
        let want = (config.sample_rate, config.max_block_size);
        if self.active != Some(want) {
            let mut core = self
                .cell
                .lock_blocking(10_000)
                .ok_or_else(|| failed("the plugin is busy"))?;
            core.deactivate();
            core.activate();
            self.latency = core.measure_latency();
            drop(core);
            self.active = Some(want);
            self.activations += 1;
        }
        Ok(Box::new(Lv2Processor {
            cell: Arc::clone(&self.cell),
        }))
    }
}

impl Drop for Lv2Instance {
    fn drop(&mut self) {
        // The UI goes before the plugin.
        self.editor = None;
        if let Some(mut core) = self.cell.lock_blocking(10_000) {
            core.live = false;
        }
    }
}
