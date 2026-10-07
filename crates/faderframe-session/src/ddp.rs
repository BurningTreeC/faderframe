//! The DDP player: a CD master's fileset (from FaderFrame or elsewhere)
//! opened and checked as a plant would — checksums, PQ against the image,
//! the Red Book rules, CD-Text in every language — and played with its
//! track and index marks; and imported back as album songs.
//!
//! Opening runs on a worker: `ddp::inspect` (with `CHECKSUM.MD5`), the
//! image's byte order (`ddp::image_big_endian`), an overview of its peaks,
//! and the image converted to a float file at the engine's rate, which the
//! engine plays instead of the project (`engine::preview`, as album
//! playback does; the two never play at once). Importing writes each
//! track's samples unchanged into a 16-bit WAV in the media folder (a
//! pregap with more than dither noise in it stays with the track before) and adds the
//! tracks as songs — titles, credits, ISRC, pauses from the pregaps, the
//! UPC and every CD-Text language — in one step.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_audio_files::{StreamSource, WavFormat, wavstream::WavWriter};
use faderframe_core::SongId;
use faderframe_disc::{CD_RATE, Disc, Language, SECTOR_BYTES, SECTOR_FRAMES, ddp};
use faderframe_project::album::{Credits, Song, SongSource, SongText, Translation};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

#[derive(Clone, Debug, PartialEq)]
pub enum DdpAction {
    /// Open the fileset in a folder (closing the one open).
    Open(PathBuf),
    Close,
    /// Play from a track's start (`None`: from where it was).
    Play(Option<usize>),
    Pause,
    /// Stop: back to the project.
    Stop,
    /// The previous (−1) or next (+1) track.
    Skip(i32),
    /// To a time on the disc (seconds).
    Seek(f64),
    /// Add the disc's tracks to the album as songs.
    Import,
    Cancel,
}

/// An opened fileset, as the player shows it.
#[derive(Clone, Debug)]
pub struct DdpDisc {
    pub dir: PathBuf,
    pub disc: Disc,
    pub image: PathBuf,
    /// `CHECKSUM.MD5` matched (`None`: the fileset has none).
    pub checksums: Option<bool>,
    /// What breaks the Red Book or plant rules (empty: nothing).
    pub problems: Vec<String>,
    /// The image's samples are big-endian (found by their sound).
    pub big_endian: bool,
    /// Peak level (0..1) of each of [`OVERVIEW`] stretches of the disc.
    pub overview: Vec<f32>,
}

/// Columns of [`DdpDisc::overview`].
pub const OVERVIEW: usize = 2048;

impl DdpDisc {
    pub fn seconds(&self) -> f64 {
        f64::from(self.disc.sectors) / 75.0
    }

    /// Where track `i` starts (index 01) and how long it is, in seconds
    /// (to the next track's pregap or start, or the end).
    pub fn track_span(&self, i: usize) -> Option<(f64, f64)> {
        let t = self.disc.tracks.get(i)?;
        let start = t.start()?;
        let end = self
            .disc
            .tracks
            .get(i + 1)
            .and_then(|n| n.pregap.or(n.start()))
            .unwrap_or(self.disc.sectors);
        Some((
            f64::from(start) / 75.0,
            f64::from(end.saturating_sub(start)) / 75.0,
        ))
    }

    /// The track and index at `sector`, and the time in the track from
    /// its index 01 (negative in its pregap, as a CD player counts).
    pub fn locate(&self, sector: u32) -> Option<(usize, u8, f64)> {
        let tracks = &self.disc.tracks;
        let i = tracks
            .iter()
            .rposition(|t| t.pregap.or(t.start()).is_some_and(|from| from <= sector))?;
        let t = &tracks[i];
        let start = t.start()?;
        let index = if sector < start {
            0
        } else {
            t.indexes
                .iter()
                .rposition(|&x| x <= sector)
                .map_or(1, |k| k as u8 + 1)
        };
        Some((i, index, (f64::from(sector) - f64::from(start)) / 75.0))
    }
}

/// The player now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DdpPlayback {
    pub playing: bool,
    /// Seconds from the start of the disc.
    pub position: f64,
    pub track: Option<usize>,
    pub index: u8,
    /// Seconds from the track's index 01 (negative in its pregap).
    pub in_track: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdpTask {
    Open,
    Import,
}

struct Job {
    task: DdpTask,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<std::result::Result<Done, String>>>,
}

