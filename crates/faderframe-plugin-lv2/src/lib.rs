//! LV2 plugin hosting (Linux).
//!
//! Plugins are described by Turtle files ([`scan`]: read without loading
//! any plugin code, so scanning needs no helper process) and loaded from
//! their bundle's shared library ([`Lv2Instance`]). The host speaks the
//! URID map, options, bounded block length, worker, state (with the
//! plugin's default state and presets), log and time extensions; MIDI
//! travels as atom sequences, and X11 UIs embed into the host's editor
//! windows.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod atom;
mod core;
pub mod features;
mod instance;
pub mod scan;
pub mod state;
pub mod sys;
pub mod ttl;
mod ui;
pub mod urid;
pub mod worker;

pub use instance::{Lv2Instance, Lv2Processor};
pub use scan::Lv2Plugin;

use faderframe_plugin_host::{
    PluginDescriptor, PluginError, PluginFactory, PluginFormat, PluginInstance,
};
use std::sync::{Arc, RwLock};

/// The plugins known to this process.
static CATALOG: RwLock<Vec<Arc<Lv2Plugin>>> = RwLock::new(Vec::new());

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replace the list of known LV2 plugins.
pub fn set_catalog(plugins: Vec<Lv2Plugin>) {
    if let Ok(mut c) = CATALOG.write() {
        *c = plugins.into_iter().map(Arc::new).collect();
    }
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Bumped whenever the catalog changes.
pub fn catalog_generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn catalog() -> Vec<Arc<Lv2Plugin>> {
    CATALOG.read().map(|c| c.clone()).unwrap_or_default()
}

/// Scan the LV2 path and make it the catalog; returns how many plugins
/// were found.
pub fn rescan() -> usize {
    let plugins = scan::scan(&scan::default_paths());
    let n = plugins.len();
    set_catalog(plugins);
    n
}

/// LV2 as a [`PluginFactory`]: lists the catalog and instantiates plugins
/// in this process.
#[derive(Default)]
pub struct Lv2Factory;

impl Lv2Factory {
    pub fn new() -> Self {
        Self
    }
}

impl PluginFactory for Lv2Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Lv2
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        catalog()
            .iter()
            .map(|p| p.scanned().descriptor(PluginFormat::Lv2))
            .collect()
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        let p = catalog()
            .into_iter()
            .find(|p| p.uri == id)
            .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
        Ok(Box::new(Lv2Instance::new(p)?))
    }
}

#[cfg(test)]
mod tests {
    /// `FADERFRAME_TEST_LV2_PATH=<dirs>`: every bundle there is described.
    #[test]
    #[ignore = "needs LV2 bundles"]
    fn bundles_on_a_path_are_described() {
        let Some(paths) = std::env::var_os("FADERFRAME_TEST_LV2_PATH") else {
            return;
        };
        let paths: Vec<_> = std::env::split_paths(&paths).collect();
        let plugins = crate::scan::scan(&paths);
        for p in &plugins {
            let s = p.scanned();
            println!(
                "{} — {} [{}] in {:?} out {:?} notes {}/{} ports {} uis {:?} presets {} req {:?}",
                p.uri,
                p.name,
                s.features.join(","),
                s.audio_inputs,
                s.audio_outputs,
                s.note_inputs,
                s.note_outputs,
                p.ports.len(),
                p.uis
                    .iter()
                    .map(|u| u.class.rsplit('#').next().unwrap_or(""))
                    .collect::<Vec<_>>(),
                p.presets.len(),
                p.required_features
                    .iter()
                    .map(|f| f.rsplit(['#', '/']).next().unwrap_or(""))
                    .collect::<Vec<_>>()
            );
        }
        assert!(!plugins.is_empty());
    }
}
