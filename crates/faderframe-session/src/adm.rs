//! Object-based masters: the project's surround master delivered as an ADM
//! BWF file (`faderframe_adm`) — its bed and its objects.
//!
//! The bed is the master's format as far as one of the profile's beds
//! (2.0 up to 7.1.2) holds it; its other channels (a 7.1.4's top front and
//! rear, a quad's rear pair) go as objects fixed at their speakers. Object
//! tracks (`Project::is_object`) go as objects, a stereo one as two, their
//! place from the panner and its automation in blocks: a new block wherever
//! the place has moved by more than [`MOVE`] (checked every [`STEP`]
//! frames). Everything is rendered in one pass: the master (the bed, through
//! its inserts and fader) and every object track (post fader, unpanned) on
//! outputs of their own, the audio streamed into the file as 24-bit PCM at
//! 48 or 96 kHz.

use crate::render::{RenderError, RenderJob, RenderProgress, RenderSettings, Rendered};
use faderframe_adm::{BEDS, BedChannel, Block, Master, Object, Profile};
use faderframe_adm::{SceneBlock, SceneSpeaker};
use faderframe_audio_files::import::{ImportProgress, ImportedAudio};
use faderframe_automation::{AutomationMode, AutomationTarget, SampleLane};
use faderframe_core::ChannelLayout;
use faderframe_core::{SurroundFormat, SurroundParam, TrackId};
use faderframe_project::{OutputRouting, Project, Track};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// Frames between checks of an object's place.
pub const STEP: u64 = 1024;
/// How far (in room units, 2 = wall to wall) a place moves before a new
/// block starts.
pub const MOVE: f32 = 0.01;

/// What an object-based master of the project holds.
#[derive(Clone, Debug, PartialEq)]
pub struct AdmPlan {
    /// The master's format.
    pub format: SurroundFormat,
    /// The bed: each channel and the master channel it comes from.
    pub bed: Vec<(BedChannel, usize)>,
    /// Master channels no bed holds, as objects at their speakers: (master
    /// channel, name, place).
    pub fixed: Vec<(usize, String, [f32; 3])>,
    /// Object tracks and their channel counts.
    pub tracks: Vec<(TrackId, String, usize)>,
}

impl AdmPlan {
    pub fn objects(&self) -> usize {
        self.fixed.len() + self.tracks.iter().map(|t| t.2).sum::<usize>()
    }

    pub fn channels(&self) -> usize {
        self.bed.len() + self.objects()
    }

    /// "7.1.2 bed and 12 objects".
    pub fn describe(&self) -> String {
        let bed = match self.bed.len() {
            0 => "no bed".to_string(),
            n => format!("{} bed", bed_name(n, self.bed.iter().any(|b| b.0.is_lfe()))),
        };
        let n = self.objects();
        format!("{bed} and {n} object{}", if n == 1 { "" } else { "s" })
    }
}

fn bed_name(channels: usize, lfe: bool) -> &'static str {
    match (channels, lfe) {
        (2, _) => "2.0",
        (3, _) => "3.0",
        (5, _) => "5.0",
        (6, true) => "5.1",
        (7, _) => "7.0",
        (8, true) => "7.1",
        (9, _) => "7.0.2",
        (10, true) => "7.1.2",
        _ => "custom",
    }
}

/// The bed channel a speaker of `format` is, if a bed has it.
fn bed_channel(
    format: SurroundFormat,
    s: &faderframe_core::surround::Speaker,
) -> Option<BedChannel> {
    use BedChannel::*;
    if s.lfe {
        return Some(Lfe);
    }
    Some(match s.label {
        "L" => L,
        "R" => R,
        "C" => C,
        // A quad's rear pair are corners no bed has without a centre.
        "Ls" if format != SurroundFormat::Quad => Ls,
        "Rs" if format != SurroundFormat::Quad => Rs,
        "Lss" => Lss,
        "Rss" => Rss,
        "Lrs" => Lrs,
        "Rrs" => Rrs,
        "Ltm" => Lts,
        "Rtm" => Rts,
        _ => return None,
    })
}

