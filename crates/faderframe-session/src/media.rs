//! Media files: where they live, opening them for streaming, the disk
//! loader thread, background imports and waveform jobs.

use faderframe_audio_files::import::{
    ImportError, ImportProgress, ImportedAudio, import_file, peaks_path_for,
};
use faderframe_audio_files::wavstream::WavFile;
use faderframe_audio_files::{PAGE_FRAMES, Page, PeakBuilder, PeakCache, StreamSource};
use faderframe_core::AudioSourceId;
use faderframe_engine::{EngineShared, StreamPlan};
use faderframe_realtime::Reclaimer;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Per-user data directory (`$XDG_DATA_HOME/faderframe` on Linux; the
/// portable folder's `Data` in portable mode).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = faderframe_core::paths::portable("Data") {
        return dir;
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else if cfg!(target_os = "macos") {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library/Application Support"))
            } else {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
            }
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("faderframe")
}

/// A fresh folder for media of a project that has not been saved yet
/// (`<pid>-<millis>-<n>`, unique within and across processes).
pub fn new_unsaved_media_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    data_dir()
        .join("unsaved")
        .join(format!("{}-{stamp}-{n}", std::process::id()))
}

/// Remove scratch media folders of sessions that are no longer running
/// (crashed or killed) once they are older than `max_age`. Returns how many
/// folders were removed.
pub fn sweep_stale_scratch(max_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(data_dir().join("unsaved")) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let pid = name.split('-').next().and_then(|p| p.parse::<u32>().ok());
        if pid == Some(std::process::id()) {
            continue;
        }
        let alive = cfg!(target_os = "linux")
            && pid.is_some_and(|p| Path::new(&format!("/proc/{p}")).exists());
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if !alive && old && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Resolve a stored media path against the project directory. Relative
/// paths use `/` (projects move between systems); a `\` from a file
/// written on Windows is read as a separator too.
pub fn resolve(stored: &Path, project_dir: Option<&Path>) -> PathBuf {
    if stored.is_absolute() {
        return stored.to_path_buf();
    }
    let text = stored.to_string_lossy();
    let portable = if cfg!(windows) || !text.contains('\\') {
        stored.to_path_buf()
    } else {
        PathBuf::from(text.replace('\\', "/"))
    };
    match project_dir {
        Some(dir) => dir.join(portable),
        None => portable,
    }
}

/// Store paths inside the project directory relatively, with `/` as the
/// separator on every system.
pub fn to_stored(path: &Path, project_dir: Option<&Path>) -> PathBuf {
    match project_dir.and_then(|d| path.strip_prefix(d).ok()) {
        Some(rel) => PathBuf::from(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        ),
        None => path.to_path_buf(),
    }
}

/// Load the peak cache next to a media file, or `None` if absent/stale.
pub fn load_peaks(media: &Path) -> Option<PeakCache> {
    let f = WavFile::open(media).ok()?;
    PeakCache::load(&peaks_path_for(media), f.frames() as usize, f.channels()).ok()
}

/// Compute (and save) the peak cache of a media file by reading it.
pub fn build_peaks(media: &Path) -> std::io::Result<PeakCache> {
    let f = WavFile::open(media)?;
    let mut b = PeakBuilder::new(f.channels());
    let chunk = 1 << 16;
    let mut bufs = vec![vec![0.0f32; chunk]; f.channels()];
    let mut scratch = Vec::new();
    let mut pos = 0u64;
    while pos < f.frames() {
        let n = chunk.min((f.frames() - pos) as usize);
        {
            let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|c| &mut c[..n]).collect();
            f.read(pos, &mut slices, &mut scratch)?;
        }
        let views: Vec<&[f32]> = bufs.iter().map(|c| &c[..n]).collect();
        b.push(&views, n);
        pos += n as u64;
    }
    let peaks = b.finish();
    let _ = peaks.save(&peaks_path_for(media));
    Ok(peaks)
}

// --- disk loader -------------------------------------------------------------

/// How far ahead of the playhead pages are kept resident.
const AHEAD_SECONDS: f64 = 3.0;

struct LoaderState {
    plan: Arc<StreamPlan>,
    engine: Arc<EngineShared>,
    loop_range: Option<(i64, i64)>,
    sample_rate: u32,
    /// Album playback's file (kept resident round its own position).
    preview: Option<Arc<StreamSource>>,
}

struct LoaderShared {
    state: Mutex<LoaderState>,
    wake: Condvar,
    stop: AtomicBool,
    pages_loaded: AtomicU64,
    errors: AtomicU64,
    resident_bytes: AtomicUsize,
}

/// The "butler": a non-realtime thread that keeps the pages around the
/// playhead (and the loop start) resident and evicts the rest.
pub struct DiskLoader {
    shared: Arc<LoaderShared>,
    thread: Option<JoinHandle<()>>,
}

