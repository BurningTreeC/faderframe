//! Picture in the session: importing video (its frame index, its sound on
//! an audio track at the same place, the project's timecode from it), a
//! proxy made in the background for every video that needs one, the frame
//! service the shell draws from, picture edits as undoable steps, movies
//! written with the mix next to the untouched picture, and the
//! flash-and-beep sync test.
//!
//! The audio engine is the clock: the picture for a moment on screen is the
//! one for the sample heard then -- the engine's position at that moment
//! (extrapolated from its last callback on the same clock), less the output
//! latency at the transport's speed, less the picture offset.
//!
//! Indexes and proxies live in the computer's cache folder (`video`, also
//! in portable mode), named by the file's path, size and time, so they are
//! made once per file and can be deleted at any time.

use crate::render::{RenderChannels, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, SessionError, media};
use faderframe_audio_files::import::{ImportProgress, ImportedAudio};
use faderframe_audio_files::{Dither, WavFormat};
use faderframe_core::timecode::{FrameRate, Timecode};
use faderframe_core::{TrackId, VideoClipId, VideoSourceId, VideoTrackId};
use faderframe_project::video::{
    ProjectTimecode, Video, VideoClip, VideoSource, VideoTrack, ns_to_samples, samples_to_ns,
};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, Command, SourceSpec, Track, TrackColor, TrackKind,
};
use faderframe_video::mux::Container;
use faderframe_video::proxy::ProxySpec;
use faderframe_video::{FrameIndex, FrameService, Media, MediaInfo, Picture, Want};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Bytes of decoded frames kept (thumbnails a tenth more).
const FRAME_BUDGET: usize = 384 << 20;

/// Proxies' height.
const PROXY_HEIGHT: u32 = 540;

/// Another place for [`cache_dir`] (tests: never the user's cache).
static CACHE_OVERRIDE: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Keep indexes and proxies in `dir` instead (`None`: the usual place).
pub fn set_cache_dir(dir: Option<PathBuf>) {
    *CACHE_OVERRIDE.write().unwrap_or_else(|p| p.into_inner()) = dir;
}

/// Where indexes and proxies are kept (deletable): the computer's own
/// cache folder, also in portable mode -- proxies take gigabytes per hour
/// of picture, and a portable folder may be on a stick.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = CACHE_OVERRIDE
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
    {
        return dir;
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("cache"))
            } else if cfg!(target_os = "macos") {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
            } else {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
            }
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("faderframe").join("video")
}

/// A file's name in the cache: its path, size and time (FNV-1a, stable).
fn cache_key(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map_or(0, |m| m.len());
    let time = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let text = format!("{}|{size}|{time}", path.display());
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The index of `path`, from the cache or made (and kept).
fn index_of(
    path: &Path,
    cancel: &AtomicBool,
    share: &Share,
) -> std::result::Result<FrameIndex, String> {
    let file = cache_dir().join(format!("{}.index.json", cache_key(path)));
    if let Some(ix) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<FrameIndex>(&b).ok())
    {
        return Ok(ix);
    }
    let ix = faderframe_video::index::index(path, cancel, |s| share.set(s))
        .map_err(|e| e.to_string())?;
    if let Ok(json) = serde_json::to_vec(&ix) {
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&file, json);
    }
    Ok(ix)
}

/// Where `path`'s proxy is (made or to be made).
fn proxy_path(path: &Path) -> PathBuf {
    cache_dir().join(format!("{}.proxy{PROXY_HEIGHT}.mkv", cache_key(path)))
}

/// A share done, shared with the shell.
#[derive(Default)]
struct Share(AtomicU64);