/// How the project's master would be delivered.
pub fn plan(project: &Project) -> Result<AdmPlan, String> {
    let format = crate::render::master_bed(project)
        .ok_or("the master is not a surround bed (Channel Format in its menu)")?;
    let speakers = format.speakers();
    let available: Vec<(BedChannel, usize)> = speakers
        .iter()
        .enumerate()
        .filter_map(|(i, s)| bed_channel(format, s).map(|c| (c, i)))
        .collect();
    // The largest bed the master has every channel of.
    let chosen = BEDS
        .iter()
        .rev()
        .find(|b| b.iter().all(|c| available.iter().any(|(a, _)| a == c)))
        .copied()
        .unwrap_or(&[]);
    let bed: Vec<(BedChannel, usize)> = chosen
        .iter()
        .filter_map(|c| available.iter().find(|(a, _)| a == c).copied())
        .collect();
    let fixed = speakers
        .iter()
        .enumerate()
        .filter(|(i, s)| !s.lfe && !bed.iter().any(|(_, b)| b == i))
        .map(|(i, s)| (i, format!("Bed {}", s.label), [s.x, s.y, s.z]))
        .collect();
    let tracks = project
        .objects()
        .map(|t| (t.id, t.name.clone(), t.layout.channel_count().clamp(1, 2)))
        .collect();
    let plan = AdmPlan {
        format,
        bed,
        fixed,
        tracks,
    };
    if plan.objects() > faderframe_adm::MAX_OBJECTS {
        return Err(format!(
            "{} objects: an Atmos master holds {}",
            plan.objects(),
            faderframe_adm::MAX_OBJECTS
        ));
    }
    if plan.channels() > faderframe_adm::MAX_CHANNELS {
        return Err(format!(
            "{} channels: an Atmos master holds {}",
            plan.channels(),
            faderframe_adm::MAX_CHANNELS
        ));
    }
    Ok(plan)
}

/// The panner's lanes of `t` that drive it (by `SurroundParam::index`).
fn lanes(project: &Project, t: &Track, rate: u32) -> [Option<SampleLane>; 6] {
    let tl = &project.timeline;
    let sr = f64::from(rate);
    SurroundParam::ALL.map(|p| {
        t.automation
            .lane(AutomationTarget::Surround(p))
            .filter(|l| l.mode != AutomationMode::Off && !l.curve.is_empty())
            .map(|l| SampleLane::from_curve(&l.curve, |m| tl.to_samples(m, sr)))
    })
}

/// Channel `c` of `t` (of `channels`) as an object from project sample
/// `start` for `frames` frames: blocks wherever its place moves.
pub fn object_blocks(
    project: &Project,
    t: &Track,
    c: usize,
    channels: usize,
    start: i64,
    frames: u64,
    rate: u32,
) -> Vec<Block> {
    let lanes = lanes(project, t, rate);
    let place = |at: i64| -> ([f32; 3], f32) {
        let pan = SurroundParam::ALL.iter().fold(t.surround, |pan, p| {
            match lanes[p.index()].as_ref().and_then(|l| l.value_at(at)) {
                Some(v) => p.set(pan, v as f32),
                None => pan,
            }
        });
        let x = if channels == 2 {
            let w = if c == 0 { -pan.width } else { pan.width };
            (pan.x + w).clamp(-1.0, 1.0)
        } else {
            pan.x
        };
        ([x, pan.y, pan.z], pan.spread)
    };
    let block = |from: u64, (position, size): ([f32; 3], f32)| Block {
        start: from,
        length: 0,
        position,
        size,
        gain: 1.0,
    };
    // Each step's place is taken at its middle: a block holds it, so a
    // movement is followed without lagging behind.
    let mid = |at: u64| start + (at + (STEP / 2).min(frames.saturating_sub(at))) as i64;
    let moving = lanes.iter().any(Option::is_some);
    let mut blocks = vec![block(0, place(if moving { mid(0) } else { start }))];
    if moving {
        let mut at = STEP;
        while at < frames {
            let now = place(mid(at));
            let Some(last) = blocks.last_mut() else { break };
            let moved = last
                .position
                .iter()
                .zip(now.0)
                .map(|(a, b)| (a - b).abs())
                .fold((last.size - now.1).abs(), f32::max);
            if moved > MOVE {
                last.length = at - last.start;
                blocks.push(block(at, now));
            }
            at += STEP;
        }
    }
    if let Some(last) = blocks.last_mut() {
        last.length = frames - last.start;
    }
    blocks
}

