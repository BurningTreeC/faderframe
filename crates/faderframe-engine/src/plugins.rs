//! Control-side ownership of plugin instances.

use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::{
    PluginEditor, PluginError, PluginEventSources, PluginFd, PluginFormat, PluginInstance,
    PluginProcessor, PluginRegistry, ProcessConfig,
};
use faderframe_project::{PluginFormat as ProjectFormat, PluginSlot, Project};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn host_format(f: ProjectFormat) -> PluginFormat {
    match f {
        ProjectFormat::Builtin => PluginFormat::Builtin,
        ProjectFormat::Clap => PluginFormat::Clap,
        ProjectFormat::Vst3 => PluginFormat::Vst3,
        ProjectFormat::AudioUnit => PluginFormat::AudioUnit,
    }
}

struct Hosted {
    instance: Box<dyn PluginInstance>,
    failed: Arc<AtomicBool>,
    /// Explicit slot values last pushed to the instance.
    applied: HashMap<ParameterId, f64>,
    /// The plugin's own value before the first explicit one (restored when
    /// the explicit value is dropped, e.g. by undo).
    baseline: HashMap<ParameterId, f64>,
    /// The slot state the instance has (a different one, e.g. a preset or
    /// its undo, is loaded).
    state: Option<String>,
}

/// Owns one [`PluginInstance`] per project plugin slot.
///
/// Built-in plugins can hand out a fresh processor for every graph build;
/// the graph's state adoption then keeps the running one (tails survive).
/// Formats whose instances can only be activated once (CLAP, VST3) will hand
/// the live processor to the first graph and placeholders afterwards; that is
/// why processors are obtained through this type and not created ad hoc.
pub struct PluginHost {
    registry: PluginRegistry,
    instances: HashMap<PluginInstanceId, Hosted>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new(PluginRegistry::default())
    }
}

/// What the graph builder receives for one slot.
pub struct ActivatedPlugin {
    pub processor: Box<dyn PluginProcessor>,
    pub latency: u32,
    /// The instance's activation the processor belongs to.
    pub activation: u64,
    pub failed: Arc<AtomicBool>,
}

