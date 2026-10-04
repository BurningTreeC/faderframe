//! Finding and describing CLAP plugins (in the `faderframe --scan-clap
//! <bundle>` helper process, see [`run_scan_subprocess`]; caching and the
//! helper protocol are in [`faderframe_plugin_host::scan`]).

use crate::host::{FfHost, FfMainThread, FfShared, host_info};
use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer};
use clack_host::prelude::*;
pub use faderframe_plugin_host::scan::{ScanCache, ScanError, ScannedPlugin};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Standard CLAP search paths (the CLAP spec's list), `$CLAP_PATH` and a
/// portable installation's `Plug-Ins/CLAP` first.
pub fn default_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    out.extend(faderframe_core::paths::portable_plugins("CLAP"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "linux") {
        if let Some(h) = &home {
            out.push(h.join(".clap"));
        }
        out.push("/usr/lib/clap".into());
        out.push("/usr/local/lib/clap".into());
    } else if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            out.push(h.join("Library/Audio/Plug-Ins/CLAP"));
        }
        out.push("/Library/Audio/Plug-Ins/CLAP".into());
    } else if cfg!(windows) {
        if let Some(p) = std::env::var_os("COMMONPROGRAMFILES") {
            out.push(PathBuf::from(p).join("CLAP"));
        }
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            out.push(PathBuf::from(p).join("Programs/Common/CLAP"));
        }
    }
    out
}

/// Every `.clap` bundle below `paths` (bundles are files on Linux and
/// Windows, directories on macOS).
pub fn find_bundles(paths: &[PathBuf]) -> Vec<PathBuf> {
    faderframe_plugin_host::scan::find_bundles(paths, "clap")
}

/// Load `bundle` *in this process* and describe its plugins. Use it only in
/// the scan helper process.
pub fn describe_bundle(bundle: &Path) -> Result<Vec<ScannedPlugin>, ScanError> {
    // SAFETY: loading a plugin library runs its code; this is why it is done
    // in the throw-away scan helper process.
    let entry = unsafe { PluginEntry::load(bundle) }
        .map_err(|e| ScanError::Load(bundle.to_path_buf(), e.to_string()))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| ScanError::NoFactory(bundle.to_path_buf()))?;
    let info = host_info().map_err(|e| ScanError::Scanner(e.to_string()))?;
    let mut out = Vec::new();
    for d in factory.plugin_descriptors() {
        let Some(id) = d.id() else { continue };
        let text = |c: Option<&std::ffi::CStr>| {
            c.map(|c| c.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let features: Vec<String> = d
            .features()
            .map(|f| f.to_string_lossy().to_string())
            .collect();
        let mut p = ScannedPlugin {
            id: id.to_string_lossy().to_string(),
            name: text(d.name()),
            vendor: text(d.vendor()),
            version: text(d.version()),
            features,
            bundle: bundle.to_path_buf(),
            audio_inputs: Vec::new(),
            audio_outputs: Vec::new(),
            note_inputs: 0,
            note_outputs: 0,
        };
        // Ports need an instance.
        let id = CString::from(id);
        if let Ok(mut instance) = PluginInstance::<FfHost>::new(
            |_| FfShared::new(),
            |shared| FfMainThread::new(shared),
            &entry,
            &id,
            &info,
        ) {
            crate::host::refresh_extensions(&mut instance);
            let ext = instance.access_shared_handler(|s| s.ext());
            let handle = instance.plugin_handle();
            if let Some(ports) = ext.audio_ports {
                let mut buffer = AudioPortInfoBuffer::new();
                for is_input in [true, false] {
                    let mut channels: Vec<(bool, u16)> = Vec::new();
                    for i in 0..ports.count(&handle, is_input) {
                        if let Some(info) = ports.get(&handle, i, is_input, &mut buffer) {
                            channels.push((
                                info.flags.contains(AudioPortFlags::IS_MAIN),
                                info.channel_count as u16,
                            ));
                        }
                    }
                    // Main port first.
                    channels.sort_by_key(|(main, _)| !main);
                    let list: Vec<u16> = channels.into_iter().map(|(_, c)| c).collect();
                    if is_input {
                        p.audio_inputs = list;
                    } else {
                        p.audio_outputs = list;
                    }
                }
            }
            if let Some(notes) = ext.note_ports {
                p.note_inputs = notes.count(&handle, true) as u16;
                p.note_outputs = notes.count(&handle, false) as u16;
            }
        } else if p.is_instrument() {
            p.audio_outputs = vec![2];
            p.note_inputs = 1;
        } else {
            p.audio_inputs = vec![2];
            p.audio_outputs = vec![2];
        }
        // Bridged plugins (yabridge) may not report ports until later.
        p.default_ports_if_missing();
        out.push(p);
    }
    Ok(out)
}

/// Entry point of the scan helper process: describe one bundle as JSON on
/// stdout. Returns the process exit code.
pub fn run_scan_subprocess(bundle: &Path) -> i32 {
    faderframe_plugin_host::scan::print_scan_result(describe_bundle(bundle))
}

/// Describe `bundle` by running `exe --scan-clap <bundle>`.
pub fn scan_in_subprocess(
    exe: &Path,
    bundle: &Path,
    timeout: Duration,
) -> Result<Vec<ScannedPlugin>, ScanError> {
    faderframe_plugin_host::scan::scan_in_subprocess(exe, "--scan-clap", bundle, timeout)
}
