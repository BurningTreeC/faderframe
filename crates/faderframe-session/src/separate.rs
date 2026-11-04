//! Stem separation: a clip's (or a track's) audio split into vocals,
//! drums, bass and the rest on new tracks.
//!
//! The model ([`faderframe_separate`], Sony's X-UMXL, CC BY 4.0) is not
//! shipped: its published checkpoint is downloaded once, on request, into
//! the data folder's `models/<id>` (the system's `curl`, a `.part` file,
//! checked against the publisher's MD5 before it takes its name). A
//! separation renders what it separates as it plays — the clips with
//! their gain, fades, warps and takes, without the track's devices, like
//! Merge Clips — in a worker, takes it to the model's rate (44.1 kHz),
//! separates it, brings each stem back to the project's rate and writes
//! it into the media folder. What the model leaves out (above its band at
//! higher project rates, and whatever the Wiener filter keeps back) goes
//! to "other", so the stems add up to what was separated exactly; the
//! instrumental is the mix less the vocals.
//!
//! When it is done the stems arrive in one undo step: several on new
//! tracks in a folder ("<name> Stems") right after the track they came
//! from, one on a track of its own, each track sent where that track
//! goes, every stem a clip where the separated audio was — and the
//! separated clips muted, so what plays is the stems.

use crate::render::{self, RenderProgress};
use crate::{GroupMenuEntry, NoticeLevel, Result, SelectMode, Session, SessionError};
use faderframe_audio_files::WavFormat;
use faderframe_core::{ChannelLayout, ClipId, TrackId};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, OutputRouting, Project,
    SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
/// The downloadable models (their licences and attributions too).
pub use faderframe_separate::models::{self, ModelInfo};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::thread::JoinHandle;

/// A stem a separation can place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Stem {
    Vocals,
    Drums,
    Bass,
    /// Everything the others are not.
    Other,
    /// Everything but the vocals (the mix less the vocals).
    Instrumental,
}

impl Stem {
    /// The four the models separate, in the order tracks are made.
    pub const SOURCES: [Stem; 4] = [Stem::Vocals, Stem::Drums, Stem::Bass, Stem::Other];

    pub fn label(self) -> &'static str {
        match self {
            Stem::Vocals => "Vocals",
            Stem::Drums => "Drums",
            Stem::Bass => "Bass",
            Stem::Other => "Other",
            Stem::Instrumental => "Instrumental",
        }
    }

    /// The model's name of the source (none for the instrumental, made
    /// from the others).
    fn source(self) -> Option<&'static str> {
        match self {
            Stem::Vocals => Some("vocals"),
            Stem::Drums => Some("drums"),
            Stem::Bass => Some("bass"),
            Stem::Other => Some("other"),
            Stem::Instrumental => None,
        }
    }

    /// From a dev action's word.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id.trim().to_ascii_lowercase().as_str() {
            "vocals" | "voice" => Stem::Vocals,
            "drums" => Stem::Drums,
            "bass" => Stem::Bass,
            "other" => Stem::Other,
            "instrumental" | "accompaniment" | "no-vocals" => Stem::Instrumental,
            _ => return None,
        })
    }

    fn color(self) -> TrackColor {
        TrackColor::palette(match self {
            Stem::Vocals => 0,
            Stem::Drums => 1,
            Stem::Bass => 2,
            Stem::Other => 3,
            Stem::Instrumental => 4,
        })
    }
}

/// What is separated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StemTarget {
    /// One audio clip (muted or not).
    Clip(ClipId),
    /// A track's audio clips as they play.
    Track(TrackId),
}

/// The undo step's name for these stems.
pub fn step_label(stems: &[Stem]) -> &'static str {
    match stems {
        [Stem::Vocals] => "Extract Vocals",
        [Stem::Instrumental] => "Remove Vocals",
        _ => "Separate Stems",
    }
}

/// How separations run (Preferences → Stems).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SeparationSettings {
    /// The model's id ([`models::MODELS`]).
    pub model: String,
    /// Wiener filter refinement passes (0 = none: fastest, the stems no
    /// longer add up; 1 = as the model's authors separate; 2 refines the
    /// stereo image further).
    pub iterations: u8,
}

impl Default for SeparationSettings {
    fn default() -> Self {
        Self {
            model: models::default_model().id.to_string(),
            iterations: 1,
        }
    }
}

/// Refinement passes the settings may ask for.
pub const MAX_ITERATIONS: u8 = 3;

