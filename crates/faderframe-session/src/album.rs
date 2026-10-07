//! The album ([`faderframe_project::album`]): song edits (each one undo
//! step, [`Command::SetAlbum`]) and analysis and export on a worker thread.
//!
//! Every song is rendered (a section — without the clips that start after
//! it, so the next song does not ring into its tail —, this project or
//! another project file) or decoded (an audio file) at the album's rate,
//! made stereo, trimmed, run through its own inserts and faded. *Analyse*
//! measures each song. *Export* writes the songs to temporary files while
//! measuring the whole album, then levels them (one gain for the album, or
//! one per song), limits the true peak, dithers and writes `NN Title.wav`
//! — and, when asked, the album as one file with a cue sheet and a CD
//! master (DDP fileset). Songs follow each other after their pause or
//! crossfade into each other ([`crate::album_master`]); with crossfades
//! the song files are cut from the album at the track marks, so they play
//! gaplessly.
//!
//! A song's inserts are hosted by the engine like any track's (editors,
//! parameters, state); the monitored song's run after the master strip, so
//! they can be heard on the project.

use crate::album_master::{Assembler, CdMaster, Gapless, Mark, check_codes, disc};
use crate::delivery::{Finished, LoudnessReport, PeakHandling};
use crate::render::{RenderProgress, process_through, render_span, sanitize};
use crate::{NoticeLevel, Result, Session, SessionError, vinyl};
use faderframe_analysis::delivery::{
    Measurement, apply_gain, gain_and_limit, measure, normalize_loudness,
};
use faderframe_analysis::vinyl::VinylReport;
use faderframe_audio_files::decode::decode_at_rate;
use faderframe_audio_files::wavstream::WavWriter;
use faderframe_audio_files::{Dither, WavFormat, read_wav, write_wav_with};
use faderframe_core::{PluginInstanceId, SongId};
use faderframe_disc::MIN_PREGAP;
use faderframe_project::album::{Album, AlbumInfo, AlbumLevel, AlbumSettings, Song, SongSource};
use faderframe_project::{Command, PluginRef, PluginSlot, Project, SourceSpec};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

pub use faderframe_disc::{Language, normalize_isrc, normalize_upc};

#[derive(Clone, Debug, PartialEq)]
pub enum AlbumAction {
    /// Every section not in the album yet, in timeline order.
    AddSections,
    /// This project, start to end, as one song.
    AddThisProject,
    /// Audio files and FaderFrame projects (by extension).
    AddFiles(Vec<PathBuf>),
    Remove(SongId),
    /// Move to position `to` of the list.
    Move {
        song: SongId,
        to: usize,
    },
    /// Title, trim, pause, crossfade, fades, ISRC and credits (an ISRC is
    /// checked and normalised).
    Update(Song),
    Settings(AlbumSettings),
    /// Title, credits and UPC/EAN of the release (the code is checked).
    Info(AlbumInfo),
    /// Append a plugin to a song's inserts (a hosted one shows its editor).
    AddInsert {
        song: SongId,
        plugin: PluginRef,
    },
    RemoveInsert {
        song: SongId,
        plugin: PluginInstanceId,
    },
    /// Move an insert to position `to` of its song's chain.
    MoveInsert {
        song: SongId,
        plugin: PluginInstanceId,
        to: usize,
    },
    BypassInsert {
        song: SongId,
        plugin: PluginInstanceId,
        bypass: bool,
    },
    /// Hear a song's inserts after the master strip (`None`: no song's).
    Monitor(Option<SongId>),
    /// Ask the shell for the details form of the release (`None`) or a
    /// song.
    Details(Option<SongId>),
    /// The release's information (with its translations) and, edited
    /// along with it, a song's: one step.
    Texts {
        info: AlbumInfo,
        song: Option<Box<Song>>,
    },
    Analyse,
    Export,
    Cancel,
    /// Play the album as it will be delivered (prepared first if the album
    /// or the project changed), from a song or from where it was.
    Play(Option<SongId>),
    Pause,
    /// Back to the project.
    StopPlaying,
    /// The previous (−1) or next (+1) song.
    Skip(i32),
    /// On vinyl: start a new side at the song (or join it to the side
    /// before). Sides split automatically keep where they are and are
    /// split by hand from then on.
    SideBreak {
        song: SongId,
        on: bool,
    },
    /// To a point of the album (seconds).
    Seek(f64),
}

/// Album playback as the view shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlbumPlayback {
    /// The album is being rendered to play (fraction 0–1).
    pub preparing: Option<f64>,
    pub playing: bool,
    /// Seconds into the album, and its length.
    pub position: f64,
    pub length: f64,
    /// The song playing (index) and seconds into it.
    pub song: Option<usize>,
    pub in_song: f64,
}

/// How a song measured.
#[derive(Clone, Debug, PartialEq)]
pub struct SongAnalysis {
    /// The song as analysed (once it changes, the analysis is stale).
    pub song: Song,
    pub report: LoudnessReport,
    pub seconds: f64,
    /// What a record's groove makes of it.
    pub vinyl: VinylReport,
}

/// How a song will come out, from its analysis and the album settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Delivered {
    pub gain_db: f64,
    /// Integrated loudness after the gain (LUFS; before limiting).
    pub loudness: f64,
    /// How far the true peak goes over the ceiling, for the limiter to
    /// take off (dB, ≥ 0).
    pub limiting: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumTask {
    Analyse,
    Export,
    /// Render the album as it will be delivered, to play it.
    Prepare,
}

/// The album as it will be delivered (levelled, limited, with its pauses
/// and crossfades; float, undithered), ready to play, and where each song
/// starts in it.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumPreview {
    pub path: PathBuf,
    pub rate: u32,
    pub frames: u64,
    /// Each song's start (frames), in album order.
    pub songs: Vec<(SongId, u64)>,
    /// What it was made from: the album and the project's revision.
    pub album: Album,
    pub revision: u64,
}

/// A running album job.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlbumProgress {
    pub task: AlbumTask,
    /// The song being worked on (index).
    pub song: usize,
    pub songs: usize,
    /// 0..1 over the whole job.
    pub fraction: f64,
}

/// The files the last export wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumExport {
    pub folder: PathBuf,
    pub files: Vec<(SongId, PathBuf, Finished)>,
    pub album_file: Option<PathBuf>,
    pub cue: Option<PathBuf>,
    /// The CD master's folder (DDP fileset, read back and verified) and
    /// its length in CD frames (1/75 s).
    pub ddp: Option<(PathBuf, u32)>,
    pub vinyl: Option<VinylExport>,
}

/// The vinyl premaster as written.
#[derive(Clone, Debug, PartialEq)]
pub struct VinylExport {
    pub folder: PathBuf,
    /// One file per side.
    pub sides: Vec<PathBuf>,
    pub sheet: PathBuf,
    /// Findings: problems (it cannot be cut as it is) and warnings.
    pub problems: usize,
    pub warnings: usize,
}

#[derive(Default)]
pub(crate) struct AlbumState {
    analysis: HashMap<SongId, SongAnalysis>,
    errors: HashMap<SongId, String>,
    job: Option<Job>,
    last_export: Option<AlbumExport>,
    /// Progress last shown (redraws while it moves).
    shown: Option<(usize, u32)>,
    /// The album rendered to play, the file playing (if one is), and the
    /// song to start from once it is prepared.
    preview: Option<AlbumPreview>,
    playing: Option<Arc<faderframe_audio_files::StreamSource>>,
    play_after: Option<Option<SongId>>,
    preview_dir: Option<PathBuf>,
}

