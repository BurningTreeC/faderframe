//! AAF interchange with Pro Tools, Media Composer, Premiere and Resolve
//! (the file format itself is [`faderframe_aaf`]).
//!
//! **Export** ([`Action::ExportAaf`]): every track holding audio becomes
//! mono AAF tracks, one per channel ("Dia", or "Gtr.L"/"Gtr.R"); each
//! clip plays its source's channel there (a mono source on every channel,
//! as the clip player routes it), with its fades, clip gain and envelope —
//! the volume and, on stereo tracks, the pan (a balance: a gain per
//! side) folded into one level envelope per channel. Take folders go as
//! their comp's pieces. What AAF cannot say — warped, pitch-edited or
//! reversed clips, generated sources, a stereo source on a mono track —
//! is rendered as it plays (without its gain, fades and envelope, which
//! stay editable) into a file of its own. Media are embedded as 24-bit
//! PCM, or written as mono 24-bit WAVE files into "<name> Media" beside
//! the AAF and linked. The project's timecode and markers (and sections)
//! go along. MIDI tracks are left out. A worker does the renders and the
//! writing; the file is placed when it is done, asking first when one is
//! there.
//!
//! **Import** ([`Action::ImportAaf`]): the top-level composition's sound
//! tracks become mono audio tracks, their clips placed by timecode (a
//! project without a timecode takes the AAF's), with fades, crossfades,
//! clip gain and envelopes; embedded media are copied out, linked media
//! looked for at their URL, beside the AAF, and in folders the user names
//! ([`UiRequest::RelinkAaf`] → [`Action::RelinkAaf`]); media never found
//! stay offline. One "Import AAF" step.

use crate::render::files::{self, Overwrite, Staged, StagedFile};
use crate::render::{self, RenderProgress};
use crate::{NoticeLevel, Result, Session, SessionError, UiRequest};
use faderframe_aaf::read::AafFile;
use faderframe_aaf::timeline::{
    Clip as AafClip, Composition, Embedded, Essence, EssenceFormat, Fade, FadeCurve, Marker, Media,
    Timecode, Track as AafTrack,
};
use faderframe_audio_files::WavFormat;
use faderframe_audio_files::import::{ImportProgress, ImportedAudio, import_file};
use faderframe_audio_files::wavstream::{WavFile, WavWriter};
use faderframe_core::{AudioSourceId, ChannelLayout, TrackId};
use faderframe_project::clip_envelope::{
    ClipEnvelope, EnvelopeLane, EnvelopePoint, FLOOR_DB, MAX_DB, balance,
};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, FadeShape, Project, SourceSpec,
    Track, TrackColor, TrackKind,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Where an export's media go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AafMedia {
    /// Inside the AAF (24-bit PCM): one file to hand over.
    #[default]
    Embed,
    /// Mono 24-bit WAVE files in "<name> Media" beside it.
    Link,
}

impl AafMedia {
    pub fn label(self) -> &'static str {
        match self {
            AafMedia::Embed => "Embedded",
            AafMedia::Link => "Linked WAVE Files",
        }
    }
}

/// How far a job is.
#[derive(Debug, Default)]
pub(crate) struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

impl Progress {
    fn fraction(&self) -> f64 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            0.0
        } else {
            (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0)
        }
    }
}

type Handle<T> = std::thread::JoinHandle<std::result::Result<T, String>>;

struct ExportJob {
    path: PathBuf,
    overwrite: Overwrite,
    progress: Arc<Progress>,
    handle: Option<Handle<Exported>>,
}

struct Exported {
    staged: StagedFile,
    written: faderframe_aaf::write::Written,
    rendered: usize,
}

struct ImportJob {
    path: PathBuf,
    progress: Arc<Progress>,
    handle: Option<Handle<ImportedAaf>>,
}

/// A medium brought into the project: per channel the mono file.
type Brought = Vec<Option<ImportedAudio>>;

/// An import read and its media brought in (some perhaps missing).
pub(crate) struct ImportedAaf {
    path: PathBuf,
    composition: Composition,
    notes: Vec<String>,
    brought: Vec<Brought>,
    /// Media whose linked files were not found.
    missing: Vec<usize>,
    /// The folders looked in already.
    relinked: bool,
}

/// AAF jobs and an import waiting to be relinked.
#[derive(Default)]
pub(crate) struct AafState {
    exports: Vec<ExportJob>,
    imports: Vec<ImportJob>,
    pending: Option<ImportedAaf>,
}

fn job<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> std::result::Result<T, String> + Send + 'static,
) -> Option<Handle<T>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = faderframe_realtime::priority::lower_this_thread();
            work()
        })
        .ok()
}

fn join<T>(handle: &mut Option<Handle<T>>) -> std::result::Result<T, String> {
    match handle.take().map(std::thread::JoinHandle::join) {
        Some(Ok(r)) => r,
        _ => Err("the job stopped".into()),
    }
}

fn temp_dir(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "faderframe-aaf-{what}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ))
}

// ---------------------------------------------------------------- export

/// Where an exported medium's samples come from.
enum Feed {
    /// A float or PCM WAVE file at the project rate.
    File(PathBuf),
    /// A clip rendered as it plays: this project from `start` for
    /// `frames`, on `outputs` channels.
    Render {
        project: Box<Project>,
        start: i64,
        frames: usize,
        outputs: usize,
    },
}

/// What an export writes, before the worker fills in renders and links.
pub struct ExportPlan {
    comp: Composition,
    feeds: Vec<Feed>,
    media: AafMedia,
    rate: u32,
    notes: Vec<String>,
}