/// The master to write for `plan` over `frames` frames from project sample
/// `start`.
pub fn master(
    project: &Project,
    plan: &AdmPlan,
    profile: Profile,
    start: i64,
    frames: u64,
    rate: u32,
) -> Master {
    let mut objects: Vec<Object> = plan
        .fixed
        .iter()
        .map(|(_, name, place)| Object {
            name: name.clone(),
            blocks: vec![Block {
                start: 0,
                length: frames,
                position: *place,
                size: 0.0,
                gain: 1.0,
            }],
        })
        .collect();
    for (id, name, channels) in &plan.tracks {
        let Some(t) = project.track(*id) else {
            continue;
        };
        for c in 0..*channels {
            let name = match (*channels, c) {
                (1, _) => name.clone(),
                (_, 0) => format!("{name} L"),
                _ => format!("{name} R"),
            };
            objects.push(Object {
                name,
                blocks: object_blocks(project, t, c, *channels, start, frames, rate),
            });
        }
    }
    Master {
        name: project.name.clone(),
        profile,
        sample_rate: rate,
        frames,
        bed: plan.bed.iter().map(|b| b.0).collect(),
        objects,
    }
}

/// The project as the one-pass render plays it: the master on the first
/// outputs, each object track (unpanned, post fader) on its own after
/// them. Returns it and where each file channel comes from.
fn routed(project: &Project, plan: &AdmPlan) -> (Project, Vec<usize>, usize) {
    let mut p = project.clone();
    let master = plan.format.channels();
    let mut next = master;
    let mut track_outputs = Vec::new();
    for (id, _, channels) in &plan.tracks {
        if let Some(t) = p.track_mut(*id) {
            t.output = OutputRouting::Hardware {
                first_channel: next as u16,
            };
            t.object = false;
        }
        track_outputs.push((next, *channels));
        next += channels;
    }
    if let Some(m) = p.master_id().and_then(|m| p.track_mut(m)) {
        m.output = OutputRouting::Hardware { first_channel: 0 };
    }
    let mut order: Vec<usize> = plan.bed.iter().map(|b| b.1).collect();
    order.extend(plan.fixed.iter().map(|f| f.0));
    for (first, channels) in track_outputs {
        order.extend(first..first + channels);
    }
    (p, order, next)
}