impl AlbumState {
    fn preview_dir(&mut self) -> PathBuf {
        self.preview_dir.get_or_insert_with(new_preview_dir).clone()
    }

    /// Remove what was rendered to play (the session goes).
    pub(crate) fn discard_preview(&mut self) {
        if let Some(d) = self.preview_dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing.is_some()
    }
}

/// What a song's audio is made from (prepared on the control thread).
enum Input {
    Render {
        project: Arc<Project>,
        a: MusicalTime,
        b: MusicalTime,
    },
    ProjectFile(PathBuf),
    File(PathBuf),
    Missing(String),
}

struct Item {
    song: Song,
    input: Input,
}

struct Shared {
    song: AtomicUsize,
    render: RenderProgress,
}

struct Job {
    task: AlbumTask,
    songs: usize,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<Outcome>>,
}

#[derive(Default)]
struct Outcome {
    analyses: Vec<(SongId, std::result::Result<SongAnalysis, String>)>,
    export: Option<std::result::Result<AlbumExport, String>>,
    preview: Option<std::result::Result<AlbumPreview, String>>,
    cancelled: bool,
}

struct Plan {
    items: Vec<Item>,
    /// Songs (with their inserts' current state), settings, release.
    album: Album,
    settings: AlbumSettings,
    rate: u32,
    folder: PathBuf,
    name: String,
}

/// A folder to render the album to play it in (one per session, removed
/// with it).
fn new_preview_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("faderframe-album-{}-{n}", std::process::id()))
}

/// The loudness of songs played one after another (energy mean weighted
/// by length; close to measuring them joined).
fn combined_loudness(parts: &[(f64, f64)]) -> f64 {
    let (mut energy, mut seconds) = (0.0, 0.0);
    for &(lufs, secs) in parts {
        if lufs.is_finite() {
            energy += 10f64.powf(lufs / 10.0) * secs;
            seconds += secs;
        }
    }
    if energy > 0.0 {
        10.0 * (energy / seconds).log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The gain for each song (dB).
fn gains(settings: &AlbumSettings, songs: &[LoudnessReport], album: f64) -> Vec<f64> {
    let Some(target) = settings.loudness.map(f64::from) else {
        return vec![0.0; songs.len()];
    };
    let ceiling = settings.ceiling.map(f64::from).filter(|_| !settings.limit);
    let to = |l: f64| if l.is_finite() { target - l } else { 0.0 };
    match settings.level {
        AlbumLevel::Album => {
            let mut g = to(album);
            if let Some(c) = ceiling {
                let tp = songs
                    .iter()
                    .fold(f64::NEG_INFINITY, |m, r| m.max(r.true_peak));
                if tp.is_finite() {
                    g = g.min(c - tp);
                }
            }
            vec![g; songs.len()]
        }
        AlbumLevel::PerSong => songs
            .iter()
            .map(|r| {
                let g = to(r.integrated);
                match ceiling {
                    Some(c) if r.true_peak.is_finite() => g.min(c - r.true_peak),
                    _ => g,
                }
            })
            .collect(),
    }
}

/// Stereo: a mono song is doubled, extra channels are dropped.
fn stereo(audio: &mut Vec<Vec<f32>>) {
    match audio.len() {
        0 => *audio = vec![Vec::new(), Vec::new()],
        1 => audio.push(audio[0].clone()),
        _ => audio.truncate(2),
    }
}

/// S-curve fades at both ends.
fn fade(audio: &mut [Vec<f32>], rate: u32, fade_in: f32, fade_out: f32) {
    let len = audio.first().map_or(0, Vec::len);
    let frames = |s: f32| ((s.max(0.0) as f64 * rate as f64) as usize).min(len);
    let (fi, fo) = (frames(fade_in), frames(fade_out));
    let curve = |i: usize, n: usize| 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / n as f64).cos();
    for ch in audio.iter_mut() {
        for (i, s) in ch[..fi].iter_mut().enumerate() {
            *s *= curve(i, fi) as f32;
        }
        for (i, s) in ch[len - fo..].iter_mut().rev().enumerate() {
            *s *= curve(i, fo) as f32;
        }
    }
}

/// `project` without the clips that start at or after `end`.
fn until(project: &Project, end: MusicalTime) -> Project {
    let mut p = project.clone();
    p.clips.retain(|_, c| c.start < end);
    for t in &mut p.tracks {
        t.clips.retain(|id| p.clips.contains_key(id));
    }
    p
}

fn song_audio(
    item: &Item,
    plan: &Plan,
    progress: &RenderProgress,
) -> std::result::Result<Vec<Vec<f32>>, String> {
    let (rate, tail) = (plan.rate, plan.settings.tail);
    let mut audio = match &item.input {
        Input::Render { project, a, b } => {
            render_span(project, rate, *a, *b, tail, progress).map_err(|e| e.to_string())?
        }
        Input::ProjectFile(path) => {
            let mut p = faderframe_project::file::load(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .project;
            let dir = path.parent();
            for s in p.sources.values_mut() {
                if let SourceSpec::File { path, .. } = &mut s.spec {
                    *path = crate::media::resolve(path, dir);
                }
            }
            let end = p.content_end();
            render_span(&p, rate, MusicalTime::ZERO, end, tail, progress)
                .map_err(|e| format!("{}: {e}", path.display()))?
        }
        Input::File(path) => decode_at_rate(path, rate, &progress.cancel)
            .map_err(|e| format!("{}: {e}", path.display()))?,
        Input::Missing(why) => return Err(why.clone()),
    };
    stereo(&mut audio);
    apply_gain(&mut audio, item.song.gain_db as f64);
    if item.song.inserts.iter().any(|s| !s.bypass) {
        audio = process_through(audio, rate, &item.song.inserts, tail, progress)
            .map_err(|e| format!("its inserts: {e}"))?;
    }
    fade(&mut audio, rate, item.song.fade_in, item.song.fade_out);
    Ok(audio)
}

fn cancelled(shared: &Shared) -> bool {
    shared.render.cancel.load(Ordering::Relaxed)
}

fn analyse(plan: &Plan, shared: &Shared) -> Outcome {
    let mut out = Outcome::default();
    for (i, item) in plan.items.iter().enumerate() {
        shared.song.store(i, Ordering::Relaxed);
        let result = song_audio(item, plan, &shared.render).map(|audio| SongAnalysis {
            song: item.song.clone(),
            report: measure(&audio, plan.rate),
            seconds: audio[0].len() as f64 / plan.rate as f64,
            vinyl: faderframe_analysis::vinyl::measure(&audio, plan.rate),
        });
        if cancelled(shared) {
            out.cancelled = true;
            return out;
        }
        out.analyses.push((item.song.id, result));
    }
    out
}

fn export(plan: &Plan, shared: &Shared) -> Outcome {
    let mut out = Outcome::default();
    let result = export_files(plan, shared, &mut out);
    if cancelled(shared) {
        out.cancelled = true;
    } else {
        out.export = Some(result);
    }
    out
}

fn export_files(
    plan: &Plan,
    shared: &Shared,
    out: &mut Outcome,
) -> std::result::Result<AlbumExport, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(&plan.folder).map_err(|e| io(&plan.folder, e))?;
    let (temps, gains, reports) = render_songs(plan, shared, out, &plan.folder)?;
    let result = check_cd_tracks(plan, out)
        .and_then(|()| deliver(plan, shared, &temps, &gains))
        .and_then(|mut done| {
            if plan.settings.vinyl.enabled {
                done.vinyl = Some(deliver_vinyl(plan, shared, out, &temps, &gains, &reports)?);
            }
            Ok(done)
        });
    for t in &temps {
        let _ = std::fs::remove_file(t);
    }
    result
}

/// Pass 1's songs: their temporary files, the gains the settings give
/// them and their measurements.
type Rendered = (Vec<PathBuf>, Vec<f64>, Vec<LoudnessReport>);

/// Pass 1: every song once, to a temporary file in `dir`, measured on its
/// own and as part of the album; the gains the settings give them.
fn render_songs(
    plan: &Plan,
    shared: &Shared,
    out: &mut Outcome,
    dir: &Path,
) -> std::result::Result<Rendered, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let rate = plan.rate;
    let mut temps: Vec<PathBuf> = Vec::new();
    let cleanup = |temps: &[PathBuf]| {
        for t in temps {
            let _ = std::fs::remove_file(t);
        }
    };
    let mut album = Measurement::new(rate);
    let mut reports = Vec::new();
    for (i, item) in plan.items.iter().enumerate() {
        shared.song.store(i, Ordering::Relaxed);
        let audio = match song_audio(item, plan, &shared.render) {
            Ok(a) => a,
            Err(e) => {
                cleanup(&temps);
                return Err(format!("{}: {e}", item.song.title));
            }
        };
        if cancelled(shared) {
            cleanup(&temps);
            return Err("cancelled".into());
        }
        let report = measure(&audio, rate);
        album.add(&audio);
        out.analyses.push((
            item.song.id,
            Ok(SongAnalysis {
                song: item.song.clone(),
                report,
                seconds: audio[0].len() as f64 / rate as f64,
                vinyl: faderframe_analysis::vinyl::measure(&audio, rate),
            }),
        ));
        reports.push(report);
        let temp = dir.join(format!(".faderframe-album-{i}.tmp.wav"));
        if let Err(e) = write_wav_with(&temp, &audio, rate, WavFormat::Float32, Dither::Off) {
            cleanup(&temps);
            return Err(io(&temp, e));
        }
        temps.push(temp);
    }
    let gains = gains(&plan.settings, &reports, album.report().integrated);
    Ok((temps, gains, reports))
}

