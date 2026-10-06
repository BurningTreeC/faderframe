//! Recording: the disk writer thread and turning finished takes into clips.
//!
//! The engine captures armed inputs into lock-free rings
//! ([`faderframe_engine::record`]); the [`RecordWriter`] thread drains them
//! into one float WAV file per armed track (created on the first captured
//! block), builds the waveform peaks on the fly and keeps a list of
//! [`Segment`]s — one per contiguous pass (loop recording produces one pass
//! per loop cycle, all in the same file).

use faderframe_audio_files::import::peaks_path_for;
use faderframe_audio_files::wavstream::WavWriter;
use faderframe_audio_files::{PeakBuilder, PeakCache, WavFormat};
use faderframe_core::TrackId;
use faderframe_engine::{MetronomeMode, RecordStreams};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// User-facing recording options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordSettings {
    /// Bars played before the record start when starting from stop.
    pub preroll_bars: u32,
    pub metronome: MetronomeMode,
    /// Extra latency compensation on top of what the driver reports
    /// (positive moves takes earlier), in frames.
    pub latency_offset: i64,
    /// Capture ring capacity: how long the disk may stall without losing
    /// audio.
    pub buffer_seconds: f64,
    pub mode: RecordMode,
    pub loop_mode: LoopRecordMode,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            preroll_bars: 0,
            metronome: MetronomeMode::Recording,
            latency_offset: 0,
            buffer_seconds: 4.0,
            mode: RecordMode::Takes,
            loop_mode: LoopRecordMode::Takes,
        }
    }
}

/// A contiguous pass inside a take file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub pass: u32,
    /// Timeline sample (engine rate) of the segment's first frame.
    pub timeline_start: i64,
    /// Frame offset in the file.
    pub file_offset: u64,
    pub frames: u64,
}

/// One finished take file.
#[derive(Debug)]
pub struct RecordedTake {
    pub track: TrackId,
    pub path: PathBuf,
    pub channels: usize,
    pub frames: u64,
    pub sample_rate: u32,
    pub segments: Vec<Segment>,
    pub peaks: PeakCache,
}

/// What the arranger draws while a take is being recorded: its peaks so
/// far and its passes. Shared between the writer thread and the UI.
#[derive(Debug)]
pub struct LiveTake {
    pub peaks: PeakBuilder,
    pub segments: Vec<Segment>,
}

pub type SharedLiveTake = Arc<Mutex<LiveTake>>;

struct TakeFile {
    writer: WavWriter,
    expected: i64,
}

/// Longest hole (overrun) filled with silence; anything longer is treated
/// as a new pass.
const MAX_GAP_SECONDS: i64 = 10;

/// What a writer produced. Takes written before an I/O error are kept.
#[derive(Debug, Default)]
pub struct RecordOutcome {
    pub takes: Vec<RecordedTake>,
    pub error: Option<String>,
}

pub struct RecordWriter {
    handle: Option<JoinHandle<RecordOutcome>>,
    /// Frames written per take file so far.
    pub frames: Arc<AtomicU64>,
    /// Live peaks per target (same order as the record targets).
    pub live: Vec<(TrackId, SharedLiveTake)>,
}

