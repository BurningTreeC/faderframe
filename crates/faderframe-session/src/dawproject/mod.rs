//! DAWproject files out and in.
//!
//! Export ([`Session::export_dawproject`]) builds the documents on the
//! control thread (plug-in states taken first: instances live here), then
//! a job writes the container beside its destination — media copied in as
//! they are, generated sources rendered — and the file moves into place as
//! exports do ([`crate::exports`]). Import ([`Session::import_dawproject`])
//! reads the container on a job: media decoded into a new scratch media
//! folder at the session's rate, plug-in states read; the document then
//! becomes a new project ([`import::build`]) the way a new project
//! replaces the old one. Both keep what could not be carried in a report
//! ([`Session::interchange_report`]); a notice sums it up.

mod export;
mod import;

use crate::render::files::{Overwrite, Staged, StagedFile};
use crate::{NoticeLevel, Result, Session, SessionError, media};
use faderframe_audio_files::GeneratorSpec;
use faderframe_audio_files::import::{ImportProgress, ImportedAudio};
use faderframe_interchange::dawproject as dp;
use faderframe_interchange::dawproject::file::{DawProjectFile, Payload};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

/// Ids of the document's elements, and what FaderFrame keeps in its own
/// way inside a DAWproject file (read back by the import).
pub(crate) mod ids {
    use faderframe_core::{PluginInstanceId, SendId, TrackId};

    /// `deviceVendor` of built-in devices.
    pub const VENDOR: &str = "FaderFrame";
    /// `deviceID` prefix of LV2 plug-ins (DAWproject has no LV2 element).
    pub const LV2_PREFIX: &str = "lv2:";
    /// A marker that is a section: its comment says where it ends.
    pub const SECTION_COMMENT: &str = "FaderFrame section; ends at beat ";
    pub const TEMPO: &str = "tempo";
    pub const TIME_SIGNATURE: &str = "time-signature";

    pub fn track(t: TrackId) -> String {
        format!("track{}", t.raw())
    }
    pub fn channel(t: TrackId) -> String {
        format!("channel{}", t.raw())
    }
    pub fn volume(t: TrackId) -> String {
        format!("volume{}", t.raw())
    }
    pub fn pan(t: TrackId) -> String {
        format!("pan{}", t.raw())
    }
    pub fn mute(t: TrackId) -> String {
        format!("mute{}", t.raw())
    }
    pub fn send(s: SendId) -> String {
        format!("send{}", s.raw())
    }
    pub fn send_volume(s: SendId) -> String {
        format!("send{}-volume", s.raw())
    }
    pub fn send_pan(s: SendId) -> String {
        format!("send{}-pan", s.raw())
    }
    pub fn send_on(s: SendId) -> String {
        format!("send{}-on", s.raw())
    }
    pub fn device(p: PluginInstanceId) -> String {
        format!("device{}", p.raw())
    }
    pub fn device_on(p: PluginInstanceId) -> String {
        format!("device{}-on", p.raw())
    }
    pub fn device_param(p: PluginInstanceId, id: u32) -> String {
        format!("device{}-p{id}", p.raw())
    }

    /// A VST3 class id as FaderFrame keeps it (the class's bytes as the
    /// platform lays them out) in the canonical order `.vstpreset` and
    /// DAWproject write.
    pub fn vst3_canonical(raw: &str) -> String {
        if cfg!(windows) {
            faderframe_interchange::presets::swap_com_order(raw).unwrap_or_else(|| raw.to_string())
        } else {
            raw.to_ascii_uppercase()
        }
    }

    /// The other way.
    pub fn vst3_raw(canonical: &str) -> String {
        vst3_canonical(canonical)
    }

    const VST3_MAGIC: &[u8; 4] = b"FFV3";

