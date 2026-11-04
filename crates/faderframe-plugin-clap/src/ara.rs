//! ARA through CLAP (`ARACLAP.h`): the entry's ARA factory (which plugins
//! of a bundle are ARA plugins) and a plugin instance's ARA extension (its
//! factory, binding it to a document controller).

use crate::host::FfHost;
use clack_host::factory::{Factory, RawFactoryPointer};
use clack_host::prelude::{PluginEntry, PluginInstance};
use std::ffi::{CStr, c_char, c_void};

/// Plugins advertise ARA with these features.
pub const FEATURE: &str = "ara:supported";
pub const FEATURE_REQUIRED: &str = "ara:required";

const PLUGIN_EXTENSION: &CStr = c"org.ara-audio.ara.pluginextension/2";
/// The draft's identifier (100 % compatible; plug-ins may still offer only
/// it until 2026).
const PLUGIN_EXTENSION_DRAFT: &CStr = c"org.ara-audio.ara.pluginextension.draft/2";

/// `clap_ara_factory_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawAraFactory {
    pub get_factory_count: Option<unsafe extern "C" fn(factory: *const RawAraFactory) -> u32>,
    pub get_ara_factory:
        Option<unsafe extern "C" fn(factory: *const RawAraFactory, index: u32) -> *const c_void>,
    pub get_plugin_id:
        Option<unsafe extern "C" fn(factory: *const RawAraFactory, index: u32) -> *const c_char>,
}

/// `clap_ara_plugin_extension_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawPluginExtension {
    get_factory: Option<unsafe extern "C" fn(plugin: *const c_void) -> *const c_void>,
    bind_to_document_controller: Option<
        unsafe extern "C" fn(
            plugin: *const c_void,
            controller: *mut c_void,
            known: i32,
            assigned: i32,
        ) -> *const c_void,
    >,
}

/// The entry's ARA factory under its final identifier.
#[derive(Clone, Copy)]
pub struct AraFactory<'a>(RawFactoryPointer<'a, RawAraFactory>);

// SAFETY: the identifier is ARACLAP.h's CLAP_EXT_ARA_FACTORY for
// `clap_ara_factory_t`.
unsafe impl<'a> Factory<'a> for AraFactory<'a> {
    const IDENTIFIERS: &'static [&'static CStr] = &[c"org.ara-audio.ara.factory/2"];
    type Raw = RawAraFactory;

    unsafe fn from_raw(raw: RawFactoryPointer<'a, Self::Raw>) -> Self {
        Self(raw)
    }
}

/// The same under the draft identifier.
#[derive(Clone, Copy)]
pub struct AraFactoryDraft<'a>(RawFactoryPointer<'a, RawAraFactory>);

// SAFETY: CLAP_EXT_ARA_FACTORY_COMPAT, the same struct.
unsafe impl<'a> Factory<'a> for AraFactoryDraft<'a> {
    const IDENTIFIERS: &'static [&'static CStr] = &[c"org.ara-audio.ara.factory.draft/2"];
    type Raw = RawAraFactory;

    unsafe fn from_raw(raw: RawFactoryPointer<'a, Self::Raw>) -> Self {
        Self(raw)
    }
}

fn ids_of(raw: &RawFactoryPointer<'_, RawAraFactory>) -> Vec<String> {
    let f = raw.get();
    let (Some(count), Some(id)) = (f.get_factory_count, f.get_plugin_id) else {
        return Vec::new();
    };
    let p = raw.as_ptr();
    // SAFETY: the entry's factory, valid while the entry is loaded; ids are
    // strings it owns.
    unsafe {
        (0..count(p).min(4096))
            .filter_map(|i| {
                let s = id(p, i);
                (!s.is_null()).then(|| CStr::from_ptr(s).to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// The ids of the entry's ARA plugins.
pub fn plugin_ids(entry: &PluginEntry) -> Vec<String> {
    if let Some(f) = entry.get_factory::<AraFactory>() {
        return ids_of(&f.0);
    }
    entry
        .get_factory::<AraFactoryDraft>()
        .map(|f| ids_of(&f.0))
        .unwrap_or_default()
}

/// The instance's ARA plug-in extension.
fn extension(instance: &PluginInstance<FfHost>) -> Option<(*const c_void, RawPluginExtension)> {
    let plugin = instance.raw_instance();
    let get = plugin.get_extension?;
    let raw = plugin as *const _ as *const c_void;
    for id in [PLUGIN_EXTENSION, PLUGIN_EXTENSION_DRAFT] {
        // SAFETY: a live plugin, main thread, after init (extensions may be
        // queried then).
        let ext = unsafe { get(plugin, id.as_ptr()) } as *const RawPluginExtension;
        if !ext.is_null() {
            // SAFETY: the plugin's extension struct for that identifier.
            return Some((raw, unsafe { ext.read() }));
        }
    }
    None
}

/// The instance's ARA factory.
pub fn factory(instance: &PluginInstance<FfHost>) -> Option<faderframe_ara::Factory> {
    let (plugin, ext) = extension(instance)?;
    let get = ext.get_factory?;
    // SAFETY: a live plugin; the factory lives as long as the entry, which
    // FaderFrame never unloads.
    unsafe { faderframe_ara::Factory::from_raw(get(plugin)) }
}

/// Bind the instance to an ARA document controller (once, before it is
/// activated, loads state or opens its GUI).
pub fn bind(
    instance: &PluginInstance<FfHost>,
    controller: faderframe_ara::ControllerRef,
    roles: faderframe_ara::Roles,
) -> Option<faderframe_ara::Extension> {
    let (plugin, ext) = extension(instance)?;
    let bind = ext.bind_to_document_controller?;
    // SAFETY: a live plugin and a live controller, main thread, called once;
    // the extension it returns lives as long as the instance, and the
    // engine drops its copy before the instance.
    unsafe {
        let raw = bind(
            plugin,
            controller.as_raw(),
            faderframe_ara::Roles::ALL.bits(),
            roles.bits(),
        );
        faderframe_ara::Extension::from_raw(raw)
    }
}
