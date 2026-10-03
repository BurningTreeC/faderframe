//! Loading VST3 modules (bundles).
//!
//! A module is loaded once per process and never unloaded: plugins keep
//! static state (and threads) that would not survive `dlclose`, and every
//! engine of the process shares the same factory.

use faderframe_plugin_host::scan::ScanError;
#[cfg(any(unix, target_os = "macos"))]
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

/// The bundle folder holding this platform's binary and its extension
/// (empty: none, as on macOS).
fn arch_dir() -> (String, &'static str) {
    let arch = std::env::consts::ARCH;
    if cfg!(windows) {
        (format!("{arch}-win"), "vst3")
    } else if cfg!(target_os = "macos") {
        ("MacOS".into(), "")
    } else {
        (format!("{arch}-linux"), "so")
    }
}

/// The shared library inside a bundle (`X.vst3/Contents/<arch>-linux/X.so`,
/// `X.vst3/Contents/<arch>-win/X.vst3`, `X.vst3/Contents/MacOS/X`), or the
/// bundle itself when it is a plain file.
pub fn binary_path(bundle: &Path) -> Option<PathBuf> {
    if bundle.is_file() {
        return Some(bundle.to_path_buf());
    }
    let (arch, ext) = arch_dir();
    let dir = bundle.join("Contents").join(arch);
    let stem = bundle.file_stem()?;
    let named = dir.join(stem).with_extension(ext);
    if named.is_file() {
        return Some(named);
    }
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file() && (ext.is_empty() || p.extension().is_some_and(|x| x == ext)))
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
    let bin = binary_path(bundle)
        .ok_or_else(|| fail("no binary for this platform in the bundle".into()))?;
    let library = open_library(bundle, &bin).map_err(fail)?;
    type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
    // SAFETY: the VST3 module ABI defines this symbol with this signature.
    unsafe {
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

/// Open the module and run its platform entry point: `ModuleEntry` with
/// the dlopen handle (Linux), `InitDll` (Windows, optional) or
/// `bundleEntry` with the bundle's `CFBundleRef` (macOS).
#[cfg(all(unix, not(target_os = "macos")))]
fn open_library(_bundle: &Path, bin: &Path) -> Result<libloading::Library, String> {
    use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
    // SAFETY: loading a plugin library runs its initialisers; the user chose
    // to load it (scanning happens in a helper process first).
    let unix =
        unsafe { Library::open(Some(bin), RTLD_NOW | RTLD_LOCAL) }.map_err(|e| e.to_string())?;
    let handle = unix.into_raw();
    // SAFETY: `handle` came from `into_raw` just above.
    let library: libloading::Library = unsafe { Library::from_raw(handle) }.into();
    type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
    // SAFETY: the VST3 ABI's Linux entry point, given the dlopen handle.
    unsafe {
        if let Ok(entry) = library.get::<Entry>(b"ModuleEntry\0")
            && !entry(handle)
        {
            return Err("ModuleEntry failed".into());
        }
    }
    Ok(library)
}

#[cfg(windows)]
fn open_library(_bundle: &Path, bin: &Path) -> Result<libloading::Library, String> {
    // SAFETY: as above (LoadLibrary runs DllMain).
    let library = unsafe { libloading::Library::new(bin) }.map_err(|e| e.to_string())?;
    type Entry = unsafe extern "system" fn() -> bool;
    // SAFETY: the VST3 ABI's optional Windows entry point.
    unsafe {
        if let Ok(entry) = library.get::<Entry>(b"InitDll\0")
            && !entry()
        {
            return Err("InitDll failed".into());
        }
    }
    Ok(library)
}

#[cfg(target_os = "macos")]
fn open_library(bundle: &Path, bin: &Path) -> Result<libloading::Library, String> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: as above.
    let library = unsafe { libloading::Library::new(bin) }.map_err(|e| e.to_string())?;
    let path = bundle.as_os_str().as_bytes();
    // SAFETY: CoreFoundation calls with a valid byte buffer; the bundle ref
    // is kept for the life of the process like the module itself.
    let cf_bundle = unsafe {
        let url = cf::CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            path.as_ptr(),
            path.len() as isize,
            1,
        );
        if url.is_null() {
            return Err("invalid bundle path".into());
        }
        let b = cf::CFBundleCreate(std::ptr::null(), url);
        cf::CFRelease(url);
        b
    };
    if cf_bundle.is_null() {
        return Err("not a bundle".into());
    }
    type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
    // SAFETY: the VST3 ABI's macOS entry point, given the CFBundleRef.
    unsafe {
        if let Ok(entry) = library.get::<Entry>(b"bundleEntry\0")
            && !entry(cf_bundle)
        {
            return Err("bundleEntry failed".into());
        }
    }
    Ok(library)
}

#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> *mut c_void;
        pub fn CFBundleCreate(allocator: *const c_void, url: *mut c_void) -> *mut c_void;
        pub fn CFRelease(cf: *mut c_void);
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