impl Share {
    fn set(&self, s: f64) {
        self.0.store(s.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// An imported video, read and its sound written.
struct Imported {
    path: PathBuf,
    info: MediaInfo,
    index: FrameIndex,
    sound: Option<ImportedAudio>,
}

enum Done {
    Imported(Box<Imported>),
    Indexed {
        source: VideoSourceId,
        index: FrameIndex,
    },
    Proxied {
        source: VideoSourceId,
        proxy: PathBuf,
        size: (u32, u32),
    },
    Exported {
        out: PathBuf,
    },
    SyncTest {
        video: PathBuf,
        sound: PathBuf,
    },
}

struct Job {
    label: String,
    share: Arc<Share>,
    cancel: Arc<AtomicBool>,
    source: Option<VideoSourceId>,
    handle: Option<JoinHandle<std::result::Result<Done, String>>>,
}

/// A video job as the shell shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoJob {
    pub label: String,
    pub share: f64,
}

/// What the session knows of a video file.
#[derive(Default)]
struct Known {
    path: PathBuf,
    index: Option<Arc<FrameIndex>>,
    proxy: Option<(PathBuf, (u32, u32))>,
    /// The service has it as it is now.
    registered: bool,
    error: Option<String>,
}

/// The state of a video file, for the shell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoSourceState {
    pub indexed: bool,
    /// A proxy is there to scrub with.
    pub proxy: bool,
    /// Making the proxy: the share done.
    pub proxying: Option<f64>,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct VideoState {
    service: Option<FrameService>,
    known: HashMap<VideoSourceId, Known>,
    jobs: Vec<Job>,
    /// Frames shown while playing, and how many were not the one wanted.
    shown: AtomicU64,
    late: AtomicU64,
    /// The last frame number shown per video (for counting).
    last: Mutex<Option<(VideoSourceId, usize)>>,
}

impl Drop for VideoState {
    fn drop(&mut self) {
        for j in &self.jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// What the picture shows now.
#[derive(Clone, Debug)]
pub struct VideoShown {
    /// The frame (`None`: not decoded yet).
    pub picture: Option<Picture>,
    pub source: VideoSourceId,
    pub clip: VideoClipId,
    /// The frame wanted, and its time in the file (ns).
    pub frame: usize,
    pub file_time: i64,
    /// The timeline position shown (samples).
    pub position: i64,
}

/// Picture edits and jobs.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoOp {
    /// Import a video (with its first sound stream on a new audio track
    /// when `sound`).
    Import {
        path: PathBuf,
        sound: bool,
    },
    /// Move a clip (timeline samples).
    MoveClip {
        clip: VideoClipId,
        start: i64,
    },
    /// Trim a clip: its start, in-point (ns in the file) and length (ns).
    TrimClip {
        clip: VideoClipId,
        start: i64,
        offset: i64,
        length: i64,
    },
    RemoveClip(VideoClipId),
    /// Place a clip where its file's start timecode is.
    SpotToTimecode(VideoClipId),
    /// The picture offset (ms; positive: later).
    SetOffset(f64),
    /// Show or hide a video track.
    ToggleTrack(VideoTrackId),
    /// Write a movie: the clip's picture (copied) with the mix.
    Export {
        clip: Option<VideoClipId>,
        path: PathBuf,
        container: Container,
    },
    /// Make and import the flash-and-beep test.
    SyncTest,
    /// Stop the running jobs.
    Cancel,
    /// Ask for a video to import (the shell's chooser).
    ChooseImport,
    /// Ask where to write the movie (the shell's chooser).
    ChooseExport,
}

impl crate::Session {
    fn video_service(&mut self) -> &FrameService {
        self.video
            .service
            .get_or_insert_with(|| FrameService::new(FRAME_BUDGET))
    }

    fn spawn_video(
        &mut self,
        label: impl Into<String>,
        source: Option<VideoSourceId>,
        work: impl FnOnce(&Share, &AtomicBool) -> std::result::Result<Done, String> + Send + 'static,
    ) {
        let share = Arc::new(Share::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c) = (Arc::clone(&share), Arc::clone(&cancel));
        let label = label.into();
        match std::thread::Builder::new()
            .name("faderframe-video-job".into())
            .spawn(move || work(&s, &c))
        {
            Ok(handle) => self.video.jobs.push(Job {
                label,
                share,
                cancel,
                source,
                handle: Some(handle),
            }),
            Err(e) => self.notify(NoticeLevel::Error, format!("{label}: {e}")),
        }
        self.revision += 1;
    }

    /// The running video jobs.
    pub fn video_jobs(&self) -> Vec<VideoJob> {
        self.video
            .jobs
            .iter()
            .map(|j| VideoJob {
                label: j.label.clone(),
                share: j.share.get(),
            })
            .collect()
    }

    /// What is known of a video file.
    pub fn video_source_state(&self, source: VideoSourceId) -> VideoSourceState {
        let k = self.video.known.get(&source);
        VideoSourceState {
            indexed: k.is_some_and(|k| k.index.is_some()),
            proxy: k.is_some_and(|k| k.proxy.is_some()),
            proxying: self
                .video
                .jobs
                .iter()
                .find(|j| j.source == Some(source) && j.label.starts_with("Proxy"))
                .map(|j| j.share.get()),
            error: k.and_then(|k| k.error.clone()).or_else(|| {
                self.video
                    .service
                    .as_ref()
                    .and_then(|s| s.error(source.raw()))
            }),
        }
    }

    /// Frames shown while playing, and of them not the one wanted.
    pub fn video_frames_late(&self) -> (u64, u64) {
        (
            self.video.shown.load(Ordering::Relaxed),
            self.video.late.load(Ordering::Relaxed),
        )
    }

    /// The picture for the moment `lead_ns` from now (when the frame being
    /// drawn reaches the screen), at most `max` pixels.
    pub fn video_picture(&self, lead_ns: i64, max: (u32, u32)) -> Option<VideoShown> {
        let rate = self.project.sample_rate;
        let playing = self.transport.playing;
        let offset = (self.project.video.offset_ms * 1e6) as i64;
        let position = if playing {
            let now = self.midi.sender.clock().now_ns() as i64;
            let at = (now + lead_ns - offset).max(0) as u64;
            let (p, _) = self.engine.position_and_jumps_at(at)?;
            let latency = self.engine.output_latency() as f64 * self.engine.speed();
            p - latency.round() as i64
        } else {
            self.transport.position
        };
        let (clip, file_time) = self.project.video.at(position, rate)?;
        let (clip, source) = (clip.id, clip.source);
        let known = self.video.known.get(&source)?;
        let index = known.index.as_ref()?;
        let frame = index.frame_at(file_time)?;
        let want = if playing { Want::Play } else { Want::Still };
        let picture = self
            .video
            .service
            .as_ref()
            .and_then(|s| s.picture(source.raw(), file_time, max, want));
        if playing {
            let mut last = self.video.last.lock().unwrap_or_else(|p| p.into_inner());
            if *last != Some((source, frame)) {
                *last = Some((source, frame));
                self.video.shown.fetch_add(1, Ordering::Relaxed);
                if !picture
                    .as_ref()
                    .is_some_and(|p| p.exact && p.number == frame)
                {
                    self.video.late.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Some(VideoShown {
            picture,
            source,
            clip,
            frame,
            file_time,
            position,
        })
    }

    /// A filmstrip picture of `source` at `file_time` (ns), `height` high
    /// (`None` until made; asking has it made).
    pub fn video_thumbnail(
        &self,
        source: VideoSourceId,
        file_time: i64,
        height: u32,
    ) -> Option<Arc<faderframe_video::Frame>> {
        let index = self.video.known.get(&source)?.index.as_ref()?;
        let n = index
            .frame_at(file_time)
            .or_else(|| index.len().checked_sub(1))?;
        self.video
            .service
            .as_ref()?
            .thumbnail(source.raw(), n, height)
    }

    /// The frame times of a video, once indexed.
    pub fn video_index(&self, source: VideoSourceId) -> Option<Arc<FrameIndex>> {
        self.video.known.get(&source)?.index.clone()
    }

    /// The project's timecode (its own, else 25 fps from 00:00:00:00).
    pub fn timecode(&self) -> ProjectTimecode {
        self.project.timecode.unwrap_or_default()
    }

    /// Start importing a video.
    pub fn import_video(&mut self, path: PathBuf, sound: bool) {
        if let Err(e) = faderframe_video::init() {
            self.notify(NoticeLevel::Error, e.to_string());
            return;
        }
        let rate = self.project.sample_rate;
        let media_dir = self.media_dir.clone();
        let name = path
            .file_name()
            .map_or_else(|| "video".into(), |n| n.to_string_lossy().into_owned());
        self.spawn_video(format!("Importing {name}"), None, move |share, cancel| {
            let info = faderframe_video::probe::probe(&path).map_err(|e| e.to_string())?;
            if info.video.is_none() {
                return Err(format!("{} has no picture", path.display()));
            }
            let index = index_of(&path, cancel, share)?;
            let sound = if sound && !info.audio.is_empty() {
                let progress = ImportProgress::default();
                Some(
                    faderframe_video::audio::extract_audio(
                        &path, 0, &media_dir, rate, &progress, cancel,
                    )
                    .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            Ok(Done::Imported(Box::new(Imported {
                path,
                info,
                index,
                sound,
            })))
        });
    }

    /// Apply finished jobs, start indexes and proxies the project's videos
    /// need, hand the service what is ready (from the tick).
    pub(crate) fn poll_video(&mut self) {
        let mut i = 0;
        while i < self.video.jobs.len() {
            if !self.video.jobs[i]
                .handle
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            {
                i += 1;
                continue;
            }
            let mut job = self.video.jobs.remove(i);
            let result = job
                .handle
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("the job stopped".into()));
            self.revision += 1;
            match result {
                Ok(done) => {
                    if let Err(e) = self.video_done(done) {
                        self.notify(NoticeLevel::Error, format!("{}: {e}", job.label));
                    }
                }
                Err(e) if job.cancel.load(Ordering::Relaxed) => {
                    tracing::info!("{}: {e}", job.label);
                }
                Err(e) => {
                    if let Some(k) = job.source.and_then(|s| self.video.known.get_mut(&s)) {
                        k.error = Some(e.clone());
                    }
                    self.notify(NoticeLevel::Error, format!("{}: {e}", job.label));
                }
            }
        }
        self.reconcile_video();
    }

    /// Bring what is known in line with the project's videos.
    fn reconcile_video(&mut self) {
        let sources: Vec<(VideoSourceId, PathBuf)> = self
            .project
            .video
            .sources
            .iter()
            .filter(|(id, _)| self.project.video.uses(**id))
            .map(|(id, s)| (*id, s.path.clone()))
            .collect();
        // Gone (removed, undone, another project).
        let gone: Vec<VideoSourceId> = self
            .video
            .known
            .keys()
            .filter(|k| !sources.iter().any(|(s, _)| s == *k))
            .copied()
            .collect();
        for g in gone {
            self.video.known.remove(&g);
            if let Some(s) = &self.video.service {
                s.remove(g.raw());
            }
        }
        let busy = |s: &Self, id: VideoSourceId| s.video.jobs.iter().any(|j| j.source == Some(id));
        for (id, path) in sources {
            let known = self.video.known.entry(id).or_default();
            if known.path != path {
                *known = Known {
                    path: path.clone(),
                    ..Known::default()
                };
            }
            if known.error.is_some() {
                continue;
            }
            if known.index.is_none() {
                if !busy(self, id) {
                    if !path.is_file() {
                        if let Some(k) = self.video.known.get_mut(&id) {
                            k.error = Some(format!("{} is missing", path.display()));
                        }
                        continue;
                    }
                    let name = path
                        .file_name()
                        .map_or_else(|| "video".into(), |n| n.to_string_lossy().into_owned());
                    self.spawn_video(format!("Reading {name}"), Some(id), move |share, cancel| {
                        index_of(&path, cancel, share)
                            .map(|index| Done::Indexed { source: id, index })
                    });
                }
                continue;
            }
            // A proxy that is already there.
            if known.proxy.is_none() {
                let p = proxy_path(&path);
                if p.is_file()
                    && let Some(s) = self.project.video.sources.get(&id)
                {
                    let size =
                        faderframe_video::fit(s.width, s.height, s.par, u32::MAX, PROXY_HEIGHT);
                    known.proxy = Some((p, size));
                    known.registered = false;
                }
            }
            if !known.registered
                && let (Some(index), Some(s)) =
                    (known.index.clone(), self.project.video.sources.get(&id))
            {
                let media = Media {
                    original: path.clone(),
                    index,
                    size: (s.width, s.height),
                    par: s.par,
                    proxy: known.proxy.clone(),
                };
                known.registered = true;
                self.video_service().set_media(id.raw(), media);
            }
            // A proxy for one that needs it, one at a time.
            let needs = self.video.known.get(&id).is_some_and(|k| {
                k.proxy.is_none() && k.index.as_ref().is_some_and(|ix| ix.longest_gop() > 1)
            }) || self.project.video.sources.get(&id).is_some_and(|s| {
                s.height > 720 && self.video.known.get(&id).is_some_and(|k| k.proxy.is_none())
            });
            let proxying = self.video.jobs.iter().any(|j| j.label.starts_with("Proxy"));
            if needs
                && !proxying
                && let Some(s) = self.project.video.sources.get(&id).cloned()
            {
                let out = proxy_path(&path);
                let name = s.name();
                self.spawn_video(
                    format!("Proxy for {name}"),
                    Some(id),
                    move |share, cancel| {
                        let spec = ProxySpec {
                            height: PROXY_HEIGHT,
                            ..ProxySpec::default()
                        };
                        let picture = (s.width, s.height, s.par);
                        faderframe_video::proxy::make_proxy(
                            &path,
                            &out,
                            picture,
                            spec,
                            cancel,
                            |x| share.set(x),
                        )
                        .map_err(|e| e.to_string())?;
                        let size =
                            faderframe_video::fit(s.width, s.height, s.par, u32::MAX, PROXY_HEIGHT);
                        Ok(Done::Proxied {
                            source: id,
                            proxy: out,
                            size,
                        })
                    },
                );
            }
        }
    }

    fn video_done(&mut self, done: Done) -> Result<()> {
        match done {
            Done::Imported(i) => {
                let Imported {
                    path,
                    info,
                    index,
                    sound,
                } = *i;
                self.place_video(path, info, index, sound)
            }
            Done::Indexed { source, index } => {
                if let Some(k) = self.video.known.get_mut(&source) {
                    k.index = Some(Arc::new(index));
                    k.registered = false;
                }
                Ok(())
            }
            Done::Proxied {
                source,
                proxy,
                size,
            } => {
                if let Some(k) = self.video.known.get_mut(&source) {
                    k.proxy = Some((proxy, size));
                    k.registered = false;
                }
                Ok(())
            }
            Done::Exported { out } => {
                self.notify(NoticeLevel::Info, format!("wrote {}", out.display()));
                Ok(())
            }
            Done::SyncTest { video, sound } => {
                self.import_video(video, false);
                self.import_audio(
                    vec![sound],
                    media::ImportTarget {
                        track: None,
                        at: faderframe_timeline::MusicalTime::ZERO,
                    },
                );
                self.notify(
                    NoticeLevel::Info,
                    "sync test: each white flash should land on its beep",
                );
                Ok(())
            }
        }
    }

    /// Put an imported video on the first video track (made if none) at
    /// its timecode (or the start), its sound on a new audio track at the
    /// same place; the project takes the video's timecode when it has
    /// none. One undo step.
    fn place_video(
        &mut self,
        path: PathBuf,
        info: MediaInfo,
        index: FrameIndex,
        sound: Option<ImportedAudio>,
    ) -> Result<()> {
        let Some(v) = info.video.clone() else {
            return Err(SessionError::Other(format!(
                "{} has no picture",
                path.display()
            )));
        };
        let rate = self.project.sample_rate;
        let mut commands = Vec::new();
        let file_rate = index.timecode.map(|(_, r)| r).or(v.frame_rate());
        // The project counts in the video's timecode when it has none.
        let timecode = match self.project.timecode {
            Some(tc) => {
                if file_rate.is_some_and(|r| r.ratio() != tc.rate.ratio()) {
                    self.notify(
                        NoticeLevel::Warning,
                        format!(
                            "the video runs at {} but the project's timecode at {}",
                            file_rate.map_or_else(String::new, FrameRate::label),
                            tc.rate.label()
                        ),
                    );
                }
                tc
            }
            None => {
                let tc = ProjectTimecode {
                    rate: file_rate.unwrap_or_default(),
                    start: index.timecode.map(|(t, _)| t).unwrap_or_default(),
                };
                commands.push(Command::SetTimecode { timecode: Some(tc) });
                tc
            }
        };
        // At the file's timecode, else at the start.
        let at = index
            .timecode
            .map_or(0, |(t, _)| timecode.position_of(t, rate));
        let (start, offset) = if at < 0 {
            (0, samples_to_ns(-at, rate))
        } else {
            (at, 0)
        };
        let length = index.end - offset;
        if length <= 0 {
            return Err(SessionError::Other(format!(
                "{} starts before the project and ends before it too",
                path.display()
            )));
        }
        let mut video = self.project.video.clone();
        let p = &mut self.project;
        let source_id: VideoSourceId = p.ids.allocate();
        video.sources.insert(
            source_id,
            VideoSource {
                path: path.clone(),
                width: v.width,
                height: v.height,
                par: v.par,
                fps: v.fps,
                codec: v.codec.clone(),
                duration: index.end,
                timecode: index.timecode,
                sound: info.audio.iter().map(|a| a.channels).collect(),
            },
        );
        if video.tracks.is_empty() {
            video.tracks.push(VideoTrack {
                id: p.ids.allocate(),
                name: "Video 1".into(),
                clips: Vec::new(),
                hidden: false,
            });
        }
        let clip_id: VideoClipId = p.ids.allocate();
        if let Some(t) = video.tracks.first_mut() {
            t.clips.push(VideoClip {
                id: clip_id,
                source: source_id,
                start,
                offset,
                length,
            });
        }
        commands.push(Command::SetVideo {
            video: Box::new(video),
        });
        // Its sound, where the picture is.
        if let Some(audio) = sound {
            let source = AudioSource {
                id: p.ids.allocate(),
                name: audio.name.clone(),
                spec: SourceSpec::File {
                    path: audio.path.clone(),
                    channels: audio.channels as u16,
                    frames: audio.frames as i64,
                    sample_rate: audio.sample_rate,
                },
            };
            let skip = ns_to_samples(offset, rate);
            let length = source.frames(rate) - skip;
            let id: TrackId = p.ids.allocate();
            let layout = if audio.channels == 1 {
                faderframe_core::ChannelLayout::Mono
            } else {
                faderframe_core::ChannelLayout::Stereo
            };
            let track = Track::new(
                id,
                TrackKind::Audio,
                format!("{} (sound)", audio.name),
                TrackColor::palette(p.tracks.len()),
            )
            .with_layout(layout);
            let index_at = p
                .tracks
                .iter()
                .rposition(|t| t.kind.has_clips())
                .map_or(0, |i| i + 1);
            let clip = Clip {
                id: p.ids.allocate(),
                track: id,
                name: audio.name.clone(),
                color: None,
                start: faderframe_timeline::MusicalTime::ZERO,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source: source.id,
                    source_offset: skip,
                    length,
                    gain_db: 0.0,
                    fades: Default::default(),
                    stretch: Default::default(),
                    reversed: false,
                    warp: None,
                    pitch: None,
                    effects: None,
                }),
            };
            let mut clip = clip;
            clip.start = self.engine.samples_to_musical(&self.project, start);
            if let Ok(s) = media::open_stream(&audio.path) {
                self.sources.insert(source.id, crate::Source::Stream(s));
            }
            self.peaks.insert(source.id, Arc::new(audio.peaks));
            commands.push(Command::AddTrack {
                track: Box::new(track),
                index: index_at,
            });
            commands.push(Command::AddSource {
                source: Box::new(source),
            });
            commands.push(Command::AddClip {
                clip: Box::new(clip),
            });
        }
        self.video.known.insert(
            source_id,
            Known {
                path,
                index: Some(Arc::new(index)),
                ..Known::default()
            },
        );
        self.edit(Command::Batch {
            label: "Import Video".into(),
            commands,
        })?;
        self.reconcile_video();
        self.notify(
            NoticeLevel::Info,
            format!(
                "imported {} ({}×{}, {})",
                v.codec,
                v.width,
                v.height,
                fps_label(v.fps)
            ),
        );
        Ok(())
    }

    /// Picture edits and jobs.
    pub(crate) fn video_op(&mut self, op: VideoOp) -> Result<()> {
        let rate = self.project.sample_rate;
        let edit = |s: &mut Self, f: &dyn Fn(&mut Video) -> bool| -> Result<()> {
            let mut v = s.project.video.clone();
            if !f(&mut v) {
                return Err(SessionError::Other("no such video clip".into()));
            }
            s.edit(Command::SetVideo { video: Box::new(v) })
        };
        match op {
            VideoOp::Import { path, sound } => {
                self.import_video(path, sound);
                Ok(())
            }
            VideoOp::MoveClip { clip, start } => edit(self, &|v| {
                v.clip_mut(clip).map(|c| c.start = start).is_some()
            }),
            VideoOp::TrimClip {
                clip,
                start,
                offset,
                length,
            } => {
                let duration = self
                    .project
                    .video
                    .clip(clip)
                    .and_then(|(_, c)| self.project.video.sources.get(&c.source))
                    .map_or(i64::MAX, |s| s.duration);
                edit(self, &|v| {
                    v.clip_mut(clip)
                        .map(|c| {
                            c.offset = offset.clamp(0, duration);
                            c.length = length.clamp(1, duration - c.offset);
                            c.start = start;
                        })
                        .is_some()
                })
            }
            VideoOp::RemoveClip(clip) => edit(self, &|v| {
                let mut found = false;
                for t in &mut v.tracks {
                    let n = t.clips.len();
                    t.clips.retain(|c| c.id != clip);
                    found |= t.clips.len() != n;
                }
                found
            }),
            VideoOp::SpotToTimecode(clip) => {
                let tc = self.timecode();
                let at = self
                    .project
                    .video
                    .clip(clip)
                    .and_then(|(_, c)| self.project.video.sources.get(&c.source))
                    .and_then(|s| s.timecode)
                    .map(|(t, _)| tc.position_of(t, rate))
                    .ok_or_else(|| SessionError::Other("the video carries no timecode".into()))?;
                edit(self, &|v| {
                    v.clip_mut(clip)
                        .map(|c| c.start = at - ns_to_samples(c.offset, rate))
                        .is_some()
                })
            }
            VideoOp::SetOffset(ms) => edit(self, &|v| {
                v.offset_ms = ms.clamp(-1000.0, 1000.0);
                true
            }),
            VideoOp::ToggleTrack(track) => edit(self, &|v| {
                v.tracks
                    .iter_mut()
                    .find(|t| t.id == track)
                    .map(|t| t.hidden = !t.hidden)
                    .is_some()
            }),
            VideoOp::Export {
                clip,
                path,
                container,
            } => self.export_video(clip, path, container),
            VideoOp::SyncTest => {
                let dir = self.media_dir.clone();
                self.spawn_video("Making the sync test", None, move |_, _| {
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    let video = media::unique_path(&dir, "Sync Test").with_extension("mkv");
                    let sound = media::unique_path(&dir, "Sync Test Beeps");
                    faderframe_video::sync_test::make_sync_test(&video, &sound, 10, 25)
                        .map_err(|e| e.to_string())?;
                    Ok(Done::SyncTest { video, sound })
                });
                Ok(())
            }
            VideoOp::ChooseImport => {
                self.ui_requests.push(crate::UiRequest::ImportVideo);
                Ok(())
            }
            VideoOp::ChooseExport => {
                if self.project.video.tracks.iter().all(|t| t.clips.is_empty()) {
                    return Err(SessionError::Other("there is no video to export".into()));
                }
                self.ui_requests.push(crate::UiRequest::ExportMovie);
                Ok(())
            }
            VideoOp::Cancel => {
                for j in &self.video.jobs {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            }
        }
    }

    /// Write `path`: the picture of `clip` (the first when `None`) as it is
    /// coded, with the mix rendered for the whole of its file.
    fn export_video(
        &mut self,
        clip: Option<VideoClipId>,
        path: PathBuf,
        container: Container,
    ) -> Result<()> {
        let rate = self.project.sample_rate;
        let video = &self.project.video;
        let c = match clip {
            Some(id) => video.clip(id).map(|(_, c)| c.clone()),
            None => video
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .min_by_key(|c| c.start)
                .cloned(),
        }
        .ok_or_else(|| SessionError::Other("there is no video to export".into()))?;
        let source = video
            .sources
            .get(&c.source)
            .cloned()
            .ok_or_else(|| SessionError::Other("the video's file is unknown".into()))?;
        // The file's time zero and end on the timeline.
        let zero = c.start - ns_to_samples(c.offset, rate);
        let end = zero + ns_to_samples(source.duration, rate);
        let lead = (-zero).max(0) as usize;
        let from = zero.max(0);
        if end <= from {
            return Err(SessionError::Other(
                "the video ends before the project starts".into(),
            ));
        }
        let start = self.engine.samples_to_musical(&self.project, from);
        let stop = self.engine.samples_to_musical(&self.project, end);
        std::fs::create_dir_all(cache_dir()).map_err(|e| SessionError::Other(e.to_string()))?;
        let wav = cache_dir().join(format!("export-{}.wav", std::process::id()));
        let copy = self.render_copy();
        let settings = RenderSettings {
            range: RenderRange::Span { start, end: stop },
            channels: RenderChannels::Master,
            sample_rate: rate,
            tail_seconds: 0.0,
            normalize_db: None,
            format: WavFormat::Float32,
            dither: Dither::Off,
            report: false,
            ..RenderSettings::defaults_for(&copy, wav.clone())
        };
        let job =
            crate::render::start(copy, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        let name = path
            .file_name()
            .map_or_else(|| "movie".into(), |n| n.to_string_lossy().into_owned());
        let frames = (end - from) as usize;
        self.spawn_video(format!("Writing {name}"), None, move |share, cancel| {
            let progress = Arc::clone(&job.progress);
            let watch = std::thread::scope(|scope| {
                let w = scope.spawn(|| {
                    while !progress.cancel.load(Ordering::Relaxed) {
                        if cancel.load(Ordering::Relaxed) {
                            progress.cancel.store(true, Ordering::Relaxed);
                        }
                        share.set(progress.fraction() * 0.8);
                        if progress.fraction() >= 1.0 {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                });
                let r = job.join();
                progress.cancel.store(true, Ordering::Relaxed);
                let _ = w.join();
                r
            });
            watch.map_err(|e| e.to_string())?;
            // Silence before the project's start; exactly the file's length.
            let mut data = faderframe_audio_files::read_wav(&wav).map_err(|e| e.to_string())?;
            for ch in &mut data.channels {
                let mut c = vec![0.0; lead];
                c.extend_from_slice(ch);
                c.resize(lead + frames, 0.0);
                *ch = c;
            }
            let (format, dither) = match container {
                Container::Mov => (WavFormat::Pcm24, Dither::Tpdf),
                _ => (WavFormat::Float32, Dither::Off),
            };
            faderframe_audio_files::write_wav_with(&wav, &data.channels, rate, format, dither)
                .map_err(|e| e.to_string())?;
            let r = faderframe_video::mux::mux(
                &source.path,
                std::slice::from_ref(&wav),
                &path,
                container,
                cancel,
                |x| share.set(0.8 + 0.2 * x),
            );
            let _ = std::fs::remove_file(&wav);
            r.map_err(|e| e.to_string())?;
            Ok(Done::Exported { out: path })
        });
        Ok(())
    }

    /// Wait for the video jobs (tests, scripting).
    pub fn wait_for_video(&mut self) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            self.poll_video();
            if self.video.jobs.is_empty() || std::time::Instant::now() > end {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Forget what was known (another project).
    pub(crate) fn forget_video(&mut self) {
        for j in &self.video.jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(s) = &self.video.service {
            for k in self.video.known.keys() {
                s.remove(k.raw());
            }
        }
        self.video.known.clear();
    }
}

fn fps_label(fps: (u32, u32)) -> String {
    match FrameRate::from_fraction(fps.0, fps.1) {
        Some(r) => r.label(),
        None if fps.0 == 0 => "variable rate".into(),
        None => format!("{:.3} fps", fps.0 as f64 / fps.1.max(1) as f64),
    }
}

/// The label of `pos` in the project's timecode.
pub fn timecode_at(project: &faderframe_project::Project, pos: i64) -> Timecode {
    project
        .timecode
        .unwrap_or_default()
        .at(pos, project.sample_rate)
}