enum Done {
    Opened {
        disc: Box<DdpDisc>,
        preview: PathBuf,
        rate: u32,
    },
    /// Each track's file and pause (seconds before it).
    Imported(Vec<(PathBuf, f32)>),
}

#[derive(Default)]
pub(crate) struct DdpState {
    disc: Option<Arc<DdpDisc>>,
    job: Option<Job>,
    /// The converted image and its rate.
    preview: Option<(PathBuf, u32)>,
    playing: Option<Arc<StreamSource>>,
    /// Play once opened, from this track.
    play_after: Option<Option<usize>>,
    dir: Option<PathBuf>,
}

impl DdpState {
    fn dir(&mut self) -> PathBuf {
        self.dir
            .get_or_insert_with(|| {
                std::env::temp_dir().join(format!("faderframe-ddp-{}", std::process::id()))
            })
            .clone()
    }

    /// Remove the converted image (the session goes).
    pub(crate) fn discard(&mut self) {
        if let Some(d) = self.dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing.is_some()
    }
}

/// Seconds of frames per chunk read from the image.
const CHUNK_FRAMES: usize = 588 * 75;

/// Open `dir`: check it, look at it, convert it for playing at `rate`.
fn open(
    dir: &Path,
    out: &Path,
    rate: u32,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> std::result::Result<Done, String> {
    let f = ddp::inspect(dir).map_err(|e| e.to_string())?;
    let problems = f
        .disc
        .validate()
        .err()
        .map(|e| e.to_string())
        .into_iter()
        .collect();
    let big_endian = ddp::image_big_endian(&f.image).map_err(|e| e.to_string())?;
    let total = u64::from(f.disc.sectors) * SECTOR_FRAMES;
    std::fs::create_dir_all(out.parent().unwrap_or(out)).map_err(|e| e.to_string())?;
    let mut writer =
        WavWriter::create(out, 2, rate, WavFormat::Float32, false).map_err(|e| e.to_string())?;
    let mut resampler = (rate != CD_RATE)
        .then(|| faderframe_audio_files::resample::StreamResampler::new(CD_RATE, rate, 2))
        .transpose()
        .map_err(|e| e.0)?;
    let mut image =
        std::io::BufReader::new(std::fs::File::open(&f.image).map_err(|e| e.to_string())?);
    let mut overview = vec![0f32; OVERVIEW];
    let mut buf = vec![0u8; CHUNK_FRAMES * 4];
    let mut planes = [vec![0f32; CHUNK_FRAMES], vec![0f32; CHUNK_FRAMES]];
    let mut done = 0u64;
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = ((total - done) as usize).min(CHUNK_FRAMES);
        image
            .read_exact(&mut buf[..n * 4])
            .map_err(|e| e.to_string())?;
        for (i, b) in buf[..n * 4].as_chunks::<4>().0.iter().enumerate() {
            let (l, r) = if big_endian {
                (
                    i16::from_be_bytes([b[0], b[1]]),
                    i16::from_be_bytes([b[2], b[3]]),
                )
            } else {
                (
                    i16::from_le_bytes([b[0], b[1]]),
                    i16::from_le_bytes([b[2], b[3]]),
                )
            };
            let (l, r) = (f32::from(l) / 32768.0, f32::from(r) / 32768.0);
            planes[0][i] = l;
            planes[1][i] = r;
            let col = (((done + i as u64) * OVERVIEW as u64) / total.max(1)) as usize;
            if let Some(c) = overview.get_mut(col) {
                *c = c.max(l.abs()).max(r.abs());
            }
        }
        let parts = [&planes[0][..n], &planes[1][..n]];
        match &mut resampler {
            None => writer.write_planar(&parts, n).map_err(|e| e.to_string())?,
            Some(r) => {
                let input = [parts[0].to_vec(), parts[1].to_vec()];
                r.push(&input, n, &mut |p, k| {
                    writer.write_planar(p, k).map_err(Into::into)
                })
                .map_err(|e| e.to_string())?;
            }
        }
        done += n as u64;
        progress.store((done * 1000 / total.max(1)) as u32, Ordering::Relaxed);
    }
    if let Some(r) = resampler.take() {
        r.finish(&mut |p, k| writer.write_planar(p, k).map_err(Into::into))
            .map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(Done::Opened {
        disc: Box::new(DdpDisc {
            dir: dir.to_path_buf(),
            disc: f.disc,
            image: f.image,
            checksums: f.checksums,
            problems,
            big_endian,
            overview,
        }),
        preview: out.to_path_buf(),
        rate,
    })
}

/// A file name of `s` (no separators or odd characters).
fn file_name(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    clean.trim().chars().take(60).collect()
}

/// A 16-bit stereo WAV at 44.1 kHz of little-endian `samples`.
fn write_cd_wav(path: &Path, samples: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(44 + samples.len());
    let data = samples.len() as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&CD_RATE.to_le_bytes());
    out.extend_from_slice(&(CD_RATE * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(samples);
    std::fs::write(path, out)
}

/// Each track's samples into a WAV in `media`; returns each file and the
/// pause before it (its pregap, unless that holds sound: then it stays
/// with the track before).
fn import(
    disc: &DdpDisc,
    media: &Path,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> std::result::Result<Done, String> {
    let d = &disc.disc;
    let mut image = std::fs::File::open(&disc.image).map_err(|e| e.to_string())?;
    let read = |image: &mut std::fs::File, from: u32, to: u32| -> std::io::Result<Vec<u8>> {
        let mut b = vec![0u8; (to.saturating_sub(from)) as usize * SECTOR_BYTES];
        image.seek(SeekFrom::Start(u64::from(from) * SECTOR_BYTES as u64))?;
        image.read_exact(&mut b)?;
        if disc.big_endian {
            for s in b.as_chunks_mut::<2>().0 {
                s.swap(0, 1);
            }
        }
        Ok(b)
    };
    // Silent: nothing above dither noise (8 LSB, −72 dBFS).
    let silent = |b: &[u8]| {
        b.as_chunks::<2>()
            .0
            .iter()
            .all(|s| i16::from_le_bytes(*s).unsigned_abs() <= 8)
    };
    let stem = file_name(if d.master_id.is_empty() {
        &d.text.title
    } else {
        &d.master_id
    });
    let stem = if stem.is_empty() {
        "DDP".to_string()
    } else {
        stem
    };
    let mut out = Vec::new();
    let n = d.tracks.len();
    for (i, t) in d.tracks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let Some(start) = t.start() else {
            continue;
        };
        let next = d.tracks.get(i + 1);
        let next_start = next.and_then(|n| n.start()).unwrap_or(d.sectors);
        let next_gap = next.and_then(|n| n.pregap);
        // The next track's pregap: a pause when silent, else this track's.
        let end = match next_gap {
            Some(g) if silent(&read(&mut image, g, next_start).map_err(|e| e.to_string())?) => g,
            _ => next_start,
        };
        let pause = match t.pregap {
            Some(g) if i > 0 => {
                let gap = read(&mut image, g, start).map_err(|e| e.to_string())?;
                if silent(&gap) {
                    (start - g) as f32 / 75.0
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        let samples = read(&mut image, start, end).map_err(|e| e.to_string())?;
        let title = file_name(&t.text.title);
        let name = if title.is_empty() {
            format!("{stem} {:02}.wav", i + 1)
        } else {
            format!("{stem} {:02} {title}.wav", i + 1)
        };
        let path = media.join(name);
        write_cd_wav(&path, &samples).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push((path, pause));
        progress.store(((i + 1) * 1000 / n.max(1)) as u32, Ordering::Relaxed);
    }
    Ok(Done::Imported(out))
}

impl Session {
    /// The opened DDP fileset.
    pub fn ddp(&self) -> Option<&DdpDisc> {
        self.ddp_state.disc.as_deref()
    }

    /// What the DDP worker does and how far it got (0..1).
    pub fn ddp_progress(&self) -> Option<(DdpTask, f32)> {
        self.ddp_state
            .job
            .as_ref()
            .map(|j| (j.task, j.progress.load(Ordering::Relaxed) as f32 / 1000.0))
    }

    /// The player now (`None`: not playing the disc).
    pub fn ddp_playback(&self) -> Option<DdpPlayback> {
        self.ddp_state.playing.as_ref()?;
        let (_, rate) = self.ddp_state.preview.as_ref()?;
        let disc = self.ddp_state.disc.as_ref()?;
        let shared = self.engine.preview();
        let position = shared.position().max(0) as f64 / f64::from((*rate).max(1));
        let sector = (position * 75.0) as u32;
        let (track, index, in_track) = disc
            .locate(sector)
            .map_or((None, 1, 0.0), |(t, i, s)| (Some(t), i, s));
        Some(DdpPlayback {
            playing: shared.is_playing(),
            position,
            track,
            index,
            in_track,
        })
    }

    pub(crate) fn ddp_action(&mut self, a: DdpAction) -> Result<()> {
        match a {
            DdpAction::Open(dir) => {
                if self.ddp_state.job.is_some() {
                    return Err(SessionError::Other("a DDP is being opened already".into()));
                }
                self.ddp_stop();
                self.ddp_state.disc = None;
                self.ddp_state.preview = None;
                let rate = self.engine.sample_rate();
                let out = self.ddp_state.dir().join("disc.wav");
                let progress = Arc::new(AtomicU32::new(0));
                let cancel = Arc::new(AtomicBool::new(false));
                let (p, c) = (Arc::clone(&progress), Arc::clone(&cancel));
                let handle = std::thread::Builder::new()
                    .name("ff-ddp".into())
                    .spawn(move || open(&dir, &out, rate, &p, &c))
                    .map_err(|e| SessionError::Other(e.to_string()))?;
                self.ddp_state.job = Some(Job {
                    task: DdpTask::Open,
                    progress,
                    cancel,
                    handle: Some(handle),
                });
            }
            DdpAction::Close => {
                self.ddp_stop();
                self.ddp_state.disc = None;
                self.ddp_state.preview = None;
            }
            DdpAction::Play(from) => self.ddp_play(from)?,
            DdpAction::Pause => {
                if self.ddp_state.playing.is_some() {
                    self.engine.preview().play(false);
                }
            }
            DdpAction::Stop => self.ddp_stop(),
            DdpAction::Skip(delta) => {
                let Some(p) = self.ddp_playback() else {
                    return Ok(());
                };
                let Some(disc) = self.ddp_state.disc.clone() else {
                    return Ok(());
                };
                let now = p.track.unwrap_or(0) as i64;
                let target = if delta < 0 && p.in_track > 3.0 {
                    now
                } else {
                    now + i64::from(delta)
                };
                let target = target.clamp(0, disc.disc.tracks.len() as i64 - 1) as usize;
                if let Some((start, _)) = disc.track_span(target) {
                    self.ddp_seek(start);
                }
            }
            DdpAction::Seek(s) => self.ddp_seek(s),
            DdpAction::Import => self.ddp_import()?,
            DdpAction::Cancel => {
                if let Some(j) = &self.ddp_state.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn ddp_seek(&mut self, seconds: f64) {
        if let Some((_, rate)) = &self.ddp_state.preview {
            self.engine
                .preview()
                .locate((seconds.max(0.0) * f64::from(*rate)) as i64);
        }
    }

    fn ddp_play(&mut self, from: Option<usize>) -> Result<()> {
        if self
            .ddp_state
            .job
            .as_ref()
            .is_some_and(|j| j.task == DdpTask::Open)
        {
            self.ddp_state.play_after = Some(from);
            return Ok(());
        }
        let Some((path, _)) = self.ddp_state.preview.clone() else {
            return Ok(());
        };
        if self.ddp_state.playing.is_none() {
            let source = StreamSource::open(&path)
                .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
            // The project and the album stop: the disc plays instead.
            self.album_stop_playing();
            if self.transport.playing {
                self.dispatch(crate::Action::Transport(crate::TransportAction::Stop))?;
            }
            self.engine.set_preview(Some(Arc::clone(&source)))?;
            self.loader.set_preview(Some(Arc::clone(&source)));
            self.ddp_state.playing = Some(source);
        }
        if let Some(i) = from
            && let Some((start, _)) = self.ddp_state.disc.as_ref().and_then(|d| d.track_span(i))
        {
            self.ddp_seek(start);
        }
        self.engine.preview().play(true);
        Ok(())
    }

    /// Back to the project (the disc stays open).
    pub(crate) fn ddp_stop(&mut self) {
        if self.ddp_state.playing.take().is_some() {
            let _ = self.engine.set_preview(None);
            self.loader.set_preview(None);
            self.revision += 1;
        }
    }

    fn ddp_import(&mut self) -> Result<()> {
        if self.ddp_state.job.is_some() {
            return Err(SessionError::Other("the DDP worker is busy".into()));
        }
        let Some(disc) = self.ddp_state.disc.clone() else {
            return Err(SessionError::Other("no DDP is open".into()));
        };
        let media = self.media_dir().to_path_buf();
        std::fs::create_dir_all(&media).map_err(|e| SessionError::Other(e.to_string()))?;
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (Arc::clone(&progress), Arc::clone(&cancel));
        let handle = std::thread::Builder::new()
            .name("ff-ddp-import".into())
            .spawn(move || import(&disc, &media, &p, &c))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.ddp_state.job = Some(Job {
            task: DdpTask::Import,
            progress,
            cancel,
            handle: Some(handle),
        });
        Ok(())
    }

    /// Wait for the DDP worker (tests).
    pub fn wait_ddp(&mut self) {
        while self
            .ddp_state
            .job
            .as_ref()
            .is_some_and(|j| j.handle.as_ref().is_some_and(|h| !h.is_finished()))
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.poll_ddp();
    }

    /// Collect a finished DDP job (from the tick).
    pub(crate) fn poll_ddp(&mut self) {
        let Some(job) = self.ddp_state.job.as_mut() else {
            return;
        };
        if !job.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            self.revision += 1;
            return;
        }
        let outcome = job.handle.take().map(JoinHandle::join);
        self.ddp_state.job = None;
        self.revision += 1;
        match outcome {
            Some(Ok(Ok(Done::Opened {
                disc,
                preview,
                rate,
            }))) => {
                let problems = disc.problems.join("; ");
                let checks = match disc.checksums {
                    Some(false) => " — its checksums do not match",
                    _ => "",
                };
                if !problems.is_empty() || !checks.is_empty() {
                    self.notify(
                        NoticeLevel::Warning,
                        format!(
                            "The DDP opens with problems{checks}{}{problems}",
                            if problems.is_empty() { "" } else { ": " }
                        ),
                    );
                }
                self.ddp_state.disc = Some(Arc::from(disc));
                self.ddp_state.preview = Some((preview, rate));
                if let Some(from) = self.ddp_state.play_after.take()
                    && let Err(e) = self.ddp_play(from)
                {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
            }
            Some(Ok(Ok(Done::Imported(files)))) => {
                if let Err(e) = self.ddp_add_songs(files) {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
            }
            Some(Ok(Err(e))) if e == "cancelled" => {}
            Some(Ok(Err(e))) => self.notify(NoticeLevel::Error, format!("DDP: {e}")),
            _ => self.notify(NoticeLevel::Error, "the DDP worker failed"),
        }
    }

    /// The imported tracks as songs, with the disc's text and codes, in
    /// one step.
    fn ddp_add_songs(&mut self, files: Vec<(PathBuf, f32)>) -> Result<()> {
        let Some(disc) = self.ddp_state.disc.clone() else {
            return Ok(());
        };
        let d = &disc.disc;
        let mut album = self.project.album.clone();
        let first_new = album.songs.is_empty();
        let mut ids: Vec<SongId> = Vec::new();
        for ((path, pause), t) in files.into_iter().zip(&d.tracks) {
            let id: SongId = self.project.ids.allocate();
            let title = if t.text.title.is_empty() {
                path.file_stem()
                    .map_or_else(|| "Track".to_string(), |s| s.to_string_lossy().to_string())
            } else {
                t.text.title.clone()
            };
            let mut song = Song::new(id, title, SongSource::AudioFile(path));
            song.pause = pause;
            song.isrc = t.isrc.clone().unwrap_or_default();
            song.credits = credits(&t.text);
            album.songs.push(song);
            ids.push(id);
        }
        // The release's text and codes when the album had none.
        if first_new {
            album.info.title.clone_from(&d.text.title);
            album.info.credits = credits(&d.text);
            album.info.upc = d.upc.clone().unwrap_or_default();
            album.info.language = d.text_language.0;
        }
        for block in &d.more_text {
            // In the album's own language: its songs' text is the main.
            if block.language == Language(album.info.language) {
                continue;
            }
            let tr: &mut Translation = album.info.translation_mut(block.language.0);
            if first_new {
                tr.title.clone_from(&block.disc.title);
                tr.credits = credits(&block.disc);
            }
            for (id, text) in ids.iter().zip(&block.tracks) {
                let s: &mut SongText = tr.song_mut(*id);
                s.title.clone_from(&text.title);
                s.credits = credits(text);
            }
        }
        self.edit(faderframe_project::Command::SetAlbum {
            album: Box::new(album),
        })
    }
}

fn credits(t: &faderframe_disc::CdText) -> Credits {
    Credits {
        performer: t.performer.clone(),
        songwriter: t.songwriter.clone(),
        composer: t.composer.clone(),
        arranger: t.arranger.clone(),
        message: t.message.clone(),
    }
}