/// A song at its gain, its true peak limited as the settings ask; the
/// gain and limiting applied (dB).
fn level(audio: &mut [Vec<f32>], s: &AlbumSettings, rate: u32, gain: f64) -> (f64, f64) {
    match s.ceiling.map(f64::from) {
        Some(c) if s.level == AlbumLevel::PerSong && s.limit && s.loudness.is_some() => {
            let target = s.loudness.map_or(0.0, f64::from);
            let n = normalize_loudness(audio, rate, target, c, PeakHandling::Limit);
            (n.gain_db, n.limited_db)
        }
        Some(c) => (gain, gain_and_limit(audio, rate, gain, c)),
        None => {
            apply_gain(audio, gain);
            (gain, 0.0)
        }
    }
}

/// Render the album to play it: pass 1, then the songs levelled and
/// limited as they will be delivered, joined after their pauses or into
/// their crossfades, into one float file in `dir`.
fn prepare(plan: &Plan, shared: &Shared, dir: &Path, revision: u64) -> Outcome {
    let mut out = Outcome::default();
    let result = (|| {
        let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        let (temps, gains, _) = render_songs(plan, shared, &mut out, dir)?;
        let path = dir.join("album.wav");
        let rate = plan.rate;
        let mut writer = WavWriter::create_with(&path, 2, rate, WavFormat::Float32, Dither::Off)
            .map_err(|e| io(&path, e))?;
        let n = plan.items.len();
        let (marks, frames) = {
            let mut sink = |_: &[Mark], _: u64, planes: &[&[f32]]| {
                writer
                    .write_planar(planes, planes[0].len())
                    .map_err(|e| e.to_string())
            };
            let mut assembler = Assembler::new(rate);
            for (i, item) in plan.items.iter().enumerate() {
                shared.song.store(n + i, Ordering::Relaxed);
                if cancelled(shared) {
                    return Err("cancelled".to_string());
                }
                let mut audio = read_wav(&temps[i]).map_err(|e| io(&temps[i], e))?.channels;
                level(&mut audio, &plan.settings, rate, gains[i]);
                let pause = if i == 0 { 0.0 } else { item.song.pause };
                let keep = plan.items.get(i + 1).map_or(0.0, |x| x.song.crossfade);
                assembler.add(&audio, pause, item.song.crossfade, keep, &mut sink)?;
            }
            assembler.finish(&mut sink)?
        };
        writer.finish().map_err(|e| io(&path, e))?;
        for t in &temps {
            let _ = std::fs::remove_file(t);
        }
        Ok(AlbumPreview {
            path,
            rate,
            frames,
            songs: plan
                .items
                .iter()
                .zip(&marks)
                .map(|(item, m)| (item.song.id, m.start))
                .collect(),
            album: plan.album.clone(),
            revision,
        })
    })();
    if cancelled(shared) {
        out.cancelled = true;
    } else {
        out.preview = Some(result);
    }
    out
}

/// With a CD master: every track (song to the next song's start) lasts
/// the 4 s a CD track needs.
fn check_cd_tracks(plan: &Plan, out: &Outcome) -> std::result::Result<(), String> {
    if !plan.settings.ddp {
        return Ok(());
    }
    let seconds: Vec<f64> = out
        .analyses
        .iter()
        .map(|(_, a)| a.as_ref().map_or(0.0, |a| a.seconds))
        .collect();
    for (i, item) in plan.items.iter().enumerate() {
        let next = plan.items.get(i + 1).map_or(0.0, |n| {
            if n.song.crossfade > 0.0 {
                -f64::from(n.song.crossfade)
            } else {
                f64::from(n.song.pause.max(0.0))
            }
        });
        let length = seconds.get(i).copied().unwrap_or(0.0) + next;
        if length < 4.0 {
            return Err(format!(
                "“{}” would be a {length:.1} s CD track; a CD track lasts at least 4 s",
                item.song.title
            ));
        }
    }
    Ok(())
}

