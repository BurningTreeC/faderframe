//! Spectral editing in the session (see [`faderframe_project::spectral`]):
//! a clip's edits changed at once (what the editor shows), rendered when
//! they have rested for a moment, in a worker, into a processed copy of
//! the clip's source ([`faderframe_spectral::apply`]); the copy then
//! becomes a source and the clip plays it, edits and all, in one
//! "Spectral Edit" step (a render a newer change overtook is dropped).
//! With clip effects the copy is what the effects process (they render
//! again). No edits left: the clip's audio back.
//!
//! The editor's picture comes from [`Session::spectrogram`]: asked for
//! while painting, computed by a worker (the newest request first), the
//! closest picture of that source shown meanwhile.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::Source;
use faderframe_project::spectral::{SpectralEdit, SpectralEdits};
use faderframe_project::{AudioClip, AudioSource, ClipContent, Command, SourceSpec};
use faderframe_spectral::Spectrogram;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long edits rest before they render.
const SETTLE: Duration = Duration::from_millis(300);
/// Pictures kept.
const PICTURES: usize = 12;

/// A change of a clip's spectral edits.
#[derive(Clone, Debug, PartialEq)]
pub enum SpectralChange {
    Add(SpectralEdit),
    Set(usize, SpectralEdit),
    Remove(usize),
    /// Every edit off: the clip's audio back.
    Clear,
}

/// Reads frames `start..` of every channel of a source.
pub(crate) type Reader<'a> =
    Box<dyn FnMut(i64, &mut [Vec<f32>]) -> std::result::Result<(), String> + 'a>;

/// Where a source's audio is, for a worker.
#[derive(Clone)]
pub(crate) enum SourceData {
    File {
        path: PathBuf,
        channels: usize,
        frames: i64,
        rate: u32,
    },
    Memory(Arc<faderframe_audio_files::AudioData>),
}

impl SourceData {
    pub(crate) fn channels(&self) -> usize {
        match self {
            SourceData::File { channels, .. } => *channels,
            SourceData::Memory(d) => d.num_channels(),
        }
    }

    pub(crate) fn frames(&self) -> i64 {
        match self {
            SourceData::File { frames, .. } => *frames,
            SourceData::Memory(d) => d.frames() as i64,
        }
    }

    pub(crate) fn rate(&self) -> u32 {
        match self {
            SourceData::File { rate, .. } => *rate,
            SourceData::Memory(d) => d.sample_rate(),
        }
    }

    /// A reader: frames `start..` of every channel, zeros outside.
    pub(crate) fn reader(&self) -> std::result::Result<Reader<'_>, String> {
        let file = match self {
            SourceData::File { path, .. } => {
                Some(WavFile::open(path).map_err(|e| format!("{}: {e}", path.display()))?)
            }
            SourceData::Memory(_) => None,
        };
        let frames = self.frames();
        let mut scratch = Vec::new();
        Ok(Box::new(move |start: i64, out: &mut [Vec<f32>]| {
            let count = out.first().map_or(0, Vec::len);
            for c in out.iter_mut() {
                c.fill(0.0);
            }
            let skip = (-start).clamp(0, count as i64) as usize;
            let from = start.max(0);
            if from >= frames || skip >= count {
                return Ok(());
            }
            let take = ((frames - from) as usize).min(count - skip);
            match (self, &file) {
                (SourceData::Memory(d), _) => {
                    for (c, dst) in out.iter_mut().enumerate() {
                        let ch = d.channel(c.min(d.num_channels().saturating_sub(1)));
                        let src = &ch[from as usize..from as usize + take];
                        dst[skip..skip + take].copy_from_slice(src);
                    }
                }
                (SourceData::File { path, .. }, Some(f)) => {
                    let mut refs: Vec<&mut [f32]> =
                        out.iter_mut().map(|c| &mut c[skip..skip + take]).collect();
                    f.read(from as u64, &mut refs, &mut scratch)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                (SourceData::File { .. }, None) => {}
            }
            Ok(())
        }))
    }
}

struct Pending {
    edits: Vec<SpectralEdit>,
    changed: Instant,
    generation: u64,
}

struct Job {
    clip: ClipId,
    generation: u64,
    edits: Vec<SpectralEdit>,
    original: AudioSourceId,
    path: PathBuf,
    progress: Arc<AtomicU32>,
    handle: JoinHandle<std::result::Result<(), String>>,
}

/// A picture asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PictureKey {
    pub source: AudioSourceId,
    pub from: i64,
    pub to: i64,
    pub columns: usize,
    pub rows: usize,
}

