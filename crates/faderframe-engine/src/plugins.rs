//! Control-side ownership of plugin instances.

use faderframe_core::PluginInstanceId;
use faderframe_plugin_host::{
    PluginError, PluginFormat, PluginInstance, PluginProcessor, PluginRegistry, ProcessConfig,
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
        Self::new(PluginRegistry::with_builtins())
    }
}

/// What the graph builder receives for one slot.
pub struct ActivatedPlugin {
    pub processor: Box<dyn PluginProcessor>,
    pub latency: u32,
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
            for p in &slot.parameters {
                let _ = instance.set_parameter(p.id, p.value);
            }
            self.instances.insert(
                slot.id,
                Hosted {
                    instance,
                    failed: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        self.instances
            .get_mut(&slot.id)
            .ok_or_else(|| PluginError::NotFound(slot.plugin.id.clone()))
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
            failed: Arc::clone(&hosted.failed),
        })
    }

    /// Drop instances whose slots no longer exist in the project.
    pub fn retain_project(&mut self, project: &Project) {
        let live: HashSet<PluginInstanceId> = project
            .tracks
            .iter()
            .flat_map(|t| t.inserts.iter().chain(t.instrument.iter()).map(|s| s.id))
            .collect();
        self.instances.retain(|id, _| live.contains(id));
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