/// Pass 2: level, limit, dither and write the songs, the album file, its
/// cue sheet and the CD master.
fn deliver(
    plan: &Plan,
    shared: &Shared,
    temps: &[PathBuf],
    gains: &[f64],
) -> std::result::Result<AlbumExport, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let (s, rate, n) = (&plan.settings, plan.rate, plan.items.len());
    let name = sanitize(&plan.name);
    let paths: Vec<PathBuf> = plan
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            plan.folder
                .join(format!("{:02} {}.wav", i + 1, sanitize(&item.song.title)))
        })
        .collect();
    // Overlapping songs: the song files are cut from the album stream.
    let gapless = plan.items.iter().skip(1).any(|i| i.song.crossfade > 0.0);
    let album_path = s
        .album_file
        .then(|| plan.folder.join(format!("{name} (Album).wav")));
    let ddp_dir = s.ddp.then(|| plan.folder.join(format!("{name} (DDP)")));
    let mut whole = match &album_path {
        Some(p) => {
            Some(WavWriter::create_with(p, 2, rate, s.format, s.dither).map_err(|e| io(p, e))?)
        }
        None => None,
    };
    let mut cd = match &ddp_dir {
        Some(d) => Some(CdMaster::create(d, rate, s.dither)?),
        None => None,
    };
    let mut cut = gapless.then(|| Gapless::new(paths.clone(), rate, s.format, s.dither));
    let streaming = whole.is_some() || cd.is_some() || cut.is_some();
    let mut files = Vec::new();
    let (marks, length) = {
        let mut sink = |marks: &[Mark], pos: u64, planes: &[&[f32]]| {
            if let Some(w) = whole.as_mut() {
                w.write_planar(planes, planes[0].len())
                    .map_err(|e| e.to_string())?;
            }
            if let Some(c) = cd.as_mut() {
                c.write(planes)?;
            }
            if let Some(g) = cut.as_mut() {
                g.write(marks, pos, planes)?;
            }
            Ok(())
        };
        let mut assembler = Assembler::new(rate);
        for (i, item) in plan.items.iter().enumerate() {
            shared.song.store(n + i, Ordering::Relaxed);
            if cancelled(shared) {
                return Err("cancelled".into());
            }
            let mut audio = read_wav(&temps[i]).map_err(|e| io(&temps[i], e))?.channels;
            let (gain_db, limited_db) = level(&mut audio, s, rate, gains[i]);
            let report = measure(&audio, rate);
            if !gapless {
                write_wav_with(&paths[i], &audio, rate, s.format, s.dither)
                    .map_err(|e| io(&paths[i], e))?;
            }
            if streaming {
                let pause = if i == 0 { 0.0 } else { item.song.pause };
                let keep = plan.items.get(i + 1).map_or(0.0, |x| x.song.crossfade);
                assembler.add(&audio, pause, item.song.crossfade, keep, &mut sink)?;
            }
            files.push((
                item.song.id,
                paths[i].clone(),
                Finished {
                    gain_db,
                    limited_db,
                    report,
                },
            ));
        }
        assembler.finish(&mut sink)?
    };
    if let Some(g) = cut {
        g.finish()?;
    }
    let (album_file, cue) = match (whole, album_path) {
        (Some(w), Some(path)) => {
            w.finish().map_err(|e| io(&path, e))?;
            let file = path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().to_string());
            let sheet = disc(&plan.album, &plan.name, &marks, (rate, length), 0, true);
            let cue_path = path.with_extension("cue");
            std::fs::write(
                &cue_path,
                faderframe_disc::cue::cue_sheet(&sheet, &file, "WAVE", 0),
            )
            .map_err(|e| io(&cue_path, e))?;
            (Some(path), Some(cue_path))
        }
        _ => (None, None),
    };
    let ddp = match (cd, ddp_dir) {
        (Some(c), Some(dir)) => {
            let master = disc(
                &plan.album,
                &plan.name,
                &marks,
                (rate, length),
                MIN_PREGAP,
                s.cd_text,
            );
            let written = c.finish(&master)?;
            // Read back what a plant will read: descriptors, CD-Text,
            // checksums of every file.
            let back = faderframe_disc::ddp::read(&dir)
                .map_err(|e| format!("the DDP fileset does not read back: {e}"))?;
            if back.checksums != Some(true) || back.disc != written.disc {
                return Err("the DDP fileset does not read back as written".into());
            }
            Some((dir, written.disc.sectors))
        }
        _ => None,
    };
    Ok(AlbumExport {
        folder: plan.folder.clone(),
        files,
        album_file,
        cue,
        ddp,
        vinyl: None,
    })
}

/// The vinyl premaster (after the digital release, from the same rendered
/// songs): the songs at their premaster gain (see
/// [`vinyl::premaster_gains`]), one continuous 24-bit file per side with
/// the pauses and crossfades inside a side, each song's own file if asked
/// for, and the cutting sheet with the checks' findings.
fn deliver_vinyl(
    plan: &Plan,
    shared: &Shared,
    out: &Outcome,
    temps: &[PathBuf],
    digital: &[f64],
    reports: &[LoudnessReport],
) -> std::result::Result<VinylExport, String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let (s, rate, n) = (&plan.settings, plan.rate, plan.items.len());
    let v = &s.vinyl;
    let folder = plan
        .folder
        .join(format!("{} (Vinyl)", sanitize(&plan.name)));
    std::fs::create_dir_all(&folder).map_err(|e| io(&folder, e))?;
    let peaks: Vec<f64> = reports.iter().map(|r| r.true_peak).collect();
    let gains = vinyl::premaster_gains(v, &peaks, digital);
    let songs: Vec<Song> = plan.items.iter().map(|i| i.song.clone()).collect();
    let analyses: Vec<Option<&SongAnalysis>> = plan
        .items
        .iter()
        .map(|item| {
            out.analyses
                .iter()
                .rev()
                .find(|(id, _)| *id == item.song.id)
                .and_then(|(_, a)| a.as_ref().ok())
        })
        .collect();
    let lengths: Vec<f64> = analyses
        .iter()
        .map(|a| a.map_or(0.0, |a| a.seconds))
        .collect();
    let sides = vinyl::plan_sides(&songs, &lengths, v);
    let album = Album {
        songs: songs.clone(),
        ..plan.album.clone()
    };
    let facts: Vec<Option<vinyl::SongFacts<'_>>> = analyses
        .iter()
        .zip(&gains)
        .map(|(a, g)| a.map(|a| vinyl::facts(a, *g)))
        .collect();
    let notes = vinyl::check(&album, v.format, &sides, &facts);
    let format = WavFormat::Pcm24;
    let mut sheet_sides = Vec::new();
    let mut side_files = Vec::new();
    for (k, side) in sides.iter().enumerate() {
        let name = format!("Side {}.wav", vinyl::side_name(k));
        let path = folder.join(&name);
        let mut writer =
            WavWriter::create_with(&path, 2, rate, format, s.dither).map_err(|e| io(&path, e))?;
        let mut sheet = Vec::new();
        let (marks, length) = {
            let mut sink = |_: &[Mark], _: u64, planes: &[&[f32]]| {
                writer
                    .write_planar(planes, planes[0].len())
                    .map_err(|e| e.to_string())
            };
            let mut assembler = Assembler::new(rate);
            for i in side.songs.clone() {
                shared.song.store(2 * n + i, Ordering::Relaxed);
                if cancelled(shared) {
                    return Err("cancelled".into());
                }
                let mut audio = read_wav(&temps[i]).map_err(|e| io(&temps[i], e))?.channels;
                if v.limit {
                    gain_and_limit(&mut audio, rate, gains[i], f64::from(v.peak));
                } else {
                    apply_gain(&mut audio, gains[i]);
                }
                let report = measure(&audio, rate);
                let first = i == side.songs.start;
                let last = i + 1 == side.songs.end;
                let file = if v.track_files {
                    let label = vinyl::track_label(k, i - side.songs.start);
                    let f = format!("{label} {}.wav", sanitize(&songs[i].title));
                    let p = folder.join(&f);
                    write_wav_with(&p, &audio, rate, format, s.dither).map_err(|e| io(&p, e))?;
                    Some(f)
                } else {
                    None
                };
                sheet.push(vinyl::SheetSong {
                    title: songs[i].title.clone(),
                    isrc: songs[i].isrc.clone(),
                    start: 0.0,
                    length: audio[0].len() as f64 / f64::from(rate),
                    true_peak: report.true_peak,
                    loudness: report.integrated,
                    file,
                });
                let pause = if first { 0.0 } else { songs[i].pause };
                let crossfade = if first { 0.0 } else { songs[i].crossfade };
                let keep = if last {
                    0.0
                } else {
                    songs.get(i + 1).map_or(0.0, |x| x.crossfade)
                };
                assembler.add(&audio, pause, crossfade, keep, &mut sink)?;
            }
            assembler.finish(&mut sink)?
        };
        writer.finish().map_err(|e| io(&path, e))?;
        for (song, mark) in sheet.iter_mut().zip(&marks) {
            song.start = mark.start as f64 / f64::from(rate);
        }
        sheet_sides.push(vinyl::SheetSide {
            seconds: length as f64 / f64::from(rate),
            file: name,
            songs: sheet,
        });
        side_files.push(path);
    }
    let text = vinyl::cutting_sheet(&album, &plan.name, rate, &sheet_sides, &notes);
    let sheet = folder.join("Cutting Sheet.txt");
    std::fs::write(&sheet, text).map_err(|e| io(&sheet, e))?;
    let count = |sev| notes.iter().filter(|n| n.severity == sev).count();
    Ok(VinylExport {
        folder,
        sides: side_files,
        sheet,
        problems: count(vinyl::Severity::Problem),
        warnings: count(vinyl::Severity::Warning),
    })
}

