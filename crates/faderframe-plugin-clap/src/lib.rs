//! CLAP plugin hosting.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod host;
mod instance;
mod processor;
pub mod scan;

pub use instance::ClapInstance;
pub use processor::ClapProcessor;

use faderframe_plugin_host::{
    PluginDescriptor, PluginError, PluginFactory, PluginFormat, PluginInstance as FfInstance,
};
use scan::ScannedPlugin;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

/// The plugins known to this process (from the scan cache, updated when a
/// background scan finishes).
static CATALOG: RwLock<Vec<ScannedPlugin>> = RwLock::new(Vec::new());

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replace the list of known CLAP plugins.
pub fn set_catalog(plugins: Vec<ScannedPlugin>) {
    if let Ok(mut c) = CATALOG.write() {
        *c = plugins;
    }
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Bumped whenever the catalog changes (views refresh on change).
pub fn catalog_generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn catalog() -> Vec<ScannedPlugin> {
    CATALOG.read().map(|c| c.clone()).unwrap_or_default()
}

/// CLAP as a [`PluginFactory`]: lists the catalog, loads bundles on first
/// use (in this process) and instantiates plugins.
#[derive(Default)]
pub struct ClapFactory {
    entries: RefCell<HashMap<PathBuf, clack_host::prelude::PluginEntry>>,
}

impl ClapFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use an already loaded entry for `bundle` (tests: plugins built with
    /// `clack-plugin` and loaded without a shared library).
    pub fn with_entry(self, bundle: PathBuf, entry: clack_host::prelude::PluginEntry) -> Self {
        self.entries.borrow_mut().insert(bundle, entry);
        self
    }
}

impl PluginFactory for ClapFactory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        catalog().iter().map(instance::descriptor_of).collect()
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn FfInstance>, PluginError> {
        let plugins = catalog();
        let p = plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
        let mut entries = self.entries.borrow_mut();
        if !entries.contains_key(&p.bundle) {
            // SAFETY: loading a plugin bundle runs third-party code; the
            // user chose to load it (it was scanned successfully before).
            let entry = unsafe { clack_host::prelude::PluginEntry::load(&p.bundle) }
                .map_err(|e| PluginError::Failed(format!("{}: {e}", p.bundle.display())))?;
            entries.insert(p.bundle.clone(), entry);
        }
        let entry = entries
            .get(&p.bundle)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
        Ok(Box::new(ClapInstance::new(entry, p)?))
    }
}