    /// FaderFrame's VST3 state: its component and controller states
    /// (anything after them is FaderFrame's own and stays behind).
    pub fn split_vst3_state(s: &[u8]) -> (&[u8], &[u8]) {
        let Some(rest) = s.strip_prefix(VST3_MAGIC) else {
            return (s, &[]);
        };
        let take = |b: &[u8]| -> Option<(usize, usize)> {
            let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
            (b.len() >= 4 + n).then_some((4, 4 + n))
        };
        let Some((a, z)) = take(rest) else {
            return (s, &[]);
        };
        let comp = &rest[a..z];
        let cont = take(&rest[z..]).map_or(&[][..], |(c, d)| &rest[z + c..z + d]);
        (comp, cont)
    }

    /// The two states as FaderFrame keeps them.
    pub fn pack_vst3_state(comp: &[u8], cont: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + comp.len() + cont.len());
        out.extend_from_slice(VST3_MAGIC);
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(comp);
        out.extend_from_slice(&(cont.len() as u32).to_le_bytes());
        out.extend_from_slice(cont);
        out
    }
}

/// A file the export puts into the container.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Media {
    File(PathBuf),
    /// Rendered at the project's rate by the job.
    Generate(GeneratorSpec),
    Bytes(Vec<u8>),
}

/// A DAWproject file read: the documents, its media (by the path the
/// document names them with) decoded into the new project's folder, its
/// plug-in states.
pub(crate) struct Loaded {
    pub doc: dp::Project,
    pub metadata: dp::MetaData,
    /// The file's name (the project's, when the metadata has no title).
    pub name: String,
    pub media: HashMap<String, std::result::Result<ImportedAudio, String>>,
    pub states: HashMap<String, Vec<u8>>,
    pub notes: Vec<String>,
}

/// What an export or import could not carry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InterchangeReport {
    /// "Exported Song.dawproject", "Imported Song.dawproject".
    pub title: String,
    pub notes: Vec<String>,
}

/// Work done in the background, a fraction done, a way to stop it.
struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

impl Progress {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
        })
    }

    fn fraction(&self) -> f64 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            0.0
        } else {
            (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0)
        }
    }
}

struct ExportJob {
    path: PathBuf,
    staged: StagedFile,
    overwrite: Overwrite,
    notes: Vec<String>,
    progress: Arc<Progress>,
    handle: JoinHandle<std::result::Result<(), String>>,
}

struct ImportJob {
    path: PathBuf,
    media_dir: PathBuf,
    progress: Arc<Progress>,
    handle: JoinHandle<std::result::Result<Loaded, String>>,
}

/// DAWproject jobs and the last report.
#[derive(Default)]
pub(crate) struct InterchangeState {
    export: Option<ExportJob>,
    import: Option<ImportJob>,
    report: Option<InterchangeReport>,
}

/// Is `path` a DAWproject file (by its extension)?
pub fn is_dawproject_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(dp::file::EXTENSION))
}