impl Session {
    pub(crate) fn album_action(&mut self, action: AlbumAction) -> Result<()> {
        let mut album = self.project.album.clone();
        match action {
            AlbumAction::AddSections => {
                let mut sections = self.project.sections.clone();
                sections.sort_by_key(|s| s.start);
                for s in sections {
                    if !album.has_section(s.id) {
                        let id = self.project.ids.allocate();
                        album
                            .songs
                            .push(Song::new(id, s.name, SongSource::Section(s.id)));
                    }
                }
                if album == self.project.album {
                    let text = if self.project.sections.is_empty() {
                        "the project has no sections — add some in the arranger's section lane"
                    } else {
                        "every section is in the album already"
                    };
                    self.notify(NoticeLevel::Info, text);
                    return Ok(());
                }
            }
            AlbumAction::AddThisProject => {
                let id = self.project.ids.allocate();
                let title = self.project.name.clone();
                album
                    .songs
                    .push(Song::new(id, title, SongSource::ThisProject));
            }
            AlbumAction::AddFiles(paths) => {
                for path in paths {
                    let id = self.project.ids.allocate();
                    let title = path
                        .file_stem()
                        .map_or_else(|| "Song".to_string(), |s| s.to_string_lossy().to_string());
                    album
                        .songs
                        .push(Song::new(id, title, SongSource::for_path(&path)));
                }
            }
            AlbumAction::Remove(id) => album.songs.retain(|s| s.id != id),
            AlbumAction::Move { song, to } => {
                let Some(from) = album.index(song) else {
                    return Ok(());
                };
                let s = album.songs.remove(from);
                album.songs.insert(to.min(album.songs.len()), s);
            }
            AlbumAction::Update(mut song) => {
                song.isrc = match song.isrc.trim() {
                    "" => String::new(),
                    code => faderframe_disc::normalize_isrc(code)
                        .map_err(|e| SessionError::Other(e.to_string()))?,
                };
                song.crossfade = song.crossfade.max(0.0);
                match album.song_mut(song.id) {
                    Some(s) => *s = song,
                    None => return Ok(()),
                }
            }
            AlbumAction::Settings(settings) => {
                // Splitting sides by hand starts from the sides as they are.
                if album.settings.vinyl.auto_sides && !settings.vinyl.auto_sides {
                    self.seed_side_breaks(&mut album);
                }
                album.settings = settings;
            }
            AlbumAction::SideBreak { song, on } => {
                if album.settings.vinyl.auto_sides {
                    self.seed_side_breaks(&mut album);
                    album.settings.vinyl.auto_sides = false;
                }
                match album.index(song) {
                    Some(i) if i > 0 => album.songs[i].side_break = on,
                    _ => return Ok(()),
                }
            }
            AlbumAction::Info(mut info) => {
                info.upc = match info.upc.trim() {
                    "" => String::new(),
                    code => faderframe_disc::normalize_upc(code)
                        .map_err(|e| SessionError::Other(e.to_string()))?,
                };
                album.info = info;
            }
            AlbumAction::Texts { mut info, song } => {
                info.upc = match info.upc.trim() {
                    "" => String::new(),
                    code => faderframe_disc::normalize_upc(code)
                        .map_err(|e| SessionError::Other(e.to_string()))?,
                };
                // No language twice, the main one not again.
                let main = info.language;
                let mut seen = Vec::new();
                info.translations.retain(|t| {
                    let keep = t.language != main && !seen.contains(&t.language);
                    seen.push(t.language);
                    keep
                });
                if let Some(mut song) = song.map(|s| *s) {
                    song.isrc = match song.isrc.trim() {
                        "" => String::new(),
                        code => faderframe_disc::normalize_isrc(code)
                            .map_err(|e| SessionError::Other(e.to_string()))?,
                    };
                    if let Some(s) = album.song_mut(song.id) {
                        *s = song;
                    }
                }
                album.info = info;
            }
            AlbumAction::AddInsert { song, plugin } => {
                let Some(s) = album.song(song) else {
                    return Ok(());
                };
                let id: PluginInstanceId = self.project.ids.allocate();
                let hosted = plugin.format != faderframe_project::PluginFormat::Builtin;
                let mut inserts = s.inserts.clone();
                inserts.push(PluginSlot {
                    id,
                    plugin,
                    bypass: false,
                    parameters: Vec::new(),
                    state: None,
                    sidechain: None,
                });
                self.edit(Command::SetSongInserts { song, inserts })?;
                // Like an insert on a track: a hosted plugin shows its editor.
                if hosted && let Some(track) = self.project.master_id() {
                    self.ui_requests.push(crate::UiRequest::PluginEditor {
                        track,
                        plugin: id,
                        generic: false,
                    });
                }
                return Ok(());
            }
            AlbumAction::RemoveInsert { song, plugin } => {
                return self.edit_song_inserts(song, |v| v.retain(|s| s.id != plugin));
            }
            AlbumAction::MoveInsert { song, plugin, to } => {
                return self.edit_song_inserts(song, |v| {
                    if let Some(from) = v.iter().position(|s| s.id == plugin) {
                        let slot = v.remove(from);
                        v.insert(to.min(v.len()), slot);
                    }
                });
            }
            AlbumAction::BypassInsert {
                song,
                plugin,
                bypass,
            } => {
                return self.edit_song_inserts(song, |v| {
                    for s in v.iter_mut().filter(|s| s.id == plugin) {
                        s.bypass = bypass;
                    }
                });
            }
            AlbumAction::Details(song) => {
                self.ui_requests.push(crate::UiRequest::AlbumDetails(song));
                self.revision += 1;
                return Ok(());
            }
            AlbumAction::Monitor(song) => {
                let song = song.filter(|id| self.project.album.song(*id).is_some());
                if song != self.engine.album_monitor() {
                    self.engine.set_album_monitor(song);
                    self.sync(faderframe_project::Impact::Graph)?;
                }
                return Ok(());
            }
            AlbumAction::Analyse => return self.start_album_job(AlbumTask::Analyse),
            AlbumAction::Export => return self.start_album_job(AlbumTask::Export),
            AlbumAction::Cancel => {
                if let Some(j) = &self.album_state.job {
                    j.shared.render.cancel.store(true, Ordering::Relaxed);
                }
                return Ok(());
            }
            AlbumAction::Play(from) => return self.album_play(from),
            AlbumAction::Pause => {
                self.album_pause();
                return Ok(());
            }
            AlbumAction::StopPlaying => {
                self.album_stop_playing();
                return Ok(());
            }
            AlbumAction::Skip(d) => return self.album_skip(d),
            AlbumAction::Seek(t) => {
                self.album_seek(t);
                return Ok(());
            }
        }
        if album == self.project.album {
            return Ok(());
        }
        self.edit(Command::SetAlbum {
            album: Box::new(album),
        })
    }