impl RecordWriter {
    /// Start draining `streams`; take `i` goes to `paths[i]` (one per target).
    pub fn spawn(streams: RecordStreams, paths: Vec<PathBuf>) -> std::io::Result<Self> {
        let frames = Arc::new(AtomicU64::new(0));
        let live: Vec<(TrackId, SharedLiveTake)> = streams
            .targets
            .iter()
            .map(|t| {
                (
                    t.track,
                    Arc::new(Mutex::new(LiveTake {
                        peaks: PeakBuilder::new(t.channels as usize),
                        segments: Vec::new(),
                    })),
                )
            })
            .collect();
        let f = Arc::clone(&frames);
        let shared: Vec<SharedLiveTake> = live.iter().map(|(_, l)| Arc::clone(l)).collect();
        let handle = std::thread::Builder::new()
            .name("faderframe-record".into())
            .spawn(move || run(streams, paths, &f, &shared))?;
        Ok(Self {
            handle: Some(handle),
            frames,
            live,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    pub fn join(mut self) -> RecordOutcome {
        match self.handle.take().map(|h| h.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => RecordOutcome {
                takes: Vec::new(),
                error: Some("the record writer panicked".into()),
            },
            None => RecordOutcome::default(),
        }
    }
}

fn run(
    mut streams: RecordStreams,
    paths: Vec<PathBuf>,
    progress: &AtomicU64,
    live: &[SharedLiveTake],
) -> RecordOutcome {
    let rate = streams.sample_rate;
    let targets = streams.targets.clone();
    let total_channels = streams.channels();
    let mut files: Vec<Option<TakeFile>> = targets.iter().map(|_| None).collect();
    let mut block: Vec<f32> = Vec::new();
    let mut zeros: Vec<f32> = Vec::new();
    let mut result: std::io::Result<()> = Ok(());
    loop {
        let mut worked = false;
        while let Ok(h) = streams.headers.pop() {
            worked = true;
            let n = h.frames as usize;
            block.resize(n * total_channels, 0.0);
            // Data is pushed before its header, so it is complete.
            if streams.data.pop_entire_slice(&mut block).is_err() {
                result = Err(std::io::Error::other("record ring out of sync"));
                break;
            }
            if result.is_err() {
                // Keep draining so the audio thread never sees a full ring,
                // but stop writing after an I/O error.
                continue;
            }
            let mut ch0 = 0;
            for (i, t) in targets.iter().enumerate() {
                let ch = t.channels as usize;
                let planes: Vec<&[f32]> = (0..ch)
                    .map(|c| &block[(ch0 + c) * n..(ch0 + c + 1) * n])
                    .collect();
                ch0 += ch;
                if files[i].is_none() {
                    match WavWriter::create(&paths[i], ch as u16, rate, WavFormat::Float32, false) {
                        Ok(writer) => {
                            files[i] = Some(TakeFile {
                                writer,
                                expected: h.position,
                            });
                        }
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }
                let Some(f) = files[i].as_mut() else { continue };
                let Ok(mut lt) = live[i].lock() else { continue };
                let lt = &mut *lt;
                let same_pass = lt.segments.last().is_some_and(|s| s.pass == h.pass);
                let gap = h.position - f.expected;
                if same_pass && gap > 0 && gap <= MAX_GAP_SECONDS * rate as i64 {
                    // Overrun: keep later audio in place.
                    zeros.resize(gap as usize, 0.0);
                    let silent: Vec<&[f32]> = (0..ch).map(|_| &zeros[..]).collect();
                    if let Err(e) = f.writer.write_planar(&silent, gap as usize) {
                        result = Err(e);
                        break;
                    }
                    lt.peaks.push(&silent, gap as usize);
                    if let Some(s) = lt.segments.last_mut() {
                        s.frames += gap as u64;
                    }
                } else if !same_pass || gap != 0 {
                    lt.segments.push(Segment {
                        pass: h.pass,
                        timeline_start: h.position,
                        file_offset: f.writer.frames(),
                        frames: 0,
                    });
                }
                if let Err(e) = f.writer.write_planar(&planes, n) {
                    result = Err(e);
                    break;
                }
                lt.peaks.push(&planes, n);
                if let Some(s) = lt.segments.last_mut() {
                    s.frames += n as u64;
                }
                f.expected = h.position + n as i64;
            }
            if let Some(f) = files.iter().flatten().next() {
                progress.store(f.writer.frames(), Ordering::Relaxed);
            }
        }
        if streams.is_finished() {
            break;
        }
        if !worked {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut takes = Vec::new();
    for (i, f) in files.into_iter().enumerate() {
        let Some(f) = f else { continue };
        let frames = f.writer.frames();
        let path = match f.writer.finish() {
            Ok(p) => p,
            Err(e) => {
                result = Err(e);
                continue;
            }
        };
        let (peaks, segments) = match live[i].lock() {
            Ok(mut lt) => {
                let builder = std::mem::replace(&mut lt.peaks, PeakBuilder::new(0));
                (builder.finish(), lt.segments.clone())
            }
            Err(_) => continue,
        };
        let _ = peaks.save(&peaks_path_for(&path));
        takes.push(RecordedTake {
            track: targets[i].track,
            path,
            channels: targets[i].channels as usize,
            frames,
            sample_rate: rate,
            segments,
            peaks,
        });
    }
    RecordOutcome {
        takes,
        error: result.err().map(|e| e.to_string()),
    }
}

// --- session integration -------------------------------------------------------

use crate::{ActiveRecording, NoticeLevel, Result, SelectMode, Session, SessionError, media};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::{RecordTarget, Source};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, Command, Project, SourceSpec, Take, TakeFolder,
    TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;

/// Commands that clear `[a, b)` on `track` for a new take (existing unmuted
/// audio clips are cut back, as on tape: the newest recording wins).
pub(crate) fn carve(
    p: &mut Project,
    track: TrackId,
    a: MusicalTime,
    b: MusicalTime,
) -> Vec<Command> {
    carve_where(p, track, a, b, |c| c.is_audio())
}

/// [`carve`] for MIDI clips (recording in Replace mode).
pub(crate) fn carve_midi(
    p: &mut Project,
    track: TrackId,
    a: MusicalTime,
    b: MusicalTime,
) -> Vec<Command> {
    carve_where(p, track, a, b, |c| {
        matches!(c.content, faderframe_project::ClipContent::Midi(_))
    })
}

/// [`carve`] for every kind of clip (launches written into the
/// arrangement).
pub(crate) fn carve_any(
    p: &mut Project,
    track: TrackId,
    a: MusicalTime,
    b: MusicalTime,
) -> Vec<Command> {
    carve_where(p, track, a, b, |_| true)
}

fn carve_where(
    p: &mut Project,
    track: TrackId,
    a: MusicalTime,
    b: MusicalTime,
    kind: impl Fn(&faderframe_project::Clip) -> bool,
) -> Vec<Command> {
    let mut out = Vec::new();
    let clips: Vec<(ClipId, MusicalTime, MusicalTime)> = p
        .clips_of(track)
        .into_iter()
        .filter(|c| !c.muted && kind(c))
        .map(|c| (c.id, c.start, c.end(&p.timeline, p.sample_rate)))
        .filter(|&(_, s, e)| s < b && e > a)
        .collect();
    for (id, start, end) in clips {
        match (start < a, end > b) {
            // Spans the range: keep both outer parts.
            (true, true) => {
                let mid: ClipId = p.ids.allocate();
                let right: ClipId = p.ids.allocate();
                out.push(Command::SplitClip {
                    clip: id,
                    at: a,
                    new_clip: mid,
                });
                out.push(Command::SplitClip {
                    clip: mid,
                    at: b,
                    new_clip: right,
                });
                out.push(Command::RemoveClip { clip: mid });
            }
            // Overlaps the range start: keep the head.
            (true, false) => {
                let tail: ClipId = p.ids.allocate();
                out.push(Command::SplitClip {
                    clip: id,
                    at: a,
                    new_clip: tail,
                });
                out.push(Command::RemoveClip { clip: tail });
            }
            // Overlaps the range end: keep the tail.
            (false, true) => {
                let tail: ClipId = p.ids.allocate();
                out.push(Command::SplitClip {
                    clip: id,
                    at: b,
                    new_clip: tail,
                });
                out.push(Command::RemoveClip { clip: id });
            }
            (false, false) => out.push(Command::RemoveClip { clip: id }),
        }
    }
    out
}

/// What recording over existing clips does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordMode {
    /// Existing material becomes takes of a take folder; the new take is
    /// comped in over the recorded range (nothing is lost).
    #[default]
    Takes,
    /// Existing clips are cut back underneath the new take (tape style).
    Replace,
}

impl RecordMode {
    pub const ALL: [RecordMode; 2] = [RecordMode::Takes, RecordMode::Replace];

    pub fn label(self) -> &'static str {
        match self {
            RecordMode::Takes => "Takes (keep and comp)",
            RecordMode::Replace => "Replace",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            RecordMode::Takes => "takes",
            RecordMode::Replace => "replace",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// What happens to the passes of a loop recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopRecordMode {
    /// Every pass becomes a take of one take folder (comp afterwards).
    #[default]
    Takes,
    /// Keep only the last pass.
    LastPass,
    /// Every pass on its own new track (all but the last pass muted).
    NewTracks,
}

impl LoopRecordMode {
    pub const ALL: [LoopRecordMode; 3] = [
        LoopRecordMode::Takes,
        LoopRecordMode::LastPass,
        LoopRecordMode::NewTracks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LoopRecordMode::Takes => "Passes as takes",
            LoopRecordMode::LastPass => "Keep last pass",
            LoopRecordMode::NewTracks => "New track per pass",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            LoopRecordMode::Takes => "takes",
            LoopRecordMode::LastPass => "last-pass",
            LoopRecordMode::NewTracks => "new-tracks",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// A freshly recorded pass, positioned in project frames.
#[derive(Clone, Debug)]
pub(crate) struct NewTake {
    pub name: String,
    pub source: AudioSourceId,
    /// Absolute timeline position (project frames) and musical start.
    pub abs_start: i64,
    pub abs_end: i64,
    pub start: MusicalTime,
    /// Source frame (project rate) at `abs_start`.
    pub source_offset: i64,
}

/// Commands placing `new` takes (in recording order) on `track`; returns
/// them and the clip that plays the result.
pub(crate) fn place_takes(
    p: &mut Project,
    track: TrackId,
    new: &[NewTake],
    overlap: RecordMode,
    folder_name: &str,
) -> (Vec<Command>, Option<ClipId>) {
    let Some(first) = new.iter().min_by_key(|t| t.abs_start) else {
        return (Vec::new(), None);
    };
    let rate = p.sample_rate as f64;
    let abs = |p: &Project, t: MusicalTime| p.timeline.to_samples(t, rate);
    let u0 = first.abs_start;
    let u1 = new.iter().map(|t| t.abs_end).max().unwrap_or(u0);
    let (ua, ub) = (first.start, p.timeline.to_musical(u1, rate));
    let plain = |t: &NewTake, p: &mut Project| Clip {
        id: p.ids.allocate(),
        track,
        name: t.name.clone(),
        color: None,
        start: t.start,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source: t.source,
            source_offset: t.source_offset,
            length: t.abs_end - t.abs_start,
            gain_db: 0.0,
            fades: Default::default(),
            stretch: Default::default(),
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
        }),
    };
    // Existing material in the way.
    let items: Vec<Clip> = if overlap == RecordMode::Takes {
        let mut v: Vec<Clip> = p
            .clips_of(track)
            .into_iter()
            .filter(|c| !c.muted && c.is_audio())
            .filter(|c| c.start < ub && c.end(&p.timeline, p.sample_rate) > ua)
            .cloned()
            .collect();
        v.sort_by_key(|c| c.start);
        v
    } else {
        Vec::new()
    };
    let mut commands = Vec::new();
    if overlap == RecordMode::Replace {
        commands.extend(carve(p, track, ua, ub));
    }
    if items.is_empty() && new.len() == 1 {
        let clip = plain(&new[0], p);
        let id = clip.id;
        commands.push(Command::AddClip {
            clip: Box::new(clip),
        });
        return (commands, Some(id));
    }

    // One folder over everything.
    let mut r0 = u0;
    let mut r0_musical = ua;
    let mut r1 = u1;
    for c in &items {
        let (a, b) = (abs(p, c.start), abs(p, c.end(&p.timeline, p.sample_rate)));
        if a < r0 {
            r0 = a;
            r0_musical = c.start;
        }
        r1 = r1.max(b);
    }
    let base = items.iter().find(|c| c.as_takes().is_some()).map(|c| c.id);
    let mut folder = match base
        .and_then(|id| p.clip(id))
        .and_then(|c| c.as_takes().map(|f| (c.start, f.clone())))
    {
        Some((start, mut f)) => {
            let a = abs(p, start);
            f.extend(a - r0, r1 - (a + f.length));
            f
        }
        None => TakeFolder::new(r1 - r0),
    };
    for c in items.iter().filter(|c| Some(c.id) != base) {
        let rel = abs(p, c.start) - r0;
        match &c.content {
            ClipContent::Audio(a) => {
                let i = folder.add_take(Take {
                    name: c.name.clone(),
                    source: a.source,
                    source_offset: a.source_offset - rel,
                    start: rel,
                    end: rel + a.length,
                    gain_db: a.gain_db,
                });
                folder.set_comp(rel, rel + a.length, Some(i));
            }
            ClipContent::Takes(other) => {
                let first_new = folder.takes.len();
                for t in &other.takes {
                    folder.add_take(Take {
                        source_offset: t.source_offset - rel,
                        start: t.start + rel,
                        end: t.end + rel,
                        ..t.clone()
                    });
                }
                for piece in other.pieces() {
                    folder.set_comp(
                        piece.start + rel,
                        piece.end + rel,
                        Some(first_new + piece.take),
                    );
                }
            }
            ClipContent::Midi(_) => {}
        }
        commands.push(Command::RemoveClip { clip: c.id });
    }
    for t in new {
        let rel = t.abs_start - r0;
        let len = t.abs_end - t.abs_start;
        let i = folder.add_take(Take {
            name: t.name.clone(),
            source: t.source,
            source_offset: t.source_offset - rel,
            start: rel,
            end: rel + len,
            gain_db: 0.0,
        });
        folder.set_comp(rel, rel + len, Some(i));
    }
    let id = match base {
        Some(id) => {
            commands.push(Command::SetClipContent {
                clip: id,
                start: r0_musical,
                content: Box::new(ClipContent::Takes(folder)),
            });
            id
        }
        None => {
            let id = p.ids.allocate();
            commands.push(Command::AddClip {
                clip: Box::new(Clip {
                    id,
                    track,
                    name: folder_name.to_string(),
                    color: None,
                    start: r0_musical,
                    muted: false,
                    content: ClipContent::Takes(folder),
                }),
            });
            id
        }
    };
    (commands, Some(id))
}

impl Session {
    /// Start playback; with record mode on and starting from stop, the
    /// record window restarts at the playhead and pre-roll applies.
    pub(crate) fn play(&mut self) -> Result<()> {
        if !self.transport.playing && self.recording.is_some() {
            let punch = self.punch_window();
            let from = match punch {
                Some((a, _)) => a,
                None => {
                    // Record from wherever the playhead is now.
                    let pos = self.transport.position;
                    if self.recording.as_ref().is_some_and(|r| r.from != pos) {
                        self.stop_recording()?;
                        self.start_recording(pos)?;
                    }
                    pos
                }
            };
            if self.record.preroll_bars > 0 {
                let meter = &self.project.timeline.meter;
                let at = self.engine.samples_to_musical(&self.project, from);
                let bar = (meter.bar_at(at) - self.record.preroll_bars as i32).max(0);
                let start = self
                    .engine
                    .musical_to_samples(&self.project, meter.bar_start(bar));
                self.engine
                    .transport(TransportCommand::Locate(start.min(from)))?;
                self.transport.position = start.min(from);
            }
        }
        self.engine.transport(TransportCommand::Play)?;
        self.loader.wake();
        self.automation_play_requested();
        if !self.transport.playing {
            self.play_started_at = Some(self.transport.position);
            self.mtc_started(self.transport.position);
            self.automation_play_started();
        }
        Ok(())
    }

    fn punch_window(&self) -> Option<(i64, i64)> {
        let p = &self.project;
        p.punch_range.filter(|_| p.punch_enabled).map(|r| {
            (
                self.engine.musical_to_samples(p, r.start),
                self.engine.musical_to_samples(p, r.end),
            )
        })
    }

    fn record_targets(&self, only: Option<TrackId>) -> Vec<RecordTarget> {
        self.project
            .tracks
            .iter()
            .filter(|t| t.record_arm && t.kind == TrackKind::Audio)
            .filter(|t| only.is_none_or(|o| o == t.id))
            .filter_map(|t| match t.input {
                faderframe_project::InputRouting::Hardware { first_channel } => {
                    Some(RecordTarget {
                        track: t.id,
                        first_channel,
                        channels: t.layout.channel_count() as u16,
                    })
                }
                faderframe_project::InputRouting::None
                | faderframe_project::InputRouting::Midi { .. } => None,
            })
            .collect()
    }

    /// Enter record mode with the window starting at `from` (or the punch
    /// range).
    pub(crate) fn start_recording(&mut self, from: i64) -> Result<()> {
        self.start_recording_only(from, None)
    }

    /// [`Self::start_recording`] for one armed track (`None`: all).
    pub(crate) fn start_recording_only(&mut self, from: i64, only: Option<TrackId>) -> Result<()> {
        let Some(info) = self.stream_info() else {
            return Err(SessionError::Other("start audio before recording".into()));
        };
        let targets = self.record_targets(only);
        let mut midi_targets = self.midi_record_targets();
        midi_targets.retain(|t| only.is_none_or(|o| o == t.track));
        if targets.is_empty() && midi_targets.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                "Nothing is armed for recording. Click R on a track to arm it (audio tracks need an input).",
            );
            return Ok(());
        }
        let (from, to) = self.punch_window().unwrap_or((from, i64::MAX));
        // MIDI: input is placed one buffer late (constant latency) and heard
        // after the output latency; the take moves back by both.
        let midi = if midi_targets.is_empty() {
            None
        } else {
            let shift = info.buffer_size as i64
                + info.output_latency as i64
                + self.record.latency_offset
                + i64::from(self.engine.varispeed_delay());
            let tracks: Vec<TrackId> = midi_targets.iter().map(|t| t.track).collect();
            let rx = self.engine.begin_midi_recording(midi_targets, from, to)?;
            Some(crate::midi::MidiTake::new(rx, tracks, shift))
        };
        let midi_tracks: Vec<TrackId> = midi.as_ref().map(|m| m.tracks.clone()).unwrap_or_default();
        if targets.is_empty() {
            self.engine.metronome().set_mode(self.record.metronome);
            self.engine
                .transport(TransportCommand::SetRecording(true))?;
            self.recording = Some(ActiveRecording {
                writer: None,
                midi,
                from,
                to,
                tracks: midi_tracks,
                latency: info.input_latency as i64
                    + info.output_latency as i64
                    + 2 * i64::from(self.engine.varispeed_delay())
                    + self.record.latency_offset,
                seen: false,
                slot: None,
            });
            self.pump_idle();
            return Ok(());
        }
        std::fs::create_dir_all(&self.media_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.media_dir.display())))?;
        let mut paths: Vec<PathBuf> = Vec::new();
        for t in &targets {
            let name = self
                .project
                .track(t.track)
                .map_or("Audio", |t| t.name.as_str());
            let n = 1 + self
                .project
                .sources
                .values()
                .filter(|s| s.name.starts_with(&format!("{name} Take")))
                .count();
            let mut path = media::unique_path(&self.media_dir, &format!("{name} Take {n}"));
            let mut k = 2;
            while paths.contains(&path) {
                path = media::unique_path(&self.media_dir, &format!("{name} Take {n}-{k}"));
                k += 1;
            }
            paths.push(path);
        }
        let tracks = targets.iter().map(|t| t.track).chain(midi_tracks).collect();
        let latency = info.input_latency as i64
                + info.output_latency as i64
                + self.record.latency_offset
                // Varispeed's resampler, in and out.
                + 2 * i64::from(self.engine.varispeed_delay());
        let streams = self
            .engine
            .begin_recording(targets, from, to, self.record.buffer_seconds)?;
        let writer = RecordWriter::spawn(streams, paths)
            .map_err(|e| SessionError::Other(format!("cannot start the record writer: {e}")))?;
        self.engine.metronome().set_mode(self.record.metronome);
        self.engine
            .transport(TransportCommand::SetRecording(true))?;
        self.recording = Some(ActiveRecording {
            writer: Some(writer),
            midi,
            from,
            to,
            tracks,
            latency,
            seen: false,
            slot: None,
        });
        self.pump_idle();
        Ok(())
    }

    /// Leave record mode; the takes become clips once the writer is done.
    pub(crate) fn stop_recording(&mut self) -> Result<()> {
        if let Some(mut r) = self.recording.take() {
            if let Some(slot) = r.slot.as_mut()
                && slot.end.is_none()
            {
                // Stopped before the slot's end was set: it ends here.
                slot.end = Some(self.engine.transport_snapshot().position.max(slot.from + 1));
            }
            self.engine
                .transport(TransportCommand::SetRecording(false))?;
            self.engine.end_recording()?;
            if let Some(m) = r.midi.as_mut() {
                // What was played while recording is in the take.
                self.capture.mark_taken();
                self.engine.end_midi_recording()?;
                m.drain();
                let at = self.engine.transport_snapshot().position - m.shift;
                m.close_all(at.max(m.last));
            }
            self.pump_idle();
            self.finishing.push(r);
        }
        Ok(())
    }

    pub(crate) fn poll_recording(&mut self) {
        // The audio thread leaves record mode on stop (or the engine went
        // away): end the session-side recording too.
        // Read the live atomics: the cached snapshot may predate the
        // command that started this recording.
        let rt_recording = self.engine.transport_snapshot().recording;
        if let Some(m) = self.recording.as_mut().and_then(|r| r.midi.as_mut()) {
            m.drain();
        }
        let ended = self.recording.as_mut().is_some_and(|r| {
            r.seen |= rt_recording;
            (r.seen && !rt_recording) || r.writer.as_ref().is_some_and(|w| w.is_finished())
        });
        if ended && let Err(e) = self.stop_recording() {
            self.notify(NoticeLevel::Error, e.to_string());
        }
        let mut i = 0;
        while i < self.finishing.len() {
            if self.finishing[i]
                .writer
                .as_ref()
                .is_none_or(|w| w.is_finished())
            {
                let r = self.finishing.remove(i);
                if let Err(e) = self.finish_recording(r) {
                    self.notify(NoticeLevel::Error, format!("recording: {e}"));
                }
            } else {
                i += 1;
            }
        }
    }

    /// Read the take being recorded on `track` (live waveform). The writer
    /// thread is blocked while `f` runs, so keep it short (one paint).
    pub fn with_live_take<R>(&self, track: TrackId, f: impl FnOnce(&LiveTake) -> R) -> Option<R> {
        let r = self.recording.as_ref()?;
        let (_, live) = r.writer.as_ref()?.live.iter().find(|(t, _)| *t == track)?;
        let guard = live.lock().ok()?;
        Some(f(&guard))
    }

    /// Stop recording and wait until all takes are on the timeline (tests,
    /// scripting, quitting).
    pub fn wait_for_recordings(&mut self) {
        if let Err(e) = self.stop_recording() {
            self.notify(NoticeLevel::Error, e.to_string());
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !self.finishing.is_empty() && std::time::Instant::now() < deadline {
            self.engine.collect_garbage();
            self.pump_idle();
            self.poll_recording();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn finish_recording(&mut self, r: ActiveRecording) -> Result<()> {
        let outcome = r.writer.map(RecordWriter::join).unwrap_or_default();
        if let Some(e) = &outcome.error {
            self.notify(NoticeLevel::Error, format!("recording: {e}"));
        }
        if let Some(slot) = r.slot {
            return self.finish_slot_recording(slot, outcome, r.midi.as_ref(), r.latency);
        }
        let (overruns, _) = self.engine.record_counters();
        let mut commands = Vec::new();
        let mut placed = Vec::new();
        let mut opened = Vec::new();
        if let Some(m) = &r.midi {
            let (c, p) = self.midi_take_commands(m);
            commands.extend(c);
            placed.extend(p);
        }
        for take in outcome.takes {
            let p = &mut self.project;
            let Some(track_name) = p.track(take.track).map(|t| t.name.clone()) else {
                continue; // track deleted while recording; the file stays
            };
            let take_rate = take.sample_rate.max(1) as f64;
            let project_rate = p.sample_rate as f64;
            let to_project = |f: i64| (f as f64 * project_rate / take_rate).round() as i64;
            let file_name = take.path.file_stem().map_or_else(
                || format!("{track_name} Take"),
                |s| s.to_string_lossy().to_string(),
            );
            let source = AudioSource {
                id: p.ids.allocate(),
                name: file_name.clone(),
                spec: SourceSpec::File {
                    path: take.path.clone(),
                    channels: take.channels as u16,
                    frames: take.frames as i64,
                    sample_rate: take.sample_rate,
                },
            };
            // Latency: what was captured at timeline position P was played
            // (and heard) `latency` frames earlier, so the file is read
            // `latency` frames later than the capture position.
            let passes = take.segments.len();
            let mut new = Vec::new();
            for (k, seg) in take.segments.iter().enumerate() {
                let offset = seg.file_offset as i64 + r.latency;
                let len = (seg.frames as i64).min(take.frames as i64 - offset);
                if len <= 0 {
                    continue;
                }
                let abs_start = to_project(seg.timeline_start);
                new.push(NewTake {
                    name: if passes > 1 {
                        format!("{file_name}.{}", k + 1)
                    } else {
                        file_name.clone()
                    },
                    source: source.id,
                    abs_start,
                    abs_end: abs_start + to_project(len),
                    start: p.timeline.to_musical(seg.timeline_start, take_rate),
                    source_offset: to_project(offset),
                });
            }
            if new.is_empty() {
                continue;
            }
            commands.push(Command::AddSource {
                source: Box::new(source.clone()),
            });
            let mode = self.record.mode;
            let folder_name = format!("{track_name} Takes");
            match self.record.loop_mode {
                _ if new.len() == 1 => {
                    let (cmds, clip) = place_takes(p, take.track, &new, mode, &folder_name);
                    commands.extend(cmds);
                    placed.extend(clip);
                }
                LoopRecordMode::Takes => {
                    let (cmds, clip) = place_takes(p, take.track, &new, mode, &folder_name);
                    commands.extend(cmds);
                    placed.extend(clip);
                }
                LoopRecordMode::LastPass => {
                    let last = new.split_off(new.len() - 1);
                    let (cmds, clip) = place_takes(p, take.track, &last, mode, &folder_name);
                    commands.extend(cmds);
                    placed.extend(clip);
                }
                LoopRecordMode::NewTracks => {
                    // The last pass goes to the armed track (in the record
                    // mode); earlier passes each get a new, muted track below.
                    let last = new.split_off(new.len() - 1);
                    let (cmds, clip) = place_takes(p, take.track, &last, mode, &folder_name);
                    commands.extend(cmds);
                    placed.extend(clip);
                    let template = p.track(take.track).cloned();
                    let first = p.track_index(take.track).map_or(0, |i| i + 1);
                    for (index, (k, pass)) in (first..).zip(new.into_iter().enumerate()) {
                        let Some(mut t) = template.clone() else { break };
                        t.id = p.ids.allocate();
                        t.name = format!("{track_name} Pass {}", k + 1);
                        t.clips.clear();
                        t.sends.clear();
                        t.inserts.clear();
                        t.instrument = None;
                        t.preamp = None;
                        t.record_arm = false;
                        t.mute = true;
                        let new_track = t.id;
                        commands.push(Command::AddTrack {
                            track: Box::new(t),
                            index,
                        });
                        let (cmds, _) =
                            place_takes(p, new_track, &[pass], RecordMode::Replace, &folder_name);
                        commands.extend(cmds);
                    }
                }
            }
            opened.push((source.id, take.path.clone(), take.peaks));
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
        let n = placed.len();
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: "Record".into(),
                commands,
            })?;
            self.selection.select_clips(&placed, SelectMode::Replace);
            self.notify(
                NoticeLevel::Info,
                format!("recorded {n} track{}", if n == 1 { "" } else { "s" }),
            );
        }
        if overruns > 0 {
            self.notify(
                NoticeLevel::Warning,
                format!("the disk could not keep up: {overruns} frames were replaced by silence"),
            );
        }
        Ok(())
    }
}

impl Session {
    /// Replace a take folder by plain audio clips of what its comp plays
    /// (keeping the comp crossfades as clip fades).
    pub(crate) fn flatten_takes(&mut self, clip: ClipId) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return Ok(());
        };
        let Some(f) = c.as_takes().cloned() else {
            return Ok(());
        };
        let p = &mut self.project;
        let rate = p.sample_rate as f64;
        let base = p.timeline.to_samples(c.start, rate);
        let half = f.crossfade.max(0) / 2;
        let pieces = f.pieces();
        let mut commands = vec![Command::RemoveClip { clip }];
        let mut created = Vec::new();
        for (i, piece) in pieces.iter().enumerate() {
            let take = &f.takes[piece.take];
            let touches_prev = i > 0 && pieces[i - 1].end == piece.start;
            let touches_next = pieces.get(i + 1).is_some_and(|n| n.start == piece.end);
            let a = if touches_prev {
                (piece.start - half).max(take.start)
            } else {
                piece.start
            };
            let b = if touches_next {
                (piece.end + half).min(take.end)
            } else {
                piece.end
            };
            let fade = |touch: bool, len: i64, edge: i64| if touch { len } else { edge };
            let id: ClipId = p.ids.allocate();
            let start = p.timeline.to_musical(base + a, rate);
            let fades = faderframe_project::ClipFades {
                fade_in: fade(
                    touches_prev,
                    piece.start + half - a,
                    if a == 0 { f.fades.fade_in } else { 0 },
                ),
                fade_out: fade(
                    touches_next,
                    b - (piece.end - half),
                    if b == f.length { f.fades.fade_out } else { 0 },
                ),
                fade_in_shape: if touches_prev {
                    faderframe_project::FadeShape::EqualPower
                } else {
                    f.fades.fade_in_shape
                },
                fade_out_shape: if touches_next {
                    faderframe_project::FadeShape::EqualPower
                } else {
                    f.fades.fade_out_shape
                },
                fade_in_bend: if touches_prev {
                    0
                } else {
                    f.fades.fade_in_bend
                },
                fade_out_bend: if touches_next {
                    0
                } else {
                    f.fades.fade_out_bend
                },
            };
            commands.push(Command::AddClip {
                clip: Box::new(Clip {
                    id,
                    track: c.track,
                    name: take.name.clone(),
                    color: c.color,
                    start,
                    muted: c.muted,
                    content: ClipContent::Audio(AudioClip {
                        source: take.source,
                        source_offset: take.source_offset + a,
                        length: b - a,
                        gain_db: f.gain_db + take.gain_db,
                        fades,
                        stretch: Default::default(),
                        reversed: false,
                        warp: None,
                        pitch: None,
                        effects: None,
                    }),
                }),
            });
            created.push(id);
        }
        self.edit(Command::Batch {
            label: "Flatten Comp".into(),
            commands,
        })?;
        self.selection.select_clips(&created, SelectMode::Replace);
        Ok(())
    }
}
