//! The interactive score exported for a game engine: a stem per state and
//! layer (rendered with its tail; the loop between the entry and exit cues
//! in the `smpl` chunk, the cues in `cue `/`LIST` chunks — frames exact,
//! the graph's latency taken off the front), a stem per stinger, and the
//! metadata ([`super::formats`]): a JSON manifest, an FMOD Studio script,
//! a Wwise Audio File Importer list and a WAAPI script.
//!
//! A layer's stem is the mix with its tracks soloed (in place: the buses
//! and returns they feed come along); the tracks in no layer make the
//! "Base" stem; without layers a state is one "Mix" stem. A section state
//! renders its span of the arrangement; a scene state renders its clips
//! placed in the arrangement from the song's start (looped to the scene's
//! length, the arrangement's own clips left out) — as the launcher plays
//! them. The files go into `Audio/` of the folder chosen; files of the
//! same names are replaced.

use crate::render::{RenderProgress, render_one};
use faderframe_audio_files::markers::{WavCue, WavLoop, WavMarkers};
use faderframe_audio_files::{Dither, WavFormat, write_wav_chunks};
use faderframe_core::{MusicLayerId, MusicStateId, StingerId, TrackId};
use faderframe_engine::render_generated_sources;
use faderframe_project::interactive::{InteractiveScore, StateSource};
use faderframe_project::{Clip, ClipContent, Project, TrackKind};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

/// Where an export went.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportReport {
    pub folder: PathBuf,
    /// Every file written (stems first).
    pub files: Vec<PathBuf>,
    pub manifest: PathBuf,
}

/// An export running in the background.
pub struct ExportJob {
    progress: Arc<RenderProgress>,
    handle: Option<JoinHandle<Result<ExportReport, String>>>,
}

impl ExportJob {
    pub fn fraction(&self) -> f64 {
        self.progress.fraction()
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> Result<ExportReport, String> {
        match self.handle.take() {
            Some(h) => h.join().unwrap_or_else(|_| Err("the export failed".into())),
            None => Err("the export was not started".into()),
        }
    }
}

/// One stem's tracks.
#[derive(Clone, Debug, PartialEq)]
pub struct StemLayer {
    /// The score's layer (`None`: the base, or the whole mix).
    pub layer: Option<MusicLayerId>,
    pub name: String,
    /// Soloed for it (empty: nothing soloed — the whole mix).
    pub tracks: Vec<TrackId>,
}

/// The stems every state is rendered into (see the module docs).
pub fn stem_layers(project: &Project, score: &InteractiveScore) -> Vec<StemLayer> {
    let sources: Vec<TrackId> = project
        .tracks
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
            )
        })
        .map(|t| t.id)
        .collect();
    let mut out: Vec<StemLayer> = score
        .layers
        .iter()
        .map(|l| StemLayer {
            layer: Some(l.id),
            name: l.name.clone(),
            tracks: l
                .tracks
                .iter()
                .copied()
                .filter(|t| project.track(*t).is_some())
                .collect(),
        })
        .filter(|l| !l.tracks.is_empty())
        .collect();
    if out.is_empty() {
        return vec![StemLayer {
            layer: None,
            name: "Mix".into(),
            tracks: Vec::new(),
        }];
    }
    let rest: Vec<TrackId> = sources
        .into_iter()
        .filter(|t| !out.iter().any(|l| l.tracks.contains(t)))
        .collect();
    if !rest.is_empty() {
        out.insert(
            0,
            StemLayer {
                layer: None,
                name: "Base".into(),
                tracks: rest,
            },
        );
    }
    out
}

/// A state as written (frames at the export's rate, from its file's
/// start).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportedState {
    pub id: MusicStateId,
    pub name: String,
    /// "section" or "scene".
    pub kind: &'static str,
    /// What it plays ("Section ‘Verse’").
    pub source: String,
    pub looping: bool,
    pub then: Option<String>,
    /// Its length without the tail, and the files' length with it.
    pub length_frames: u64,
    pub file_frames: u64,
    pub entry_frame: u64,
    pub exit_frame: u64,
    pub cues: Vec<(String, u64)>,
    /// Project markers inside it.
    pub markers: Vec<(String, u64)>,
    pub bpm: f64,
    pub signature: (u8, u8),
    /// Tempo changes inside it.
    pub tempo_changes: Vec<(u64, f64)>,
    /// Every beat: frame, bar (1-based from the state's first) and beat
    /// (1-based).
    pub beats: Vec<(u64, u32, u32)>,
    /// Its stems: the layer's name and the file (relative to the folder).
    pub stems: Vec<(String, String)>,
}

