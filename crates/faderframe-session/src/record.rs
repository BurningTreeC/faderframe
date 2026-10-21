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

/// The unit of a pre- or post-roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollUnit {
    Bars,
    /// Of the time signature (a quarter in 4/4, an eighth in 6/8).
    Beats,
    Seconds,
}

/// How much is played before a take (pre-roll) or after a punch-out
/// (post-roll); `amount` 0 is none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roll {
    pub amount: f64,
    pub unit: RollUnit,
}

impl Roll {
    pub const OFF: Roll = Roll {
        amount: 0.0,
        unit: RollUnit::Bars,
    };
    /// The lengths the menus offer.
    pub const PRESETS: [Roll; 9] = [
        Roll::OFF,
        Roll::beats(1.0),
        Roll::beats(2.0),
        Roll::bars(1.0),
        Roll::bars(2.0),
        Roll::bars(4.0),
        Roll::seconds(1.0),
        Roll::seconds(2.0),
        Roll::seconds(4.0),
    ];

    pub const fn bars(amount: f64) -> Roll {
        Roll {
            amount,
            unit: RollUnit::Bars,
        }
    }

    pub const fn beats(amount: f64) -> Roll {
        Roll {
            amount,
            unit: RollUnit::Beats,
        }
    }

    pub const fn seconds(amount: f64) -> Roll {
        Roll {
            amount,
            unit: RollUnit::Seconds,
        }
    }

    pub fn is_off(&self) -> bool {
        self.amount <= 0.0
    }

    /// "2 bars", "1 beat", "1.5 s"; "Off".
    pub fn label(&self) -> String {
        if self.is_off() {
            return "Off".into();
        }
        let n = if self.amount.fract() == 0.0 {
            format!("{}", self.amount as i64)
        } else {
            format!("{}", self.amount)
        };
        let one = self.amount == 1.0;
        match self.unit {
            RollUnit::Bars => format!("{n} {}", if one { "bar" } else { "bars" }),
            RollUnit::Beats => format!("{n} {}", if one { "beat" } else { "beats" }),
            RollUnit::Seconds => format!("{n} s"),
        }
    }

    /// A stable id ("2bars", "1beats", "1.5s", "off") for menus and the
    /// preferences.
    pub fn id(&self) -> String {
        if self.is_off() {
            return "off".into();
        }
        let unit = match self.unit {
            RollUnit::Bars => "bars",
            RollUnit::Beats => "beats",
            RollUnit::Seconds => "s",
        };
        format!("{}{unit}", self.amount)
    }

    pub fn from_id(id: &str) -> Option<Roll> {
        if id == "off" {
            return Some(Roll::OFF);
        }
        let (n, unit) = if let Some(n) = id.strip_suffix("bars") {
            (n, RollUnit::Bars)
        } else if let Some(n) = id.strip_suffix("beats") {
            (n, RollUnit::Beats)
        } else {
            (id.strip_suffix('s')?, RollUnit::Seconds)
        };
        let amount: f64 = n.parse().ok()?;
        (amount.is_finite() && (0.0..=64.0).contains(&amount)).then_some(Roll { amount, unit })
    }
}

