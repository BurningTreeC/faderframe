//! Capture Audio: "I played the best take while not recording."
//!
//! While audio runs, the inputs of the armed tracks (by the settings, also
//! those monitoring their input, or every audio track with an input) are
//! kept for a few minutes, recording or not: the engine copies the device
//! channels they use into rings exactly where the recorder would
//! ([`faderframe_engine::input_capture`]), and a keeper thread moves them
//! into a rolling store in memory ([`store`]: per channel, chunks of
//! samples and a level mark per 256 frames; the oldest chunk goes when a
//! channel holds its share of the length and of the memory limit, whichever
//! is less). Memory, not disk: nothing is written while nobody captures
//! (no disk traffic beside streaming, no wear), and the limit bounds it —
//! 48 kHz float is 11.5 MB per channel and minute, so the default five
//! minutes of a stereo track hold 115 MB and the default limit of 1 GB
//! some 89 channel-minutes (fewer minutes per track as more are kept).
//!
//! "Capture Audio" (Ctrl+Alt+C) takes the newest phrase ([`phrase`]): played
//! while playing, the run back to the transport start, its passes placed
//! where they were heard — the same frames at the same positions as a
//! recording of that run, compensated by the same latency
//! ([`Session::audio_take_latency`], as it was then); played while
//! stopped, the phrase back to a pause, its first sound on the bar at or
//! before the playhead (as Capture MIDI). Files are written on a thread
//! from the store's chunks (shared, not copied); the takes are placed as
//! recording places them (record mode, loop recording mode, take folders
//! over existing clips) in one "Capture Audio" step. What a recording took
//! is not offered again; what an aborted recording played is.

mod keeper;
pub mod phrase;
pub mod store;

pub use keeper::TRANSFER_SECONDS;

use crate::record::{NewTake, place_passes};
use crate::{NoticeLevel, Result, SelectMode, Session, SessionError, media};
use faderframe_audio_files::import::peaks_path_for;
use faderframe_audio_files::wavstream::WavWriter;
use faderframe_audio_files::{PeakBuilder, PeakCache, WavFormat};
use faderframe_core::TrackId;
use faderframe_engine::Source;
use faderframe_project::{AudioSource, Command, InputRouting, MonitorMode, SourceSpec, TrackKind};
use phrase::{Kept, Taken};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use store::{Piece, Store};

/// Which tracks' inputs are kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTracks {
    /// Armed tracks.
    Armed,
    /// Armed tracks and those monitoring their input.
    #[default]
    Monitored,
    /// Every audio track with a hardware input.
    All,
}

impl CaptureTracks {
    pub const ALL: [CaptureTracks; 3] = [
        CaptureTracks::Armed,
        CaptureTracks::Monitored,
        CaptureTracks::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CaptureTracks::Armed => "Armed tracks",
            CaptureTracks::Monitored => "Armed and input-monitored tracks",
            CaptureTracks::All => "Every audio track with an input",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            CaptureTracks::Armed => "armed",
            CaptureTracks::Monitored => "monitored",
            CaptureTracks::All => "all",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// Shortest and longest a track's input is kept (seconds).
pub const MIN_SECONDS: u32 = 10;
pub const MAX_SECONDS: u32 = 600;
/// Bounds of the memory limit (megabytes).
pub const MIN_MEGABYTES: u32 = 1;
pub const MAX_MEGABYTES: u32 = 65_536;

/// How Capture Audio keeps the inputs (`Preferences::audio_capture`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioCaptureSettings {
    /// Keep the inputs at all.
    pub on: bool,
    /// How long each track's input is kept (seconds).
    pub seconds: u32,
    /// All kept input together at most this much memory (megabytes).
    pub max_megabytes: u32,
    pub tracks: CaptureTracks,
}

impl Default for AudioCaptureSettings {
    fn default() -> Self {
        Self {
            on: true,
            seconds: 300,
            max_megabytes: 1024,
            tracks: CaptureTracks::default(),
        }
    }
}

impl AudioCaptureSettings {
    /// Within the bounds.
    pub fn clamped(self) -> Self {
        Self {
            seconds: self.seconds.clamp(MIN_SECONDS, MAX_SECONDS),
            max_megabytes: self.max_megabytes.clamp(MIN_MEGABYTES, MAX_MEGABYTES),
            ..self
        }
    }