impl ExportPlan {
    /// The composition as planned (tests).
    pub fn composition(&self) -> &Composition {
        &self.comp
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}

fn curve(shape: FadeShape) -> FadeCurve {
    match shape {
        FadeShape::Linear => FadeCurve::Linear,
        FadeShape::EqualPower => FadeCurve::EqualPower,
        FadeShape::SCurve => FadeCurve::SCurve,
        FadeShape::Exponential => FadeCurve::Slow,
        FadeShape::Logarithmic => FadeCurve::Fast,
    }
}

fn shape(curve: FadeCurve) -> FadeShape {
    match curve {
        FadeCurve::Linear => FadeShape::Linear,
        FadeCurve::EqualPower => FadeShape::EqualPower,
        FadeCurve::SCurve => FadeShape::SCurve,
        FadeCurve::Slow => FadeShape::Exponential,
        FadeCurve::Fast => FadeShape::Logarithmic,
    }
}

fn amp_db(a: f64) -> f64 {
    if a <= 0.0 {
        FLOOR_DB
    } else {
        (20.0 * a.log10()).max(FLOOR_DB)
    }
}

/// A clip envelope as one channel's level points (dB, straight in
/// amplitude between them): the volume lane, times the side of the pan
/// lane's balance (`side`: 0 left, 1 right, on stereo tracks).
pub(crate) fn channel_envelope(
    env: Option<&ClipEnvelope>,
    length: i64,
    side: Option<usize>,
) -> Vec<(i64, f64)> {
    let Some(env) = env else {
        return Vec::new();
    };
    let pan = side.filter(|_| !env.pan.is_empty());
    if env.volume.is_empty() && pan.is_none() {
        return Vec::new();
    }
    let gain = |x: f64| -> f64 {
        let v = f64::from(env.gain_at(x));
        let p = pan.and_then(|s| {
            env.value_at(EnvelopeLane::Pan, x).map(|p| {
                let (l, r) = balance(p);
                f64::from(if s == 0 { l } else { r })
            })
        });
        v * p.unwrap_or(1.0)
    };
    let mut times: Vec<i64> = env
        .volume
        .iter()
        .chain(if pan.is_some() { &env.pan[..] } else { &[] })
        .map(|p| p.at.clamp(0, length))
        .chain([0, length])
        .collect();
    times.sort_unstable();
    times.dedup();
    let mut out = Vec::new();
    fn fill(out: &mut Vec<(i64, f64)>, gain: &dyn Fn(f64) -> f64, a: i64, b: i64, depth: u32) {
        let (ga, gb) = (gain(a as f64), gain(b as f64));
        let mid = a + (b - a) / 2;
        let straight = ga + (gb - ga) * ((mid - a) as f64 / (b - a).max(1) as f64);
        if depth < 10 && b - a > 64 && (amp_db(straight) - amp_db(gain(mid as f64))).abs() > 0.1 {
            fill(out, gain, a, mid, depth + 1);
            fill(out, gain, mid, b, depth + 1);
        } else if out.last().is_none_or(|l: &(i64, f64)| l.0 != b) {
            out.push((b, amp_db(gb)));
        }
    }
    out.push((times[0], amp_db(gain(times[0] as f64))));
    for w in times.windows(2) {
        fill(&mut out, &gain, w[0], w[1], 0);
    }
    if out.iter().all(|p| p.1.abs() < 1e-3) {
        return Vec::new();
    }
    out
}

/// Level points (straight in amplitude) as volume envelope points
/// (straight in dB): points added where the two differ by more than a
/// tenth of a dB.
pub(crate) fn volume_points(points: &[(i64, f64)]) -> Vec<EnvelopePoint> {
    let amp = |d: f64| 10f64.powf(d / 20.0);
    let mut out: Vec<EnvelopePoint> = Vec::new();
    fn fill(
        out: &mut Vec<EnvelopePoint>,
        amp: &dyn Fn(f64) -> f64,
        a: (i64, f64),
        b: (i64, f64),
        depth: u32,
    ) {
        let mid = a.0 + (b.0 - a.0) / 2;
        let t = (mid - a.0) as f64 / (b.0 - a.0).max(1) as f64;
        let true_db = amp_db(amp(a.1) + (amp(b.1) - amp(a.1)) * t);
        let straight = a.1 + (b.1 - a.1) * t;
        if depth < 10 && b.0 - a.0 > 64 && (true_db - straight).abs() > 0.1 {
            let m = (mid, true_db);
            fill(out, amp, a, m, depth + 1);
            fill(out, amp, m, b, depth + 1);
        } else {
            out.push(EnvelopePoint::new(b.0, b.1.clamp(FLOOR_DB, MAX_DB)));
        }
    }
    let Some(&first) = points.first() else {
        return out;
    };
    out.push(EnvelopePoint::new(first.0, first.1.clamp(FLOOR_DB, MAX_DB)));
    for w in points.windows(2) {
        if w[1].0 > w[0].0 {
            fill(&mut out, &amp, w[0], w[1], 0);
        }
    }
    out
}

/// What a clip plays from: a source file AAF can point at directly.
fn plain_source(project: &Project, a: &AudioClip) -> Option<(PathBuf, u16, i64)> {
    if a.warp.is_some() || a.reversed || a.pitch.is_some() {
        return None;
    }
    let s = project.sources.get(&a.source)?;
    match &s.spec {
        SourceSpec::File {
            path,
            channels,
            frames,
            sample_rate,
        } if *sample_rate == project.sample_rate => Some((path.clone(), *channels, *frames)),
        _ => None,
    }
}

/// A project that plays only `clip` (its gain, fades and envelope taken
/// off) on a plain track of `outputs` channels into a plain master.
fn render_project(whole: &Project, clip: &Clip, outputs: usize) -> Project {
    let rate = whole.sample_rate;
    let mut p = Project::new("AAF Render", rate);
    p.timeline = whole.timeline.clone();
    p.ids = whole.ids.clone();
    let layout = if outputs == 1 {
        ChannelLayout::Mono
    } else {
        ChannelLayout::Stereo
    };
    if let Some(m) = p.tracks.iter_mut().find(|t| t.kind == TrackKind::Master) {
        m.layout = layout;
    }
    let id: TrackId = p.ids.allocate();
    let mut t =
        Track::new(id, TrackKind::Audio, "Render", TrackColor::palette(0)).with_layout(layout);
    let mut c = clip.clone();
    c.track = id;
    c.muted = false;
    match &mut c.content {
        ClipContent::Audio(a) => {
            a.gain_db = 0.0;
            a.fades = ClipFades::default();
            a.envelope = None;
        }
        ClipContent::Takes(f) => {
            f.gain_db = 0.0;
            f.fades = ClipFades::default();
        }
        ClipContent::Midi(_) => {}
    }
    for s in c.content.referenced_sources() {
        if let Some(src) = whole.sources.get(&s) {
            p.sources.insert(s, src.clone());
        }
    }
    t.clips.push(c.id);
    p.clips.insert(c.id, c);
    p.tracks.insert(0, t);
    p
}

/// The AAF names of a track's channels.
fn channel_names(name: &str, channels: usize) -> Vec<String> {
    match channels {
        1 => vec![name.to_string()],
        2 => vec![format!("{name}.L"), format!("{name}.R")],
        n => (1..=n).map(|k| format!("{name}.{k}")).collect(),
    }
}

/// The timecode the project counts in, as AAF writes it.
fn aaf_timecode(p: &Project) -> Timecode {
    let tc = p.timecode.unwrap_or_default();
    let (num, den) = tc.rate.ratio();
    Timecode {
        start: tc.start.total_frames(tc.rate),
        fps: tc.rate.nominal() as u16,
        drop: tc.rate.is_drop(),
        rate: (num as i32, den as i32),
    }
}

/// A piece of a clip on one medium (a take folder's comp has several).
struct Piece<'a> {
    media: usize,
    offset: i64,
    position: i64,
    length: i64,
    gain: f64,
    fade_in: Option<Fade>,
    fade_out: Option<Fade>,
    envelope: Option<&'a ClipEnvelope>,
}

