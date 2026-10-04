//! Audio Unit hosting (macOS).
//!
//! Units are listed from the system's component registry ([`scan`]: no
//! helper process needed, nothing is instantiated) and hosted through the
//! AUv2 C API, which also reaches AUv3 extensions. An [`AuInstance`] owns
//! the unit: parameters, state (the `ClassInfo` property list, which is
//! also the `.aupreset` format), latency/tail and the editor view (the
//! unit's Cocoa UI or the generic view). The audio side ([`AuProcessor`])
//! renders with preallocated buffers, feeds automation as scheduled
//! parameter events and MIDI through `MusicDeviceMIDIEvent`.
//!
//! On other platforms this crate is empty.

#![cfg_attr(not(target_os = "macos"), allow(unused))]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod instance;
#[cfg(target_os = "macos")]
mod processor;
#[cfg(target_os = "macos")]
pub mod scan;
#[cfg(target_os = "macos")]
mod view;

#[cfg(target_os = "macos")]
pub use instance::{AuInstance, has_generic_editor};
#[cfg(target_os = "macos")]
pub use processor::AuProcessor;

#[cfg(target_os = "macos")]
mod factory {
    use super::*;
    use faderframe_plugin_host::scan::ScannedPlugin;
    use faderframe_plugin_host::{
        PluginDescriptor, PluginError, PluginFactory, PluginFormat, PluginInstance,
    };
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CATALOG: RwLock<Option<Vec<ScannedPlugin>>> = RwLock::new(None);
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    /// The installed units (listed on first use; [`rescan`] refreshes).
    pub fn catalog() -> Vec<ScannedPlugin> {
        if let Ok(c) = CATALOG.read()
            && let Some(list) = c.as_ref()
        {
            return list.clone();
        }
        rescan()
    }

    /// List the installed units again.
    pub fn rescan() -> Vec<ScannedPlugin> {
        let list = scan::scan();
        if let Ok(mut c) = CATALOG.write() {
            *c = Some(list.clone());
        }
        GENERATION.fetch_add(1, Ordering::Relaxed);
        list
    }

    /// Bumped whenever the catalog changes.
    pub fn catalog_generation() -> u64 {
        GENERATION.load(Ordering::Relaxed)
    }

    /// Audio Units as a [`PluginFactory`].
    #[derive(Default)]
    pub struct AuFactory;

    impl AuFactory {
        pub fn new() -> Self {
            Self
        }
    }

    impl PluginFactory for AuFactory {
        fn format(&self) -> PluginFormat {
            PluginFormat::AudioUnit
        }

        fn scan(&self) -> Vec<PluginDescriptor> {
            catalog()
                .iter()
                .map(|p| p.descriptor(PluginFormat::AudioUnit))
                .collect()
        }

        fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
            let p = catalog()
                .into_iter()
                .find(|p| p.id == id)
                .ok_or_else(|| PluginError::NotFound(id.to_string()))?;
            Ok(Box::new(AuInstance::new(&p)?))
        }
    }
}

#[cfg(target_os = "macos")]
pub use factory::{AuFactory, catalog, catalog_generation, rescan};
