//! Finding and describing CLAP plugins.
//!
//! Plugins are third-party code: describing a bundle loads it, so it runs
//! in a helper process (`faderframe --scan-clap <bundle>`, see
//! [`run_scan_subprocess`]) with a timeout. Results are cached by bundle
//! path, size and modification time, so only new or changed bundles are
//! scanned again.

use crate::host::{FfHost, FfMainThread, FfShared, host_info};
use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer};
use clack_host::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What we know about one plugin without instantiating it again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScannedPlugin {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub features: Vec<String>,
    pub bundle: PathBuf,
    /// Channel counts of the audio ports, main port first.
    pub audio_inputs: Vec<u16>,
    pub audio_outputs: Vec<u16>,
    pub note_inputs: u16,
    pub note_outputs: u16,
}

impl ScannedPlugin {
    pub fn is_instrument(&self) -> bool {
        self.features.iter().any(|f| f == "instrument")
    }

    pub fn category(&self) -> faderframe_plugin_host::PluginCategory {
        use faderframe_plugin_host::PluginCategory::*;
        let has = |f: &str| self.features.iter().any(|x| x == f);
        if has("instrument") {
            Instrument
        } else if has("analyzer") {
            Analyzer
        } else if has("audio-effect") {
            Effect
        } else {
            Utility
        }
    }
}

/// Standard CLAP search paths (the CLAP spec's list), `$CLAP_PATH` first.
pub fn default_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
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
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "clap") {
                out.push(p);
            } else if depth < 6 && p.is_dir() {
                walk(&p, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        walk(p, &mut out, 0);
    }
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("cannot load {0}: {1}")]
    Load(PathBuf, String),
    #[error("{0} has no plugin factory")]
    NoFactory(PathBuf),
    #[error("scanner: {0}")]
    Scanner(String),
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
        // Bridged plugins (yabridge) may not report ports until later:
        // assume the usual stereo layout for their type.
        if p.audio_inputs.is_empty() && p.audio_outputs.is_empty() {
            if p.is_instrument() {
                p.audio_outputs = vec![2];
            } else {
                p.audio_inputs = vec![2];
                p.audio_outputs = vec![2];
            }
        }
        out.push(p);
    }
    Ok(out)
}

/// Entry point of the scan helper process: describe one bundle as JSON on
/// stdout. Returns the process exit code.
pub fn run_scan_subprocess(bundle: &Path) -> i32 {
    match describe_bundle(bundle) {
        Ok(plugins) => match serde_json::to_string(&plugins) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Describe `bundle` by running `exe --scan-clap <bundle>`; a plugin that
/// crashes or hangs only takes the helper down.
pub fn scan_in_subprocess(
    exe: &Path,
    bundle: &Path,
    timeout: Duration,
) -> Result<Vec<ScannedPlugin>, ScanError> {
    let mut child = Command::new(exe)
        .arg("--scan-clap")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ScanError::Scanner(format!("cannot start {}: {e}", exe.display())))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ScanError::Scanner(format!(
                    "{} timed out",
                    bundle.display()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(ScanError::Scanner(e.to_string())),
        }
    };
    let mut stdout = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut stdout);
    }
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        return Err(ScanError::Scanner(format!(
            "{} failed ({status}): {}",
            bundle.display(),
            stderr.trim()
        )));
    }
    // Plugins may print to stdout themselves: the JSON is the last line.
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with('['))
        .unwrap_or("[]");
    serde_json::from_str(line).map_err(|e| ScanError::Scanner(format!("{}: {e}", bundle.display())))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    plugins: Vec<ScannedPlugin>,
    #[serde(default)]
    error: Option<String>,
}

/// Scan results by bundle path.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanCache {
    bundles: BTreeMap<PathBuf, CacheEntry>,
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((m.len(), mtime))
}

impl ScanCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// Bring the cache up to date with `bundles`, scanning new or changed
    /// ones with `scan`. Returns the errors of bundles that failed.
    pub fn update(
        &mut self,
        bundles: &[PathBuf],
        mut scan: impl FnMut(&Path) -> Result<Vec<ScannedPlugin>, ScanError>,
    ) -> Vec<String> {
        self.bundles.retain(|p, _| bundles.contains(p));
        let mut errors = Vec::new();
        for b in bundles {
            let Some((size, mtime)) = stamp(b) else {
                continue;
            };
            if self
                .bundles
                .get(b)
                .is_some_and(|e| e.size == size && e.mtime == mtime)
            {
                continue;
            }
            let (plugins, error) = match scan(b) {
                Ok(p) => (p, None),
                Err(e) => {
                    errors.push(e.to_string());
                    (Vec::new(), Some(e.to_string()))
                }
            };
            self.bundles.insert(
                b.clone(),
                CacheEntry {
                    size,
                    mtime,
                    plugins,
                    error,
                },
            );
        }
        errors
    }

    pub fn plugins(&self) -> impl Iterator<Item = &ScannedPlugin> {
        self.bundles.values().flat_map(|e| e.plugins.iter())
    }

    /// Bundles that failed to scan, with the reason.
    pub fn failures(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.bundles
            .iter()
            .filter_map(|(p, e)| e.error.as_deref().map(|err| (p.as_path(), err)))
    }
}