    pub fn max_bytes(&self) -> usize {
        self.max_megabytes as usize * 1024 * 1024
    }
}

/// What is kept now (the Preferences page, scripts, tests).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioCaptureStatus {
    pub on: bool,
    /// The tracks whose input is kept.
    pub tracks: Vec<TrackId>,
    /// Device channels kept.
    pub channels: usize,
    /// Seconds the longest kept channel holds.
    pub seconds: f64,
    /// Memory held by the kept samples.
    pub bytes: usize,
    pub limit_bytes: usize,
    /// Frames the keeper fell behind on (kept as silence).
    pub lost_frames: u64,
    /// Captures still being written.
    pub writing: usize,
}

/// One take placed from a captured file: where (engine samples) and
/// which of its frames.
#[derive(Clone, Copy, Debug)]
struct Placement {
    abs_start: i64,
    file_offset: i64,
    frames: i64,
}

/// A track's file of a capture.
struct Planned {
    track: TrackId,
    path: PathBuf,
    channels: usize,
    placements: Vec<Placement>,
}

/// What a capture's writer made of one track.
struct Written {
    frames: u64,
    peaks: PeakCache,
}

/// A capture being written.
struct CaptureJob {
    plans: Vec<Planned>,
    rate: u32,
    handle: Option<JoinHandle<Vec<std::io::Result<Written>>>>,
}

/// The session's side: settings, the keeper and its store, what the
/// engine keeps now, what was taken.
#[derive(Default)]
pub(crate) struct AudioCaptureState {
    settings: AudioCaptureSettings,
    keeper: Option<keeper::Keeper>,
    /// The rate the keeper's store is at.
    rate: u32,
    /// Device channels the engine keeps now.
    channels: Vec<u16>,
    /// The latency in effect from a store frame on (frames).
    latency: Vec<(u64, u64)>,
    /// Input before this store frame was captured or is not offered.
    taken: u64,
    /// About where the recording under way began (store frames).
    recording_from: u64,
    /// Something to capture (refreshed four times a second).
    can: bool,
    checked: Option<Instant>,
    jobs: Vec<CaptureJob>,
}

impl AudioCaptureState {
    fn latency_at(&self, frame: u64, now: u64) -> u64 {
        let i = self.latency.partition_point(|(f, _)| *f <= frame);
        match i.checked_sub(1) {
            Some(i) => self.latency[i].1,
            None => self.latency.first().map_or(now, |p| p.1),
        }
    }
}

/// Reads a channel's pieces in order.
struct Reader<'a> {
    pieces: &'a [Piece],
    i: usize,
    at: usize,
}

impl Reader<'_> {
    fn fill(&mut self, out: &mut [f32]) {
        let mut o = 0;
        while o < out.len() {
            let Some(p) = self.pieces.get(self.i) else {
                out[o..].fill(0.0);
                return;
            };
            let k = (p.len() - self.at).min(out.len() - o);
            match p {
                Piece::Shared(c, r) => {
                    let a = r.start + self.at;
                    out[o..o + k].copy_from_slice(&c[a..a + k]);
                }
                Piece::Owned(v) => out[o..o + k].copy_from_slice(&v[self.at..self.at + k]),
                Piece::Silence(_) => out[o..o + k].fill(0.0),
            }
            o += k;
            self.at += k;
            if self.at >= p.len() {
                self.i += 1;
                self.at = 0;
            }
        }
    }
}