/// Render the project (absolute media paths) to an ADM BWF file.
pub(crate) fn start(
    project: Project,
    settings: RenderSettings,
    profile: Profile,
) -> Result<RenderJob, RenderError> {
    let mut project = project;
    project.loop_enabled = false;
    let plan = plan(&project).map_err(RenderError::Adm)?;
    // The profile's rates.
    let rate = if settings.sample_rate == 96_000 {
        96_000
    } else {
        48_000
    };
    let (a, b) = crate::render::resolve_range(&project, settings.range)?;
    let sr = f64::from(rate);
    let start = project.timeline.to_samples(a, sr);
    let end =
        project.timeline.to_samples(b, sr) + (settings.tail_seconds.max(0.0) as f64 * sr) as i64;
    let frames = (end - start).max(0) as u64;
    if frames == 0 {
        return Err(RenderError::EmptyRange);
    }
    let meta = master(&project, &plan, profile, start, frames, rate);
    faderframe_adm::validate(&meta).map_err(|e| RenderError::Adm(e.to_string()))?;
    let (routed, order, outputs) = routed(&project, &plan);
    let progress = Arc::new(RenderProgress::default());
    progress.total.store(frames, Ordering::Relaxed);
    let p = Arc::clone(&progress);
    let path = settings.output.clone();
    RenderJob::spawn(progress, move || {
        let io = |source: std::io::Error| RenderError::Io {
            path: path.clone(),
            source,
        };
        let mut sources = faderframe_engine::render_generated_sources(&routed, rate);
        for (_, file, e) in crate::media::open_file_sources(&routed, None, &mut sources) {
            tracing::warn!("render: {}: {e}", file.display());
        }
        let config = faderframe_engine::EngineConfig {
            sample_rate: rate,
            max_block_size: 1024,
            measure_nodes: false,
            ..faderframe_engine::EngineConfig::default()
        };
        let mut r = faderframe_engine::offline::OfflineRenderer::new(
            &routed, &sources, config, 1024, outputs,
        )?;
        let workers = faderframe_realtime::default_worker_count();
        if workers > 0 {
            r.processor
                .set_worker_pool(Some(Arc::new(faderframe_realtime::WorkerPool::new(
                    faderframe_realtime::PoolConfig::new(workers),
                ))));
        }
        // The plugins' delay comes off the front: metadata and audio line up.
        let mut skip = r.controller.graph_stats().output_latency as u64;
        let mut w = faderframe_audio_files::bw64::Bw64Writer::create(
            &path,
            order.len() as u16,
            rate,
            faderframe_audio_files::WavFormat::Pcm24,
            settings.dither,
            &[(*b"chna", faderframe_adm::chna(&meta))],
        )
        .map_err(io)?;
        r.play_from(start)?;
        let mut done = 0u64;
        while done < frames {
            if p.cancel.load(Ordering::Relaxed) {
                drop(w);
                let _ = std::fs::remove_file(&path);
                return Err(RenderError::Cancelled);
            }
            let n = (frames - done).min(16_384) + skip.min(16_384);
            let chunk = r.render(n as usize);
            let from = skip.min(n) as usize;
            skip -= from as u64;
            let take = (n as usize - from).min((frames - done) as usize);
            if take == 0 {
                continue;
            }
            let silent = vec![0.0f32; take];
            let channels: Vec<&[f32]> = order
                .iter()
                .map(|&c| chunk.get(c).map_or(&silent[..], |v| &v[from..from + take]))
                .collect();
            w.write_planar(&channels, take).map_err(io)?;
            done += take as u64;
            p.done.store(done, Ordering::Relaxed);
        }
        w.finish(&[(*b"axml", faderframe_adm::axml(&meta).into_bytes())])
            .map_err(io)?;
        Ok(vec![Rendered {
            path: path.clone(),
            finished: None,
        }])
    })
}

// --- import -------------------------------------------------------------------

/// An ADM file read and split into media files, ready to become tracks.
pub(crate) struct Imported {
    pub name: String,
    /// The master's format for it (a bed holding the file's bed; with
    /// heights when an object rises).
    pub master: Option<SurroundFormat>,
    pub bed: Option<(ChannelLayout, ImportedAudio)>,
    pub objects: Vec<(String, ImportedAudio, Vec<SceneBlock>)>,
    pub notes: Vec<String>,
}

pub(crate) struct ImportJob {
    pub path: PathBuf,
    pub progress: Arc<ImportProgress>,
    handle: Option<std::thread::JoinHandle<Result<Imported, String>>>,
}

impl ImportJob {
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    pub fn join(mut self) -> Result<Imported, String> {
        match self.handle.take().map(std::thread::JoinHandle::join) {
            Some(Ok(r)) => r,
            _ => Err("the import stopped".into()),
        }
    }
}