/// A stinger as written.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportedStinger {
    pub id: StingerId,
    pub name: String,
    pub file: String,
    pub length_frames: u64,
    pub file_frames: u64,
    pub pickup_frames: u64,
    pub sync: faderframe_project::interactive::SyncPoint,
}

/// What the metadata writers are given.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportData {
    /// The score's name (the project's).
    pub name: String,
    pub folder: PathBuf,
    pub sample_rate: u32,
    pub bits: u16,
    pub channels: u16,
    pub states: Vec<ExportedState>,
    pub layers: Vec<StemLayer>,
    /// Track names by id (for the layers).
    pub track_names: Vec<(TrackId, String)>,
    pub stingers: Vec<ExportedStinger>,
}

impl ExportData {
    pub fn state(&self, id: MusicStateId) -> Option<&ExportedState> {
        self.states.iter().find(|s| s.id == id)
    }

    pub fn track_name(&self, id: TrackId) -> String {
        self.track_names
            .iter()
            .find(|(t, _)| *t == id)
            .map_or_else(|| "?".into(), |(_, n)| n.clone())
    }
}

/// `name` as a file name.
pub fn file_stem(name: &str) -> String {
    let s = crate::render::sanitize(name);
    if s.is_empty() { "Untitled".into() } else { s }
}

/// A project that renders a launcher scene as its state plays: the scene's
/// clips (as played) placed from the song's start, repeated to `length`,
/// the arrangement's clips left out.
pub fn scene_project(
    project: &Project,
    scene: faderframe_core::SceneId,
    length: MusicalTime,
) -> Project {
    let mut p = project.clone();
    let launcher = p.launcher_clips();
    for t in &mut p.tracks {
        t.clips.clear();
    }
    p.clips.retain(|id, _| launcher.contains(id));
    let keys: Vec<_> = p
        .launcher
        .slots
        .keys()
        .filter(|k| k.scene == scene)
        .copied()
        .collect();
    let rate = p.sample_rate;
    for key in keys {
        let Some(played) = p.launcher_clip_as_played(key) else {
            continue;
        };
        let at_zero = Clip {
            start: MusicalTime::ZERO,
            ..played.clone()
        };
        let clip_len = at_zero.end(&p.timeline, rate);
        if clip_len <= MusicalTime::ZERO {
            continue;
        }
        let mut start = MusicalTime::ZERO;
        while start < length {
            let id = p.ids.allocate();
            let mut c = Clip {
                id,
                start,
                ..played.clone()
            };
            // The last time round cut at the scene's end.
            if start + clip_len > length {
                let left = length - start;
                match &mut c.content {
                    ClipContent::Audio(a) => {
                        let s = p.timeline.to_samples(start, f64::from(rate));
                        let e = p.timeline.to_samples(length, f64::from(rate));
                        a.length = (e - s).clamp(1, a.length);
                    }
                    ClipContent::Midi(m) => {
                        m.length = left.min(m.length);
                        let end = m.length;
                        m.notes.retain(|n| n.start < end);
                    }
                    ClipContent::Takes(_) => {}
                }
            }
            if let Some(t) = p.track_mut(key.track) {
                t.clips.push(id);
            }
            p.clips.insert(id, c);
            start += clip_len;
        }
    }
    p.launcher.slots.clear();
    p.launcher.follow.clear();
    p.launcher.launch.clear();
    keep_listed_clips(&mut p);
    p
}

/// Only the clips a track lists stay (the launcher's are gone with it).
fn keep_listed_clips(p: &mut Project) {
    let listed: std::collections::HashSet<faderframe_core::ClipId> = p
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().copied())
        .collect();
    p.clips.retain(|id, _| listed.contains(id));
}

/// `project` with `tracks` soloed (none: nothing soloed).
fn soloed(project: &Project, tracks: &[TrackId]) -> Project {
    let mut p = project.clone();
    for t in &mut p.tracks {
        t.solo = tracks.contains(&t.id);
    }
    p
}

/// What one file is.
struct Plan {
    project: Project,
    start: MusicalTime,
    end: MusicalTime,
    path: PathBuf,
    markers: WavMarkers,
}