#[derive(Default)]
struct Pictures {
    /// The newest pictures (newest last).
    done: Vec<(PictureKey, Arc<Spectrogram>)>,
    /// The next request (a newer one replaces it).
    next: Option<(PictureKey, SourceData)>,
    /// Being computed.
    busy: Option<PictureKey>,
    quit: bool,
}

/// The spectrogram worker: the newest request wins.
struct PictureService {
    shared: Arc<(Mutex<Pictures>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl PictureService {
    fn start() -> Self {
        let shared: Arc<(Mutex<Pictures>, Condvar)> = Arc::default();
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("faderframe-spectrogram".into())
            .spawn(move || {
                let (lock, wake) = &*worker;
                loop {
                    let (key, data) = {
                        let Ok(mut p) = lock.lock() else { return };
                        loop {
                            if p.quit {
                                return;
                            }
                            if let Some(job) = p.next.take() {
                                p.busy = Some(job.0);
                                break job;
                            }
                            p = match wake.wait(p) {
                                Ok(p) => p,
                                Err(_) => return,
                            };
                        }
                    };
                    let picture = data.reader().and_then(|mut read| {
                        Spectrogram::compute(
                            &mut read,
                            data.channels(),
                            key.from,
                            key.to,
                            key.columns,
                            key.rows,
                            f64::from(data.rate()),
                        )
                    });
                    let Ok(mut p) = lock.lock() else { return };
                    p.busy = None;
                    if let Ok(picture) = picture {
                        p.done.retain(|(k, _)| *k != key);
                        p.done.push((key, Arc::new(picture)));
                        if p.done.len() > PICTURES {
                            p.done.remove(0);
                        }
                    }
                }
            })
            .ok();
        Self { shared, thread }
    }
}

impl Drop for PictureService {
    fn drop(&mut self) {
        if let Ok(mut p) = self.shared.0.lock() {
            p.quit = true;
        }
        self.shared.1.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct SpectralState {
    /// The clip the editor shows.
    pub(crate) shown: Option<ClipId>,
    pending: HashMap<ClipId, Pending>,
    jobs: Vec<Job>,
    generation: u64,
    pictures: Option<PictureService>,
}

/// The source a clip's spectral edits apply to (before its effects).
fn edited_source(a: &AudioClip) -> AudioSourceId {
    a.effects.as_ref().map_or(a.source, |e| e.original.source)
}

impl Session {
    /// The clip the spectral editor shows.
    pub fn spectral_clip(&self) -> Option<ClipId> {
        self.spectral.shown.filter(|c| {
            self.project
                .clip(*c)
                .is_some_and(|c| c.as_audio().is_some())
        })
    }

    /// A clip's spectral edits as they are being changed (or as rendered).
    pub fn spectral_edits(&self, clip: ClipId) -> Vec<SpectralEdit> {
        if let Some(p) = self.spectral.pending.get(&clip) {
            return p.edits.clone();
        }
        self.project
            .clip(clip)
            .and_then(|c| c.as_audio())
            .and_then(|a| a.spectral.as_ref())
            .map(|s| s.edits.clone())
            .unwrap_or_default()
    }

    /// Are a clip's edits waiting to render or rendering? With how far a
    /// render is (0…1).
    pub fn spectral_busy(&self, clip: ClipId) -> Option<f32> {
        if let Some(j) = self.spectral.jobs.iter().find(|j| j.clip == clip) {
            return Some(f32::from_bits(j.progress.load(Ordering::Relaxed)));
        }
        self.spectral.pending.contains_key(&clip).then_some(0.0)
    }

    /// The sources of a clip's spectral picture: the one it plays (its
    /// edits applied) and its unedited one.
    pub fn spectral_sources(&self, clip: ClipId) -> Option<(AudioSourceId, AudioSourceId)> {
        let a = self.project.clip(clip)?.as_audio()?;
        let now = edited_source(a);
        Some((now, a.spectral.as_ref().map_or(now, |s| s.original)))
    }

    /// The rate, channels and frames of a loaded source.
    pub fn source_format(&self, source: AudioSourceId) -> Option<(u32, usize, i64)> {
        let d = self.source_data(source)?;
        Some((d.rate(), d.channels(), d.frames()))
    }

    pub(crate) fn source_data(&self, source: AudioSourceId) -> Option<SourceData> {
        Some(match self.sources.get(&source)? {
            Source::Memory(d) => SourceData::Memory(Arc::clone(d)),
            Source::Stream(s) => SourceData::File {
                path: s.path().to_path_buf(),
                channels: s.channels(),
                frames: s.frames() as i64,
                rate: s.sample_rate(),
            },
        })
    }

    /// The spectrogram of `key`, or (while it is computed) the newest one
    /// of that source; asks for it when it is not there.
    pub fn spectrogram(&self, key: PictureKey) -> Option<(PictureKey, Arc<Spectrogram>)> {
        let service = self.spectral.pictures.as_ref()?;
        let (lock, wake) = &*service.shared;
        let mut p = lock.lock().ok()?;
        if let Some((k, s)) = p.done.iter().find(|(k, _)| *k == key) {
            return Some((*k, Arc::clone(s)));
        }
        if p.busy != Some(key)
            && p.next.as_ref().is_none_or(|(k, _)| *k != key)
            && let Some(data) = self.source_data(key.source)
        {
            p.next = Some((key, data));
            wake.notify_one();
        }
        p.done
            .iter()
            .rev()
            .find(|(k, _)| k.source == key.source)
            .map(|(k, s)| (*k, Arc::clone(s)))
    }

    /// Is a picture being computed or waiting?
    pub fn spectrogram_pending(&self) -> bool {
        self.spectral.pictures.as_ref().is_some_and(|s| {
            s.shared
                .0
                .lock()
                .is_ok_and(|p| p.busy.is_some() || p.next.is_some())
        })
    }

    /// Show a clip in the spectral editor.
    pub(crate) fn open_spectral(&mut self, clip: ClipId) -> Result<()> {
        if self.project.clip(clip).and_then(|c| c.as_audio()).is_none() {
            return Err(SessionError::Other(
                "spectral editing is for audio clips".into(),
            ));
        }
        self.spectral.shown = Some(clip);
        if self.spectral.pictures.is_none() {
            self.spectral.pictures = Some(PictureService::start());
        }
        self.workspace_action(crate::WorkspaceAction::ShowView(
            faderframe_workspace::ViewId::spectral(),
        ))?;
        self.revision += 1;
        Ok(())
    }

    /// Change a clip's spectral edits (see the module docs).
    pub(crate) fn edit_spectral(&mut self, clip: ClipId, change: SpectralChange) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Err(SessionError::Other(format!("no clip {clip}")));
        };
        let Some(a) = c.as_audio().cloned() else {
            return Err(SessionError::Other(
                "spectral editing is for audio clips".into(),
            ));
        };
        let mut edits = self.spectral_edits(clip);
        match change {
            SpectralChange::Add(e) => edits.push(e),
            SpectralChange::Set(i, e) if i < edits.len() => edits[i] = e,
            SpectralChange::Remove(i) if i < edits.len() => {
                edits.remove(i);
            }
            SpectralChange::Clear => edits.clear(),
            _ => return Ok(()),
        }
        if edits.is_empty() {
            // Nothing left: the unedited audio back (an undo step).
            self.spectral.pending.remove(&clip);
            if let Some(s) = a.spectral.as_deref() {
                let mut back = a.clone();
                match back.effects.as_mut() {
                    Some(fx) => fx.original.source = s.original,
                    None => back.source = s.original,
                }
                back.spectral = None;
                self.batch(
                    "Remove Spectral Edits",
                    vec![Command::SetClipContent {
                        clip,
                        start: c.start,
                        content: Box::new(ClipContent::Audio(back)),
                    }],
                )?;
                if a.effects.is_some() {
                    self.rerender_clip_fx(clip);
                }
            }
            self.revision += 1;
            return Ok(());
        }
        self.spectral.generation += 1;
        let generation = self.spectral.generation;
        self.spectral.pending.insert(
            clip,
            Pending {
                edits,
                changed: Instant::now(),
                generation,
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Start renders of edits that have rested; place the finished ones
    /// (from the session tick).
    pub(crate) fn poll_spectral(&mut self) {
        let ready: Vec<ClipId> = self
            .spectral
            .pending
            .iter()
            .filter(|(c, p)| {
                p.changed.elapsed() >= SETTLE
                    && !self
                        .spectral
                        .jobs
                        .iter()
                        .any(|j| j.clip == **c && j.generation == p.generation)
            })
            .map(|(c, _)| *c)
            .collect();
        for clip in ready {
            if let Err(e) = self.start_spectral_render(clip) {
                self.spectral.pending.remove(&clip);
                self.notify(NoticeLevel::Error, format!("spectral edit: {e}"));
            }
        }
        let mut i = 0;
        while i < self.spectral.jobs.len() {
            if !self.spectral.jobs[i].handle.is_finished() {
                i += 1;
                continue;
            }
            let j = self.spectral.jobs.remove(i);
            let latest = self
                .spectral
                .pending
                .get(&j.clip)
                .is_none_or(|p| p.generation == j.generation);
            let result = j
                .handle
                .join()
                .unwrap_or_else(|_| Err("the render stopped".into()));
            if !latest {
                // Overtaken by a newer change.
                let _ = std::fs::remove_file(&j.path);
                continue;
            }
            self.spectral.pending.remove(&j.clip);
            let placed = result
                .map_err(SessionError::Other)
                .and_then(|()| self.place_spectral(j.clip, j.edits, j.original, &j.path));
            if let Err(e) = placed {
                self.notify(NoticeLevel::Error, format!("spectral edit: {e}"));
            }
            self.revision += 1;
        }
    }

    /// Wait until every spectral render is placed (tests, scripts).
    pub fn wait_for_spectral(&mut self) {
        let start = Instant::now();
        while (!self.spectral.pending.is_empty() || !self.spectral.jobs.is_empty())
            && start.elapsed() < Duration::from_secs(120)
        {
            for p in self.spectral.pending.values_mut() {
                p.changed = Instant::now() - SETTLE;
            }
            self.poll_spectral();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for the picture of `key` (tests).
    pub fn wait_for_spectrogram(&self, key: PictureKey) -> Option<Arc<Spectrogram>> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            if let Some((k, s)) = self.spectrogram(key)
                && k == key
            {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn start_spectral_render(&mut self, clip: ClipId) -> Result<()> {
        let Some(p) = self.spectral.pending.get(&clip) else {
            return Ok(());
        };
        let (edits, generation) = (p.edits.clone(), p.generation);
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other("the clip is gone".into()))?;
        let a = c
            .as_audio()
            .ok_or_else(|| SessionError::Other("not an audio clip".into()))?;
        let original = a
            .spectral
            .as_ref()
            .map_or_else(|| edited_source(a), |s| s.original);
        let data = self
            .source_data(original)
            .ok_or_else(|| SessionError::Other("the clip's audio is not loaded".into()))?;
        let name = self
            .project
            .sources
            .get(&original)
            .map_or_else(|| c.name.clone(), |s| s.name.clone());
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let path = crate::media::unique_path(&self.media_dir, &format!("{name} Spectral"));
        let progress = Arc::new(AtomicU32::new(0));
        let (job_edits, job_path, job_progress) =
            (edits.clone(), path.clone(), Arc::clone(&progress));
        let handle = std::thread::Builder::new()
            .name("faderframe-spectral".into())
            .spawn(move || render(&job_edits, &data, &job_path, &job_progress))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.spectral.jobs.push(Job {
            clip,
            generation,
            edits,
            original,
            path,
            progress,
            handle,
        });
        Ok(())
    }

    fn place_spectral(
        &mut self,
        clip: ClipId,
        edits: Vec<SpectralEdit>,
        original: AudioSourceId,
        path: &std::path::Path,
    ) -> Result<()> {
        let wav = WavFile::open(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let (frames, channels, rate) = (wav.frames() as i64, wav.channels(), wav.sample_rate());
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(()); // deleted meanwhile
        };
        let Some(mut a) = c.as_audio().cloned() else {
            return Ok(());
        };
        let source = AudioSource {
            id: self.project.ids.allocate(),
            name: path
                .file_stem()
                .map_or_else(|| c.name.clone(), |s| s.to_string_lossy().to_string()),
            spec: SourceSpec::File {
                path: path.to_path_buf(),
                channels: channels as u16,
                frames,
                sample_rate: rate,
            },
        };
        match a.effects.as_mut() {
            Some(fx) => fx.original.source = source.id,
            None => a.source = source.id,
        }
        let fx = a.effects.is_some();
        a.spectral = Some(Box::new(SpectralEdits { original, edits }));
        self.batch(
            "Spectral Edit",
            vec![
                Command::AddSource {
                    source: Box::new(source),
                },
                Command::SetClipContent {
                    clip,
                    start: c.start,
                    content: Box::new(ClipContent::Audio(a)),
                },
            ],
        )?;
        if fx {
            self.rerender_clip_fx(clip);
        }
        Ok(())
    }
}

/// Render `edits` of `data` into a float WAV at `path`.
fn render(
    edits: &[SpectralEdit],
    data: &SourceData,
    path: &std::path::Path,
    progress: &AtomicU32,
) -> std::result::Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    let channels = data.channels();
    let mut w = WavWriter::create(
        path,
        channels as u16,
        data.rate(),
        WavFormat::Float32,
        false,
    )
    .map_err(io)?;
    let mut read = data.reader()?;
    let mut write = |chunk: &[&[f32]]| {
        let n = chunk.first().map_or(0, |c| c.len());
        w.write_planar(chunk, n)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    faderframe_spectral::apply(
        edits,
        channels,
        data.frames(),
        f64::from(data.rate()),
        &mut read,
        &mut write,
        &mut |f| progress.store(f.to_bits(), Ordering::Relaxed),
    )?;
    w.finish().map(|_| ()).map_err(io)
}