/// Our layout for a file's bed, and for each of its channels the file
/// channel that carries it (`None`: silent).
pub(crate) fn bed_layout(bed: &[SceneSpeaker]) -> Option<(ChannelLayout, Vec<Option<usize>>)> {
    let has: Vec<BedChannel> = bed.iter().filter_map(|s| s.channel).collect();
    let track_of = |c: BedChannel| bed.iter().find(|s| s.channel == Some(c)).map(|s| s.track);
    if has.is_empty() {
        return None;
    }
    if has
        .iter()
        .all(|c| matches!(c, BedChannel::L | BedChannel::R))
    {
        return Some((
            ChannelLayout::Stereo,
            vec![track_of(BedChannel::L), track_of(BedChannel::R)],
        ));
    }
    let mut formats = SurroundFormat::ALL;
    formats.sort_by_key(|f| f.channels());
    formats.into_iter().find_map(|f| {
        let ours: Vec<Option<BedChannel>> =
            f.speakers().iter().map(|s| bed_channel(f, s)).collect();
        has.iter().all(|c| ours.contains(&Some(*c))).then(|| {
            (
                ChannelLayout::Surround(f),
                ours.iter().map(|c| c.and_then(track_of)).collect(),
            )
        })
    })
}

/// Read and split `path` into the project's media folder at `rate`.
pub(crate) fn start_import(path: PathBuf, media: PathBuf, rate: u32) -> ImportJob {
    let progress = Arc::new(ImportProgress::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c, file) = (Arc::clone(&progress), Arc::clone(&cancel), path.clone());
    let handle = std::thread::Builder::new()
        .name("faderframe-adm-import".into())
        .spawn(move || import(&file, &media, rate, &p, &c))
        .ok();
    ImportJob {
        path,
        progress,
        handle,
    }
}