/// Start exporting `project`'s score into `dir` (media paths absolute).
pub fn start(project: Project, dir: PathBuf) -> Result<ExportJob, String> {
    let score = project.interactive.clone();
    let rate = score
        .export
        .sample_rate
        .unwrap_or(project.sample_rate)
        .max(8_000);
    let format = score.export.format;
    let tail = score.export.tail_seconds.max(0.0);
    let audio_dir = dir.join("Audio");
    std::fs::create_dir_all(&audio_dir).map_err(|e| format!("{}: {e}", audio_dir.display()))?;
    let mut project = project;
    // Never round the project's own loop while rendering.
    project.loop_enabled = false;
    let (data, plans) = plan(&project, &score, &dir, rate, format, tail)?;
    let total: u64 = plans
        .iter()
        .map(|p| frames_of(&p.project, p.start, p.end, rate, tail))
        .sum();
    let progress = Arc::new(RenderProgress::default());
    progress.total.store(total.max(1), Ordering::Relaxed);
    let p = Arc::clone(&progress);
    let handle = std::thread::Builder::new()
        .name("faderframe-score-export".into())
        .spawn(move || {
            let mut files = Vec::new();
            let mut sources = render_generated_sources(&project, rate);
            for (_, path, e) in crate::media::open_file_sources(&project, None, &mut sources) {
                tracing::warn!("score export: {}: {e}", path.display());
            }
            for plan in &plans {
                if p.cancel.load(Ordering::Relaxed) {
                    return Err("export cancelled".into());
                }
                let sr = f64::from(rate);
                let start = plan.project.timeline.to_samples(plan.start, sr);
                let frames = frames_of(&plan.project, plan.start, plan.end, rate, tail) as usize;
                // (The progress counts on over the files.)
                let audio = render_one(
                    &plan.project,
                    rate,
                    &sources,
                    start,
                    frames,
                    &p,
                    2,
                    None,
                    false,
                )
                .map_err(|e| e.to_string())?;
                let dither = if format.is_integer() {
                    Dither::Tpdf
                } else {
                    Dither::Off
                };
                write_wav_chunks(
                    &plan.path,
                    &audio,
                    rate,
                    format,
                    dither,
                    None,
                    &plan.markers.chunks(rate),
                )
                .map_err(|e| format!("{}: {e}", plan.path.display()))?;
                files.push(plan.path.clone());
            }
            let written = super::formats::write_all(&score, &data)?;
            let manifest = written
                .first()
                .cloned()
                .unwrap_or_else(|| data.folder.clone());
            files.extend(written);
            Ok(ExportReport {
                folder: data.folder.clone(),
                files,
                manifest,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(ExportJob {
        progress,
        handle: Some(handle),
    })
}

fn frames_of(project: &Project, a: MusicalTime, b: MusicalTime, rate: u32, tail: f32) -> u64 {
    let sr = f64::from(rate);
    let len = project.timeline.to_samples(b, sr) - project.timeline.to_samples(a, sr);
    (len.max(0) + (f64::from(tail) * sr) as i64) as u64
}

/// The files to render and the metadata (see the module docs).
fn plan(
    project: &Project,
    score: &InteractiveScore,
    dir: &Path,
    rate: u32,
    format: WavFormat,
    tail: f32,
) -> Result<(ExportData, Vec<Plan>), String> {
    let sr = f64::from(rate);
    let layers = stem_layers(project, score);
    let mut states = Vec::new();
    let mut plans = Vec::new();
    let mut taken = std::collections::HashSet::new();
    let mut unique = |base: String| {
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.to_lowercase()) {
            name = format!("{base} ({n})");
            n += 1;
        }
        name
    };
    for s in &score.states {
        let Some(span) = project.state_span(s) else {
            tracing::warn!("score export: ‘{}’ plays nothing: left out", s.name);
            continue;
        };
        let (render_project, kind) = match s.source {
            StateSource::Section { .. } => (project.clone(), "section"),
            StateSource::Scene { scene } => (scene_project(project, scene, span.end), "scene"),
        };
        let tl = &render_project.timeline;
        let start = tl.to_samples(span.start, sr);
        let frame = |t: MusicalTime| (tl.to_samples(t, sr) - start).max(0) as u64;
        let length_frames = frame(span.end);
        let entry_frame = frame(span.loop_start);
        let exit_frame = frame(span.loop_end);
        let cues: Vec<(String, u64)> = s
            .cues
            .iter()
            .filter(|c| span.start + c.at <= span.end)
            .map(|c| (c.name.clone(), frame(span.start + c.at)))
            .collect();
        let markers: Vec<(String, u64)> = match s.source {
            StateSource::Section { .. } => project
                .markers
                .iter()
                .filter(|m| m.position >= span.start && m.position < span.end)
                .map(|m| (m.name.clone(), frame(m.position)))
                .collect(),
            StateSource::Scene { .. } => Vec::new(),
        };
        let mut wav = WavMarkers::default();
        if s.looping {
            wav.loops.push(WavLoop {
                start: entry_frame as u32,
                end: exit_frame as u32,
            });
        }
        wav.cues.push(WavCue {
            frame: entry_frame as u32,
            label: "Entry".into(),
        });
        wav.cues.push(WavCue {
            frame: exit_frame as u32,
            label: "Exit".into(),
        });
        for (name, f) in cues.iter().chain(&markers) {
            wav.cues.push(WavCue {
                frame: *f as u32,
                label: name.clone(),
            });
        }
        let file_frames = length_frames + (f64::from(tail) * sr) as u64;
        // Tempo and beats.
        let bpm = tl.tempo.bpm_at(span.start);
        let first_bar = tl.meter.bar_at(span.start);
        let sig = tl.meter.signature_of_bar(first_bar);
        let tempo_changes = tl
            .tempo
            .points()
            .iter()
            .filter(|p| p.position > span.start && p.position < span.end)
            .map(|p| (frame(p.position), p.bpm))
            .collect();
        let mut beats = Vec::new();
        let mut bar = first_bar;
        loop {
            let bar_start = tl.meter.bar_start(bar);
            if bar_start >= span.end {
                break;
            }
            let bsig = tl.meter.signature_of_bar(bar);
            for k in 0..i64::from(bsig.numerator.max(1)) {
                let t = bar_start + bsig.beat_length() * k;
                if t >= span.start && t < span.end {
                    beats.push((frame(t), (bar - first_bar + 1) as u32, k as u32 + 1));
                }
            }
            bar += 1;
        }
        let mut stems = Vec::new();
        for l in &layers {
            let file = unique(format!("{} - {}", file_stem(&s.name), file_stem(&l.name)));
            let rel = format!("Audio/{file}.wav");
            plans.push(Plan {
                project: if l.tracks.is_empty() {
                    render_project.clone()
                } else {
                    soloed(&render_project, &l.tracks)
                },
                start: span.start,
                end: span.end,
                path: dir.join(&rel),
                markers: wav.clone(),
            });
            stems.push((l.name.clone(), rel));
        }
        states.push(ExportedState {
            id: s.id,
            name: s.name.clone(),
            kind,
            source: project.state_source_name(s.source),
            looping: s.looping,
            then: s.then.and_then(|t| score.state(t)).map(|t| t.name.clone()),
            length_frames,
            file_frames,
            entry_frame,
            exit_frame,
            cues,
            markers,
            bpm,
            signature: (sig.numerator, sig.denominator),
            tempo_changes,
            beats,
            stems,
        });
    }
    if plans.is_empty() {
        return Err("no state of the score plays anything to export".into());
    }
    let mut stingers = Vec::new();
    for st in &score.stingers {
        let Some(length) = project.stinger_length(st) else {
            tracing::warn!(
                "score export: the stinger ‘{}’ plays an empty slot: left out",
                st.name
            );
            continue;
        };
        let mut p = scene_project(project, st.slot.scene, length);
        // Only the stinger's own track.
        for t in &mut p.tracks {
            if t.id != st.slot.track {
                t.clips.clear();
            }
        }
        keep_listed_clips(&mut p);
        let p = soloed(&p, &[st.slot.track]);
        let file = unique(format!("Stinger - {}", file_stem(&st.name)));
        let rel = format!("Audio/{file}.wav");
        let tl = &p.timeline;
        let length_frames = tl.to_samples(length, sr).max(0) as u64;
        let pickup_frames = tl.to_samples(st.pickup, sr).max(0) as u64;
        let mut wav = WavMarkers::default();
        wav.cues.push(WavCue {
            frame: pickup_frames as u32,
            label: "Sync".into(),
        });
        plans.push(Plan {
            project: p,
            start: MusicalTime::ZERO,
            end: length,
            path: dir.join(&rel),
            markers: wav,
        });
        stingers.push(ExportedStinger {
            id: st.id,
            name: st.name.clone(),
            file: rel,
            length_frames,
            file_frames: length_frames + (f64::from(tail) * sr) as u64,
            pickup_frames,
            sync: st.sync.clone(),
        });
    }
    let data = ExportData {
        name: project.name.clone(),
        folder: dir.to_path_buf(),
        sample_rate: rate,
        bits: format.bits(),
        channels: 2,
        states,
        layers,
        track_names: project
            .tracks
            .iter()
            .map(|t| (t.id, t.name.clone()))
            .collect(),
        stingers,
    };
    Ok((data, plans))
}
