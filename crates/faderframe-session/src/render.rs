//! Offline render / export ("bounce").
//!
//! A render runs on a worker thread through the same engine code path as
//! live playback ([`faderframe_engine::offline::OfflineRenderer`]), faster
//! than realtime. The project is cloned for the job, so editing can continue
//! while it runs.

use faderframe_audio_files::{WavFormat, write_wav};
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
    /// Normalise to this peak level (dBFS).
    pub normalize_db: Option<f32>,
    /// TPDF dither for integer formats.
    pub dither: bool,
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
            dither: true,
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

pub struct RenderJob {
    pub progress: Arc<RenderProgress>,
    handle: Option<JoinHandle<Result<Vec<PathBuf>, RenderError>>>,
}

impl RenderJob {
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    /// Wait for the result (call after `is_finished`, or to block).
    pub fn join(mut self) -> Result<Vec<PathBuf>, RenderError> {
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
    };
    if b <= a {
        return Err(RenderError::EmptyRange);
    }
    Ok((a, b))
}

fn sanitize(name: &str) -> String {
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

fn render_one(
    project: &Project,
    settings: &RenderSettings,
    sources: &faderframe_engine::SourceMap,
    start: i64,
    frames: usize,
    progress: &RenderProgress,
) -> Result<Vec<Vec<f32>>, RenderError> {
    let config = EngineConfig {
        sample_rate: settings.sample_rate,
        max_block_size: 1024,
        measure_nodes: false,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(project, sources, config, 1024, 2)?;
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
) -> Result<(), RenderError> {
    if settings.channels == RenderChannels::Mono {
        let mono: Vec<f32> = audio[0]
            .iter()
            .zip(&audio[1])
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        audio = vec![mono];
    }
    if let Some(target) = settings.normalize_db {
        let peak = audio.iter().flatten().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak > 1e-9 {
            let g = db_to_gain(target) / peak;
            for ch in &mut audio {
                for s in ch.iter_mut() {
                    *s *= g;
                }
            }
        }
    }
    let dither = settings.dither && settings.format.is_integer();
    write_wav(path, &audio, settings.sample_rate, settings.format, dither).map_err(|source| {
        RenderError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
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
        .spawn(move || -> Result<Vec<PathBuf>, RenderError> {
            let sources = render_generated_sources(&project, settings.sample_rate);
            let mut written = Vec::new();
            if stems.is_empty() {
                let audio = render_one(&project, &settings, &sources, start, frames, &p)?;
                finish(audio, &settings, &settings.output)?;
                written.push(settings.output.clone());
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
                    let audio = render_one(&stem, &settings, &sources, start, frames, &p)?;
                    let path = settings.output.join(format!(
                        "{} - {}.wav",
                        sanitize(&project.name),
                        sanitize(name)
                    ));
                    finish(audio, &settings, &path)?;
                    written.push(path);
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
        assert_eq!(files, vec![path.clone()]);
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
        assert!(files.iter().all(|f| f.exists()));
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