fn import(
    path: &Path,
    media: &Path,
    rate: u32,
    progress: &ImportProgress,
    cancel: &AtomicBool,
) -> Result<Imported, String> {
    use faderframe_audio_files::wavstream::{WavFile, WavWriter, read_chunk};
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let (Some(axml), Some(chna)) = (
        read_chunk(path, b"axml").map_err(err)?,
        read_chunk(path, b"chna").map_err(err)?,
    ) else {
        return Err(format!(
            "{} is not an ADM BWF file (no axml and chna chunks)",
            path.display()
        ));
    };
    let scene =
        faderframe_adm::parse(&String::from_utf8_lossy(&axml), &chna).map_err(|e| e.to_string())?;
    let file = WavFile::open(path).map_err(err)?;
    let (frames, channels, file_rate) = (file.frames(), file.channels(), file.sample_rate());
    let mut notes = scene.notes.clone();
    let bed = bed_layout(&scene.bed);
    if !scene.bed.is_empty() && bed.is_none() {
        notes.push("the bed's layout is not one FaderFrame has: left out".into());
    }
    let heights = scene
        .objects
        .iter()
        .any(|o| o.blocks.iter().any(|b| b.position[2] > 0.01));
    let master = match bed.as_ref().map(|b| b.0) {
        Some(ChannelLayout::Surround(f)) if !heights || f.has_heights() => Some(f),
        _ if heights => Some(SurroundFormat::S714),
        Some(_) | None if !scene.objects.is_empty() => Some(SurroundFormat::S51),
        Some(_) | None => None,
    };
    // Split into files of our own at the file's rate.
    let tmp = std::env::temp_dir().join(format!(
        "faderframe-adm-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&tmp).map_err(err)?;
    let result = (|| {
        let mut bed_writer = match &bed {
            Some((layout, _)) => Some(
                WavWriter::create_with_mask(
                    &tmp.join("Bed.wav"),
                    layout.channel_count() as u16,
                    file_rate,
                    faderframe_audio_files::WavFormat::Float32,
                    faderframe_audio_files::Dither::Off,
                    match layout {
                        ChannelLayout::Surround(f) => Some(f.channel_mask()),
                        _ => None,
                    },
                )
                .map_err(err)?,
            ),
            None => None,
        };
        let mut object_writers = Vec::new();
        for (k, o) in scene.objects.iter().enumerate() {
            let name: String = o
                .name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == ' ' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let w = WavWriter::create(
                &tmp.join(format!("{k:03} {name}.wav")),
                1,
                file_rate,
                faderframe_audio_files::WavFormat::Float32,
                false,
            )
            .map_err(err)?;
            object_writers.push(w);
        }
        progress.total.store(frames * 2, Ordering::Relaxed);
        let chunk = 1 << 15;
        let mut bufs = vec![vec![0.0f32; chunk]; channels];
        let mut scratch = Vec::new();
        let silent = vec![0.0f32; chunk];
        let mut at = 0u64;
        while at < frames {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            let n = chunk.min((frames - at) as usize);
            {
                let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
                file.read(at, &mut slices, &mut scratch).map_err(err)?;
            }
            if let (Some(w), Some((_, from))) = (bed_writer.as_mut(), bed.as_ref()) {
                let chans: Vec<&[f32]> = from
                    .iter()
                    .map(|t| {
                        t.and_then(|t| bufs.get(t))
                            .map_or(&silent[..n], |b| &b[..n])
                    })
                    .collect();
                w.write_planar(&chans, n).map_err(err)?;
            }
            for (w, o) in object_writers.iter_mut().zip(&scene.objects) {
                let b = bufs.get(o.track).map_or(&silent[..n], |b| &b[..n]);
                w.write_planar(&[b], n).map_err(err)?;
            }
            at += n as u64;
            progress.done.store(at, Ordering::Relaxed);
        }
        let bed_file = match bed_writer {
            Some(w) => Some(w.finish().map_err(err)?),
            None => None,
        };
        let mut object_files = Vec::new();
        for w in object_writers {
            object_files.push(w.finish().map_err(err)?);
        }
        // Into the media folder at the project's rate.
        let quiet = ImportProgress::default();
        let bring = |f: &Path| {
            faderframe_audio_files::import::import_file(f, media, rate, &quiet, cancel)
                .map_err(|e| format!("{}: {e}", f.display()))
        };
        let bed = match (bed_file, bed.as_ref()) {
            (Some(f), Some((layout, _))) => Some((*layout, bring(&f)?)),
            _ => None,
        };
        let mut objects = Vec::new();
        for (f, o) in object_files.iter().zip(&scene.objects) {
            objects.push((o.name.clone(), bring(f)?, o.blocks.clone()));
            progress.done.fetch_add(
                frames / scene.objects.len().max(1) as u64,
                Ordering::Relaxed,
            );
        }
        Ok(Imported {
            name: path
                .file_stem()
                .map_or_else(|| "ADM".into(), |s| s.to_string_lossy().to_string()),
            master,
            bed,
            objects,
            notes,
        })
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// An object's blocks as automation points of one value: `(seconds,
/// value)`, as the file moves it (jumps over their interpolation, others
/// across the block), points that change nothing left out.
pub(crate) fn lane_points(
    blocks: &[SceneBlock],
    value: impl Fn(&SceneBlock) -> f32,
) -> Vec<(f64, f32)> {
    let mut points: Vec<(f64, f32)> = Vec::new();
    let mut prev: Option<f32> = None;
    for b in blocks {
        let v = value(b);
        match prev {
            None => points.push((b.start, v)),
            Some(p) if b.jump => {
                if (p - v).abs() > 1e-6 {
                    points.push((b.start, p));
                    points.push((b.start + b.interpolation.max(0.0), v));
                }
            }
            Some(_) => points.push((b.start + b.length.unwrap_or(0.0), v)),
        }
        prev = Some(v);
    }
    // Drop points between two of the same value.
    let mut out: Vec<(f64, f32)> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let same_before = out
            .last()
            .is_some_and(|q: &(f64, f32)| (q.1 - p.1).abs() < 1e-6);
        let same_after = points.get(i + 1).is_some_and(|q| (q.1 - p.1).abs() < 1e-6);
        if !(same_before && same_after) {
            out.push(*p);
        }
    }
    out
}

impl crate::Session {
    /// Read an ADM BWF file into the project: its bed as a track of its
    /// layout, each object as a mono object track with its movement as
    /// automation, the master in a format that holds them.
    pub(crate) fn start_adm_import(&mut self, path: PathBuf) {
        let job = start_import(
            path,
            self.media_dir().to_path_buf(),
            self.project.sample_rate,
        );
        self.adm_imports.push(job);
    }

    /// Imports that are done become tracks.
    pub(crate) fn poll_adm_imports(&mut self) {
        let mut i = 0;
        while i < self.adm_imports.len() {
            if !self.adm_imports[i].is_finished() {
                i += 1;
                continue;
            }
            let job = self.adm_imports.remove(i);
            let path = job.path.clone();
            match job
                .join()
                .and_then(|imported| self.finish_adm_import(imported).map_err(|e| e.to_string()))
            {
                Ok(()) => {}
                Err(e) => self.notify(
                    crate::NoticeLevel::Error,
                    format!("{}: {e}", path.display()),
                ),
            }
            self.revision += 1;
        }
    }

    /// How far the ADM imports are (0 … 1), while there are any.
    pub fn adm_import_progress(&self) -> Option<f64> {
        (!self.adm_imports.is_empty()).then(|| {
            self.adm_imports
                .iter()
                .map(|j| j.progress.fraction())
                .sum::<f64>()
                / self.adm_imports.len() as f64
        })
    }

    /// Wait for ADM imports (tests, scripted runs).
    pub fn wait_for_adm_imports(&mut self) {
        while !self.adm_imports.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_adm_imports();
        }
    }

    fn finish_adm_import(&mut self, imported: Imported) -> crate::Result<()> {
        use faderframe_automation::{AutomationCurve, AutomationLane, AutomationPoint, CurveShape};
        use faderframe_project::{
            AudioClip, AudioSource, Clip, ClipContent, Command, SourceSpec, TrackColor, TrackKind,
        };
        let rate = f64::from(self.project.sample_rate);
        let mut commands = Vec::new();
        if let (Some(f), Some(master)) = (imported.master, self.project.master_id())
            && self.project.track(master).map(|t| t.layout) != Some(ChannelLayout::Surround(f))
        {
            commands.push(Command::SetTrackLayout {
                track: master,
                layout: ChannelLayout::Surround(f),
            });
        }
        let mut index = self
            .project
            .tracks
            .iter()
            .rposition(|t| t.kind.has_clips())
            .map_or(0, |i| i + 1);
        let mut colour = self.project.tracks.len();
        let mut count = 0;
        let mut add =
            |s: &mut Self,
             commands: &mut Vec<Command>,
             name: &str,
             layout: ChannelLayout,
             audio: ImportedAudio,
             edit: &dyn Fn(&mut Track, &mut faderframe_core::IdAllocator)| {
                let p = &mut s.project;
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
                let length = source.frames(p.sample_rate);
                let id: TrackId = p.ids.allocate();
                let mut t = Track::new(id, TrackKind::Audio, name, TrackColor::palette(colour))
                    .with_layout(layout);
                edit(&mut t, &mut p.ids);
                colour += 1;
                let clip = Clip {
                    id: p.ids.allocate(),
                    track: id,
                    name: name.to_string(),
                    color: None,
                    start: faderframe_timeline::MusicalTime::ZERO,
                    muted: false,
                    content: ClipContent::Audio(AudioClip {
                        source: source.id,
                        source_offset: 0,
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
                if let Ok(st) = crate::media::open_stream(&audio.path) {
                    s.sources
                        .insert(source.id, faderframe_engine::Source::Stream(st));
                }
                s.peaks.insert(source.id, Arc::new(audio.peaks));
                commands.push(Command::AddSource {
                    source: Box::new(source),
                });
                commands.push(Command::AddTrack {
                    track: Box::new(t),
                    index,
                });
                commands.push(Command::AddClip {
                    clip: Box::new(clip),
                });
                index += 1;
            };
        let objects = imported.objects.len();
        if let Some((layout, audio)) = imported.bed {
            add(self, &mut commands, "Bed", layout, audio, &|_, _| {});
        }
        for (name, audio, blocks) in imported.objects {
            let timeline = self.project.timeline.clone();
            let edit = move |t: &mut Track, ids: &mut faderframe_core::IdAllocator| {
                t.object = true;
                if let Some(b) = blocks.first() {
                    t.surround = faderframe_core::SurroundPan {
                        x: b.position[0],
                        y: b.position[1],
                        z: b.position[2],
                        spread: b.size,
                        ..faderframe_core::SurroundPan::default()
                    }
                    .clamped();
                    if (b.gain - 1.0).abs() > 1e-4 && b.gain > 0.0 {
                        t.volume_db = 20.0 * b.gain.log10();
                    }
                }
                type Value = fn(&SceneBlock) -> f32;
                let lanes: [(SurroundParam, Value); 4] = [
                    (SurroundParam::X, |b| b.position[0]),
                    (SurroundParam::Y, |b| b.position[1]),
                    (SurroundParam::Z, |b| b.position[2]),
                    (SurroundParam::Spread, |b| b.size),
                ];
                for (param, value) in lanes {
                    let points = lane_points(&blocks, value);
                    if points.len() < 2 {
                        continue;
                    }
                    let curve = AutomationCurve::from_points(
                        points
                            .iter()
                            .map(|&(s, v)| AutomationPoint {
                                time: timeline.to_musical((s * rate).round() as i64, rate),
                                value: f64::from(param.get(&param.set(t.surround, v))),
                                shape: CurveShape::Linear,
                            })
                            .collect(),
                    );
                    t.automation.lanes.push(AutomationLane {
                        id: ids.allocate(),
                        target: AutomationTarget::Surround(param),
                        curve,
                        mode: AutomationMode::Read,
                        visible: false,
                    });
                }
            };
            add(
                self,
                &mut commands,
                &name,
                ChannelLayout::Mono,
                audio,
                &edit,
            );
            count += 1;
        }
        self.batch("Import ADM Master", commands)?;
        let mut text = format!(
            "imported '{}': {}{} object{}",
            imported.name,
            if self.project.tracks.iter().any(|t| t.name == "Bed") {
                "a bed and "
            } else {
                ""
            },
            count,
            if count == 1 { "" } else { "s" }
        );
        debug_assert_eq!(count, objects);
        for n in &imported.notes {
            text.push_str("; ");
            text.push_str(n);
        }
        self.notify(crate::NoticeLevel::Info, text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beds_hold_what_the_profile_has() {
        let mut p = Project::new("Bed", 48_000);
        let master = p.master_id().unwrap();
        let mut plan_for = |f: SurroundFormat| {
            p.track_mut(master).unwrap().layout = faderframe_core::ChannelLayout::Surround(f);
            plan(&p).unwrap()
        };
        let s714 = plan_for(SurroundFormat::S714);
        assert_eq!(s714.describe(), "7.1 bed and 4 objects");
        assert_eq!(s714.fixed[0].1, "Bed Ltf");
        let s712 = plan_for(SurroundFormat::S712);
        assert_eq!(s712.describe(), "7.1.2 bed and 0 objects");
        // Our 7.x order is L R C LFE Lrs Rrs Lss Rss Ltm Rtm; Dolby's has
        // the sides before the rears.
        let from: Vec<usize> = s712.bed.iter().map(|b| b.1).collect();
        assert_eq!(from, [0, 1, 2, 3, 6, 7, 4, 5, 8, 9]);
        assert_eq!(
            plan_for(SurroundFormat::S512).describe(),
            "5.1 bed and 2 objects"
        );
        assert_eq!(
            plan_for(SurroundFormat::Quad).describe(),
            "2.0 bed and 2 objects"
        );
        assert_eq!(
            plan_for(SurroundFormat::Lcr).describe(),
            "3.0 bed and 0 objects"
        );
    }
}