impl DiskLoader {
    pub fn start(plan: Arc<StreamPlan>, engine: Arc<EngineShared>, sample_rate: u32) -> Self {
        let shared = Arc::new(LoaderShared {
            state: Mutex::new(LoaderState {
                plan,
                engine,
                loop_range: None,
                sample_rate,
                preview: None,
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            pages_loaded: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            resident_bytes: AtomicUsize::new(0),
        });
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("faderframe-disk".into())
            .spawn(move || Self::run(&s))
            .ok();
        Self { shared, thread }
    }

    fn run(shared: &LoaderShared) {
        let mut reclaimer: Reclaimer<Page> = Reclaimer::default();
        let mut scratch = Vec::new();
        let mut tick = 0u64;
        while !shared.stop.load(Ordering::Relaxed) {
            // Every engine of a session shares one epoch (see
            // `faderframe_engine::create_with_epoch`), so retired pages stay
            // valid across engine replacement.
            let (plan, engine, loop_range, rate, preview) = {
                let Ok(guard) = shared.state.lock() else {
                    return;
                };
                let st = &*guard;
                (
                    Arc::clone(&st.plan),
                    Arc::clone(&st.engine),
                    st.loop_range,
                    st.sample_rate,
                    st.preview.clone(),
                )
            };
            if let Some(src) = &preview {
                let pos = engine.preview.position();
                let ahead = (AHEAD_SECONDS * f64::from(src.sample_rate().max(1))) as i64;
                let range = src.page_range(pos - PAGE_FRAMES as i64, pos + ahead);
                match src.ensure(range.clone(), &engine.epoch, &mut reclaimer, &mut scratch) {
                    Ok(n) => {
                        shared.pages_loaded.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(e) => {
                        shared.errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("album playback: {e}");
                    }
                }
                if tick.is_multiple_of(10) {
                    let keep = range.start.saturating_sub(1)..range.end + 1;
                    src.evict_except(&[keep], &engine.epoch, &mut reclaimer);
                }
            }
            if !plan.is_empty() {
                let pos = engine.transport.snapshot().position;
                let ahead = (AHEAD_SECONDS * rate as f64) as i64;
                let mut windows = vec![(pos - PAGE_FRAMES as i64, pos + ahead)];
                if let Some((a, _)) = loop_range {
                    windows.push((a, a + ahead));
                }
                match plan.ensure(&windows, &engine.epoch, &mut reclaimer, &mut scratch) {
                    Ok(n) => {
                        shared.pages_loaded.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(e) => {
                        shared.errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("disk streaming: {e}");
                    }
                }
                if tick.is_multiple_of(10) {
                    let margin = rate as i64;
                    let keep: Vec<(i64, i64)> = windows
                        .iter()
                        .map(|&(a, b)| (a - margin, b + margin))
                        .collect();
                    plan.evict_outside(&keep, &engine.epoch, &mut reclaimer);
                }
                shared
                    .resident_bytes
                    .store(plan.resident_bytes(), Ordering::Relaxed);
            }
            reclaimer.collect(&engine.epoch);
            tick += 1;
            let Ok(guard) = shared.state.lock() else {
                return;
            };
            let _ = shared.wake.wait_timeout(guard, Duration::from_millis(10));
        }
    }

    /// Publish a new plan / engine / loop range.
    pub fn update(
        &self,
        plan: Arc<StreamPlan>,
        engine: Arc<EngineShared>,
        loop_range: Option<(i64, i64)>,
        rate: u32,
    ) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.plan = plan;
            st.engine = engine;
            st.loop_range = loop_range;
            st.sample_rate = rate;
        }
        self.shared.wake.notify_all();
    }

    pub fn wake(&self) {
        self.shared.wake.notify_all();
    }

    /// Keep album playback's file resident (or stop).
    pub fn set_preview(&self, preview: Option<Arc<StreamSource>>) {
        if let Ok(mut st) = self.shared.state.lock() {
            st.preview = preview;
        }
        self.shared.wake.notify_all();
    }

    pub fn resident_bytes(&self) -> usize {
        self.shared.resident_bytes.load(Ordering::Relaxed)
    }

    pub fn errors(&self) -> u64 {
        self.shared.errors.load(Ordering::Relaxed)
    }
}

impl Drop for DiskLoader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// --- background jobs -----------------------------------------------------------

/// Where imported files go on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportTarget {
    /// Put the first file on this (audio) track; others get new tracks.
    pub track: Option<faderframe_core::TrackId>,
    pub at: faderframe_timeline::MusicalTime,
}

#[derive(Default)]
pub struct JobProgress {
    pub index: AtomicUsize,
    pub total: AtomicUsize,
    pub file: ImportProgress,
    pub cancel: AtomicBool,
}

pub type ImportResult = Result<ImportedAudio, (PathBuf, ImportError)>;

pub struct ImportJob {
    pub target: ImportTarget,
    pub progress: Arc<JobProgress>,
    pub names: Vec<String>,
    handle: Option<JoinHandle<Vec<ImportResult>>>,
}

impl ImportJob {
    pub fn spawn(files: Vec<PathBuf>, dest: PathBuf, rate: u32, target: ImportTarget) -> Self {
        let progress = Arc::new(JobProgress::default());
        progress.total.store(files.len(), Ordering::Relaxed);
        let names = files
            .iter()
            .map(|f| {
                f.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().to_string())
            })
            .collect();
        let p = Arc::clone(&progress);
        let handle = std::thread::Builder::new()
            .name("faderframe-import".into())
            .spawn(move || {
                let mut out = Vec::new();
                for (i, f) in files.iter().enumerate() {
                    p.index.store(i, Ordering::Relaxed);
                    p.file.done.store(0, Ordering::Relaxed);
                    match import_file(f, &dest, rate, &p.file, &p.cancel) {
                        Ok(a) => out.push(Ok(a)),
                        Err(ImportError::Cancelled) => break,
                        Err(e) => out.push(Err((f.clone(), e))),
                    }
                }
                out
            })
            .ok();
        Self {
            target,
            progress,
            names,
            handle,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Vec<ImportResult> {
        self.handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }

    /// Overall progress 0..=1.
    pub fn fraction(&self) -> f64 {
        let total = self.progress.total.load(Ordering::Relaxed).max(1);
        let i = self.progress.index.load(Ordering::Relaxed);
        (i as f64 + self.progress.file.fraction()) / total as f64
    }

    pub fn current_name(&self) -> &str {
        let i = self.progress.index.load(Ordering::Relaxed);
        self.names.get(i).map_or("", String::as_str)
    }
}

/// Builds missing waveform caches in the background.
pub struct PeakJob {
    pub source: AudioSourceId,
    handle: Option<JoinHandle<std::io::Result<PeakCache>>>,
}

impl PeakJob {
    pub fn spawn(source: AudioSourceId, media: PathBuf) -> Self {
        let handle = std::thread::Builder::new()
            .name("faderframe-peaks".into())
            .spawn(move || build_peaks(&media))
            .ok();
        Self { source, handle }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Option<PeakCache> {
        self.handle
            .take()
            .and_then(|h| h.join().ok())
            .and_then(Result::ok)
    }
}

/// A `.wav` path in `dir` named after `stem` that does not exist yet.
pub fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    faderframe_audio_files::import::unique_path(dir, stem, "wav")
}

/// Open a media file for streaming.
pub fn open_stream(path: &Path) -> std::io::Result<Arc<StreamSource>> {
    StreamSource::open(path)
}

/// Move (or copy, across file systems) a media file and its peak cache.
pub fn relocate(from: &Path, to_dir: &Path, keep_original: bool) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(to_dir)?;
    let stem = from
        .file_stem()
        .map_or_else(|| "audio".to_string(), |s| s.to_string_lossy().to_string());
    let to = faderframe_audio_files::import::unique_path(to_dir, &stem, "wav");
    let moved = !keep_original && std::fs::rename(from, &to).is_ok();
    if !moved {
        std::fs::copy(from, &to)?;
    }
    let pk_from = peaks_path_for(from);
    if pk_from.exists() {
        let pk_to = peaks_path_for(&to);
        if keep_original || std::fs::rename(&pk_from, &pk_to).is_err() {
            let _ = std::fs::copy(&pk_from, &pk_to);
        }
    }
    Ok(to)
}

/// Open every file source of `project` that is not in `sources` yet
/// (paths resolved against `project_dir`). Returns the sources whose files
/// could not be opened.
pub fn open_file_sources(
    project: &faderframe_project::Project,
    project_dir: Option<&Path>,
    sources: &mut faderframe_engine::SourceMap,
) -> Vec<(AudioSourceId, PathBuf, std::io::Error)> {
    let mut missing = Vec::new();
    for s in project.sources.values() {
        let faderframe_project::SourceSpec::File { path, .. } = &s.spec else {
            continue;
        };
        if sources.contains_key(&s.id) {
            continue;
        }
        let resolved = resolve(path, project_dir);
        match open_stream(&resolved) {
            Ok(stream) => {
                sources.insert(s.id, faderframe_engine::Source::Stream(stream));
            }
            Err(e) => missing.push((s.id, resolved, e)),
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_paths_are_portable() {
        let dir = std::env::temp_dir().join("ff-project");
        let file = dir.join("Audio").join("Kick.wav");
        let stored = to_stored(&file, Some(&dir));
        assert_eq!(stored.to_string_lossy(), "Audio/Kick.wav");
        assert_eq!(
            resolve(&stored, Some(&dir)),
            dir.join("Audio").join("Kick.wav")
        );
        // Written on Windows before paths were portable.
        assert_eq!(
            resolve(Path::new("Audio\\Kick.wav"), Some(&dir)),
            dir.join("Audio").join("Kick.wav")
        );
        // Outside the project: kept as they are.
        let elsewhere = std::env::temp_dir().join("other.wav");
        assert_eq!(to_stored(&elsewhere, Some(&dir)), elsewhere);
    }
}
