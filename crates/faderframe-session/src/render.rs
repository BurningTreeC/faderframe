//! Offline render / export ("bounce").
//!
//! A render runs on a worker thread through the same engine code path as
//! live playback ([`faderframe_engine::offline::OfflineRenderer`]), faster
//! than realtime. The project is cloned for the job, so editing can continue
//! while it runs.

use crate::delivery::{Finish, Finished};
use faderframe_audio_files::{Dither, WavFormat, write_wav_with};
use faderframe_core::{TrackId, db_to_gain};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::{Project, TrackKind};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderRange {
    /// From the start to the end of the last clip.
    Project,
    /// The loop range.
    Loop,
    /// Bars `start..end` (0-based, end exclusive).
    Bars { start: i32, end: i32 },
    /// An exact span.
    Span {
        start: MusicalTime,
        end: MusicalTime,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderSource {
    /// The master output.
    Master,
    /// One file per audio-producing track (solo-in-place, so buses and
    /// returns the track feeds are included).
    Stems,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderChannels {
    Stereo,
    Mono,
    /// The first output channel alone (a mono master, e.g. freezing a mono
    /// track).
    First,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderSettings {
    pub range: RenderRange,
    pub source: RenderSource,
    pub format: WavFormat,
    pub sample_rate: u32,
    pub channels: RenderChannels,
    /// Extra time after the range for reverb/echo tails.
    pub tail_seconds: f32,
    /// Normalise to this peak level (dBFS; when `finish` does nothing).
    pub normalize_db: Option<f32>,
    /// Loudness normalisation and true-peak limiting for delivery.
    pub finish: Finish,
    /// Dither for integer formats.
    pub dither: Dither,
    /// Measure every written file (loudness, true peak).
    pub report: bool,
    /// File for a master render, directory for stems.
    pub output: PathBuf,
}

impl RenderSettings {
    pub fn defaults_for(project: &Project, output: PathBuf) -> Self {
        Self {
            range: RenderRange::Project,
            source: RenderSource::Master,
            format: WavFormat::Pcm24,
            sample_rate: project.sample_rate,
            channels: RenderChannels::Stereo,
            tail_seconds: 2.0,
            normalize_db: None,
            finish: Finish::default(),
            dither: Dither::Tpdf,
            report: true,
            output,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("the project has no loop range")]
    NoLoop,
    #[error("the render range is empty")]
    EmptyRange,
    #[error("there are no tracks to render as stems")]
    NoStems,
    #[error("render cancelled")]
    Cancelled,
    #[error(transparent)]
    Engine(#[from] faderframe_engine::EngineError),
    #[error("cannot write {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Shared progress of a running job.
#[derive(Debug, Default)]
pub struct RenderProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl RenderProgress {
    pub fn fraction(&self) -> f64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.done.load(Ordering::Relaxed) as f64 / total as f64
        }
    }
}

/// One written file.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub path: PathBuf,
    /// What finishing did and how the file measures (`None` without
    /// [`RenderSettings::report`] and finishing).
    pub finished: Option<Finished>,
}

pub struct RenderJob {
    pub progress: Arc<RenderProgress>,
    handle: Option<JoinHandle<Result<Vec<Rendered>, RenderError>>>,
}

impl RenderJob {
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    /// Wait for the result (call after `is_finished`, or to block).
    pub fn join(mut self) -> Result<Vec<Rendered>, RenderError> {
        match self.handle.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(RenderError::Cancelled),
            None => Err(RenderError::Cancelled),
        }
    }
}

/// Musical range to render (without tail).
pub fn resolve_range(
    project: &Project,
    range: RenderRange,
) -> Result<(MusicalTime, MusicalTime), RenderError> {
    let (a, b) = match range {
        RenderRange::Project => (MusicalTime::ZERO, project.content_end()),
        RenderRange::Loop => {
            let r = project.loop_range.ok_or(RenderError::NoLoop)?;
            (r.start, r.end)
        }
        RenderRange::Bars { start, end } => (
            project.timeline.meter.bar_start(start.max(0)),
            project.timeline.meter.bar_start(end.max(start + 1)),
        ),
        RenderRange::Span { start, end } => (start, end),
    };
    if b <= a {
        return Err(RenderError::EmptyRange);
    }
    Ok((a, b))
}

pub(crate) fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn stem_tracks(project: &Project) -> Vec<(TrackId, String)> {
    project
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Master && t.kind.has_audio())
        .map(|t| (t.id, t.name.clone()))
        .collect()
}

/// Render `project` (absolute media paths) from `a` to `b` plus `tail`
/// seconds at `sample_rate`, in stereo, on the calling thread (the album's
/// worker). `progress` counts frames.
pub(crate) fn render_span(
    project: &Project,
    sample_rate: u32,
    a: MusicalTime,
    b: MusicalTime,
    tail: f32,
    progress: &RenderProgress,
) -> Result<Vec<Vec<f32>>, RenderError> {
    let mut unlooped;
    let project = if project.loop_enabled {
        unlooped = project.clone();
        unlooped.loop_enabled = false;
        &unlooped
    } else {
        project
    };
    let sr = sample_rate as f64;
    let start = project.timeline.to_samples(a, sr);
    let end = project.timeline.to_samples(b, sr) + (tail.max(0.0) as f64 * sr) as i64;
    let frames = (end - start).max(0) as usize;
    if frames == 0 {
        return Err(RenderError::EmptyRange);
    }
    progress.done.store(0, Ordering::Relaxed);
    progress.total.store(frames as u64, Ordering::Relaxed);
    let mut sources = render_generated_sources(project, sample_rate);
    for (_, path, e) in crate::media::open_file_sources(project, None, &mut sources) {
        tracing::warn!("render: {}: {e}", path.display());
    }
    render_one(project, sample_rate, &sources, start, frames, progress)
}

fn render_one(
    project: &Project,
    sample_rate: u32,
    sources: &faderframe_engine::SourceMap,
    start: i64,
    frames: usize,
    progress: &RenderProgress,
) -> Result<Vec<Vec<f32>>, RenderError> {
    let config = EngineConfig {
        sample_rate,
        max_block_size: 1024,
        measure_nodes: false,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(project, sources, config, 1024, 2)?;
    // Faster than realtime on every core.
    let workers = faderframe_realtime::default_worker_count();
    if workers > 0 {
        r.processor.set_worker_pool(Some(std::sync::Arc::new(
            faderframe_realtime::WorkerPool::new(faderframe_realtime::PoolConfig::new(workers)),
        )));
    }
    r.play_from(start)?;
    let mut out = vec![Vec::with_capacity(frames), Vec::with_capacity(frames)];
    while out[0].len() < frames {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        let n = (frames - out[0].len()).min(16_384);
        let chunk = r.render(n);
        for (o, c) in out.iter_mut().zip(chunk) {
            o.extend_from_slice(&c);
        }
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(out)
}

fn finish(
    mut audio: Vec<Vec<f32>>,
    settings: &RenderSettings,
    path: &Path,
) -> Result<Rendered, RenderError> {
    if settings.channels == RenderChannels::First {
        audio.truncate(1);
    }
    if settings.channels == RenderChannels::Mono {
        let mono: Vec<f32> = audio[0]
            .iter()
            .zip(&audio[1])
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        audio = vec![mono];
    }
    let rate = settings.sample_rate;
    let finished = if !settings.finish.is_none() {
        Some(settings.finish.apply(&mut audio, rate))
    } else {
        let mut gain_db = 0.0;
        if let Some(target) = settings.normalize_db {
            let peak = audio.iter().flatten().fold(0.0f32, |m, s| m.max(s.abs()));
            if peak > 1e-9 {
                let g = db_to_gain(target) / peak;
                gain_db = 20.0 * (g as f64).log10();
                for ch in &mut audio {
                    for s in ch.iter_mut() {
                        *s *= g;
                    }
                }
            }
        }
        settings.report.then(|| Finished {
            gain_db,
            limited_db: 0.0,
            report: faderframe_analysis::delivery::measure(&audio, rate),
        })
    };
    let dither = if settings.format.is_integer() {
        settings.dither
    } else {
        Dither::Off
    };
    write_wav_with(path, &audio, rate, settings.format, dither).map_err(|source| {
        RenderError::Io {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(Rendered {
        path: path.to_path_buf(),
        finished,
    })
}

/// A project that renders only `track` after its inserts and before its
/// fader (unity, centre, no sends or fader automation, straight to a plain
/// master), and the span its clips cover. `None` when it has no clips.
pub fn track_render_project(
    project: &Project,
    track: TrackId,
) -> Option<(Project, MusicalTime, MusicalTime)> {
    use faderframe_automation::AutomationTarget as A;
    let clips = project.clips_of(track);
    let start = clips.iter().map(|c| c.start).min()?;
    let end = clips
        .iter()
        .map(|c| c.end(&project.timeline, project.sample_rate))
        .max()?;
    let mono = project
        .track(track)
        .is_some_and(|t| t.layout == faderframe_core::ChannelLayout::Mono);
    let mut p = project.clone();
    for t in &mut p.tracks {
        t.solo = t.id == track;
        if t.kind == TrackKind::Master {
            // A mono track renders through a mono master: no pan law.
            if mono {
                t.layout = faderframe_core::ChannelLayout::Mono;
            }
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.mute = false;
            t.phase_invert = false;
            t.inserts.clear();
            t.automation.lanes.clear();
        }
        if t.id == track {
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.mute = false;
            t.freeze = None;
            t.sends.clear();
            t.output = faderframe_project::OutputRouting::Master;
            t.automation
                .lanes
                .retain(|l| !matches!(l.target, A::TrackVolume | A::TrackPan | A::TrackMute));
        }
    }
    Some((p, start, end))
}

/// Start rendering `project` on a worker thread.
pub fn start(project: Project, settings: RenderSettings) -> Result<RenderJob, RenderError> {
    let mut project = project;
    // Never wrap around the loop while bouncing.
    project.loop_enabled = false;
    let (a, b) = resolve_range(&project, settings.range)?;
    let sr = settings.sample_rate as f64;
    let start = project.timeline.to_samples(a, sr);
    let end =
        project.timeline.to_samples(b, sr) + (settings.tail_seconds.max(0.0) as f64 * sr) as i64;
    let frames = (end - start).max(0) as usize;
    if frames == 0 {
        return Err(RenderError::EmptyRange);
    }
    let stems = match settings.source {
        RenderSource::Master => Vec::new(),
        RenderSource::Stems => {
            let s = stem_tracks(&project);
            if s.is_empty() {
                return Err(RenderError::NoStems);
            }
            s
        }
    };
    let progress = Arc::new(RenderProgress::default());
    let jobs = stems.len().max(1) as u64;
    progress
        .total
        .store(frames as u64 * jobs, Ordering::Relaxed);
    let p = Arc::clone(&progress);
    let handle = std::thread::Builder::new()
        .name("faderframe-render".into())
        .spawn(move || -> Result<Vec<Rendered>, RenderError> {
            // File paths are absolute here (see `Session::render`). Streams
            // are opened afresh: their page tables must not be shared with
            // the live engine.
            let mut sources = render_generated_sources(&project, settings.sample_rate);
            for (_, path, e) in crate::media::open_file_sources(&project, None, &mut sources) {
                tracing::warn!("render: {}: {e}", path.display());
            }
            let mut written = Vec::new();
            if stems.is_empty() {
                let audio =
                    render_one(&project, settings.sample_rate, &sources, start, frames, &p)?;
                written.push(finish(audio, &settings, &settings.output)?);
            } else {
                std::fs::create_dir_all(&settings.output).map_err(|source| RenderError::Io {
                    path: settings.output.clone(),
                    source,
                })?;
                for (track, name) in &stems {
                    let mut stem = project.clone();
                    for t in &mut stem.tracks {
                        t.solo = t.id == *track;
                    }
                    let audio =
                        render_one(&stem, settings.sample_rate, &sources, start, frames, &p)?;
                    let path = settings.output.join(format!(
                        "{} - {}.wav",
                        sanitize(&project.name),
                        sanitize(name)
                    ));
                    written.push(finish(audio, &settings, &path)?);
                }
            }
            Ok(written)
        })
        .map_err(|source| RenderError::Io {
            path: PathBuf::from("<thread>"),
            source,
        })?;
    Ok(RenderJob {
        progress,
        handle: Some(handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_audio_files::read_wav;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ff-render-{}-{name}", std::process::id()))
    }

    #[test]
    fn renders_loop_range_of_demo_at_44k1_mono_normalised() {
        let project = faderframe_project::demo::demo_project(48_000);
        let path = tmp("loop.wav");
        let settings = RenderSettings {
            range: RenderRange::Bars { start: 4, end: 5 },
            channels: RenderChannels::Mono,
            sample_rate: 44_100,
            tail_seconds: 0.5,
            normalize_db: Some(-1.0),
            format: WavFormat::Float32,
            ..RenderSettings::defaults_for(&project, path.clone())
        };
        let job = start(project.clone(), settings).unwrap();
        let files = job.join().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path);
        let report = files[0].finished.unwrap().report;
        assert!((report.sample_peak - -1.0).abs() < 1e-3, "{report:?}");
        let wav = read_wav(&path).unwrap();
        assert_eq!(wav.sample_rate, 44_100);
        assert_eq!(wav.channels.len(), 1);
        // One 4/4 bar at 112 BPM plus 0.5 s tail.
        let expected = (4.0 * 60.0 / 112.0 * 44_100.0f64).round() as usize + 22_050;
        assert!(
            (wav.channels[0].len() as i64 - expected as i64).abs() <= 1,
            "{}",
            wav.channels[0].len()
        );
        let peak = wav.channels[0].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - db_to_gain(-1.0)).abs() < 1e-4,
            "normalised peak {peak}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn stems_write_one_file_per_track_and_cancel_works() {
        let project = faderframe_project::demo::demo_project(48_000);
        let dir = tmp("stems");
        let settings = RenderSettings {
            range: RenderRange::Bars { start: 0, end: 1 },
            source: RenderSource::Stems,
            tail_seconds: 0.0,
            ..RenderSettings::defaults_for(&project, dir.clone())
        };
        let files = start(project.clone(), settings.clone())
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(files.len(), stem_tracks(&project).len());
        assert!(files.iter().all(|f| f.path.exists()));
        std::fs::remove_dir_all(&dir).unwrap();

        let long = RenderSettings {
            range: RenderRange::Project,
            source: RenderSource::Master,
            output: tmp("cancel.wav"),
            ..settings
        };
        let job = start(project, long).unwrap();
        job.cancel();
        assert!(matches!(job.join(), Err(RenderError::Cancelled)));
    }

    #[test]
    fn delivery_renders_reach_the_loudness_target() {
        let project = faderframe_project::demo::demo_project(48_000);
        let path = tmp("delivery.wav");
        let settings = RenderSettings {
            range: RenderRange::Bars { start: 4, end: 8 },
            finish: crate::delivery::DELIVERY_PRESETS[0].finish,
            format: WavFormat::Pcm16,
            dither: Dither::Shaped,
            ..RenderSettings::defaults_for(&project, path.clone())
        };
        let files = start(project, settings).unwrap().join().unwrap();
        let f = files[0].finished.unwrap();
        assert!((f.report.integrated - -14.0).abs() < 0.1, "{f:?}");
        assert!(f.report.true_peak <= -0.98, "{f:?}");
        // The file holds what was measured (16-bit, dithered).
        let wav = read_wav(&path).unwrap();
        let again = faderframe_analysis::delivery::measure(&wav.channels, wav.sample_rate);
        assert!((again.integrated - f.report.integrated).abs() < 0.05);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn range_errors() {
        let mut p = faderframe_project::Project::new("e", 48_000);
        p.loop_range = None;
        assert!(matches!(
            resolve_range(&p, RenderRange::Loop),
            Err(RenderError::NoLoop)
        ));
        assert!(
            resolve_range(&p, RenderRange::Project).is_ok(),
            "at least one bar"
        );
    }
}