/// Write one track's capture: float WAV and its peaks.
fn write_file(path: &Path, channels: &[Vec<Piece>], rate: u32) -> std::io::Result<Written> {
    const BLOCK: usize = 8192;
    let n = channels.len().max(1);
    let total = channels
        .first()
        .map_or(0, |c| c.iter().map(Piece::len).sum());
    let mut writer = WavWriter::create(path, n as u16, rate, WavFormat::Float32, false)?;
    let mut peaks = PeakBuilder::new(n);
    let mut readers: Vec<Reader> = channels
        .iter()
        .map(|p| Reader {
            pieces: p,
            i: 0,
            at: 0,
        })
        .collect();
    let mut bufs = vec![vec![0.0f32; BLOCK]; n];
    let mut done = 0;
    while done < total {
        let k = BLOCK.min(total - done);
        for (r, b) in readers.iter_mut().zip(&mut bufs) {
            r.fill(&mut b[..k]);
        }
        let planes: Vec<&[f32]> = bufs.iter().map(|b| &b[..k]).collect();
        writer.write_planar(&planes, k)?;
        peaks.push(&planes, k);
        done += k;
    }
    let frames = writer.frames();
    let path = writer.finish()?;
    let peaks = peaks.finish();
    let _ = peaks.save(&peaks_path_for(&path));
    Ok(Written { frames, peaks })
}

impl Session {
    /// How Capture Audio keeps the inputs.
    pub fn audio_capture_settings(&self) -> AudioCaptureSettings {
        self.audio_capture.settings
    }

    /// Keep the inputs this way from now on (switched off: what was kept
    /// goes).
    pub fn set_audio_capture(&mut self, settings: AudioCaptureSettings) {
        let s = settings.clamped();
        if s == self.audio_capture.settings {
            return;
        }
        self.audio_capture.settings = s;
        if let Some(k) = &self.audio_capture.keeper {
            k.store().set_limits(f64::from(s.seconds), s.max_bytes());
        }
        self.audio_capture.checked = None;
        self.tick_audio_capture();
        self.revision += 1;
    }

