//! VST3 plugin hosting.
//!
//! Bundles (`*.vst3`) are loaded once per process and stay loaded
//! ([`module`]); scanning runs in the `faderframe --scan-vst3` helper
//! process ([`scan`]). An instance ([`Vst3Instance`]) owns the component
//! and its edit controller (one object, or two joined through connection
//! points), forwards edits made in the plugin's editor to the processor and
//! returns the processor's parameter changes to the controller. The audio
//! side ([`Vst3Processor`]) fills preallocated parameter-change and event
//! lists; nothing allocates per block, and any (single) thread may run it.
#![deny(unsafe_op_in_unsafe_fn)]
// The bindings' constant types differ between platforms (u32 on Linux,
// i32 on Windows): casts that are no-ops here are needed there.
#![allow(clippy::unnecessary_cast)]

pub mod com;
mod instance;
pub mod module;
mod presets;
mod processor;
pub mod scan;
pub mod util;

pub use instance::Vst3Instance;
pub use processor::Vst3Processor;

use faderframe_plugin_host::scan::ScannedPlugin;
use faderframe_plugin_host::{
    PluginDescriptor, PluginError, PluginFactory, PluginFormat, PluginInstance,
};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The plugins known to this process (from the scan cache, updated when a
/// background scan finishes).
static CATALOG: RwLock<Vec<ScannedPlugin>> = RwLock::new(Vec::new());

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Replace the list of known VST3 plugins.
pub fn set_catalog(plugins: Vec<ScannedPlugin>) {
    if let Ok(mut c) = CATALOG.write() {
        *c = plugins;
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Add plugins to the catalog (tests: in-process plugins).
pub fn extend_catalog(plugins: impl IntoIterator<Item = ScannedPlugin>) {
    if let Ok(mut c) = CATALOG.write() {
        for p in plugins {
            c.retain(|x| x.id != p.id);
            c.push(p);
        }
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Bumped whenever the catalog changes (views refresh on change).
pub fn catalog_generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn catalog() -> Vec<ScannedPlugin> {
    CATALOG.read().map(|c| c.clone()).unwrap_or_default()
}

/// VST3 as a [`PluginFactory`]: lists the catalog, loads modules on first
/// use and instantiates plugins.
#[derive(Default)]
pub struct Vst3Factory;

impl Vst3Factory {
    pub fn new() -> Self {
        Self
    }
}

impl PluginFactory for Vst3Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Vst3
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        catalog()
            .iter()
            .map(|p| p.descriptor(PluginFormat::Vst3))
            .collect()
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        let plugins = catalog();
        let p = plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
        let m = module::load(&p.bundle).map_err(|e| PluginError::Failed(e.to_string()))?;
        Ok(Box::new(Vst3Instance::new(m, p)?))
    }
}