/// Plan an export of `project` (media paths absolute).
pub(crate) fn export_plan(project: &Project, media: AafMedia) -> Result<ExportPlan> {
    let rate = project.sample_rate;
    let sr = f64::from(rate);
    let mut plan = ExportPlan {
        comp: Composition {
            name: project.name.clone(),
            rate,
            timecode: Some(aaf_timecode(project)),
            ..Composition::default()
        },
        feeds: Vec::new(),
        media,
        rate,
        notes: Vec::new(),
    };
    let mut by_source: HashMap<AudioSourceId, usize> = HashMap::new();
    let mut muted = 0;
    let mut midi = 0;
    let mut rendered = 0;
    let essence = |channels: u16| vec![Essence::Embed { bits: 24 }; usize::from(channels)];
    for track in project.folder_order() {
        let clips = project.clips_of(track.id);
        if !clips.iter().any(|c| c.content.is_audio()) {
            if clips.iter().any(|c| c.as_midi().is_some()) {
                midi += 1;
            }
            continue;
        }
        let out = track.layout.channel_count().max(1);
        let names = channel_names(&track.name, out);
        let first = plan.comp.tracks.len();
        plan.comp
            .tracks
            .extend(names.into_iter().map(|name| AafTrack {
                name,
                clips: Vec::new(),
            }));
        for clip in clips {
            if clip.muted && clip.content.is_audio() {
                muted += 1;
                continue;
            }
            let start = project.timeline.to_samples(clip.start, sr);
            // The pieces it plays.
            let mut pieces: Vec<Piece> = Vec::new();
            let fade = |len: i64, s: FadeShape| {
                (len > 0).then_some(Fade {
                    length: len,
                    curve: curve(s),
                })
            };
            let mut medium = |plan: &mut ExportPlan,
                              source: AudioSourceId,
                              path: PathBuf,
                              channels: u16,
                              frames: i64| {
                *by_source.entry(source).or_insert_with(|| {
                    let name = project
                        .sources
                        .get(&source)
                        .map_or_else(|| "Audio".to_string(), |s| s.name.clone());
                    plan.comp.media.push(Media {
                        name,
                        channels,
                        rate,
                        frames,
                        essence: essence(channels),
                    });
                    plan.feeds.push(Feed::File(path));
                    plan.comp.media.len() - 1
                })
            };
            let render = |plan: &mut ExportPlan, length: i64| {
                let outputs = out.min(2);
                plan.comp.media.push(Media {
                    name: format!("{} (rendered)", clip.name),
                    channels: outputs as u16,
                    rate,
                    frames: length,
                    essence: essence(outputs as u16),
                });
                plan.feeds.push(Feed::Render {
                    project: Box::new(render_project(project, clip, outputs)),
                    start,
                    frames: length.max(0) as usize,
                    outputs,
                });
                plan.comp.media.len() - 1
            };
            match &clip.content {
                ClipContent::Midi(_) => continue,
                ClipContent::Audio(a) => {
                    let fi = fade(a.fades.fade_in, a.fades.fade_in_shape);
                    let fo = fade(a.fades.fade_out, a.fades.fade_out_shape);
                    let env = a.envelope.as_deref();
                    match plain_source(project, a) {
                        Some((path, ch, frames)) if !(out == 1 && ch > 1) => {
                            let m = medium(&mut plan, a.source, path, ch, frames);
                            pieces.push(Piece {
                                media: m,
                                offset: a.source_offset,
                                position: start,
                                length: a.length,
                                gain: f64::from(a.gain_db),
                                fade_in: fi,
                                fade_out: fo,
                                envelope: env,
                            });
                        }
                        _ => {
                            let m = render(&mut plan, a.length);
                            rendered += 1;
                            pieces.push(Piece {
                                media: m,
                                offset: 0,
                                position: start,
                                length: a.length,
                                gain: f64::from(a.gain_db),
                                fade_in: fi,
                                fade_out: fo,
                                envelope: env,
                            });
                        }
                    }
                }
                ClipContent::Takes(f) => {
                    let plain: Option<Vec<_>> = f
                        .pieces()
                        .into_iter()
                        .map(|piece| {
                            let take = f.takes.get(piece.take)?;
                            // A take with edits of its own: the folder is
                            // rendered.
                            let a = &take.audio;
                            if !take.plays_source_as_is()
                                || a.envelope.is_some()
                                || a.fades != Default::default()
                            {
                                return None;
                            }
                            let s = project.sources.get(&a.source)?;
                            match &s.spec {
                                SourceSpec::File {
                                    path,
                                    channels,
                                    frames,
                                    sample_rate,
                                } if *sample_rate == rate && !(out == 1 && *channels > 1) => {
                                    Some((piece, take.clone(), path.clone(), *channels, *frames))
                                }
                                _ => None,
                            }
                        })
                        .collect();
                    match plain {
                        Some(list) => {
                            for (piece, take, path, ch, frames) in list {
                                let m = medium(&mut plan, take.audio.source, path, ch, frames);
                                let fi = (piece.start == 0)
                                    .then(|| fade(f.fades.fade_in, f.fades.fade_in_shape))
                                    .flatten();
                                let fo = (piece.end >= f.length)
                                    .then(|| fade(f.fades.fade_out, f.fades.fade_out_shape))
                                    .flatten();
                                pieces.push(Piece {
                                    media: m,
                                    offset: take.audio.source_offset + piece.start - take.start,
                                    position: start + piece.start,
                                    length: piece.end - piece.start,
                                    gain: f64::from(f.gain_db + take.audio.gain_db),
                                    fade_in: fi,
                                    fade_out: fo,
                                    envelope: None,
                                });
                            }
                        }
                        None => {
                            let m = render(&mut plan, f.length);
                            rendered += 1;
                            let fi = fade(f.fades.fade_in, f.fades.fade_in_shape);
                            let fo = fade(f.fades.fade_out, f.fades.fade_out_shape);
                            pieces.push(Piece {
                                media: m,
                                offset: 0,
                                position: start,
                                length: f.length,
                                gain: f64::from(f.gain_db),
                                fade_in: fi,
                                fade_out: fo,
                                envelope: None,
                            });
                        }
                    }
                }
            }
            for Piece {
                media: m,
                offset,
                position,
                length,
                gain,
                fade_in: fi,
                fade_out: fo,
                envelope: env,
            } in pieces
            {
                let src = usize::from(plan.comp.media[m].channels);
                for d in 0..out {
                    let channel = if src == out {
                        d
                    } else if src == 1 {
                        0
                    } else if d < src {
                        d
                    } else {
                        continue;
                    };
                    let side = (out == 2).then_some(d);
                    plan.comp.tracks[first + d].clips.push(AafClip {
                        name: clip.name.clone(),
                        position,
                        length,
                        media: m,
                        channel: channel as u16,
                        offset,
                        gain_db: gain,
                        envelope: channel_envelope(env, length, side),
                        fade_in: fi,
                        fade_out: fo,
                    });
                }
            }
        }
    }
    for m in &project.markers {
        plan.comp.markers.push(Marker {
            position: project.timeline.to_samples(m.position, sr),
            name: m.name.clone(),
            color: None,
        });
    }
    for s in &project.sections {
        plan.comp.markers.push(Marker {
            position: project.timeline.to_samples(s.start, sr),
            name: s.name.clone(),
            color: Some([s.color.r, s.color.g, s.color.b]),
        });
    }
    if plan.comp.tracks.is_empty() {
        return Err(SessionError::Other(
            "there is no audio to export as AAF".into(),
        ));
    }
    if muted > 0 {
        plan.notes.push(format!(
            "{muted} muted clip{} left out",
            if muted == 1 { "" } else { "s" }
        ));
    }
    if midi > 0 {
        plan.notes.push(format!(
            "{midi} MIDI track{} left out (AAF carries audio)",
            if midi == 1 { "" } else { "s" }
        ));
    }
    if rendered > 0 {
        plan.notes.push(format!(
            "{rendered} clip{} rendered as {} (warped, pitch-edited, reversed or generated)",
            if rendered == 1 { "" } else { "s" },
            if rendered == 1 {
                "it plays"
            } else {
                "they play"
            }
        ));
    }
    Ok(plan)
}