/// The export job: generated media rendered, the container written.
fn write_container(
    to: &Path,
    project: &dp::Project,
    metadata: &dp::MetaData,
    files: Vec<(String, Media)>,
    rate: u32,
    progress: &Progress,
) -> std::result::Result<(), String> {
    let scratch = std::env::temp_dir().join(format!(
        "faderframe-dawproject-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let result = (|| {
        let mut payloads = Vec::with_capacity(files.len());
        for (k, (name, m)) in files.into_iter().enumerate() {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            payloads.push((
                name,
                match m {
                    Media::File(p) => Payload::File(p),
                    Media::Bytes(b) => Payload::Bytes(b),
                    Media::Generate(spec) => {
                        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
                        let data = faderframe_audio_files::generate(&spec, rate);
                        let channels: Vec<Vec<f32>> = (0..data.num_channels())
                            .map(|c| data.channel(c).to_vec())
                            .collect();
                        let file = scratch.join(format!("{k}.wav"));
                        faderframe_audio_files::write_wav(
                            &file,
                            &channels,
                            rate,
                            faderframe_audio_files::WavFormat::Float32,
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                        Payload::File(file)
                    }
                },
            ));
        }
        let mut report = |done: u64, total: u64| {
            progress.total.store(total, Ordering::Relaxed);
            progress.done.store(done, Ordering::Relaxed);
        };
        dp::file::write(
            to,
            project,
            metadata,
            &payloads,
            &mut report,
            &progress.cancel,
        )
        .map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// The import job: the documents read, media decoded into `media_dir`,
/// plug-in states read.
fn read_container(
    path: &Path,
    media_dir: &Path,
    rate: u32,
    progress: &Progress,
) -> std::result::Result<Loaded, String> {
    let mut f = DawProjectFile::open(path).map_err(|e| e.to_string())?;
    // Every audio file and state the document names.
    let mut audio: Vec<(String, bool)> = Vec::new();
    let mut states: Vec<String> = Vec::new();
    fn walk(t: &dp::Timeline, audio: &mut Vec<(String, bool)>) {
        let clip = |c: &dp::Clip, audio: &mut Vec<(String, bool)>| {
            if let Some(x) = &c.content {
                walk(x, audio);
            }
        };
        match t {
            dp::Timeline::Lanes(l) => l.lanes.iter().for_each(|x| walk(x, audio)),
            dp::Timeline::Clips(c) => c.clips.iter().for_each(|k| clip(k, audio)),
            dp::Timeline::Warps(w) => {
                if let Some(x) = &w.content {
                    walk(x, audio);
                }
            }
            dp::Timeline::ClipSlot(s) => {
                if let Some(k) = &s.clip {
                    clip(k, audio);
                }
            }
            dp::Timeline::Audio(m) if !audio.iter().any(|(p, _)| *p == m.file.path) => {
                audio.push((m.file.path.clone(), m.file.external == Some(true)));
            }
            _ => {}
        }
    }
    if let Some(l) = f
        .project
        .arrangement
        .as_ref()
        .and_then(|a| a.lanes.as_ref())
    {
        l.lanes.iter().for_each(|x| walk(x, &mut audio));
    }
    for c in f.project.channels() {
        for d in &c.devices {
            if let Some(s) = &d.state {
                states.push(s.path.clone());
            }
        }
    }
    let total = audio.len().max(1) as u64;
    progress.total.store(total, Ordering::Relaxed);
    let scratch = media_dir.join(".dawproject");
    let mut media = HashMap::new();
    let quiet = ImportProgress::default();
    for (k, (name, external)) in audio.into_iter().enumerate() {
        if progress.cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err("cancelled".into());
        }
        let result = (|| -> std::result::Result<ImportedAudio, String> {
            let source = if external || !f.contains(&name) {
                let p = f.external(&name);
                if !p.is_file() {
                    return Err(format!("{name}: not in the file nor beside it"));
                }
                p
            } else {
                std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
                // Keep its name (the clip and source are named after it).
                let base = Path::new(&name)
                    .file_name()
                    .map_or_else(|| format!("{k}"), |n| n.to_string_lossy().to_string());
                let dir = scratch.join(format!("{k}"));
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let to = dir.join(base);
                f.extract(&name, &to).map_err(|e| e.to_string())?;
                to
            };
            let cancel = AtomicBool::new(false);
            faderframe_audio_files::import::import_file(&source, media_dir, rate, &quiet, &cancel)
                .map_err(|e| format!("{name}: {e}"))
        })();
        media.insert(name, result);
        progress.done.store(k as u64 + 1, Ordering::Relaxed);
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let mut state_bytes = HashMap::new();
    for s in states {
        if let Ok(b) = f.read(&s) {
            state_bytes.insert(s, b);
        }
    }
    Ok(Loaded {
        name: path
            .file_stem()
            .map_or_else(|| "Imported".into(), |s| s.to_string_lossy().to_string()),
        doc: f.project,
        metadata: f.metadata,
        notes: f.notes,
        media,
        states: state_bytes,
    })
}

/// A notice's sum-up of what was left out.
fn summary(notes: &[String]) -> String {
    match notes {
        [] => String::new(),
        [one] => format!("; not carried: {one}"),
        [a, b] => format!("; not carried: {a}; {b}"),
        [a, b, rest @ ..] => format!("; not carried: {a}; {b}; and {} more", rest.len()),
    }
}

impl Session {
    /// Write the project as a DAWproject file at `path` (asked about when
    /// something is there; written in the background, see
    /// [`Self::wait_for_dawproject`]).
    pub fn export_dawproject(&mut self, path: &Path) -> Result<()> {
        if self.interchange.export.is_some() {
            return Err(SessionError::Other(
                "a DAWproject file is being written already".into(),
            ));
        }
        let mut path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if path.extension().is_none() {
            path.set_extension(dp::file::EXTENSION);
        }
        crate::exports::files_only(std::slice::from_ref(&path))?;
        crate::exports::make_parent(&path)?;
        self.capture_plugin_states();
        let available = self.available_plugins();
        let info = |slot: &faderframe_project::PluginSlot| {
            let known = available
                .iter()
                .find(|p| p.plugin.format == slot.plugin.format && p.plugin.id == slot.plugin.id);
            export::DeviceInfo {
                vendor: known.map(|p| p.vendor.clone()).filter(|v| !v.is_empty()),
                version: known.map(|p| p.version.clone()),
                instrument: known.is_some_and(|p| p.instrument)
                    || self.engine.plugin_is_instrument(slot.id),
                note_effect: known.is_some_and(|p| p.midi_effect),
                parameters: self
                    .engine
                    .plugin_parameters(slot.id)
                    .map(<[_]>::to_vec)
                    .unwrap_or_default(),
            }
        };
        let exported = export::export(&self.project, &info);
        let rate = self.project.sample_rate;
        let dest = path.clone();
        self.export_to("DAWproject file", vec![path], move |s, overwrite| {
            let staged = StagedFile::new(&dest).map_err(|e| SessionError::Other(e.to_string()))?;
            let progress = Progress::new();
            let to = staged.temp().to_path_buf();
            let job_progress = progress.clone();
            let export::Exported {
                project,
                metadata,
                files,
                notes,
            } = exported;
            let handle = std::thread::Builder::new()
                .name("faderframe-dawproject".into())
                .spawn(move || {
                    write_container(&to, &project, &metadata, files, rate, &job_progress)
                })
                .map_err(|e| SessionError::Other(e.to_string()))?;
            s.interchange.export = Some(ExportJob {
                path: dest,
                staged,
                overwrite,
                notes,
                progress,
                handle,
            });
            s.revision += 1;
            Ok(())
        })
    }

    /// Read a DAWproject file as a new project (in the background, see
    /// [`Self::wait_for_dawproject`]; the project open now is replaced
    /// when it is read — ask about unsaved changes first).
    pub fn import_dawproject(&mut self, path: &Path) -> Result<()> {
        if self.interchange.import.is_some() {
            return Err(SessionError::Other(
                "a DAWproject file is being read already".into(),
            ));
        }
        if !path.is_file() {
            return Err(SessionError::Other(format!(
                "{}: no such file",
                path.display()
            )));
        }
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let media_dir = media::new_unsaved_media_dir();
        let rate = self.engine.sample_rate();
        let progress = Progress::new();
        let (p, dir, job_progress) = (path.clone(), media_dir.clone(), progress.clone());
        let handle = std::thread::Builder::new()
            .name("faderframe-dawproject".into())
            .spawn(move || {
                faderframe_realtime::priority::lower_this_thread();
                read_container(&p, &dir, rate, &job_progress)
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.interchange.import = Some(ImportJob {
            path,
            media_dir,
            progress,
            handle,
        });
        self.notify(NoticeLevel::Info, "reading the DAWproject file…");
        self.revision += 1;
        Ok(())
    }

    /// A DAWproject file being written or read: what, and how far (0…1).
    pub fn dawproject_progress(&self) -> Option<(String, f64)> {
        if let Some(j) = &self.interchange.export {
            return Some((
                format!("Writing {}", file_label(&j.path)),
                j.progress.fraction(),
            ));
        }
        self.interchange.import.as_ref().map(|j| {
            (
                format!("Reading {}", file_label(&j.path)),
                j.progress.fraction(),
            )
        })
    }

    /// Stop writing or reading a DAWproject file (nothing is replaced).
    pub fn cancel_dawproject(&self) {
        if let Some(j) = &self.interchange.export {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(j) = &self.interchange.import {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// What the last DAWproject export or import could not carry.
    pub fn interchange_report(&self) -> Option<&InterchangeReport> {
        self.interchange.report.as_ref()
    }

    /// Wait for DAWproject jobs (tests, scripted runs).
    pub fn wait_for_dawproject(&mut self) {
        while self.interchange.export.is_some() || self.interchange.import.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_dawproject();
        }
    }

    /// Finished jobs: an export moves into place, an import becomes the
    /// project.
    pub(crate) fn poll_dawproject(&mut self) {
        if self
            .interchange
            .export
            .as_ref()
            .is_some_and(|j| j.handle.is_finished())
            && let Some(job) = self.interchange.export.take()
        {
            self.finish_export(job);
            self.revision += 1;
        }
        if self
            .interchange
            .import
            .as_ref()
            .is_some_and(|j| j.handle.is_finished())
            && let Some(job) = self.interchange.import.take()
        {
            if let Err(e) = self.finish_import_dawproject(job) {
                let text = e.to_string();
                let level = if text.ends_with("cancelled") {
                    NoticeLevel::Info
                } else {
                    NoticeLevel::Error
                };
                self.notify(level, format!("DAWproject import: {text}"));
            }
            self.revision += 1;
        }
    }

    fn finish_export(&mut self, job: ExportJob) {
        let ExportJob {
            path,
            staged,
            overwrite,
            notes,
            handle,
            ..
        } = job;
        match handle.join() {
            Ok(Ok(())) => {
                let staged = Staged::new(vec![crate::render::files::written(staged)]);
                let shown = path.clone();
                let r = self.commit_export("DAWproject file", staged, &overwrite, move |s, _| {
                    let text = format!("exported {}{}", shown.display(), summary(&notes));
                    s.interchange.report = Some(InterchangeReport {
                        title: format!("Exported {}", file_label(&shown)),
                        notes,
                    });
                    s.notify(NoticeLevel::Info, text);
                    Ok(())
                });
                if let Err(e) = r {
                    tracing::warn!("DAWproject export: {e}");
                }
            }
            Ok(Err(e)) => self.notify(
                if e == "cancelled" {
                    NoticeLevel::Info
                } else {
                    NoticeLevel::Error
                },
                format!("DAWproject export {e}: {} left as it was", path.display()),
            ),
            Err(_) => self.notify(NoticeLevel::Error, "DAWproject export failed"),
        }
    }

    fn finish_import_dawproject(&mut self, job: ImportJob) -> Result<()> {
        let ImportJob {
            path,
            media_dir,
            handle,
            ..
        } = job;
        let loaded = match handle.join() {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                let _ = std::fs::remove_dir_all(&media_dir);
                return Err(SessionError::Other(format!("{}: {e}", path.display())));
            }
            Err(_) => {
                let _ = std::fs::remove_dir_all(&media_dir);
                return Err(SessionError::Other("the reading failed".into()));
            }
        };
        let (project, mut notes) = import::build(&loaded, self.engine.sample_rate());
        notes.splice(0..0, loaded.notes.iter().cloned());
        let tracks = project.tracks.len().saturating_sub(1);
        let clips = project.clips.len();
        // As a new project does: the old one's scratch media go.
        self.discard_unsaved_media();
        self.path = None;
        self.media_dir = media_dir;
        self.unsaved_media = true;
        self.merging.set_lineage(None);
        self.replace_project(project, None)?;
        let text = format!(
            "imported {}: {tracks} track{}, {clips} clip{}{}",
            file_label(&path),
            if tracks == 1 { "" } else { "s" },
            if clips == 1 { "" } else { "s" },
            summary(&notes)
        );
        self.interchange.report = Some(InterchangeReport {
            title: format!("Imported {}", file_label(&path)),
            notes,
        });
        self.notify(NoticeLevel::Info, text);
        Ok(())
    }
}

fn file_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    )
}
