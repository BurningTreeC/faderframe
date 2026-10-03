//! Format-independent plugin scanning: what a scan found, the scan cache
//! and the helper process that describes bundles.
//!
//! Plugins are third-party code: describing a bundle loads it, so it runs
//! in a helper process (`faderframe --scan-<format> <bundle>`) with a
//! timeout. Results are cached by bundle path, size and modification time
//! (of the files inside, for bundles that are directories), so only new or
//! changed bundles are scanned again.

use crate::{AudioPortInfo, PluginCategory, PluginDescriptor, PluginFormat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// Feature tags in CLAP's vocabulary ("instrument", "audio-effect",
    /// "equalizer", …); other formats map their categories onto it.
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

    pub fn category(&self) -> PluginCategory {
        use PluginCategory::*;
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

    /// Assume the usual stereo layout for the plugin's type when it reported
    /// no audio ports (bridged plugins may report them only later).
    pub fn default_ports_if_missing(&mut self) {
        if self.audio_inputs.is_empty() && self.audio_outputs.is_empty() {
            if self.is_instrument() {
                self.audio_outputs = vec![2];
            } else {
                self.audio_inputs = vec![2];
                self.audio_outputs = vec![2];
            }
        }
    }

    pub fn descriptor(&self, format: PluginFormat) -> PluginDescriptor {
        let ports = |list: &[u16]| {
            list.iter()
                .enumerate()
                .map(|(i, &c)| AudioPortInfo {
                    channels: c,
                    is_main: i == 0,
                })
                .collect()
        };
        PluginDescriptor {
            format,
            id: self.id.clone(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            version: self.version.clone(),
            category: self.category(),
            audio_inputs: ports(&self.audio_inputs),
            audio_outputs: ports(&self.audio_outputs),
            note_inputs: self.note_inputs,
            note_outputs: self.note_outputs,
        }
    }
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

/// Every bundle with extension `ext` below `paths`. Bundles that are
/// directories are not searched further.
pub fn find_bundles(paths: &[PathBuf], ext: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == ext) {
                out.push(p);
            } else if depth < 6 && p.is_dir() {
                walk(&p, ext, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        walk(p, ext, &mut out, 0);
    }
    out.sort();
    out.dedup();
    out
}

/// Entry point of a scan helper process: print the result as JSON on
/// stdout. Returns the process exit code.
pub fn print_scan_result(result: Result<Vec<ScannedPlugin>, ScanError>) -> i32 {
    match result {
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

/// Describe `bundle` by running `exe <flag> <bundle>`; a plugin that
/// crashes or hangs only takes the helper down.
pub fn scan_in_subprocess(
    exe: &Path,
    flag: &str,
    bundle: &Path,
    timeout: Duration,
) -> Result<Vec<ScannedPlugin>, ScanError> {
    let mut child = Command::new(exe)
        .arg(flag)
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ScanError::Scanner(format!("cannot start {}: {e}", exe.display())))?;
    // Drain the pipes while waiting: a chatty plugin must not block on a
    // full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
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
    let stdout = out.join().unwrap_or_default();
    if !status.success() {
        let stderr = err.join().unwrap_or_default();
        let last: Vec<&str> = stderr.lines().rev().take(3).collect();
        return Err(ScanError::Scanner(format!(
            "{} failed ({status}): {}",
            bundle.display(),
            last.into_iter().rev().collect::<Vec<_>>().join(" / ")
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

/// (Total size, latest modification) of a file, or of the files inside a
/// bundle directory.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    fn secs(m: &std::fs::Metadata) -> u64 {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    }
    fn walk(dir: &Path, acc: &mut (u64, u64), depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if depth < 4 {
                    walk(&e.path(), acc, depth + 1);
                }
            } else {
                acc.0 += m.len();
                acc.1 = acc.1.max(secs(&m));
            }
        }
    }
    let m = std::fs::metadata(path).ok()?;
    if m.is_dir() {
        let mut acc = (0, secs(&m));
        walk(path, &mut acc, 0);
        Some(acc)
    } else {
        Some((m.len(), secs(&m)))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_bundles_are_stamped_by_their_contents() {
        let dir = std::env::temp_dir().join(format!("ff-scan-stamp-{}", std::process::id()));
        let bin = dir.join("X.vst3/Contents/x86_64-linux");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("X.so"), b"abc").unwrap();
        let bundle = dir.join("X.vst3");
        let (size, _) = stamp(&bundle).unwrap();
        assert_eq!(size, 3);
        std::fs::write(bin.join("X.so"), b"abcdef").unwrap();
        assert_eq!(stamp(&bundle).unwrap().0, 6, "a changed binary is noticed");
        assert_eq!(
            find_bundles(std::slice::from_ref(&dir), "vst3"),
            vec![bundle]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