/// Reads exported media from their files.
struct FeedSamples {
    files: Vec<Option<WavFile>>,
    scratch: Vec<u8>,
    bufs: Vec<Vec<f32>>,
}

impl faderframe_aaf::write::Samples for FeedSamples {
    fn read(
        &mut self,
        media: usize,
        channel: u16,
        at: u64,
        out: &mut [f32],
    ) -> std::io::Result<()> {
        let Some(Some(f)) = self.files.get(media) else {
            out.fill(0.0);
            return Ok(());
        };
        let n = out.len();
        self.bufs.resize_with(f.channels(), Vec::new);
        for b in &mut self.bufs {
            b.resize(n, 0.0);
        }
        let mut slices: Vec<&mut [f32]> = self.bufs.iter_mut().map(|b| &mut b[..n]).collect();
        f.read(at, &mut slices, &mut self.scratch)?;
        match self.bufs.get(usize::from(channel)) {
            Some(b) => out.copy_from_slice(&b[..n]),
            None => out.fill(0.0),
        }
        Ok(())
    }
}

/// The export worker: renders, links, the file.
fn run_export(
    mut plan: ExportPlan,
    dest: &Path,
    progress: &Progress,
) -> std::result::Result<Exported, String> {
    let rate = plan.rate;
    let tmp = temp_dir("export");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        let render_frames: u64 = plan
            .feeds
            .iter()
            .map(|f| match f {
                Feed::Render { frames, .. } => *frames as u64,
                Feed::File(_) => 0,
            })
            .sum();
        let media_frames: u64 = plan
            .comp
            .media
            .iter()
            .map(|m| m.frames.max(0) as u64 * u64::from(m.channels))
            .sum();
        progress
            .total
            .store(render_frames + media_frames, Ordering::Relaxed);
        // Renders into files of their own.
        let mut rendered = 0;
        for (k, feed) in plan.feeds.iter_mut().enumerate() {
            let Feed::Render {
                project,
                start,
                frames,
                outputs,
            } = feed
            else {
                continue;
            };
            let path = tmp.join(format!("render-{k}.wav"));
            let mut w = WavWriter::create(&path, *outputs as u16, rate, WavFormat::Float32, false)
                .map_err(|e| e.to_string())?;
            let rp = RenderProgress::default();
            let mut sources = faderframe_engine::render_generated_sources(project, rate);
            if let Some((_, p, e)) = crate::media::open_file_sources(project, None, &mut sources)
                .into_iter()
                .next()
            {
                return Err(format!("{}: {e}", p.display()));
            }
            let mut err = None;
            render::render_each(
                project,
                rate,
                &sources,
                (*start, *frames),
                &rp,
                *outputs,
                None,
                false,
                &mut |piece| {
                    let n = piece.first().map_or(0, |c| c.len());
                    if err.is_none()
                        && let Err(e) = w.write_planar(piece, n)
                    {
                        err = Some(e.to_string());
                    }
                    progress.done.fetch_add(n as u64, Ordering::Relaxed);
                },
            )
            .map_err(|e| e.to_string())?;
            if let Some(e) = err {
                return Err(e);
            }
            w.finish().map_err(|e| e.to_string())?;
            *feed = Feed::File(path);
            rendered += 1;
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
        }
        let open = |f: &Feed| match f {
            Feed::File(p) => WavFile::open(p).ok(),
            Feed::Render { .. } => None,
        };
        let files: Vec<Option<WavFile>> = plan.feeds.iter().map(open).collect();
        for (m, f) in plan.comp.media.iter().zip(&files) {
            if f.is_none() {
                return Err(format!("{}: the medium could not be read", m.name));
            }
        }
        let mut samples = FeedSamples {
            files,
            scratch: Vec::new(),
            bufs: Vec::new(),
        };
        // Linked: mono 24-bit files beside the AAF.
        if plan.media == AafMedia::Link {
            let stem = dest
                .file_stem()
                .map_or_else(|| "AAF".into(), |s| s.to_string_lossy().to_string());
            let dir = dest
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("{stem} Media"));
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for m in 0..plan.comp.media.len() {
                let (name, channels, frames) = {
                    let md = &plan.comp.media[m];
                    (md.name.clone(), md.channels, md.frames.max(0) as u64)
                };
                for c in 0..channels {
                    let stem = if channels == 1 {
                        name.clone()
                    } else {
                        format!("{name}.A{}", c + 1)
                    };
                    let path = faderframe_audio_files::import::unique_path(&dir, &stem, "wav");
                    let mut w = WavWriter::create(&path, 1, rate, WavFormat::Pcm24, false)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    let mut at = 0u64;
                    let mut buf = vec![0f32; 1 << 16];
                    use faderframe_aaf::write::Samples;
                    while at < frames {
                        let n = (buf.len() as u64).min(frames - at) as usize;
                        samples
                            .read(m, c, at, &mut buf[..n])
                            .map_err(|e| e.to_string())?;
                        w.write_planar(&[&buf[..n]], n).map_err(|e| e.to_string())?;
                        at += n as u64;
                        progress.done.fetch_add(n as u64, Ordering::Relaxed);
                        if progress.cancel.load(Ordering::Relaxed) {
                            return Err("cancelled".into());
                        }
                    }
                    let path = w.finish().map_err(|e| e.to_string())?;
                    let header = faderframe_aaf::wave::wave_summary(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    plan.comp.media[m].essence[usize::from(c)] = Essence::Link {
                        url: faderframe_aaf::url::from_path(&path),
                        wave_header: header,
                    };
                }
            }
        }
        let staged = StagedFile::new(dest).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(staged.temp()).map_err(|e| e.to_string())?;
        let options = faderframe_aaf::write::WriteOptions {
            version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        };
        let before = progress.done.load(Ordering::Relaxed);
        let (file, written) = faderframe_aaf::write::write(
            std::io::BufWriter::with_capacity(1 << 20, file),
            &plan.comp,
            &mut samples,
            options,
            &mut |done, _| {
                progress.done.store(before + done / 3, Ordering::Relaxed);
                !progress.cancel.load(Ordering::Relaxed)
            },
        )
        .map_err(|e| e.to_string())?;
        file.into_inner()
            .map_err(|e| e.to_string())?
            .sync_all()
            .map_err(|e| e.to_string())?;
        Ok(Exported {
            staged,
            written,
            rendered,
        })
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// ---------------------------------------------------------------- import

/// A file with `name` (any case) in `dir` or below, `depth` levels down.
fn find_named(dir: &Path, name: &str, depth: usize, budget: &mut usize) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
        {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs.sort();
    subdirs
        .into_iter()
        .find_map(|d| find_named(&d, name, depth - 1, budget))
}

/// Where a linked file is: at its URL, beside the AAF (or in a folder
/// below it), in the folder the user named.
fn locate(urls: &[String], aaf: &Path, relink: Option<&Path>) -> Option<PathBuf> {
    for u in urls {
        if let Some(p) = faderframe_aaf::url::to_path(u)
            && p.is_file()
        {
            return Some(p);
        }
    }
    let names: Vec<String> = urls
        .iter()
        .filter_map(|u| faderframe_aaf::url::file_name(u))
        .collect();
    let mut places: Vec<(PathBuf, usize)> = Vec::new();
    if let Some(dir) = aaf.parent() {
        places.push((dir.to_path_buf(), 2));
    }
    if let Some(r) = relink {
        places.push((r.to_path_buf(), 4));
    }
    for (dir, depth) in places {
        for n in &names {
            let mut budget = 200_000;
            if let Some(p) = find_named(&dir, n, depth, &mut budget) {
                return Some(p);
            }
        }
    }
    None
}

/// Bring the media of `imported` that are not here yet (and can be found)
/// into `media_dir`.
fn bring_media(
    imported: &mut ImportedAaf,
    media_dir: &Path,
    rate: u32,
    relink: Option<&Path>,
    progress: &Progress,
) -> std::result::Result<(), String> {
    let tmp = temp_dir("import");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut file = AafFile::open_path(&imported.path).map_err(|e| e.to_string())?;
        let mut used: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
        for t in &imported.composition.tracks {
            for c in &t.clips {
                let v = used.entry(c.media).or_default();
                if !v.contains(&c.channel) {
                    v.push(c.channel);
                }
            }
        }
        let total: u64 = used
            .keys()
            .map(|&m| imported.composition.media[m].frames.max(0) as u64)
            .sum();
        progress.total.store(total.max(1), Ordering::Relaxed);
        imported.missing.clear();
        let quiet = ImportProgress::default();
        let cancel = AtomicBool::new(false);
        // Copies of what was brought, by the file a channel came from.
        let mut done: HashMap<(String, u16), ImportedAudio> = HashMap::new();
        for (m, channels) in used {
            let medium = imported.composition.media[m].clone();
            if imported.brought[m].iter().any(Option::is_some) {
                continue;
            }
            let mut original: Option<(PathBuf, String)> = None;
            for &c in &channels {
                let Some(e) = medium.essence.get(usize::from(c)) else {
                    continue;
                };
                let (key, file_channel) = match e {
                    Essence::Embedded(emb) => (format!("entry {}", emb.entry), emb.channel),
                    Essence::Linked { urls, channel, .. } => {
                        match locate(urls, &imported.path, relink) {
                            Some(p) => (p.to_string_lossy().to_string(), *channel),
                            None => {
                                if !imported.missing.contains(&m) {
                                    imported.missing.push(m);
                                }
                                continue;
                            }
                        }
                    }
                    _ => continue,
                };
                if let Some(a) = done.get(&(key.clone(), file_channel)) {
                    imported.brought[m][usize::from(c)] = Some(a.clone());
                    continue;
                }
                // The file to decode: an embedded stream copied out, or
                // the linked file itself.
                if original.as_ref().is_none_or(|o| o.1 != key) {
                    let path = match e {
                        Essence::Embedded(emb) => extract(&mut file, emb, &medium, &tmp, m)?,
                        _ => PathBuf::from(&key),
                    };
                    original = Some((path, key.clone()));
                }
                let Some((path, _)) = &original else { continue };
                let whole = import_file(path, &tmp, rate, &quiet, &cancel)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let mono = if whole.channels == 1 {
                    whole
                } else {
                    split_channel(&whole, file_channel, &tmp, &medium.name, rate)?
                };
                // Into the project's media folder, peaks along.
                let moved = crate::media::relocate(&mono.path, media_dir, false)
                    .map_err(|e| format!("{}: {e}", media_dir.display()))?;
                let mono = ImportedAudio {
                    peaks_path: faderframe_audio_files::import::peaks_path_for(&moved),
                    path: moved,
                    name: medium.name.clone(),
                    ..mono
                };
                done.insert((key.clone(), file_channel), mono.clone());
                imported.brought[m][usize::from(c)] = Some(mono);
            }
            progress
                .done
                .fetch_add(medium.frames.max(0) as u64, Ordering::Relaxed);
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// An embedded essence stream as a file a decoder opens.
fn extract(
    file: &mut AafFile<std::fs::File>,
    e: &Embedded,
    medium: &Media,
    tmp: &Path,
    k: usize,
) -> std::result::Result<PathBuf, String> {
    let stem = faderframe_audio_files::import::clean_stem(&medium.name);
    let (head, ext): (Vec<u8>, &str) = match e.format {
        EssenceFormat::Wave => (Vec::new(), "wav"),
        EssenceFormat::Aiff => (Vec::new(), "aif"),
        EssenceFormat::Pcm {
            bits,
            channels,
            rate,
            big_endian,
            float,
        } => faderframe_aaf::wave::header_for_pcm(
            faderframe_aaf::wave::PcmInfo {
                channels,
                rate,
                bits,
                float,
            },
            big_endian,
            e.len,
        ),
    };
    let path = tmp.join(format!("{k} {stem}.{ext}"));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
    use std::io::Write;
    out.write_all(&head).map_err(|e| e.to_string())?;
    file.copy_essence(e, &mut out, &mut |_| {})
        .map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(path)
}

/// One channel of an imported file as a mono file of its own.
fn split_channel(
    whole: &ImportedAudio,
    channel: u16,
    tmp: &Path,
    name: &str,
    rate: u32,
) -> std::result::Result<ImportedAudio, String> {
    let f = WavFile::open(&whole.path).map_err(|e| e.to_string())?;
    let channels = f.channels();
    let channel = usize::from(channel).min(channels.saturating_sub(1));
    let path =
        faderframe_audio_files::import::unique_path(tmp, &format!("{name} {}", channel + 1), "wav");
    let mut w = WavWriter::create(&path, 1, f.sample_rate(), WavFormat::Float32, false)
        .map_err(|e| e.to_string())?;
    let mut bufs = vec![vec![0f32; 1 << 15]; channels];
    let mut scratch = Vec::new();
    let mut at = 0;
    while at < f.frames() {
        let n = (bufs[0].len() as u64).min(f.frames() - at) as usize;
        let mut slices: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
        f.read(at, &mut slices, &mut scratch)
            .map_err(|e| e.to_string())?;
        w.write_planar(&[&bufs[channel][..n]], n)
            .map_err(|e| e.to_string())?;
        at += n as u64;
    }
    let path = w.finish().map_err(|e| e.to_string())?;
    let quiet = ImportProgress::default();
    let cancel = AtomicBool::new(false);
    let sub = tmp.join("mono");
    import_file(&path, &sub, rate, &quiet, &cancel).map_err(|e| e.to_string())
}

impl Session {
    /// Export the project as an AAF file at `path` (see the module).
    pub(crate) fn export_aaf(&mut self, path: PathBuf, media: AafMedia) -> Result<()> {
        crate::exports::files_only(std::slice::from_ref(&path))?;
        let whole = self.absolute_copy();
        let plan = export_plan(&whole, media)?;
        let dest = path.clone();
        self.export_to("AAF", vec![path], move |s, overwrite| {
            let progress = Arc::new(Progress::default());
            let p = Arc::clone(&progress);
            let to = dest.clone();
            let notes = plan.notes.clone();
            let handle = job("faderframe-aaf-export", move || run_export(plan, &to, &p));
            for n in notes {
                s.notify(NoticeLevel::Info, format!("AAF: {n}"));
            }
            s.aaf.exports.push(ExportJob {
                path: dest,
                overwrite,
                progress,
                handle,
            });
            Ok(())
        })
    }

    /// Read an AAF file's composition into the project (see the module).
    pub(crate) fn import_aaf(&mut self, path: PathBuf) {
        let rate = self.project.sample_rate;
        let media_dir = self.media_dir().to_path_buf();
        let progress = Arc::new(Progress::default());
        let p = Arc::clone(&progress);
        let file = path.clone();
        let handle = job("faderframe-aaf-import", move || {
            let mut f = AafFile::open_path(&file).map_err(|e| e.to_string())?;
            let read = f.composition(rate).map_err(|e| e.to_string())?;
            drop(f);
            let brought = read
                .composition
                .media
                .iter()
                .map(|m| vec![None; usize::from(m.channels)])
                .collect();
            let mut imported = ImportedAaf {
                path: file,
                composition: read.composition,
                notes: read.notes,
                brought,
                missing: Vec::new(),
                relinked: false,
            };
            bring_media(&mut imported, &media_dir, rate, None, &p)?;
            Ok(imported)
        });
        self.aaf.imports.push(ImportJob {
            path,
            progress,
            handle,
        });
    }

    /// Look for an import's missing media in `folder` (`None`: import
    /// without them; they stay offline).
    pub(crate) fn relink_aaf(&mut self, folder: Option<PathBuf>) -> Result<()> {
        let Some(mut pending) = self.aaf.pending.take() else {
            return Ok(());
        };
        let Some(folder) = folder else {
            return self.place_aaf(pending);
        };
        let rate = self.project.sample_rate;
        let media_dir = self.media_dir().to_path_buf();
        let progress = Arc::new(Progress::default());
        let p = Arc::clone(&progress);
        let path = pending.path.clone();
        pending.relinked = true;
        let handle = job("faderframe-aaf-relink", move || {
            bring_media(&mut pending, &media_dir, rate, Some(&folder), &p)?;
            Ok(pending)
        });
        self.aaf.imports.push(ImportJob {
            path,
            progress,
            handle,
        });
        Ok(())
    }

    /// AAF jobs that finished: exports placed, imports placed (or asking
    /// for missing media).
    pub(crate) fn poll_aaf(&mut self) {
        let mut i = 0;
        while i < self.aaf.exports.len() {
            if !self.aaf.exports[i]
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            {
                i += 1;
                continue;
            }
            let mut j = self.aaf.exports.remove(i);
            let path = j.path.clone();
            let result = join(&mut j.handle)
                .map_err(SessionError::Other)
                .and_then(|e| {
                    let what = format!("AAF {}", path.display());
                    let summary = format!(
                        "exported AAF “{}”: {} track{}, {} clip{}{}{}",
                        path.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().to_string()),
                        e.written.slots,
                        if e.written.slots == 1 { "" } else { "s" },
                        e.written.clips,
                        if e.written.clips == 1 { "" } else { "s" },
                        if e.written.embedded > 0 {
                            format!(
                                ", {:.1} MB of audio inside",
                                e.written.embedded as f64 / 1e6
                            )
                        } else {
                            String::new()
                        },
                        if e.rendered > 0 {
                            format!(", {} rendered", e.rendered)
                        } else {
                            String::new()
                        }
                    );
                    self.commit_export(
                        &what,
                        Staged::new(vec![files::written(e.staged)]),
                        &j.overwrite,
                        move |s, _| {
                            s.notify(NoticeLevel::Info, summary);
                            Ok(())
                        },
                    )
                });
            if let Err(e) = result {
                self.notify(NoticeLevel::Error, format!("AAF {}: {e}", path.display()));
            }
            self.revision += 1;
        }
        let mut i = 0;
        while i < self.aaf.imports.len() {
            if !self.aaf.imports[i]
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            {
                i += 1;
                continue;
            }
            let mut j = self.aaf.imports.remove(i);
            let result = join(&mut j.handle)
                .map_err(SessionError::Other)
                .and_then(|imported| {
                    if !imported.missing.is_empty() && !imported.relinked {
                        let missing = imported
                            .missing
                            .iter()
                            .filter_map(|&m| match imported.composition.media[m].essence.first() {
                                Some(Essence::Linked { urls, .. }) => {
                                    urls.first().and_then(|u| faderframe_aaf::url::file_name(u))
                                }
                                _ => None,
                            })
                            .collect();
                        self.ui_requests.push(UiRequest::RelinkAaf {
                            file: imported.path.clone(),
                            missing,
                        });
                        self.aaf.pending = Some(imported);
                        Ok(())
                    } else {
                        self.place_aaf(imported)
                    }
                });
            if let Err(e) = result {
                self.notify(NoticeLevel::Error, format!("AAF {}: {e}", j.path.display()));
            }
            self.revision += 1;
        }
    }

    /// The import's tracks, clips, markers and timecode as one step.
    fn place_aaf(&mut self, imported: ImportedAaf) -> Result<()> {
        let rate = self.project.sample_rate;
        let sr = f64::from(rate);
        let comp = &imported.composition;
        let mut commands = Vec::new();
        // Placed by timecode: the AAF's start where the project counts it.
        let mut offset = 0i64;
        if let Some(tc) = comp.timecode {
            let real = |frames: i64, (num, den): (i64, i64)| -> i64 {
                if num <= 0 {
                    0
                } else {
                    ((i128::from(frames) * i128::from(rate) * i128::from(den)) / i128::from(num))
                        as i64
                }
            };
            let aaf_start = real(tc.start, (i64::from(tc.rate.0), i64::from(tc.rate.1)));
            match self.project.timecode {
                Some(ptc) => {
                    let (n, d) = ptc.rate.ratio();
                    offset = aaf_start
                        - real(
                            ptc.start.total_frames(ptc.rate),
                            (i64::from(n), i64::from(d)),
                        );
                }
                None => {
                    use faderframe_core::timecode::{FrameRate, Timecode as Tc};
                    // The exact rate, else one counting the same frames
                    // (a slot at 30/1 counting 29.97 drop-frame).
                    let exact = FrameRate::ALL.into_iter().find(|r| {
                        let (n, d) = r.ratio();
                        (n as i32, d as i32) == tc.rate && r.is_drop() == tc.drop
                    });
                    let counting = || {
                        FrameRate::ALL
                            .into_iter()
                            .find(|r| r.nominal() == u32::from(tc.fps) && r.is_drop() == tc.drop)
                    };
                    if let Some(fr) = exact.or_else(counting) {
                        commands.push(Command::SetTimecode {
                            timecode: Some(faderframe_project::video::ProjectTimecode {
                                rate: fr,
                                start: Tc::from_frames(tc.start, fr),
                            }),
                        });
                    }
                }
            }
        }
        let mut index = self
            .project
            .tracks
            .iter()
            .rposition(|t| t.kind.has_clips())
            .map_or(0, |i| i + 1);
        let mut colour = self.project.tracks.len();
        // A source per brought mono file.
        let mut sources: HashMap<PathBuf, AudioSourceId> = HashMap::new();
        let mut offline: HashMap<usize, AudioSourceId> = HashMap::new();
        let (mut clips_placed, mut tracks_made, mut left_out) = (0usize, 0usize, 0usize);
        for t in &comp.tracks {
            if t.clips.is_empty() {
                continue;
            }
            let track_id: TrackId = self.project.ids.allocate();
            let mut track = Track::new(
                track_id,
                TrackKind::Audio,
                &t.name,
                TrackColor::palette(colour),
            )
            .with_layout(ChannelLayout::Mono);
            colour += 1;
            let mut clips = Vec::new();
            for c in &t.clips {
                let medium = &comp.media[c.media];
                let source = match imported
                    .brought
                    .get(c.media)
                    .and_then(|b| b.get(usize::from(c.channel)))
                    .and_then(Option::as_ref)
                {
                    Some(a) => *sources.entry(a.path.clone()).or_insert_with(|| {
                        let id: AudioSourceId = self.project.ids.allocate();
                        if let Ok(st) = crate::media::open_stream(&a.path) {
                            self.sources
                                .insert(id, faderframe_engine::Source::Stream(st));
                        }
                        self.peaks.insert(id, Arc::new(a.peaks.clone()));
                        commands.push(Command::AddSource {
                            source: Box::new(AudioSource {
                                id,
                                name: a.name.clone(),
                                spec: SourceSpec::File {
                                    path: a.path.clone(),
                                    channels: a.channels as u16,
                                    frames: a.frames as i64,
                                    sample_rate: a.sample_rate,
                                },
                            }),
                        });
                        id
                    }),
                    None => {
                        // Offline: where it was linked, as the AAF says it is.
                        let linked = match medium.essence.get(usize::from(c.channel)) {
                            Some(Essence::Linked { urls, .. }) => {
                                urls.first().and_then(|u| faderframe_aaf::url::to_path(u))
                            }
                            _ => None,
                        };
                        let Some(path) = linked else {
                            left_out += 1;
                            continue;
                        };
                        *offline.entry(c.media).or_insert_with(|| {
                            let id: AudioSourceId = self.project.ids.allocate();
                            commands.push(Command::AddSource {
                                source: Box::new(AudioSource {
                                    id,
                                    name: medium.name.clone(),
                                    spec: SourceSpec::File {
                                        path,
                                        channels: medium.channels,
                                        frames: medium.frames,
                                        sample_rate: medium.rate,
                                    },
                                }),
                            });
                            id
                        })
                    }
                };
                // Before the project's start: what is left of it.
                let mut position = c.position + offset;
                let (mut length, mut from) = (c.length, c.offset);
                if position < 0 {
                    let cut = -position;
                    if cut >= length {
                        left_out += 1;
                        continue;
                    }
                    position = 0;
                    length -= cut;
                    from += cut;
                }
                let fade = |f: Option<Fade>| {
                    f.map_or((0, FadeShape::Linear), |f| {
                        (f.length.min(length), shape(f.curve))
                    })
                };
                let (fi, fis) = fade(c.fade_in);
                let (fo, fos) = fade(c.fade_out);
                let envelope = (!c.envelope.is_empty()).then(|| {
                    Box::new(ClipEnvelope {
                        volume: volume_points(&c.envelope),
                        pan: Vec::new(),
                    })
                });
                let id = self.project.ids.allocate();
                clips.push(Clip {
                    id,
                    track: track_id,
                    name: if c.name.is_empty() {
                        medium.name.clone()
                    } else {
                        c.name.clone()
                    },
                    color: None,
                    start: self.project.timeline.at_sample(position, sr),
                    muted: false,
                    content: ClipContent::Audio(AudioClip {
                        source,
                        source_offset: from,
                        length,
                        gain_db: c.gain_db as f32,
                        fades: ClipFades {
                            fade_in: fi,
                            fade_out: fo,
                            fade_in_shape: fis,
                            fade_out_shape: fos,
                            ..ClipFades::default()
                        },
                        stretch: Default::default(),
                        reversed: false,
                        warp: None,
                        pitch: None,
                        effects: None,
                        spectral: None,
                        envelope,
                    }),
                });
            }
            if clips.is_empty() {
                continue;
            }
            track.clips = clips.iter().map(|c| c.id).collect();
            commands.push(Command::AddTrack {
                track: Box::new(track),
                index,
            });
            index += 1;
            tracks_made += 1;
            clips_placed += clips.len();
            commands.extend(clips.into_iter().map(|clip| Command::AddClip {
                clip: Box::new(clip),
            }));
        }
        for m in &comp.markers {
            let position = m.position + offset;
            if position < 0 {
                continue;
            }
            commands.push(Command::AddMarker {
                marker: faderframe_project::Marker {
                    id: self.project.ids.allocate(),
                    position: self.project.timeline.at_sample(position, sr),
                    name: m.name.clone(),
                    time_base: Default::default(),
                },
            });
        }
        if tracks_made == 0 {
            return Err(SessionError::Other(format!(
                "{}: no audio to import",
                imported.path.display()
            )));
        }
        self.batch("Import AAF", commands)?;
        let mut text = format!(
            "imported AAF “{}”: {tracks_made} track{}, {clips_placed} clip{}",
            comp.name,
            if tracks_made == 1 { "" } else { "s" },
            if clips_placed == 1 { "" } else { "s" }
        );
        if !offline.is_empty() {
            text.push_str(&format!(
                "; {} medi{} not found (offline)",
                offline.len(),
                if offline.len() == 1 { "um" } else { "a" }
            ));
        }
        if left_out > 0 {
            text.push_str(&format!("; {left_out} clips without media left out"));
        }
        for n in &imported.notes {
            text.push_str("; ");
            text.push_str(n);
        }
        self.notify(NoticeLevel::Info, text);
        Ok(())
    }

    /// AAF exports and imports under way: what and how far (0 … 1).
    pub fn aaf_activity(&self) -> Option<(String, f64)> {
        let e = self.aaf.exports.first().map(|j| {
            (
                format!("Exporting AAF {}", j.path.display()),
                j.progress.fraction(),
            )
        });
        e.or_else(|| {
            self.aaf.imports.first().map(|j| {
                (
                    format!("Importing AAF {}", j.path.display()),
                    j.progress.fraction(),
                )
            })
        })
    }

    /// An AAF import waiting for its missing media.
    pub fn aaf_waiting_for_media(&self) -> bool {
        self.aaf.pending.is_some()
    }

    /// Stop the AAF exports and imports under way.
    pub fn cancel_aaf(&mut self) {
        for j in &self.aaf.exports {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
        for j in &self.aaf.imports {
            j.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Wait for the AAF jobs (tests, scripted runs).
    pub fn wait_for_aaf(&mut self) {
        while !self.aaf.exports.is_empty() || !self.aaf.imports.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.poll_aaf();
        }
    }

    /// The AAF an export of the project would write (tests).
    pub fn aaf_export_plan(&mut self, media: AafMedia) -> Result<ExportPlan> {
        let whole = self.absolute_copy();
        export_plan(&whole, media)
    }
}

#[cfg(test)]
mod tests;