/// User-facing recording options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordSettings {
    /// Played before the record start when starting from stop.
    pub preroll: Roll,
    /// The pre-roll starts on a bar line (back from where it would).
    pub preroll_on_bar: bool,
    /// The pre-roll plays the click alone (the arrangement waits).
    pub count_in: bool,
    /// Played after a punch-out, then playback stops (punch range only).
    pub postroll: Roll,
    pub metronome: MetronomeMode,
    /// Extra latency compensation on top of what the driver reports
    /// (positive moves takes earlier), in frames.
    pub latency_offset: i64,
    /// Capture ring capacity: how long the disk may stall without losing
    /// audio.
    pub buffer_seconds: f64,
    pub mode: RecordMode,
    pub loop_mode: LoopRecordMode,
    /// Input quantize (the transport's Q): recorded MIDI notes move to the
    /// grid as they are placed (with the editor's quantize strength, swing
    /// and ends).
    pub input_quantize: bool,
    /// Its grid (None: the edit grid).
    pub input_grid: Option<faderframe_timeline::GridDivision>,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            preroll: Roll::OFF,
            preroll_on_bar: true,
            count_in: false,
            postroll: Roll::OFF,
            metronome: MetronomeMode::Recording,
            latency_offset: 0,
            buffer_seconds: 4.0,
            mode: RecordMode::Takes,
            loop_mode: LoopRecordMode::Takes,
            input_quantize: false,
            input_grid: None,
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
        // Ahead of everything but the audio path (the capture rings hold
        // seconds; a writer starved by other work would overrun them).
        faderframe_realtime::priority::follow_audio(faderframe_realtime::priority::Helper::Feeder);
        let mut worked = false;
        while let Ok(h) = streams.headers.pop() {
            worked = true;
            // Catching up never sleeps: within the realtime time limit.
            faderframe_realtime::priority::breathe();
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
            spectral: None,
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
                    rating: 0,
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
            rating: 0,
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
    /// The transport's Play (and Record from stop): from stop, the
    /// playhead first goes to the Play Start Marker while it is on.
    /// Programmatic starts that put the playhead somewhere first (a song
    /// of the setlist, an ADR cue's pre-roll, the shuttle, the launcher)
    /// call [`Self::play`] and play from there.
    pub(crate) fn locate_play_start(&mut self) -> Result<()> {
        let at = self.play_start_position();
        if !self.transport.playing && at != self.transport.position {
            self.engine.transport(TransportCommand::Locate(at))?;
            self.show_position(at);
        }
        Ok(())
    }

    /// Start playback from the playhead; with record mode on and starting
    /// from stop, the record window restarts at the playhead and pre-roll
    /// applies.
    pub(crate) fn play(&mut self) -> Result<()> {
        // Where playback starts, before any pre-roll: Return to Start on
        // Stop comes back here, so the next take pre-rolls the same way.
        let start = self.transport.position;
        let mut count_in = 0;
        if !self.transport.playing && self.recording.is_some() {
            // Slot recordings ignore the punch range and count in by the
            // launcher's setting.
            let slot = self.recording.as_ref().is_some_and(|r| r.slot.is_some());
            let punch = self.punch_window().filter(|_| !slot);
            let from = match punch {
                Some((a, _)) => a,
                None => {
                    // Record from wherever the playhead is now.
                    let pos = self.transport.position;
                    if !slot && self.recording.as_ref().is_some_and(|r| r.from != pos) {
                        self.stop_recording()?;
                        self.start_recording(pos)?;
                    }
                    pos
                }
            };
            let (roll, on_bar) = if slot {
                (Roll::bars(f64::from(self.project.launcher.count_in)), true)
            } else {
                (self.record.preroll, self.record.preroll_on_bar)
            };
            if !roll.is_off() {
                // What the pre-roll cannot play before the project's start
                // is counted in before playback starts there.
                let (start, frames) = self.pre_roll(from, roll, on_bar);
                self.engine.transport(TransportCommand::Locate(start))?;
                self.show_position(start);
                count_in = frames;
            }
            // Post-roll: on after the punch-out, then stop.
            if let Some((_, end)) = punch
                && !self.record.postroll.is_off()
                && !self.show.on
            {
                let at = self.roll_on(end, self.record.postroll);
                self.engine.stop_at(Some(at))?;
                self.postroll_armed = true;
            }
        }
        self.engine.transport(if count_in > 0 {
            TransportCommand::PlayAfterCountIn(count_in)
        } else {
            TransportCommand::Play
        })?;
        self.loader.wake();
        self.automation_play_requested();
        if !self.transport.playing {
            // Where Stop returns to (Return to Start on Stop): where
            // playback started, before any pre-roll.
            self.play_started_at = Some(start);
            self.mtc_started(self.transport.position);
            self.automation_play_started();
            // A Play pressed again before the engine reports playing (Record
            // and Play together) finds it playing: it neither restarts the
            // take from the pre-roll's start nor locates again.
            self.requested_playing(true);
        }
        Ok(())
    }

    /// Where Play starts from stop (engine samples): the Play Start
    /// Marker while it is on, else the playhead.
    pub(crate) fn play_start_position(&self) -> i64 {
        match self.project.play_start {
            Some(m) if m.on && !self.play_here => {
                self.engine.musical_to_samples(&self.project, m.at)
            }
            _ => self.transport.position,
        }
    }

    /// Whether a take is being recorded or still being written.
    pub fn recording_in_progress(&self) -> bool {
        self.recording.is_some() || !self.finishing.is_empty()
    }

    /// Throw the take being recorded away (Abort Recording, Ctrl+Space):
    /// playback stops, nothing of this pass reaches the project or the
    /// undo history, its files are deleted once the writer lets go of them
    /// (never earlier takes or other media), and the playhead goes back to
    /// where recording began — with a punch range, to where playback
    /// started. What was played on MIDI stays capturable. `restart`: record
    /// again from there as soon as playback has stopped, with the same
    /// pre-roll and count-in. A launcher slot's recording is thrown away
    /// while playback goes on (`restart`: the slot records again from its
    /// next launch position). Not recording: Stop (a restart does nothing).
    pub(crate) fn abort_recording(&mut self, restart: bool) -> Result<()> {
        self.restart_recording = None;
        let Some(mut r) = self.recording.take() else {
            if restart {
                self.notify(NoticeLevel::Info, "Nothing is being recorded");
                return Ok(());
            }
            if self.transport.playing {
                return self.transport_action(crate::TransportAction::Stop);
            }
            return Ok(());
        };
        // Out of record mode whatever else fails (a device gone): the take
        // is thrown away all the same.
        let mut failed: Option<SessionError> = None;
        let mut note = |e: Result<()>| {
            if let Err(e) = e
                && failed.is_none()
            {
                failed = Some(e);
            }
        };
        note(self.disarm_postroll());
        note(
            self.engine
                .transport(TransportCommand::SetRecording(false))
                .map_err(Into::into),
        );
        note(self.engine.end_recording().map_err(Into::into));
        if r.midi.take().is_some() {
            // Played, not taken: Capture MIDI still has it.
            note(self.engine.end_midi_recording().map_err(Into::into));
        }
        let slot = r.slot.take();
        let from = r.from;
        self.discarding.push(r);
        self.pump_idle();
        self.notify(NoticeLevel::Info, "Recording discarded");
        self.revision += 1;
        if let Some(slot) = slot {
            if restart {
                note(self.dispatch(crate::Action::Launcher(
                    crate::launcher::LauncherOp::Record {
                        track: slot.track,
                        scene: slot.scene,
                    },
                )));
            }
            return failed.map_or(Ok(()), Err);
        }
        let back = match self.punch_window() {
            Some(_) => self.play_started_at.unwrap_or(from),
            None => from,
        };
        if self.transport.playing {
            note(
                self.engine
                    .transport(TransportCommand::Stop)
                    .map_err(Into::into),
            );
            self.automation_stop_sent();
        }
        note(
            self.engine
                .transport(TransportCommand::Locate(back))
                .map_err(Into::into),
        );
        self.show_position(back);
        self.play_started_at = None;
        if restart {
            self.restart_recording = Some((back, std::time::Instant::now()));
        }
        failed.map_or(Ok(()), Err)
    }

    /// An abort-and-restart's new take, once playback has stopped (the
    /// stop is deferred while rendering ahead primes it; the new take must
    /// not catch what still plays before it). Every tick.
    pub(crate) fn tick_restart_recording(&mut self) {
        let Some((at, asked)) = self.restart_recording else {
            return;
        };
        if self.transport.playing || self.engine.transport_snapshot().started() {
            if asked.elapsed() > Duration::from_secs(2) {
                // It never stopped (the device is gone?): no retake.
                self.restart_recording = None;
            }
            return;
        }
        self.restart_recording = None;
        let result = (|| -> Result<()> {
            self.engine.transport(TransportCommand::Locate(at))?;
            self.show_position(at);
            self.start_recording(at)?;
            if self.recording.is_some() {
                self.play()?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.notify(NoticeLevel::Error, format!("restarting the recording: {e}"));
        }
    }

    /// Delete an aborted recording's files (its writer is done): only the
    /// files it made, never one a source of the project names.
    fn discard_recording(&mut self, r: ActiveRecording) {
        let outcome = r.writer.map(RecordWriter::join).unwrap_or_default();
        let mut files = r.paths;
        for t in outcome.takes {
            if !files.contains(&t.path) {
                files.push(t.path);
            }
        }
        let used: std::collections::HashSet<PathBuf> = self
            .project
            .sources
            .values()
            .filter_map(|s| match &s.spec {
                SourceSpec::File { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        for f in files {
            if used.contains(&f) {
                continue;
            }
            for file in [peaks_path_for(&f), f] {
                if file.exists()
                    && let Err(e) = std::fs::remove_file(&file)
                {
                    tracing::warn!("cannot remove {}: {e}", file.display());
                }
            }
        }
    }

    /// Wait (a while) for aborted recordings' writers and delete their
    /// files (tests, quitting).
    pub fn wait_for_discarded_recordings(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !self.discarding.is_empty() && std::time::Instant::now() < deadline {
            self.engine.collect_garbage();
            self.pump_idle();
            self.poll_recording();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The project is replaced (closed) while recording: the recording
    /// ends without its takes reaching either project (the shell places
    /// them before asking about unsaved changes, see
    /// [`Self::wait_for_recordings`]); their files stay where they are.
    pub(crate) fn end_recordings_for_close(&mut self) {
        let mut ended = Vec::new();
        if let Some(mut r) = self.recording.take() {
            let _ = self.engine.transport(TransportCommand::SetRecording(false));
            let _ = self.engine.end_recording();
            if r.midi.take().is_some() {
                let _ = self.engine.end_midi_recording();
            }
            ended.push(r);
        }
        ended.append(&mut self.finishing);
        self.restart_recording = None;
        if ended.is_empty() && self.discarding.is_empty() {
            return;
        }
        self.pump_idle();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        for r in ended {
            while r.writer.as_ref().is_some_and(|w| !w.is_finished())
                && std::time::Instant::now() < deadline
            {
                self.engine.collect_garbage();
                self.pump_idle();
                std::thread::sleep(Duration::from_millis(2));
            }
            if let Some(w) = r.writer
                && w.is_finished()
            {
                for t in w.join().takes {
                    tracing::warn!(
                        "a take recorded while the project closed stays at {}",
                        t.path.display()
                    );
                }
            }
        }
        self.wait_for_discarded_recordings();
    }

    /// Where a pre-roll of `roll` before `from` (engine samples) starts:
    /// as far back, or (`on_bar`) from the start of that bar; never before
    /// the project's start.
    pub(crate) fn roll_back(&self, from: i64, roll: Roll, on_bar: bool) -> i64 {
        self.pre_roll(from, roll, on_bar).0
    }

    /// A pre-roll of `roll` before `from` (engine samples): where playback
    /// starts — as far back, or (`on_bar`) from the start of that bar —
    /// and the frames counted in before it. Before the project's start
    /// there is nothing to play: the pre-roll starts at the start, and the
    /// length it could not reach is a count-in before playback (the
    /// timeline's first tempo and metre carried back, so the beats and
    /// bar lines before the start are where the pre-roll would have had
    /// them).
    pub(crate) fn pre_roll(&self, from: i64, roll: Roll, on_bar: bool) -> (i64, u64) {
        if roll.is_off() {
            return (from, 0);
        }
        let p = &self.project;
        let meter = &p.timeline.meter;
        let at = self.engine.samples_to_musical(p, from);
        // Musical positions and samples before the start are negative.
        let back = match roll.unit {
            RollUnit::Bars => {
                // Whole bars back keep the place in the bar.
                let bar = meter.bar_at(at);
                let whole = roll.amount.floor() as i32;
                let into = at - meter.bar_start(bar);
                let earlier = bar - whole;
                let t = meter.bar_start(earlier) + into;
                let len = meter.bar_start(earlier + 1) - meter.bar_start(earlier);
                Some(t - MusicalTime::from_quarters(len.quarters() * roll.amount.fract()))
            }
            RollUnit::Beats => {
                let sig = meter.signature_of_bar(meter.bar_at(at));
                let beat = 4.0 / f64::from(sig.denominator.max(1));
                Some(at - MusicalTime::from_quarters(beat * roll.amount))
            }
            RollUnit::Seconds => None,
        };
        let start = match back {
            Some(t) => self.engine.musical_to_samples(p, t),
            None => from - (roll.amount * f64::from(self.engine.sample_rate())).round() as i64,
        };
        let start = if on_bar {
            let t = self.engine.samples_to_musical(p, start);
            self.engine
                .musical_to_samples(p, meter.bar_start(meter.bar_at(t)))
        } else {
            start
        }
        .min(from);
        if start < 0 {
            (0, start.unsigned_abs())
        } else {
            (start, 0)
        }
    }

    /// [`Self::pre_roll`] for tests.
    #[doc(hidden)]
    pub fn pre_roll_for_tests(&self, from: i64, roll: Roll, on_bar: bool) -> (i64, u64) {
        self.pre_roll(from, roll, on_bar)
    }

    /// Where a post-roll of `roll` after `end` (engine samples) ends.
    pub(crate) fn roll_on(&self, end: i64, roll: Roll) -> i64 {
        let p = &self.project;
        let meter = &p.timeline.meter;
        let at = self.engine.samples_to_musical(p, end);
        let quarters = match roll.unit {
            RollUnit::Bars => {
                let bar = meter.bar_at(at);
                let len = meter.bar_start(bar + 1) - meter.bar_start(bar);
                len.quarters() * roll.amount
            }
            RollUnit::Beats => {
                let sig = meter.signature_of_bar(meter.bar_at(at));
                4.0 / f64::from(sig.denominator.max(1)) * roll.amount
            }
            RollUnit::Seconds => {
                return end + (roll.amount * f64::from(self.engine.sample_rate())).round() as i64;
            }
        };
        self.engine
            .musical_to_samples(p, at + MusicalTime::from_quarters(quarters))
            .max(end)
    }

    /// [`Self::roll_back`] for tests.
    #[doc(hidden)]
    pub fn roll_back_for_tests(&self, from: i64, roll: Roll, on_bar: bool) -> i64 {
        self.roll_back(from, roll, on_bar)
    }

    /// [`Self::roll_on`] for tests.
    #[doc(hidden)]
    pub fn roll_on_for_tests(&self, end: i64, roll: Roll) -> i64 {
        self.roll_on(end, roll)
    }

    /// A post-roll stop no longer wanted (the user stopped first).
    pub(crate) fn disarm_postroll(&mut self) -> Result<()> {
        if std::mem::take(&mut self.postroll_armed) {
            self.engine.stop_at(None)?;
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
                | faderframe_project::InputRouting::Midi { .. }
                | faderframe_project::InputRouting::Plugin { .. } => None,
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
        let mut targets = self.record_targets(only);
        let mut midi_targets = self.midi_record_targets();
        midi_targets.retain(|t| only.is_none_or(|o| o == t.track));
        // A frozen track takes no clips: recording it would refuse every
        // track's take at the end (one undo step), so it is left out.
        let mut frozen: Vec<TrackId> = targets
            .iter()
            .map(|t| t.track)
            .chain(midi_targets.iter().map(|t| t.track))
            .filter(|t| self.is_frozen(*t))
            .collect();
        frozen.dedup();
        if !frozen.is_empty() {
            targets.retain(|t| !frozen.contains(&t.track));
            midi_targets.retain(|t| !frozen.contains(&t.track));
            let names: Vec<String> = frozen
                .iter()
                .filter_map(|t| self.project.track(*t).map(|t| t.name.clone()))
                .collect();
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "{} {} frozen and not recorded: unfreeze to record",
                    names.join(", "),
                    if names.len() == 1 { "is" } else { "are" }
                ),
            );
            if targets.is_empty() && midi_targets.is_empty() {
                return Ok(());
            }
        }
        if targets.is_empty() && midi_targets.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                "Nothing is armed for recording. Click R on a track to arm it (audio tracks need an input).",
            );
            return Ok(());
        }
        // A slot recording (one track) ignores the punch range.
        let (from, to) = match only {
            Some(_) => (from, i64::MAX),
            None => self.punch_window().unwrap_or((from, i64::MAX)),
        };
        // What the player hears comes the graph's output latency (plugin
        // delay compensation) after it is processed: they play along with
        // that, so every take moves back by it too.
        let graph = i64::from(self.engine.graph_latency());
        // MIDI: input is placed one buffer late (constant latency) and heard
        // after the output latency; the take moves back by both.
        let midi = if midi_targets.is_empty() {
            None
        } else {
            let shift = info.buffer_size as i64
                + info.output_latency as i64
                + graph
                + self.record.latency_offset
                + i64::from(self.engine.varispeed_delay());
            let tracks: Vec<TrackId> = midi_targets.iter().map(|t| t.track).collect();
            let rx = self.engine.begin_midi_recording(midi_targets, from, to)?;
            let mut take = crate::midi::MidiTake::new(rx, tracks, shift);
            take.window = (from, to);
            take.loop_end = self
                .project
                .loop_range
                .filter(|_| self.project.loop_enabled)
                .map(|r| self.engine.musical_to_samples(&self.project, r.end));
            Some(take)
        };
        let midi_tracks: Vec<TrackId> = midi.as_ref().map(|m| m.tracks.clone()).unwrap_or_default();
        if targets.is_empty() {
            self.engine.metronome().set_mode(self.record.metronome);
            self.engine
                .transport(TransportCommand::SetRecording(true))?;
            self.recording = Some(ActiveRecording {
                writer: None,
                paths: Vec::new(),
                midi,
                from,
                to,
                tracks: midi_tracks,
                latency: info.input_latency as i64
                    + info.output_latency as i64
                    + graph
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
                + graph
                + self.record.latency_offset
                // Varispeed's resampler, in and out.
                + 2 * i64::from(self.engine.varispeed_delay());
        let streams = self
            .engine
            .begin_recording(targets, from, to, self.record.buffer_seconds)?;
        let writer = RecordWriter::spawn(streams, paths.clone())
            .map_err(|e| SessionError::Other(format!("cannot start the record writer: {e}")))?;
        self.engine.metronome().set_mode(self.record.metronome);
        self.engine
            .transport(TransportCommand::SetRecording(true))?;
        self.recording = Some(ActiveRecording {
            writer: Some(writer),
            paths,
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
                m.close_all(at);
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
        while i < self.discarding.len() {
            if self.discarding[i]
                .writer
                .as_ref()
                .is_none_or(|w| w.is_finished())
            {
                let r = self.discarding.remove(i);
                self.discard_recording(r);
            } else {
                i += 1;
            }
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
        // New track per pass: (armed track's index, pass tracks put under it).
        let mut pass_tracks: Vec<(usize, usize)> = Vec::new();
        // The loop (engine samples) the passes wrapped at.
        let loop_samples = self
            .project
            .loop_range
            .filter(|_| self.project.loop_enabled)
            .map(|l| {
                (
                    self.engine.musical_to_samples(&self.project, l.start),
                    self.engine.musical_to_samples(&self.project, l.end),
                )
            });
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
                // A pass reads `latency` frames past its own: the next
                // pass's first frames are what was played at its end only
                // when the next one followed at once (the loop wrapped
                // inside the window). After a punch-out or a locate they are
                // other audio, so the pass ends with its own frames.
                let follows = take.segments.get(k + 1).is_some_and(|next| {
                    loop_samples.is_some_and(|(start, end)| {
                        seg.timeline_start + seg.frames as i64 == end
                            && next.timeline_start == start
                    })
                });
                let own_end = if follows {
                    take.frames as i64
                } else {
                    (seg.file_offset + seg.frames) as i64
                };
                let len = (seg.frames as i64)
                    .min(take.frames as i64 - offset)
                    .min(own_end - offset);
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
                    // The last complete pass: one cut short by Stop (just
                    // after a wrap, say) does not throw away the whole
                    // pass before it; with none complete, the last.
                    let longest = new.iter().map(|t| t.abs_end - t.abs_start).max();
                    let keep = longest
                        .and_then(|l| {
                            let enough = l - (l / 100).max(64);
                            new.iter().rposition(|t| t.abs_end - t.abs_start >= enough)
                        })
                        .unwrap_or(new.len() - 1);
                    let last = vec![new.swap_remove(keep)];
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
                    // Right under the armed track as the batch has it then:
                    // pass tracks put in above it for other armed tracks
                    // moved it down.
                    let own = p.track_index(take.track).unwrap_or(0);
                    let above: usize = pass_tracks
                        .iter()
                        .filter(|(i, _)| *i < own)
                        .map(|(_, n)| *n)
                        .sum();
                    pass_tracks.push((own, new.len()));
                    let first = own + above + 1;
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
            // As the engine plays the comp: an edge inside the folder that
            // touches no other piece gets a short declick.
            let declick = faderframe_project::COMP_DECLICK.min(b - a);
            let (inner_in, inner_out) = (!touches_prev && a > 0, !touches_next && b < f.length);
            let id: ClipId = p.ids.allocate();
            let start = p.timeline.to_musical(base + a, rate);
            let fades = faderframe_project::ClipFades {
                fade_in: fade(
                    touches_prev,
                    piece.start + half - a,
                    if a == 0 { f.fades.fade_in } else { declick },
                ),
                fade_out: fade(
                    touches_next,
                    b - (piece.end - half),
                    if b == f.length {
                        f.fades.fade_out
                    } else {
                        declick
                    },
                ),
                fade_in_shape: if touches_prev {
                    faderframe_project::FadeShape::EqualPower
                } else if inner_in {
                    faderframe_project::FadeShape::Linear
                } else {
                    f.fades.fade_in_shape
                },
                fade_out_shape: if touches_next {
                    faderframe_project::FadeShape::EqualPower
                } else if inner_out {
                    faderframe_project::FadeShape::Linear
                } else {
                    f.fades.fade_out_shape
                },
                fade_in_bend: if touches_prev || inner_in {
                    0
                } else {
                    f.fades.fade_in_bend
                },
                fade_out_bend: if touches_next || inner_out {
                    0
                } else {
                    f.fades.fade_out_bend
                },
            };
            // Both fades fit the piece.
            let mut fades = fades;
            fades.fade_in = fades.fade_in.clamp(0, b - a);
            fades.fade_out = fades.fade_out.clamp(0, b - a - fades.fade_in);
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
                        spectral: None,
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