/// A separator: the model, or a stand-in (tests).
pub trait StemSeparator: Send + Sync {
    /// The rate it separates at.
    fn sample_rate(&self) -> u32;
    /// Its sources' names, in the order [`Self::separate`] gives them.
    fn sources(&self) -> Vec<String>;
    /// Separate a stereo recording at [`Self::sample_rate`]: per source,
    /// two channels as long as the input. `progress` hears 0…1 and
    /// answers whether to go on.
    fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> std::result::Result<Vec<[Vec<f32>; 2]>, String>;
}

/// The model with the settings' refinement.
struct ModelSeparator {
    model: faderframe_separate::Separator,
    options: faderframe_separate::Options,
}

impl StemSeparator for ModelSeparator {
    fn sample_rate(&self) -> u32 {
        self.model.sample_rate()
    }

    fn sources(&self) -> Vec<String> {
        self.model.sources().to_vec()
    }

    fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> std::result::Result<Vec<[Vec<f32>; 2]>, String> {
        self.model
            .separate(left, right, &self.options, progress)
            .map(|stems| stems.into_iter().map(|s| s.channels).collect())
            .map_err(|e| e.to_string())
    }
}

/// Where a model lives.
pub fn model_dir(model: &ModelInfo) -> PathBuf {
    crate::media::data_dir().join("models").join(model.id)
}

fn model_file(model: &ModelInfo) -> PathBuf {
    model_dir(model).join(model.file)
}

/// Is the model on this computer (its file, as long as published)?
pub fn model_ready(model: &ModelInfo) -> bool {
    std::fs::metadata(model_file(model)).is_ok_and(|m| m.len() == model.bytes)
}

fn model_by_id(id: &str) -> Result<&'static ModelInfo> {
    models::by_id(id).ok_or_else(|| SessionError::Other(format!("no separation model “{id}”")))
}

fn megabytes(bytes: u64) -> u64 {
    bytes.div_ceil(1_000_000)
}

fn download(model: &ModelInfo) -> std::result::Result<(), String> {
    let dir = model_dir(model);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = model_file(model);
    let part = dir.join(format!("{}.part", model.file));
    let _ = std::fs::remove_file(&part);
    let status = std::process::Command::new("curl")
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--output",
        ])
        .arg(&part)
        .arg(model.url)
        .status()
        .map_err(|e| {
            format!(
                "curl: {e} (download {} by hand into {})",
                model.url,
                dir.display()
            )
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not download {} ({status})", model.file));
    }
    if let Err(e) = models::verify(&part, model) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &target).map_err(|e| format!("{}: {e}", target.display()))
}

/// A download under way.
struct Download {
    model: &'static ModelInfo,
    job: JoinHandle<std::result::Result<(), String>>,
}

/// What a running separation is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeparationStage {
    /// Rendering what is separated.
    Rendering,
    /// Loading the model.
    Loading,
    Separating,
    /// Writing the stems.
    Writing,
}

impl SeparationStage {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Rendering,
            1 => Self::Loading,
            2 => Self::Separating,
            _ => Self::Writing,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rendering => "Rendering",
            Self::Loading => "Loading the model",
            Self::Separating => "Separating",
            Self::Writing => "Writing the stems",
        }
    }
}

#[derive(Default)]
struct Progress {
    stage: AtomicU8,
    /// Of the separating stage, per mille.
    per_mille: AtomicU32,
    cancel: AtomicBool,
    render: RenderProgress,
}

impl Progress {
    fn set(&self, stage: SeparationStage) {
        self.stage.store(stage as u8, Ordering::Relaxed);
    }

    /// Overall, 0…1: rendering a tenth, separating most, writing the end.
    fn fraction(&self) -> f32 {
        match SeparationStage::from_u8(self.stage.load(Ordering::Relaxed)) {
            SeparationStage::Rendering => 0.1 * self.render.fraction() as f32,
            SeparationStage::Loading => 0.1,
            SeparationStage::Separating => {
                0.1 + 0.85 * self.per_mille.load(Ordering::Relaxed) as f32 / 1000.0
            }
            SeparationStage::Writing => 0.95,
        }
    }
}

/// A separation running, for the status line.
#[derive(Clone, Debug, PartialEq)]
pub struct SeparationStatus {
    /// What is separated (the clip's or track's name).
    pub name: String,
    pub stage: SeparationStage,
    /// Overall, 0…1.
    pub fraction: f32,
}

/// A written stem.
struct Written {
    stem: Stem,
    path: PathBuf,
    frames: i64,
    channels: u16,
}