    /// Change a song's inserts (one undo step; nothing when unchanged).
    fn edit_song_inserts(
        &mut self,
        song: SongId,
        change: impl FnOnce(&mut Vec<PluginSlot>),
    ) -> Result<()> {
        let Some(s) = self.project.album.song(song) else {
            return Ok(());
        };
        let mut inserts = s.inserts.clone();
        change(&mut inserts);
        if inserts == s.inserts {
            return Ok(());
        }
        self.edit(Command::SetSongInserts { song, inserts })
    }

    /// The song whose inserts run after the master strip.
    pub fn album_monitor(&self) -> Option<SongId> {
        self.engine
            .album_monitor()
            .filter(|id| self.project.album.song(*id).is_some())
    }

    /// The album's sample rate.
    pub fn album_rate(&self) -> u32 {
        self.project
            .album
            .settings
            .sample_rate
            .unwrap_or(self.project.sample_rate)
    }

    /// The folder an export writes to.
    pub fn album_folder(&self) -> PathBuf {
        if let Some(f) = &self.project.album.settings.output {
            return f.clone();
        }
        let base = self.project_dir().unwrap_or_else(|| {
            std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from)
        });
        base.join(format!("{} Album", sanitize(&self.project.name)))
    }

    /// The song's analysis, if it still describes the song.
    pub fn album_analysis(&self, song: &Song) -> Option<&SongAnalysis> {
        self.album_state
            .analysis
            .get(&song.id)
            .filter(|a| a.song.same_audio(song))
    }

    /// Why the song could not be analysed or exported last time.
    pub fn album_error(&self, song: SongId) -> Option<&str> {
        self.album_state.errors.get(&song).map(String::as_str)
    }

    /// The album's loudness from its songs' analyses (`None` until every
    /// song has a current one).
    pub fn album_loudness(&self) -> Option<LoudnessReport> {
        let songs = &self.project.album.songs;
        let analyses: Option<Vec<&SongAnalysis>> =
            songs.iter().map(|s| self.album_analysis(s)).collect();
        let analyses = analyses.filter(|a| !a.is_empty())?;
        let parts: Vec<(f64, f64)> = analyses
            .iter()
            .map(|a| (a.report.integrated, a.seconds))
            .collect();
        let max = |f: fn(&LoudnessReport) -> f64| {
            analyses
                .iter()
                .map(|a| f(&a.report))
                .fold(f64::NEG_INFINITY, f64::max)
        };
        Some(LoudnessReport {
            integrated: combined_loudness(&parts),
            range: max(|r| r.range),
            true_peak: max(|r| r.true_peak),
            sample_peak: max(|r| r.sample_peak),
            max_short_term: max(|r| r.max_short_term),
        })
    }

    /// How each song will be delivered (from current analyses).
    pub fn album_delivered(&self, song: SongId) -> Option<Delivered> {
        let album = &self.project.album;
        let i = album.index(song)?;
        let a = self.album_analysis(&album.songs[i])?;
        let reports: Vec<LoudnessReport> = album
            .songs
            .iter()
            .map(|s| self.album_analysis(s).map_or(a.report, |x| x.report))
            .collect();
        let whole = self
            .album_loudness()
            .map_or(a.report.integrated, |r| r.integrated);
        let gain_db = gains(&album.settings, &reports, whole)[i];
        let limiting = album
            .settings
            .ceiling
            .map_or(0.0, |c| (a.report.true_peak + gain_db - c as f64).max(0.0));
        Some(Delivered {
            gain_db,
            loudness: a.report.integrated + gain_db,
            limiting,
        })
    }

    pub fn album_progress(&self) -> Option<AlbumProgress> {
        let j = self.album_state.job.as_ref()?;
        let song = j.shared.song.load(Ordering::Relaxed);
        let steps = match j.task {
            AlbumTask::Analyse => j.songs,
            AlbumTask::Export if self.project.album.settings.vinyl.enabled => j.songs * 3,
            AlbumTask::Export | AlbumTask::Prepare => j.songs * 2,
        }
        .max(1);
        let within = if song < j.songs {
            j.shared.render.fraction()
        } else {
            1.0
        };
        Some(AlbumProgress {
            task: j.task,
            song: song % j.songs.max(1),
            songs: j.songs,
            fraction: ((song as f64 + within) / steps as f64).min(1.0),
        })
    }

    /// What the last export wrote.
    pub fn album_export(&self) -> Option<&AlbumExport> {
        self.album_state.last_export.as_ref()
    }

    fn start_album_job(&mut self, task: AlbumTask) -> Result<()> {
        if self.album_state.job.is_some() {
            return Err(SessionError::Other(
                "the album is being analysed or exported already".into(),
            ));
        }
        if self.project.album.songs.is_empty() {
            return Err(SessionError::Other("the album has no songs".into()));
        }
        if task == AlbumTask::Export {
            check_codes(&self.project.album).map_err(SessionError::Other)?;
        }
        // Takes the plugins' current state (song inserts too) first.
        let project = Arc::new(self.render_copy());
        let album: Album = project.album.clone();
        let items = album
            .songs
            .iter()
            .map(|song| {
                let input = match &song.source {
                    SongSource::Section(id) => {
                        match project.sections.iter().find(|s| s.id == *id) {
                            Some(s) => Input::Render {
                                project: Arc::new(until(&project, s.end)),
                                a: s.start,
                                b: s.end,
                            },
                            None => Input::Missing("its section was removed".into()),
                        }
                    }
                    SongSource::ThisProject => Input::Render {
                        project: Arc::clone(&project),
                        a: MusicalTime::ZERO,
                        b: project.content_end(),
                    },
                    SongSource::Project(p) => Input::ProjectFile(p.clone()),
                    SongSource::AudioFile(p) => Input::File(p.clone()),
                };
                Item {
                    song: song.clone(),
                    input,
                }
            })
            .collect();
        let plan = Plan {
            items,
            album: album.clone(),
            settings: album.settings.clone(),
            // Playback renders at the engine's rate (it plays as it is).
            rate: if task == AlbumTask::Prepare {
                self.engine.sample_rate()
            } else {
                self.album_rate()
            },
            folder: self.album_folder(),
            name: self.project.name.clone(),
        };
        let preview_dir = self.album_state.preview_dir();
        let revision = self.history.revision();
        let shared = Arc::new(Shared {
            song: AtomicUsize::new(0),
            render: RenderProgress::default(),
        });
        let s = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("faderframe-album".into())
            .spawn(move || match task {
                AlbumTask::Analyse => analyse(&plan, &s),
                AlbumTask::Export => export(&plan, &s),
                AlbumTask::Prepare => prepare(&plan, &s, &preview_dir, revision),
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.album_state.job = Some(Job {
            task,
            songs: album.songs.len(),
            shared,
            handle: Some(handle),
        });
        self.revision += 1;
        Ok(())
    }

    /// Finish a done album job (from the tick); redraw while one runs.
    pub(crate) fn poll_album(&mut self) {
        let Some(job) = self.album_state.job.as_mut() else {
            return;
        };
        if !job.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            let p = self.album_progress();
            let shown = p.map(|p| (p.song, (p.fraction * 200.0) as u32));
            if shown != self.album_state.shown {
                self.album_state.shown = shown;
                self.revision += 1;
            }
            return;
        }
        let task = job.task;
        let outcome = job.handle.take().map(JoinHandle::join);
        self.album_state.job = None;
        self.album_state.shown = None;
        self.revision += 1;
        let Some(Ok(outcome)) = outcome else {
            self.notify(NoticeLevel::Error, "the album job failed");
            return;
        };
        let mut failed = Vec::new();
        for (id, r) in outcome.analyses {
            match r {
                Ok(a) => {
                    self.album_state.errors.remove(&id);
                    self.album_state.analysis.insert(id, a);
                }
                Err(e) => {
                    failed.push(e.clone());
                    self.album_state.errors.insert(id, e);
                }
            }
        }
        if outcome.cancelled {
            self.album_state.play_after = None;
            self.notify(NoticeLevel::Info, "album job cancelled");
            return;
        }
        match outcome.preview {
            Some(Ok(p)) => {
                self.album_state.preview = Some(p);
                if let Some(from) = self.album_state.play_after.take()
                    && let Err(e) = self.album_play(from)
                {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
                return;
            }
            Some(Err(e)) => {
                self.album_state.play_after = None;
                self.notify(NoticeLevel::Error, format!("album playback: {e}"));
                return;
            }
            None => {}
        }
        match outcome.export {
            Some(Ok(done)) => {
                let mut extras = Vec::new();
                if done.album_file.is_some() {
                    extras.push("the album file and its cue sheet".to_string());
                }
                if let Some((_, sectors)) = &done.ddp {
                    extras.push(format!(
                        "a verified CD master (DDP, {})",
                        faderframe_disc::msf_colon(*sectors)
                    ));
                }
                if let Some(v) = &done.vinyl {
                    extras.push(format!(
                        "a vinyl premaster ({} side{} and a cutting sheet)",
                        v.sides.len(),
                        if v.sides.len() == 1 { "" } else { "s" }
                    ));
                }
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "Album exported: {} song(s){}{} to {}",
                        done.files.len(),
                        if extras.is_empty() { "" } else { ", " },
                        extras.join(", "),
                        done.folder.display()
                    ),
                );
                // Blank CD-Rs and most plants take 79:57 at most.
                if done
                    .ddp
                    .as_ref()
                    .is_some_and(|(_, s)| *s > (79 * 60 + 57) * 75)
                {
                    self.notify(
                        NoticeLevel::Warning,
                        "the CD master is longer than 79:57 — check with the plant",
                    );
                }
                if let Some(v) = done.vinyl.as_ref().filter(|v| v.problems + v.warnings > 0) {
                    self.notify(
                        if v.problems > 0 {
                            NoticeLevel::Warning
                        } else {
                            NoticeLevel::Info
                        },
                        format!(
                            "vinyl: {} problem(s) and {} warning(s) — see the cutting sheet",
                            v.problems, v.warnings
                        ),
                    );
                }
                self.album_state.last_export = Some(done);
            }
            Some(Err(e)) => self.notify(NoticeLevel::Error, format!("album export: {e}")),
            None if task == AlbumTask::Analyse && !failed.is_empty() => self.notify(
                NoticeLevel::Warning,
                format!("album analysis: {}", failed.join("; ")),
            ),
            None => {}
        }
    }

    /// Whether the prepared album is still the album (and the project) it
    /// was made from.
    fn preview_fresh(&self) -> bool {
        self.album_state.preview.as_ref().is_some_and(|p| {
            p.album == self.project.album
                && p.revision == self.history.revision()
                && p.rate == self.engine.sample_rate()
        })
    }

    /// Play the album as it will be delivered, from `from` (or where it
    /// was, or the start).
    pub(crate) fn album_play(&mut self, from: Option<SongId>) -> Result<()> {
        if !self.preview_fresh() {
            // (Re)render it first; playback starts once it is ready.
            self.album_stop_playing();
            self.album_state.play_after = Some(from);
            return self.start_album_job(AlbumTask::Prepare);
        }
        let Some(p) = self.album_state.preview.clone() else {
            return Ok(());
        };
        if self.album_state.playing.is_none() {
            let source = faderframe_audio_files::StreamSource::open(&p.path)
                .map_err(|e| SessionError::Other(format!("{}: {e}", p.path.display())))?;
            // The project stops: the album plays instead.
            if self.transport.playing {
                self.dispatch(crate::Action::Transport(crate::TransportAction::Stop))?;
            }
            self.engine.set_preview(Some(Arc::clone(&source)))?;
            self.loader.set_preview(Some(Arc::clone(&source)));
            self.album_state.playing = Some(source);
        }
        let preview = self.engine.preview();
        if let Some(id) = from
            && let Some((_, start)) = p.songs.iter().find(|(s, _)| *s == id)
        {
            preview.locate(*start as i64);
        }
        preview.play(true);
        self.revision += 1;
        Ok(())
    }

    pub(crate) fn album_pause(&mut self) {
        self.engine.preview().play(false);
        self.revision += 1;
    }

    /// Back to the project (the prepared album is kept).
    pub(crate) fn album_stop_playing(&mut self) {
        if self.album_state.playing.take().is_some() {
            let _ = self.engine.set_preview(None);
            self.loader.set_preview(None);
            self.revision += 1;
        }
    }

    pub(crate) fn album_skip(&mut self, delta: i32) -> Result<()> {
        let Some(pb) = self.album_playback() else {
            return Ok(());
        };
        let Some(p) = self.album_state.preview.as_ref() else {
            return Ok(());
        };
        let now = pb.song.unwrap_or(0) as i64;
        // Back within the first seconds of a song: the one before;
        // otherwise its start.
        let target = if delta < 0 && pb.in_song > 3.0 {
            now
        } else {
            now + i64::from(delta)
        };
        let target = target.clamp(0, p.songs.len() as i64 - 1) as usize;
        let start = p.songs[target].1;
        self.engine.preview().locate(start as i64);
        self.revision += 1;
        Ok(())
    }

    pub(crate) fn album_seek(&mut self, seconds: f64) {
        if let Some(p) = &self.album_state.preview {
            self.engine
                .preview()
                .locate((seconds.max(0.0) * f64::from(p.rate)) as i64);
            self.revision += 1;
        }
    }

    /// Where each song of the prepared album starts and how long it is
    /// (seconds; while it plays).
    pub fn album_marks(&self) -> Option<(Vec<(SongId, f64)>, f64)> {
        self.album_state.playing.as_ref()?;
        let p = self.album_state.preview.as_ref()?;
        let rate = f64::from(p.rate.max(1));
        Some((
            p.songs
                .iter()
                .map(|(id, f)| (*id, *f as f64 / rate))
                .collect(),
            p.frames as f64 / rate,
        ))
    }

    /// Mark where the automatic sides start now (on `album`, a copy of
    /// the project's).
    fn seed_side_breaks(&self, album: &mut Album) {
        let starts: Vec<usize> = self
            .vinyl_sides()
            .unwrap_or_default()
            .iter()
            .map(|s| s.songs.start)
            .collect();
        for (i, song) in album.songs.iter_mut().enumerate() {
            song.side_break = i > 0 && starts.contains(&i);
        }
    }

    /// A song's length: as analysed, else (a section, this project) from
    /// the tempo map; files are known once analysed.
    pub fn album_song_seconds(&self, song: &Song) -> Option<f64> {
        if let Some(a) = self.album_analysis(song) {
            return Some(a.seconds);
        }
        let p = &self.project;
        let (a, b) = match &song.source {
            SongSource::Section(id) => {
                let s = p.sections.iter().find(|s| s.id == *id)?;
                (s.start, s.end)
            }
            SongSource::ThisProject => (MusicalTime::ZERO, p.content_end()),
            _ => return None,
        };
        let tempo = &p.timeline.tempo;
        Some(tempo.musical_to_seconds(b) - tempo.musical_to_seconds(a))
    }

    /// The album's sides on vinyl (`None`: no vinyl premaster). Songs not
    /// measured yet count with their estimate, files as nothing.
    pub fn vinyl_sides(&self) -> Option<Vec<vinyl::Side>> {
        let album = &self.project.album;
        if !album.settings.vinyl.enabled {
            return None;
        }
        let lengths: Vec<f64> = album
            .songs
            .iter()
            .map(|s| self.album_song_seconds(s).unwrap_or(0.0))
            .collect();
        Some(vinyl::plan_sides(
            &album.songs,
            &lengths,
            &album.settings.vinyl,
        ))
    }

    /// What a lathe will make of the album (worst first; empty without a
    /// vinyl premaster).
    pub fn vinyl_notes(&self) -> Vec<vinyl::VinylNote> {
        let Some(sides) = self.vinyl_sides() else {
            return Vec::new();
        };
        let album = &self.project.album;
        let analyses: Vec<Option<&SongAnalysis>> =
            album.songs.iter().map(|s| self.album_analysis(s)).collect();
        let digital: Vec<f64> = album
            .songs
            .iter()
            .map(|s| self.album_delivered(s.id).map_or(0.0, |d| d.gain_db))
            .collect();
        let peaks: Vec<f64> = analyses
            .iter()
            .map(|a| a.map_or(f64::NEG_INFINITY, |a| a.report.true_peak))
            .collect();
        let gains = vinyl::premaster_gains(&album.settings.vinyl, &peaks, &digital);
        let facts: Vec<Option<vinyl::SongFacts<'_>>> = analyses
            .iter()
            .zip(&gains)
            .map(|(a, g)| a.map(|a| vinyl::facts(a, *g)))
            .collect();
        let mut notes = vinyl::check(album, album.settings.vinyl.format, &sides, &facts);
        let missing = analyses.iter().filter(|a| a.is_none()).count();
        if missing > 0 {
            notes.push(vinyl::VinylNote {
                severity: vinyl::Severity::Note,
                song: None,
                side: None,
                text: format!(
                    "{missing} song{} not measured yet: analyse the album for the vinyl checks and exact side times.",
                    if missing == 1 { "" } else { "s" }
                ),
            });
        }
        notes
    }

    /// Album playback now (`None`: neither playing nor being prepared).
    pub fn album_playback(&self) -> Option<AlbumPlayback> {
        let preparing = self
            .album_state
            .job
            .as_ref()
            .filter(|j| j.task == AlbumTask::Prepare)
            .and_then(|_| self.album_progress())
            .map(|p| p.fraction);
        if self.album_state.playing.is_none() {
            return preparing.map(|f| AlbumPlayback {
                preparing: Some(f),
                playing: false,
                position: 0.0,
                length: 0.0,
                song: None,
                in_song: 0.0,
            });
        }
        let p = self.album_state.preview.as_ref()?;
        let rate = f64::from(p.rate.max(1));
        let shared = self.engine.preview();
        let pos = shared.position().max(0) as u64;
        let song = p.songs.iter().rposition(|(_, start)| *start <= pos);
        let in_song = song.map_or(0.0, |i| (pos - p.songs[i].1) as f64 / rate);
        Some(AlbumPlayback {
            preparing,
            playing: shared.is_playing(),
            position: pos as f64 / rate,
            length: p.frames as f64 / rate,
            song,
            in_song,
        })
    }

    /// Wait for a running album job (tests and shutdown).
    pub fn wait_album(&mut self) {
        while self
            .album_state
            .job
            .as_ref()
            .is_some_and(|j| j.handle.as_ref().is_some_and(|h| !h.is_finished()))
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.poll_album();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_reach_silence_at_both_ends_and_spare_the_middle() {
        let mut a = vec![vec![1.0f32; 1000]; 2];
        fade(&mut a, 1000, 0.1, 0.2);
        for ch in &a {
            assert_eq!(ch[0], 0.0);
            assert!(ch[999] < 1e-3);
            assert!(ch[50] > 0.4 && ch[50] < 0.6, "{}", ch[50]);
            assert!(ch[100..800].iter().all(|v| *v == 1.0));
            assert!(ch[1..100].windows(2).all(|w| w[1] >= w[0]));
        }
        // Longer than the song: clamped.
        let mut b = vec![vec![1.0f32; 10]];
        fade(&mut b, 1000, 5.0, 5.0);
        assert!(b[0].iter().all(|v| v.is_finite() && *v <= 1.0));
    }

    #[test]
    fn album_gains_keep_relative_levels_or_level_each_song() {
        let report = |integrated: f64, true_peak: f64| LoudnessReport {
            integrated,
            range: 0.0,
            true_peak,
            sample_peak: true_peak,
            max_short_term: integrated,
        };
        let songs = [report(-20.0, -6.0), report(-12.0, -1.0)];
        let mut s = AlbumSettings::default();
        let whole = combined_loudness(&[(-20.0, 100.0), (-12.0, 100.0)]);
        assert!((whole - -14.37).abs() < 0.01, "{whole}");
        let g = gains(&s, &songs, whole);
        assert_eq!(g[0], g[1], "one gain for the album");
        assert!((whole + g[0] - -14.0).abs() < 1e-9);
        s.level = AlbumLevel::PerSong;
        let g = gains(&s, &songs, whole);
        assert_eq!(g, vec![6.0, -2.0]);
        // Less gain instead of limiting: the loud song's peak decides.
        s.limit = false;
        s.loudness = Some(-9.0);
        let g = gains(&s, &songs, whole);
        assert_eq!(g, vec![5.0, 0.0]);
        // No target: no gain.
        s.loudness = None;
        assert_eq!(gains(&s, &songs, whole), vec![0.0, 0.0]);
    }
}
