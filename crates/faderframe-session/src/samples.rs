//! Samples for the built-in samplers: the files picked for a slot are
//! copied into the project's media folder (an SFZ instrument is referenced
//! where it lies, with its samples) and decoded on a worker thread; once
//! done, the sampler's new state is one undo step ("Load Samples"), and
//! loads instantly from the shared sample cache. The paths in samplers'
//! states are absolute in memory, like media sources: relative to the
//! project folder in the file, and moved with the media on the first save.

use faderframe_core::{PluginInstanceId, builtin};
use faderframe_plugin_host::devices::samples::{self, SampleDoc, SampleSet};
use faderframe_project::{PluginFormat, PluginSlot, Project};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

/// The subfolder of the media folder samples are copied to.
pub const SAMPLES_FOLDER: &str = "Samples";

/// Whether a slot holds one of the samplers.
pub fn is_sampler(slot: &PluginSlot) -> bool {
    slot.plugin.format == PluginFormat::Builtin
        && matches!(slot.plugin.id.as_str(), builtin::SAMPLER | builtin::DRUMS)
}

/// The document a sampler's slot state carries (empty without one).
pub fn doc_of(slot: &PluginSlot) -> SampleDoc {
    slot.state
        .as_deref()
        .and_then(faderframe_engine::decode_state)
        .and_then(|b| samples::unpack(&b).map(|(_, d)| d))
        .unwrap_or_default()
}

/// Map every file every sampler of `project` names through `f` (states
/// whose paths do not change are left alone). Returns whether any did.
pub fn map_states(project: &mut Project, f: impl Fn(&Path) -> PathBuf) -> bool {
    let mut changed = false;
    let slots = project.tracks.iter_mut().flat_map(|t| {
        t.instrument
            .iter_mut()
            .chain(t.preamp.iter_mut())
            .chain(t.inserts.iter_mut())
    });
    for slot in slots.filter(|s| is_sampler(s)) {
        let Some(bytes) = slot
            .state
            .as_deref()
            .and_then(faderframe_engine::decode_state)
        else {
            continue;
        };
        let Some(mapped) = samples::map_paths(&bytes, &f) else {
            continue;
        };
        if mapped != bytes {
            slot.state = Some(faderframe_engine::encode_state(&mapped));
            changed = true;
        }
    }
    changed
}

/// A copy of `file` in `dir` (under its own name, made unique), unless it
/// is in the media folder already.
fn copy_in(file: &Path, dir: &Path, media: &Path) -> std::io::Result<PathBuf> {
    if file.starts_with(media) {
        return Ok(file.to_path_buf());
    }
    std::fs::create_dir_all(dir)?;
    let stem = file.file_stem().map_or_else(
        || "sample".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let ext = file
        .extension()
        .map_or_else(|| "wav".to_string(), |e| e.to_string_lossy().into_owned());
    let mut to = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while to.exists() {
        // The same file picked again: use the copy there is.
        if std::fs::metadata(&to).map(|m| m.len()).ok()
            == std::fs::metadata(file).map(|m| m.len()).ok()
            && std::fs::read(&to).ok() == std::fs::read(file).ok()
        {
            return Ok(to);
        }
        to = dir.join(format!("{stem} {n}.{ext}"));
        n += 1;
    }
    std::fs::copy(file, &to)?;
    Ok(to)
}

/// What a finished job hands back.
pub struct Loaded {
    pub plugin: PluginInstanceId,
    /// The slots filled and their files.
    pub assigned: Vec<(usize, String)>,
    /// Keeps the decoded samples in the cache until the state is applied.
    pub set: Arc<SampleSet>,
    pub notes: Vec<String>,
}

/// Copies and decodes picked files for a sampler.
pub struct SampleJob {
    pub plugin: PluginInstanceId,
    handle: Option<JoinHandle<Loaded>>,
}

impl SampleJob {
    /// Load `files` for the slots from `slot` on (one each), copied into
    /// `media`'s samples folder.
    pub fn spawn(
        plugin: PluginInstanceId,
        slot: usize,
        files: Vec<PathBuf>,
        media: PathBuf,
    ) -> Self {
        let handle = std::thread::Builder::new()
            .name("faderframe-samples".into())
            .spawn(move || {
                let mut assigned = Vec::new();
                let mut notes = Vec::new();
                let dir = media.join(SAMPLES_FOLDER);
                for (k, file) in files.iter().enumerate() {
                    let path = if samples::is_sfz(file) {
                        Ok(file.clone())
                    } else {
                        copy_in(file, &dir, &media)
                    };
                    match path {
                        Ok(p) => assigned.push((slot + k, p.to_string_lossy().into_owned())),
                        Err(e) => notes.push(format!("could not copy {}: {e}", file.display())),
                    }
                }
                // Decoded here, they wait in the cache for the instance.
                let set = Arc::new(samples::load(&SampleDoc {
                    files: assigned.iter().map(|(_, f)| Some(f.clone())).collect(),
                }));
                notes.extend(set.errors.iter().cloned());
                Loaded {
                    plugin,
                    assigned,
                    set,
                    notes,
                }
            })
            .ok();
        Self { plugin, handle }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn join(mut self) -> Option<Loaded> {
        self.handle.take().and_then(|h| h.join().ok())
    }
}

/// Move the samples a never-saved project copied into its scratch media
/// folder into the project's (`from` and `to` are the media folders).
/// Returns the moves (old → new path).
pub fn move_folder(from: &Path, to: &Path) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let src = from.join(SAMPLES_FOLDER);
    let Ok(entries) = std::fs::read_dir(&src) else {
        return Ok(Vec::new());
    };
    let dst = to.join(SAMPLES_FOLDER);
    std::fs::create_dir_all(&dst)?;
    let mut moves = Vec::new();
    for e in entries.flatten() {
        let old = e.path();
        if !old.is_file() {
            continue;
        }
        let name = old
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        let mut new = dst.join(&name);
        let mut n = 2;
        while new.exists() {
            let stem = old
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            let ext = old
                .extension()
                .map_or_else(String::new, |s| format!(".{}", s.to_string_lossy()));
            new = dst.join(format!("{stem} {n}{ext}"));
            n += 1;
        }
        if std::fs::rename(&old, &new).is_err() {
            std::fs::copy(&old, &new)?;
            let _ = std::fs::remove_file(&old);
        }
        moves.push((old, new));
    }
    Ok(moves)
}