impl PluginHost {
    pub fn new(registry: PluginRegistry) -> Self {
        Self {
            registry,
            instances: HashMap::new(),
        }
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    fn ensure(&mut self, slot: &PluginSlot) -> Result<&mut Hosted, PluginError> {
        if !self.instances.contains_key(&slot.id) {
            let mut instance = self
                .registry
                .instantiate(host_format(slot.plugin.format), &slot.plugin.id)?;
            // Saved state first (complete), then explicit parameter values.
            if let Some(state) = slot.state.as_deref().and_then(decode_state) {
                let _ = instance.load_state(&state);
            }
            let mut applied = HashMap::new();
            let mut baseline = HashMap::new();
            for p in &slot.parameters {
                if let Some(v) = instance.parameter(p.id) {
                    baseline.insert(p.id, v);
                }
                let _ = instance.set_parameter(p.id, p.value);
                applied.insert(p.id, p.value);
            }
            self.instances.insert(
                slot.id,
                Hosted {
                    instance,
                    failed: Arc::new(AtomicBool::new(false)),
                    applied,
                    baseline,
                    state: slot.state.clone(),
                },
            );
        }
        self.instances
            .get_mut(&slot.id)
            .ok_or_else(|| PluginError::NotFound(slot.plugin.id.clone()))
    }

    /// Let every instance handle its requests; the merged result says
    /// whether the graph must be rebuilt.
    pub fn poll(&mut self) -> faderframe_plugin_host::PluginPoll {
        self.instances.values_mut().map(|h| h.instance.poll()).fold(
            faderframe_plugin_host::PluginPoll::default(),
            faderframe_plugin_host::PluginPoll::merge,
        )
    }

    /// Parameter moves made in plugins' own editors since the last call.
    pub fn take_editor_edits(
        &mut self,
    ) -> Vec<(PluginInstanceId, faderframe_plugin_host::EditorEdit)> {
        let mut out = Vec::new();
        for (id, h) in &mut self.instances {
            out.extend(h.instance.take_editor_edits().into_iter().map(|e| (*id, e)));
        }
        out
    }

    /// The slot state now matches the instance (it was just captured from
    /// it): nothing to load.
    pub fn note_state(&mut self, plugin: PluginInstanceId, state: &str) {
        if let Some(h) = self.instances.get_mut(&plugin) {
            h.state = Some(state.to_string());
        }
    }

    /// Preset files of the plugin format's own folders.
    pub fn preset_files(&self, plugin: PluginInstanceId) -> Vec<std::path::PathBuf> {
        self.instances
            .get(&plugin)
            .map_or_else(Vec::new, |h| h.instance.preset_files())
    }

    /// The state (encoded for [`PluginSlot::state`]) of a preset file.
    pub fn state_from_preset_file(
        &self,
        plugin: PluginInstanceId,
        data: &[u8],
    ) -> Result<String, PluginError> {
        let h = self
            .instances
            .get(&plugin)
            .ok_or_else(|| PluginError::NotFound(format!("{plugin}")))?;
        Ok(encode_state(&h.instance.state_from_preset_file(data)?))
    }

    /// The current state of an instantiated plugin, encoded for
    /// [`PluginSlot::state`].
    pub fn capture_state(&mut self, plugin: PluginInstanceId) -> Option<String> {
        let bytes = self
            .instances
            .get_mut(&plugin)?
            .instance
            .save_state()
            .ok()?;
        (!bytes.is_empty()).then(|| encode_state(&bytes))
    }

    /// Push changed explicit parameter values (slot `parameters`) to the
    /// instantiated plugins; values dropped from a slot fall back to the
    /// plugin's own value from before.
    pub fn sync_parameters(&mut self, project: &Project) {
        for slot in project
            .tracks
            .iter()
            .flat_map(|t| t.inserts.iter().chain(t.instrument.iter()))
        {
            let Some(h) = self.instances.get_mut(&slot.id) else {
                continue;
            };
            if h.state != slot.state {
                // A new state (preset, undo): load it; explicit values follow.
                if let Some(bytes) = slot.state.as_deref().and_then(decode_state)
                    && let Err(e) = h.instance.load_state(&bytes)
                {
                    tracing::warn!("{}: cannot load the state: {e}", slot.plugin.name);
                }
                h.state = slot.state.clone();
                h.applied.clear();
                h.baseline.clear();
            }
            for p in &slot.parameters {
                if h.applied.get(&p.id) == Some(&p.value) {
                    continue;
                }
                if !h.baseline.contains_key(&p.id)
                    && let Some(v) = h.instance.parameter(p.id)
                {
                    h.baseline.insert(p.id, v);
                }
                let _ = h.instance.set_parameter(p.id, p.value);
                h.applied.insert(p.id, p.value);
            }
            if h.applied.len() != slot.parameters.len() {
                let dropped: Vec<ParameterId> = h
                    .applied
                    .keys()
                    .filter(|id| !slot.parameters.iter().any(|p| p.id == **id))
                    .copied()
                    .collect();
                for id in dropped {
                    h.applied.remove(&id);
                    if let Some(v) = h.baseline.get(&id) {
                        let _ = h.instance.set_parameter(id, *v);
                    }
                }
            }
        }
    }

    /// The plugin's own text for a value.
    pub fn format_parameter(
        &mut self,
        plugin: PluginInstanceId,
        id: ParameterId,
        value: f64,
    ) -> Option<String> {
        self.instances
            .get_mut(&plugin)?
            .instance
            .format_parameter(id, value)
    }

    /// Current value of a parameter (plain units).
    pub fn parameter_value(&mut self, plugin: PluginInstanceId, id: ParameterId) -> Option<f64> {
        self.instances.get_mut(&plugin)?.instance.parameter(id)
    }

    /// Note a value the plugin changed itself (its editor) as applied, so the
    /// next sync does not push the stale slot value back.
    pub fn note_parameter(&mut self, plugin: PluginInstanceId, id: ParameterId, value: f64) {
        if let Some(h) = self.instances.get_mut(&plugin) {
            h.applied.insert(id, value);
        }
    }

    /// The plugin's own editor, if it has one.
    pub fn editor(&mut self, plugin: PluginInstanceId) -> Option<&mut dyn PluginEditor> {
        self.instances.get_mut(&plugin)?.instance.editor()
    }

    /// File descriptors and timers every plugin registered.
    pub fn event_sources(&self) -> Vec<(PluginInstanceId, PluginEventSources)> {
        self.instances
            .iter()
            .map(|(id, h)| (*id, h.instance.event_sources()))
            .filter(|(_, s)| !s.fds.is_empty() || !s.timers.is_empty())
            .collect()
    }

    pub fn on_fd(&mut self, plugin: PluginInstanceId, fd: PluginFd) {
        if let Some(h) = self.instances.get_mut(&plugin) {
            h.instance.on_fd(fd);
        }
    }

    pub fn on_timer(&mut self, plugin: PluginInstanceId, timer: u32) {
        if let Some(h) = self.instances.get_mut(&plugin) {
            h.instance.on_timer(timer);
        }
    }

    /// Parameters of an instantiated plugin.
    pub fn parameters(
        &self,
        plugin: PluginInstanceId,
    ) -> Option<&[faderframe_plugin_host::ParameterInfo]> {
        self.instances.get(&plugin).map(|h| h.instance.parameters())
    }

    /// Does the instantiated plugin have a sidechain (second audio) input?
    pub fn has_sidechain(&self, plugin: PluginInstanceId) -> bool {
        self.instances.get(&plugin).is_some_and(|h| {
            h.instance
                .descriptor()
                .audio_inputs
                .get(1)
                .is_some_and(|p| p.channels > 0)
        })
    }

    pub fn instance(&mut self, slot: &PluginSlot) -> Result<&mut dyn PluginInstance, PluginError> {
        Ok(self.ensure(slot)?.instance.as_mut())
    }

    /// Activate a processor for `slot` (instantiating on first use).
    pub fn activate(
        &mut self,
        slot: &PluginSlot,
        config: &ProcessConfig,
    ) -> Result<ActivatedPlugin, PluginError> {
        let hosted = self.ensure(slot)?;
        let processor = hosted.instance.create_processor(config)?;
        Ok(ActivatedPlugin {
            processor,
            latency: hosted.instance.latency_samples(),
            activation: hosted.instance.activation(),
            failed: Arc::clone(&hosted.failed),
        })
    }

    /// Drop instances whose slots no longer exist in the project.
    pub fn retain_project(&mut self, project: &Project) {
        // Frozen tracks' plugins are unloaded (restored from their slots).
        let live: HashSet<PluginInstanceId> = project
            .tracks
            .iter()
            .filter(|t| t.freeze.is_none())
            .flat_map(|t| t.inserts.iter().chain(t.instrument.iter()).map(|s| s.id))
            .collect();
        self.instances.retain(|id, _| live.contains(id));
    }

    /// Latency a hosted plugin reports (samples).
    pub fn latency(&self, plugin: PluginInstanceId) -> Option<u32> {
        self.instances
            .get(&plugin)
            .map(|h| h.instance.latency_samples())
    }

    /// Plugins that reported a processing failure.
    pub fn failed(&self) -> Vec<PluginInstanceId> {
        self.instances
            .iter()
            .filter(|(_, h)| h.failed.load(std::sync::atomic::Ordering::Relaxed))
            .map(|(id, _)| *id)
            .collect()
    }
}

/// Plugin state in project files: base64 of the plugin's own bytes.
pub fn encode_state(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_state(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .ok()
}