/// What a separation needs to place its stems.
struct Request {
    name: String,
    stems: Vec<Stem>,
    /// The track the stems go after (and whose output they take).
    track: TrackId,
    /// The clips separated (muted when the stems arrive).
    clips: Vec<ClipId>,
    start: MusicalTime,
    rate: u32,
}

struct Job {
    request: Request,
    progress: Arc<Progress>,
    handle: JoinHandle<std::result::Result<Vec<Written>, String>>,
}

/// A separation waiting for its model's download.
struct Waiting {
    target: StemTarget,
    model: &'static ModelInfo,
    stems: Vec<Stem>,
}

#[derive(Default)]
pub(crate) struct SeparationState {
    settings: SeparationSettings,
    download: Option<Download>,
    waiting: Vec<Waiting>,
    jobs: Vec<Job>,
    /// A stand-in for the model (tests).
    separator: Option<Arc<dyn StemSeparator>>,
}

/// `x` (channels of equal length) from rate `from` to `to`, exactly
/// `len` frames long.
fn resample(
    x: &[Vec<f32>],
    from: u32,
    to: u32,
    len: usize,
) -> std::result::Result<Vec<Vec<f32>>, String> {
    if from == to {
        return Ok(x
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.resize(len, 0.0);
                c
            })
            .collect());
    }
    let mut r = faderframe_audio_files::resample::StreamResampler::new(from, to, x.len())
        .map_err(|e| e.to_string())?;
    let mut out: Vec<Vec<f32>> = x.iter().map(|_| Vec::with_capacity(len + 1024)).collect();
    let mut sink = |planes: &[&[f32]], n: usize| {
        for (o, p) in out.iter_mut().zip(planes) {
            o.extend_from_slice(&p[..n]);
        }
        Ok(())
    };
    let frames = x.first().map_or(0, Vec::len);
    let step = 1 << 16;
    let mut at = 0;
    while at < frames {
        let n = step.min(frames - at);
        let chunk: Vec<Vec<f32>> = x.iter().map(|c| c[at..at + n].to_vec()).collect();
        r.push(&chunk, n, &mut sink).map_err(|e| e.to_string())?;
        at += n;
    }
    r.finish(&mut sink).map_err(|e| e.to_string())?;
    for o in &mut out {
        o.resize(len, 0.0);
    }
    Ok(out)
}

