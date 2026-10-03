//! Loading VST3 modules (bundles).
//!
//! A module is loaded once per process and never unloaded: plugins keep
//! static state (and threads) that would not survive `dlclose`, and every
//! engine of the process shares the same factory.

use faderframe_plugin_host::scan::ScanError;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vst3::ComPtr;
use vst3::Steinberg::IPluginFactory;

pub struct Module {
    pub bundle: PathBuf,
    pub factory: ComPtr<IPluginFactory>,
    _library: Option<libloading::Library>,
}

static MODULES: Mutex<Vec<&'static Module>> = Mutex::new(Vec::new());

fn arch_dir() -> String {
    format!("{}-linux", std::env::consts::ARCH)
}

/// The shared library inside a bundle
/// (`X.vst3/Contents/<arch>-linux/X.so`), or the bundle itself when it is a
/// plain file.
pub fn binary_path(bundle: &Path) -> Option<PathBuf> {
    if bundle.is_file() {
        return Some(bundle.to_path_buf());
    }
    let dir = bundle.join("Contents").join(arch_dir());
    let stem = bundle.file_stem()?;
    let named = dir.join(stem).with_extension("so");
    if named.is_file() {
        return Some(named);
    }
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "so"))
}

fn find(bundle: &Path) -> Option<&'static Module> {
    MODULES
        .lock()
        .ok()?
        .iter()
        .copied()
        .find(|m| m.bundle == bundle)
}

fn keep(m: Module) -> &'static Module {
    let m: &'static Module = Box::leak(Box::new(m));
    if let Ok(mut list) = MODULES.lock() {
        list.push(m);
    }
    m
}

/// Load `bundle` (once) and return its factory.
pub fn load(bundle: &Path) -> Result<&'static Module, ScanError> {
    if let Some(m) = find(bundle) {
        return Ok(m);
    }
    let fail = |e: String| ScanError::Load(bundle.to_path_buf(), e);
    let bin = binary_path(bundle).ok_or_else(|| fail("no Linux binary in the bundle".into()))?;
    use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
    // SAFETY: loading a plugin library runs its initialisers; the user chose
    // to load it (scanning happens in a helper process first).
    let unix = unsafe { Library::open(Some(&bin), RTLD_NOW | RTLD_LOCAL) }
        .map_err(|e| fail(e.to_string()))?;
    let handle = unix.into_raw();
    // SAFETY: `handle` came from `into_raw` just above.
    let library: libloading::Library = unsafe { Library::from_raw(handle) }.into();
    type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
    type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
    // SAFETY: the VST3 module ABI defines these symbols with these
    // signatures (ModuleEntry gets the dlopen handle on Linux).
    unsafe {
        if let Ok(entry) = library.get::<Entry>(b"ModuleEntry\0")
            && !entry(handle)
        {
            return Err(fail("ModuleEntry failed".into()));
        }
        let get = library
            .get::<GetFactory>(b"GetPluginFactory\0")
            .map_err(|_| ScanError::NoFactory(bundle.to_path_buf()))?;
        // GetPluginFactory returns a reference owned by the caller.
        let factory =
            ComPtr::from_raw(get()).ok_or_else(|| ScanError::NoFactory(bundle.to_path_buf()))?;
        Ok(keep(Module {
            bundle: bundle.to_path_buf(),
            factory,
            _library: Some(library),
        }))
    }
}

/// Use an in-process factory for `bundle` (tests: plugins written with the
/// `vst3` crate, without a shared library).
pub fn register(bundle: PathBuf, factory: ComPtr<IPluginFactory>) -> &'static Module {
    if let Some(m) = find(&bundle) {
        return m;
    }
    keep(Module {
        bundle,
        factory,
        _library: None,
    })
}