    /// The tracks whose input is kept, with their device channels (those
    /// the stream has).
    fn kept_tracks(&self) -> Vec<Kept> {
        let inputs = self.engine.stream_inputs();
        let which = self.audio_capture.settings.tracks;
        self.project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .filter(|t| match which {
                CaptureTracks::Armed => t.record_arm,
                CaptureTracks::Monitored => t.record_arm || t.monitor == MonitorMode::Input,
                CaptureTracks::All => true,
            })
            .filter_map(|t| match t.input {
                InputRouting::Hardware { first_channel } => {
                    let n = t.layout.channel_count() as u16;
                    let channels: Vec<u16> = (first_channel..first_channel.saturating_add(n))
                        .filter(|c| usize::from(*c) < inputs)
                        .collect();
                    (!channels.is_empty()).then_some(Kept {
                        track: t.id,
                        channels,
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// What is kept now.
    pub fn audio_capture_status(&self) -> AudioCaptureStatus {
        let s = &self.audio_capture;
        let mut status = AudioCaptureStatus {
            on: s.settings.on,
            limit_bytes: s.settings.max_bytes(),
            writing: s.jobs.len(),
            ..Default::default()
        };
        if s.channels.is_empty() {
            return status;
        }
        status.tracks = self.kept_tracks().into_iter().map(|k| k.track).collect();
        if let Some(k) = &s.keeper {
            let store = k.store();
            status.channels = store.channels().count();
            status.seconds = store.seconds_kept();
            status.bytes = store.bytes();
            status.lost_frames = store.lost();
        }
        status
    }

    /// Something played to capture (and nothing recording)? Cheap: looked
    /// for four times a second by the tick.
    pub fn can_capture_audio(&self) -> bool {
        self.audio_capture.can && self.recording.is_none()
    }

    /// Captures still being written.
    pub fn capturing_audio(&self) -> bool {
        !self.audio_capture.jobs.is_empty()
    }

    /// Wait until every capture is on the timeline (tests, scripting).
    pub fn wait_for_audio_captures(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.capturing_audio() && Instant::now() < deadline {
            self.poll_audio_captures();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Keep what the settings ask for (from the tick): start, change or
    /// end the engine's capture, note the latency, look for something to
    /// capture, place finished captures.
    pub(crate) fn tick_audio_capture(&mut self) {
        self.poll_audio_captures();
        let settings = self.audio_capture.settings;
        let rate = self.engine.sample_rate();
        if !settings.on || self.audio_capture.keeper.is_some() && self.audio_capture.rate != rate {
            // Off, or another rate: what is kept goes.
            self.stop_audio_capture();
            if !settings.on {
                return;
            }
        }
        let running = self.audio.is_some() && self.engine.stream_rate() > 0;
        let mut wanted: Vec<u16> = if running {
            self.kept_tracks()
                .into_iter()
                .flat_map(|k| k.channels)
                .collect()
        } else {
            Vec::new()
        };
        wanted.sort_unstable();
        wanted.dedup();
        let cap = &mut self.audio_capture;
        // The engine let go without being asked (a new engine): ask the
        // new one.
        if cap.keeper.as_ref().is_some_and(keeper::Keeper::orphaned) {
            cap.channels.clear();
        }
        if wanted != cap.channels {
            if wanted.is_empty() {
                if let Err(e) = self.engine.end_input_capture() {
                    tracing::warn!("capture audio: {e}");
                    return;
                }
                let cap = &mut self.audio_capture;
                if let Some(k) = &cap.keeper {
                    k.store().set_channels(&[]);
                }
                cap.channels.clear();
            } else {
                if cap.keeper.is_none() {
                    let store = Store::new(rate, f64::from(settings.seconds), settings.max_bytes());
                    match keeper::Keeper::start(store) {
                        Ok(k) => {
                            cap.keeper = Some(k);
                            cap.rate = rate;
                        }
                        Err(e) => {
                            tracing::warn!("capture audio: cannot start its thread: {e}");
                            return;
                        }
                    }
                }
                match self.engine.capture_inputs(wanted.clone(), TRANSFER_SECONDS) {
                    Ok(streams) => {
                        let cap = &mut self.audio_capture;
                        if let Some(k) = cap.keeper.as_mut() {
                            k.feed(streams);
                        }
                        cap.channels = wanted;
                    }
                    // A full command queue: again with the next tick.
                    Err(e) => tracing::warn!("capture audio: {e}"),
                }
            }
        }
        // The latency takes are moved back by, as it changes.
        if running
            && self.audio_capture.keeper.is_some()
            && let Some(info) = self.stream_info()
        {
            let latency = self.audio_take_latency(&info).max(0) as u64;
            let cap = &mut self.audio_capture;
            if let Some(k) = &cap.keeper {
                let (newest, oldest) = {
                    let s = k.store();
                    (s.newest(), s.oldest())
                };
                if cap.latency.last().map(|p| p.1) != Some(latency) {
                    cap.latency.push((newest, latency));
                }
                while cap.latency.len() > 1 && cap.latency[1].0 <= oldest {
                    cap.latency.remove(0);
                }
            }
        }
        if self
            .audio_capture
            .checked
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.audio_capture.checked = Some(Instant::now());
            self.audio_capture.can = self.recording.is_none() && self.find_audio_phrase().is_some();
        }
    }

    /// No more capturing: the engine stops and what was kept goes.
    fn stop_audio_capture(&mut self) {
        if !self.audio_capture.channels.is_empty()
            && let Err(e) = self.engine.end_input_capture()
        {
            tracing::warn!("capture audio: {e}");
        }
        let cap = &mut self.audio_capture;
        cap.channels.clear();
        cap.keeper = None;
        cap.latency.clear();
        cap.taken = 0;
        cap.can = false;
    }

    /// The latency in effect at the newest frame kept (0 without audio).
    fn audio_latency_now(&self) -> u64 {
        self.stream_info()
            .map_or(0, |info| self.audio_take_latency(&info).max(0) as u64)
    }

    fn find_audio_phrase(&self) -> Option<(phrase::Phrase, u32)> {
        let cap = &self.audio_capture;
        let keeper = cap.keeper.as_ref()?;
        if cap.channels.is_empty() {
            return None;
        }
        let kept = self.kept_tracks();
        let now = self.audio_latency_now();
        let store = keeper.store();
        let found = phrase::find(&store, &kept, cap.taken, |f| cap.latency_at(f, now))?;
        Some((found, store.rate))
    }

    /// Recording begins: what it takes is not offered again.
    pub(crate) fn audio_capture_recording_starts(&mut self) {
        if let Some(k) = &self.audio_capture.keeper {
            self.audio_capture.recording_from = k.store().recording_starts();
        }
    }

    /// The take was thrown away (Abort Recording): what was played is
    /// offered again.
    pub(crate) fn audio_capture_recording_discarded(&mut self) {
        let cap = &mut self.audio_capture;
        if let Some(k) = &cap.keeper {
            k.store().forgive_recording(cap.recording_from);
        }
        cap.checked = None;
    }

    /// A project replaced: what was kept belongs to the old one, and so
    /// do captures being written (their files stay).
    pub(crate) fn audio_capture_project_replaced(&mut self) {
        let cap = &mut self.audio_capture;
        cap.jobs.clear();
        if let Some(k) = &cap.keeper {
            cap.taken = k.store().newest();
        }
        cap.can = false;
    }

    /// Turn what the kept tracks played last into takes (see the module
    /// docs).
    pub(crate) fn capture_audio(&mut self) -> Result<()> {
        if self.recording.is_some() {
            return Err(SessionError::Other(
                "recording keeps what is played already".into(),
            ));
        }
        if !self.audio_capture.settings.on {
            self.notify(
                NoticeLevel::Info,
                "Capture Audio keeps nothing: switch it on (Transport → Keep Input for Capture Audio)",
            );
            return Ok(());
        }
        let kept = self.kept_tracks();
        let Some((found, rate)) = self.find_audio_phrase() else {
            let text = if kept.is_empty() {
                "nothing played to capture: arm an audio track with an input (or monitor its input) and play"
            } else {
                "nothing played to capture"
            };
            self.notify(NoticeLevel::Info, text);
            return Ok(());
        };
        // A frozen track takes no clips (as recording).
        let mut tracks = found.tracks.clone();
        let frozen: Vec<String> = tracks
            .iter()
            .filter(|t| self.is_frozen(**t))
            .filter_map(|t| self.project.track(*t).map(|t| t.name.clone()))
            .collect();
        tracks.retain(|t| !self.is_frozen(*t));
        if !frozen.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "{} {} frozen and not captured: unfreeze to capture",
                    frozen.join(", "),
                    if frozen.len() == 1 { "is" } else { "are" }
                ),
            );
        }
        if tracks.is_empty() {
            return Ok(());
        }
        // The frames each file holds and where its takes go.
        let (from, to, placements) = match &found.taken {
            Taken::Stopped { start, onset, end } => {
                // The first sound on the bar at or before the playhead.
                let timeline = &self.project.timeline;
                let bar = timeline
                    .meter
                    .bar_start(timeline.meter.bar_at(self.playhead()));
                let mut base = self.engine.musical_to_samples(&self.project, bar);
                while self.engine.samples_to_musical(&self.project, base) < bar {
                    base += 1;
                }
                let place = Placement {
                    abs_start: base - (onset - start) as i64,
                    file_offset: 0,
                    frames: (end - start) as i64,
                };
                (*start, *end, vec![place])
            }
            Taken::Playing { pieces, latency } => {
                let from = pieces.iter().map(|p| p.from + latency).min().unwrap_or(0);
                let to = pieces
                    .iter()
                    .map(|p| p.from + p.frames + latency)
                    .max()
                    .unwrap_or(from);
                let placements = pieces
                    .iter()
                    .map(|p| Placement {
                        abs_start: p.position,
                        file_offset: (p.from + latency - from) as i64,
                        frames: p.frames as i64,
                    })
                    .collect();
                (from, to, placements)
            }
        };
        // Before the project's start: cut.
        let placements: Vec<Placement> = placements
            .into_iter()
            .filter_map(|mut p| {
                if p.abs_start < 0 {
                    p.file_offset -= p.abs_start;
                    p.frames += p.abs_start;
                    p.abs_start = 0;
                }
                (p.frames > 0).then_some(p)
            })
            .collect();
        if placements.is_empty() {
            self.notify(NoticeLevel::Info, "nothing played to capture");
            return Ok(());
        }
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        // Each track's channels from the store (shared chunks).
        let mut plans = Vec::new();
        let mut data: Vec<(PathBuf, Vec<Vec<Piece>>)> = Vec::new();
        let info = self.stream_info();
        let devices = self.stream_devices();
        {
            let Some(keeper) = self.audio_capture.keeper.as_ref() else {
                return Ok(());
            };
            let store = keeper.store();
            for t in &tracks {
                let Some(track) = self.project.track(*t) else {
                    continue;
                };
                let InputRouting::Hardware { first_channel } = track.input else {
                    continue;
                };
                let n = track.layout.channel_count() as u16;
                let len = (to - from) as usize;
                // Inputs of a device following another's clock arrive later
                // (its ring and resampler): read them that much later, as
                // recording moves their takes back (`ActiveRecording::later`).
                let later = info.as_ref().map_or(0, |info| {
                    (first_channel..first_channel.saturating_add(n.max(1)))
                        .map(|c| faderframe_audio::channel_input_latency(&devices, info, c))
                        .max()
                        .map_or(0, |most| most.saturating_sub(info.input_latency))
                });
                let (from, to) = (from + u64::from(later), to + u64::from(later));
                let channels: Vec<Vec<Piece>> = (first_channel..first_channel.saturating_add(n))
                    .map(|ch| {
                        let mut pieces = store
                            .channel(ch)
                            .map(|c| c.extract(from, to))
                            .unwrap_or_default();
                        let got: usize = pieces.iter().map(Piece::len).sum();
                        if got < len {
                            pieces.push(Piece::Silence(len - got));
                        }
                        pieces
                    })
                    .collect();
                let name = &track.name;
                let n = 1
                    + self
                        .project
                        .sources
                        .values()
                        .filter(|s| s.name.starts_with(&format!("{name} Capture")))
                        .count()
                    + self.audio_capture.jobs.len();
                let mut path = media::unique_path(&self.media_dir, &format!("{name} Capture {n}"));
                let mut k = 2;
                while data.iter().any(|(p, _)| *p == path) {
                    path = media::unique_path(&self.media_dir, &format!("{name} Capture {n}-{k}"));
                    k += 1;
                }
                plans.push(Planned {
                    track: *t,
                    path: path.clone(),
                    channels: channels.len(),
                    placements: placements.clone(),
                });
                data.push((path, channels));
            }
        }
        if plans.is_empty() {
            return Ok(());
        }
        let handle = std::thread::Builder::new()
            .name("faderframe-capture-write".into())
            .spawn(move || {
                data.iter()
                    .map(|(path, channels)| write_file(path, channels, rate))
                    .collect()
            })
            .map_err(|e| SessionError::Other(format!("cannot write the capture: {e}")))?;
        let cap = &mut self.audio_capture;
        cap.jobs.push(CaptureJob {
            plans,
            rate,
            handle: Some(handle),
        });
        // Taken: not offered again.
        cap.taken = found.until;
        cap.can = false;
        cap.checked = Some(Instant::now());
        Ok(())
    }

    /// Place the captures whose files are written.
    fn poll_audio_captures(&mut self) {
        let mut i = 0;
        while i < self.audio_capture.jobs.len() {
            let done = self.audio_capture.jobs[i]
                .handle
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            if !done {
                i += 1;
                continue;
            }
            let mut job = self.audio_capture.jobs.remove(i);
            let written = match job.handle.take().map(JoinHandle::join) {
                Some(Ok(w)) => w,
                _ => {
                    self.notify(NoticeLevel::Error, "capture audio: the writer failed");
                    continue;
                }
            };
            if let Err(e) = self.place_audio_capture(job, written) {
                self.notify(NoticeLevel::Error, format!("capture audio: {e}"));
            }
        }
    }

    fn place_audio_capture(
        &mut self,
        job: CaptureJob,
        written: Vec<std::io::Result<Written>>,
    ) -> Result<()> {
        let mut commands = Vec::new();
        let mut placed = Vec::new();
        let mut opened = Vec::new();
        let mut pass_tracks = Vec::new();
        let mut longest = 0i64;
        let mut tracks = 0;
        let (mode, loop_mode) = (self.record.mode, self.record.loop_mode);
        for (plan, w) in job.plans.into_iter().zip(written) {
            let w = match w {
                Ok(w) => w,
                Err(e) => {
                    self.notify(
                        NoticeLevel::Error,
                        format!("capture audio: {}: {e}", plan.path.display()),
                    );
                    continue;
                }
            };
            let p = &mut self.project;
            let Some(track_name) = p.track(plan.track).map(|t| t.name.clone()) else {
                continue; // the track went meanwhile; the file stays
            };
            let rate = f64::from(job.rate.max(1));
            let project_rate = f64::from(p.sample_rate);
            let to_project = |f: i64| (f as f64 * project_rate / rate).round() as i64;
            let file_name = plan.path.file_stem().map_or_else(
                || format!("{track_name} Capture"),
                |s| s.to_string_lossy().to_string(),
            );
            let source = AudioSource {
                id: p.ids.allocate(),
                name: file_name.clone(),
                spec: SourceSpec::File {
                    path: plan.path.clone(),
                    channels: plan.channels as u16,
                    frames: w.frames as i64,
                    sample_rate: job.rate,
                },
            };
            let passes = plan.placements.len();
            let new: Vec<NewTake> = plan
                .placements
                .iter()
                .enumerate()
                .filter(|(_, pl)| pl.file_offset + pl.frames <= w.frames as i64)
                .map(|(k, pl)| {
                    let abs_start = to_project(pl.abs_start);
                    NewTake {
                        name: if passes > 1 {
                            format!("{file_name}.{}", k + 1)
                        } else {
                            file_name.clone()
                        },
                        source: source.id,
                        abs_start,
                        abs_end: abs_start + to_project(pl.frames),
                        start: p.timeline.to_musical(pl.abs_start, rate),
                        source_offset: to_project(pl.file_offset),
                    }
                })
                .collect();
            if new.is_empty() {
                continue;
            }
            longest = longest.max(new.iter().map(|t| t.abs_end - t.abs_start).sum());
            commands.push(Command::AddSource {
                source: Box::new(source.clone()),
            });
            let (cmds, clips) = place_passes(
                p,
                plan.track,
                &track_name,
                new,
                mode,
                loop_mode,
                &mut pass_tracks,
            );
            commands.extend(cmds);
            placed.extend(clips);
            tracks += 1;
            opened.push((source.id, plan.path, w.peaks));
        }
        for (id, path, peaks) in opened {
            match media::open_stream(&path) {
                Ok(s) => {
                    self.sources.insert(id, Source::Stream(s));
                }
                Err(e) => self.notify(NoticeLevel::Error, format!("{}: {e}", path.display())),
            }
            self.peaks.insert(id, std::sync::Arc::new(peaks));
        }
        if commands.is_empty() {
            return Ok(());
        }
        self.edit(Command::Batch {
            label: "Capture Audio".into(),
            commands,
        })?;
        self.selection.select_clips(&placed, SelectMode::Replace);
        let seconds = longest as f64 / f64::from(self.project.sample_rate.max(1));
        self.notify(
            NoticeLevel::Info,
            format!(
                "captured {seconds:.1} s on {tracks} track{}",
                if tracks == 1 { "" } else { "s" }
            ),
        );
        Ok(())
    }
}