/// The worker: render, separate, write. `mix` is what was rendered (one
/// or two channels at `rate`).
#[allow(clippy::too_many_arguments)]
fn run(
    project: Project,
    span: (MusicalTime, MusicalTime),
    mono: bool,
    separator: Option<Arc<dyn StemSeparator>>,
    model: &'static ModelInfo,
    iterations: u8,
    stems: Vec<Stem>,
    media: PathBuf,
    name: String,
    progress: Arc<Progress>,
) -> std::result::Result<Vec<Written>, String> {
    let rate = project.sample_rate;
    progress.set(SeparationStage::Rendering);
    let rendered = render::render_span(&project, rate, span.0, span.1, 0.0, &progress.render)
        .map_err(|e| e.to_string())?;
    drop(project);
    let cancelled = || progress.cancel.load(Ordering::Relaxed);
    if cancelled() {
        return Err("cancelled".into());
    }
    let mut mix = rendered;
    if mono {
        mix.truncate(1);
    }
    let len = mix.first().map_or(0, Vec::len);
    if len == 0 || mix.iter().all(|c| c.iter().all(|v| *v == 0.0)) {
        return Err("there is nothing to hear in it".into());
    }
    progress.set(SeparationStage::Loading);
    let separator: Arc<dyn StemSeparator> = match separator {
        Some(s) => s,
        None => {
            let path = model_file(model);
            models::verify(&path, model)?;
            Arc::new(ModelSeparator {
                model: faderframe_separate::Separator::load(&path).map_err(|e| e.to_string())?,
                options: faderframe_separate::Options {
                    iterations: usize::from(iterations.min(MAX_ITERATIONS)),
                    threads: std::thread::available_parallelism()
                        .map_or(2, |n| n.get().saturating_sub(1).max(1)),
                },
            })
        }
    };
    let names = separator.sources();
    let index = |s: Stem| s.source().and_then(|n| names.iter().position(|m| m == n));
    // With the filter refining, the stems add up: "other" takes what the
    // rest leave of the mix (above the model's band too).
    let refined = iterations > 0;
    let needed: Vec<Stem> = stems
        .iter()
        .flat_map(|s| match s {
            Stem::Instrumental => vec![Stem::Vocals],
            Stem::Other if refined => Stem::SOURCES.to_vec(),
            s => vec![*s],
        })
        .collect();
    if let Some(s) = needed.iter().find(|s| index(**s).is_none()) {
        return Err(format!(
            "the model has no {} stem",
            s.label().to_lowercase()
        ));
    }
    let model_rate = separator.sample_rate();
    let model_len = (len as f64 * f64::from(model_rate) / f64::from(rate)).round() as usize;
    let stereo = if mix.len() == 1 {
        vec![mix[0].clone(), mix[0].clone()]
    } else {
        mix.clone()
    };
    let input = resample(&stereo, rate, model_rate, model_len)?;
    drop(stereo);
    progress.set(SeparationStage::Separating);
    let separated = separator.separate(&input[0], &input[1], &mut |f| {
        progress
            .per_mille
            .store((f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
        !cancelled()
    });
    drop(input);
    let separated = separated?;
    if cancelled() {
        return Err("cancelled".into());
    }
    progress.set(SeparationStage::Writing);
    // Each source back at the project's rate, in the mix's channels.
    let mut back: Vec<Option<Vec<Vec<f32>>>> = vec![None; names.len()];
    for s in Stem::SOURCES {
        let Some(i) = index(s) else { continue };
        if !needed.contains(&s) {
            continue;
        }
        let ch = &separated[i];
        let mut channels = resample(&[ch[0].clone(), ch[1].clone()], model_rate, rate, len)?;
        if mono {
            let r = channels.pop().unwrap_or_default();
            for (l, r) in channels[0].iter_mut().zip(&r) {
                *l = (*l + r) * 0.5;
            }
        }
        back[i] = Some(channels);
    }
    drop(separated);
    if let Some(o) = index(Stem::Other)
        && needed.contains(&Stem::Other)
        && refined
    {
        let mut rest = mix.clone();
        for s in [Stem::Vocals, Stem::Drums, Stem::Bass] {
            if let Some(Some(x)) = index(s).map(|i| &back[i]) {
                for (r, c) in rest.iter_mut().zip(x) {
                    for (a, b) in r.iter_mut().zip(c) {
                        *a -= b;
                    }
                }
            }
        }
        back[o] = Some(rest);
    }
    std::fs::create_dir_all(&media).map_err(|e| format!("{}: {e}", media.display()))?;
    let mut written = Vec::new();
    let result = (|| {
        for &stem in &stems {
            let audio: Vec<Vec<f32>> = match stem {
                Stem::Instrumental => {
                    let mut rest = mix.clone();
                    if let Some(Some(v)) = index(Stem::Vocals).map(|i| &back[i]) {
                        for (r, c) in rest.iter_mut().zip(v) {
                            for (a, b) in r.iter_mut().zip(c) {
                                *a -= b;
                            }
                        }
                    }
                    rest
                }
                s => index(s)
                    .and_then(|i| back[i].clone())
                    .ok_or_else(|| format!("no {} stem", s.label()))?,
            };
            let path = crate::media::unique_path(&media, &format!("{name} {}", stem.label()));
            faderframe_audio_files::write_wav(&path, &audio, rate, WavFormat::Float32, false)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(Written {
                stem,
                path,
                frames: len as i64,
                channels: audio.len() as u16,
            });
            if cancelled() {
                return Err("cancelled".to_string());
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        for w in &written {
            let _ = std::fs::remove_file(&w.path);
        }
        return Err(e);
    }
    Ok(written)
}

impl Session {
    /// How separations run.
    pub fn separation_settings(&self) -> &SeparationSettings {
        &self.separation.settings
    }

    pub fn set_separation_settings(&mut self, settings: SeparationSettings) {
        self.separation.settings = SeparationSettings {
            iterations: settings.iterations.min(MAX_ITERATIONS),
            model: if models::by_id(&settings.model).is_some() {
                settings.model
            } else {
                models::default_model().id.to_string()
            },
        };
    }

    /// Separate with `separator` instead of a downloaded model (tests and
    /// tools; `None`: the model again).
    pub fn set_stem_separator(&mut self, separator: Option<Arc<dyn StemSeparator>>) {
        self.separation.separator = separator;
    }

    /// Is the model on this computer?
    pub fn separation_model_ready(&self, id: &str) -> bool {
        models::by_id(id).is_some_and(model_ready)
    }

    /// The model downloading, with bytes so far and in all.
    pub fn separation_download(&self) -> Option<(&'static ModelInfo, u64, u64)> {
        self.separation.download.as_ref().map(|d| {
            let part = model_dir(d.model).join(format!("{}.part", d.model.file));
            let done = std::fs::metadata(part).map_or(0, |m| m.len());
            (d.model, done.min(d.model.bytes), d.model.bytes)
        })
    }

    /// Download a separation model (`None`: the one chosen in the
    /// settings); a notice says when it is there.
    pub fn download_separation_model(&mut self, id: Option<&str>) -> Result<()> {
        let id = id.map_or_else(|| self.separation.settings.model.clone(), String::from);
        let model = model_by_id(&id)?;
        if model_ready(model) {
            self.notify(
                NoticeLevel::Info,
                format!("The separation model {} is here already", model.name),
            );
            return Ok(());
        }
        if let Some(d) = &self.separation.download {
            if d.model.id != model.id {
                return Err(SessionError::Other(format!(
                    "{} is downloading: one model at a time",
                    d.model.name
                )));
            }
            return Ok(());
        }
        let job = std::thread::Builder::new()
            .name("faderframe-model-download".into())
            .spawn(move || download(model))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.separation.download = Some(Download { model, job });
        self.notify(
            NoticeLevel::Info,
            format!(
                "Downloading the separation model {} ({} MB, {})…",
                model.name,
                megabytes(model.bytes),
                model.licence
            ),
        );
        self.revision += 1;
        Ok(())
    }

    /// Separate `target` into `stems` with `model` (`None`: the settings'
    /// one); a model not yet here is downloaded first.
    pub(crate) fn separate_stems(
        &mut self,
        target: StemTarget,
        model: Option<String>,
        stems: Vec<Stem>,
    ) -> Result<()> {
        let mut wanted: Vec<Stem> = Vec::new();
        for s in stems {
            if !wanted.contains(&s) {
                wanted.push(s);
            }
        }
        if wanted.is_empty() {
            return Err(SessionError::Other("no stems asked for".into()));
        }
        let model = model_by_id(model.as_deref().unwrap_or(&self.separation.settings.model))?;
        if self.separation.separator.is_none() && !model_ready(model) {
            // What is asked is checked now, the download after; the
            // separation starts when the model is here.
            self.separation_request(target, &wanted)?;
            self.download_separation_model(Some(model.id))?;
            if self
                .separation
                .download
                .as_ref()
                .is_some_and(|d| d.model.id == model.id)
            {
                self.separation.waiting.push(Waiting {
                    target,
                    model,
                    stems: wanted,
                });
            }
            return Ok(());
        }
        self.start_separation(target, model, wanted)
    }

    /// The clips a separation takes, its track, name and span; refused
    /// when there is no audio to separate.
    fn separation_request(
        &self,
        target: StemTarget,
        stems: &[Stem],
    ) -> Result<(Request, Vec<Clip>)> {
        let p = &self.project;
        let (track, name, clips): (TrackId, String, Vec<Clip>) = match target {
            StemTarget::Clip(id) => {
                let c = p
                    .clip(id)
                    .ok_or_else(|| SessionError::Other(format!("no clip {id}")))?;
                if c.as_midi().is_some() {
                    return Err(SessionError::Other(
                        "only audio clips have stems to separate".into(),
                    ));
                }
                if p.is_launcher_clip(id) {
                    return Err(SessionError::Other(
                        "send the clip to the arrangement to separate it".into(),
                    ));
                }
                // Asked for by name: heard even when muted.
                let mut c = c.clone();
                c.muted = false;
                (c.track, c.name.clone(), vec![c])
            }
            StemTarget::Track(id) => {
                let t = p
                    .track(id)
                    .ok_or_else(|| SessionError::Other(format!("no track {id}")))?;
                let clips: Vec<Clip> = p
                    .clips_of(id)
                    .into_iter()
                    .filter(|c| c.as_midi().is_none() && !c.muted)
                    .cloned()
                    .collect();
                (id, t.name.clone(), clips)
            }
        };
        if clips.is_empty() {
            return Err(SessionError::Other(format!(
                "‘{name}’ has no audio to separate"
            )));
        }
        if self.separation.jobs.iter().any(|j| {
            j.request.track == track
                && j.request
                    .clips
                    .iter()
                    .any(|c| clips.iter().any(|x| x.id == *c))
        }) {
            return Err(SessionError::Other(format!(
                "‘{name}’ is being separated already"
            )));
        }
        let start = clips.iter().map(|c| c.start).min().unwrap_or_default();
        Ok((
            Request {
                name,
                stems: stems.to_vec(),
                track,
                clips: clips.iter().map(|c| c.id).collect(),
                start,
                rate: p.sample_rate,
            },
            clips,
        ))
    }

    fn start_separation(
        &mut self,
        target: StemTarget,
        model: &'static ModelInfo,
        stems: Vec<Stem>,
    ) -> Result<()> {
        let (mut request, clips) = self.separation_request(target, &stems)?;
        let whole = self.absolute_copy();
        let mono = whole
            .track(request.track)
            .is_some_and(|t| t.layout == ChannelLayout::Mono);
        // A project of its own: the clips on a track without devices.
        let rate = whole.sample_rate;
        let mut p = Project::new("Separate Stems", rate);
        p.timeline = whole.timeline.clone();
        p.ids = whole.ids.clone();
        let track = request.track;
        let mut t = Track::new(track, TrackKind::Audio, "Separate", TrackColor::palette(0))
            .with_layout(if mono {
                ChannelLayout::Mono
            } else {
                ChannelLayout::Stereo
            });
        for c in &clips {
            for s in c.content.sources() {
                if let Some(src) = whole.sources.get(&s) {
                    p.sources.insert(s, src.clone());
                }
            }
            t.clips.push(c.id);
            p.clips.insert(c.id, Clip { track, ..c.clone() });
        }
        p.tracks.insert(0, t);
        let (copy, start, end) = render::track_render_project(&p, track)
            .ok_or_else(|| SessionError::Other("nothing to separate".into()))?;
        request.start = start;
        let progress = Arc::new(Progress::default());
        let pr = Arc::clone(&progress);
        let separator = self.separation.separator.clone();
        let iterations = self.separation.settings.iterations;
        let media = self.media_dir.clone();
        let name = request.name.clone();
        let wanted = stems.clone();
        let handle = std::thread::Builder::new()
            .name("faderframe-separate".into())
            .spawn(move || {
                run(
                    copy,
                    (start, end),
                    mono,
                    separator,
                    model,
                    iterations,
                    wanted,
                    media,
                    name,
                    pr,
                )
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.notify(
            NoticeLevel::Info,
            format!(
                "Separating ‘{}’ into {}…",
                request.name,
                stems
                    .iter()
                    .map(|s| s.label().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        self.separation.jobs.push(Job {
            request,
            progress,
            handle,
        });
        self.revision += 1;
        Ok(())
    }

    /// Separations running (or waiting for their model).
    pub fn separating(&self) -> bool {
        !self.separation.jobs.is_empty() || !self.separation.waiting.is_empty()
    }

    /// The separations running, for the status line.
    pub fn separations(&self) -> Vec<SeparationStatus> {
        self.separation
            .jobs
            .iter()
            .map(|j| SeparationStatus {
                name: j.request.name.clone(),
                stage: SeparationStage::from_u8(j.progress.stage.load(Ordering::Relaxed)),
                fraction: j.progress.fraction(),
            })
            .collect()
    }

    /// Stop every separation (and drop those waiting for a download).
    pub fn cancel_separations(&mut self) {
        for j in &self.separation.jobs {
            j.progress.cancel.store(true, Ordering::Relaxed);
            j.progress.render.cancel.store(true, Ordering::Relaxed);
        }
        self.separation.waiting.clear();
        self.revision += 1;
    }

    /// Wait for the separations (tests).
    pub fn wait_for_separations(&mut self) {
        while self.separating() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_separations();
        }
    }

    /// Finish downloads and separations (the session's tick).
    pub(crate) fn poll_separations(&mut self) {
        if self
            .separation
            .download
            .as_ref()
            .is_some_and(|d| d.job.is_finished())
            && let Some(d) = self.separation.download.take()
        {
            let result = d
                .job
                .join()
                .unwrap_or_else(|_| Err("the download failed".into()));
            let waiting: Vec<Waiting> = {
                let (now, later) = std::mem::take(&mut self.separation.waiting)
                    .into_iter()
                    .partition(|w| w.model.id == d.model.id);
                self.separation.waiting = later;
                now
            };
            match result {
                Ok(()) => {
                    self.notify(
                        NoticeLevel::Info,
                        format!("The separation model {} is here", d.model.name),
                    );
                    for w in waiting {
                        if let Err(e) = self.start_separation(w.target, w.model, w.stems) {
                            self.notify(NoticeLevel::Error, format!("separate stems: {e}"));
                        }
                    }
                }
                Err(e) => self.notify(NoticeLevel::Error, format!("separation model: {e}")),
            }
            self.revision += 1;
        }
        let mut i = 0;
        while i < self.separation.jobs.len() {
            if !self.separation.jobs[i].handle.is_finished() {
                i += 1;
                continue;
            }
            let job = self.separation.jobs.remove(i);
            let cancelled = job.progress.cancel.load(Ordering::Relaxed);
            match job.handle.join() {
                Ok(Ok(written)) if cancelled => remove_files(&written),
                Ok(Ok(written)) => {
                    if let Err(e) = self.place_stems(&job.request, &written) {
                        remove_files(&written);
                        self.notify(NoticeLevel::Error, format!("separate stems: {e}"));
                    }
                }
                Ok(Err(_)) if cancelled => self.notify(
                    NoticeLevel::Info,
                    format!("Separating ‘{}’ cancelled", job.request.name),
                ),
                Ok(Err(e)) => self.notify(
                    NoticeLevel::Error,
                    format!("separate ‘{}’: {e}", job.request.name),
                ),
                Err(_) => self.notify(
                    NoticeLevel::Error,
                    format!("separate ‘{}’: the separation failed", job.request.name),
                ),
            }
            self.revision += 1;
        }
    }

    /// The stems on new tracks (in a folder when there are several), the
    /// separated clips muted: one undo step.
    fn place_stems(&mut self, r: &Request, written: &[Written]) -> Result<()> {
        let p = &self.project;
        // Clips gone meanwhile: the stems are no longer wanted.
        if r.clips.iter().all(|c| p.clip(*c).is_none()) || p.track(r.track).is_none() {
            remove_files(written);
            return Ok(());
        }
        let source_track = p
            .track(r.track)
            .ok_or_else(|| SessionError::Other("the track is gone".into()))?;
        let output = match source_track.output {
            OutputRouting::None => OutputRouting::Master,
            o => o,
        };
        let parent = source_track.folder;
        let color = source_track.color;
        let mut index = p.track_index(r.track).map_or(p.tracks.len(), |i| i + 1);
        // After whatever folder the track is: its contents follow it.
        if source_track.is_folder() {
            index = p
                .tracks
                .iter()
                .rposition(|t| p.in_folder(t, r.track))
                .map_or(index, |i| i + 1);
        }
        if p.sample_rate != r.rate {
            return Err(SessionError::Other(
                "the project's sample rate changed while separating: separate again".into(),
            ));
        }
        let mut commands = Vec::new();
        let folder = if written.len() > 1 {
            let id: TrackId = self.project.ids.allocate();
            let mut f = Track::new(id, TrackKind::Folder, format!("{} Stems", r.name), color);
            f.folder = parent;
            f.input = faderframe_project::InputRouting::None;
            f.output = OutputRouting::None;
            commands.push(Command::AddTrack {
                track: Box::new(f),
                index,
            });
            index += 1;
            Some(id)
        } else {
            None
        };
        let mut tracks = Vec::new();
        let mut clips = Vec::new();
        for w in written {
            let source = AudioSource {
                id: self.project.ids.allocate(),
                name: w.path.file_stem().map_or_else(
                    || w.stem.label().to_string(),
                    |s| s.to_string_lossy().to_string(),
                ),
                spec: SourceSpec::File {
                    path: w.path.clone(),
                    channels: w.channels,
                    frames: w.frames,
                    sample_rate: r.rate,
                },
            };
            let track_id: TrackId = self.project.ids.allocate();
            let label = match folder {
                Some(_) => w.stem.label().to_string(),
                None => format!("{} {}", r.name, w.stem.label()),
            };
            let mut t = Track::new(track_id, TrackKind::Audio, label, w.stem.color()).with_layout(
                if w.channels == 1 {
                    ChannelLayout::Mono
                } else {
                    ChannelLayout::Stereo
                },
            );
            t.folder = folder.or(parent);
            t.output = output;
            let clip_id: ClipId = self.project.ids.allocate();
            commands.push(Command::AddSource {
                source: Box::new(source.clone()),
            });
            commands.push(Command::AddTrack {
                track: Box::new(t),
                index,
            });
            index += 1;
            commands.push(Command::AddClip {
                clip: Box::new(Clip {
                    id: clip_id,
                    track: track_id,
                    name: format!("{} {}", r.name, w.stem.label()),
                    color: None,
                    start: r.start,
                    muted: false,
                    content: ClipContent::Audio(AudioClip {
                        source: source.id,
                        source_offset: 0,
                        length: w.frames.max(1),
                        gain_db: 0.0,
                        fades: ClipFades::default(),
                        stretch: StretchSettings::Off,
                        reversed: false,
                        warp: None,
                        pitch: None,
                        effects: None,
                        spectral: None,
                        envelope: None,
                    }),
                }),
            });
            tracks.push(track_id);
            clips.push(clip_id);
        }
        // What was separated stops playing: the stems take its place.
        for c in &r.clips {
            if self.project.clip(*c).is_some_and(|c| !c.muted) {
                commands.push(Command::SetClipMuted {
                    clip: *c,
                    muted: true,
                });
            }
        }
        self.batch(step_label(&r.stems), commands)?;
        if let Some(f) = folder {
            self.workspace.closed_folders.remove(&f);
        }
        self.selection.select_tracks(&tracks, SelectMode::Replace);
        self.notify(
            NoticeLevel::Info,
            format!(
                "‘{}’: {} on {} (the original is muted)",
                r.name,
                written
                    .iter()
                    .map(|w| w.stem.label().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", "),
                if written.len() > 1 {
                    format!("{} new tracks", written.len())
                } else {
                    "a new track".into()
                },
            ),
        );
        Ok(())
    }

    /// The stem entries of a clip's or track's menu (none for MIDI).
    pub fn stems_menu(&self, target: StemTarget) -> Vec<GroupMenuEntry> {
        let p = &self.project;
        let ok = match target {
            StemTarget::Clip(c) => {
                p.clip(c).is_some_and(|c| c.as_midi().is_none()) && !p.is_launcher_clip(c)
            }
            StemTarget::Track(t) => p.track(t).is_some_and(|t| {
                t.kind == TrackKind::Audio && p.clips_of(t.id).iter().any(|c| c.as_midi().is_none())
            }),
        };
        if !ok {
            return Vec::new();
        }
        let model =
            models::by_id(&self.separation.settings.model).unwrap_or(models::default_model());
        let entry = |label: &str, stems: Vec<Stem>| GroupMenuEntry {
            label: label.into(),
            action: Some(crate::Action::SeparateStems {
                target,
                model: None,
                stems,
            }),
            checked: None,
            separated: false,
        };
        let mut out = vec![
            entry(
                "Separate Stems (Vocals, Drums, Bass, Other)",
                Stem::SOURCES.to_vec(),
            ),
            entry("Extract Vocals", vec![Stem::Vocals]),
            entry("Remove Vocals (Instrumental)", vec![Stem::Instrumental]),
            entry(
                "Vocals and Instrumental",
                vec![Stem::Vocals, Stem::Instrumental],
            ),
        ];
        if self.separation.separator.is_none() && !model_ready(model) {
            out.push(GroupMenuEntry {
                label: format!(
                    "{} is downloaded first ({} MB, {})",
                    model.name,
                    megabytes(model.bytes),
                    model.licence
                ),
                action: None,
                checked: None,
                separated: true,
            });
        }
        out
    }
}

fn remove_files(written: &[Written]) {
    for w in written {
        let _ = std::fs::remove_file(&w.path);
        let _ = std::fs::remove_file(faderframe_audio_files::import::peaks_path_for(&w.path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_name_their_steps() {
        assert_eq!(step_label(&[Stem::Vocals]), "Extract Vocals");
        assert_eq!(step_label(&[Stem::Instrumental]), "Remove Vocals");
        assert_eq!(step_label(&Stem::SOURCES), "Separate Stems");
        assert_eq!(Stem::from_id("no-vocals"), Some(Stem::Instrumental));
    }

    #[test]
    fn a_resampled_signal_keeps_its_length() {
        let x = vec![
            (0..48_000)
                .map(|i| (i as f32 * 0.01).sin())
                .collect::<Vec<f32>>(),
        ];
        let y = resample(&x, 48_000, 44_100, 44_100).unwrap();
        assert_eq!(y[0].len(), 44_100);
        let z = resample(&y, 44_100, 48_000, 48_000).unwrap();
        assert_eq!(z[0].len(), 48_000);
        // A low tone comes back as it went.
        let worst = (2000..46_000)
            .map(|i| (z[0][i] - x[0][i]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "{worst}");
    }
}
