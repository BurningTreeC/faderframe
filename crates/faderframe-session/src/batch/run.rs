//! Running a batch: worker threads taking the files in turn, each decoded,
//! processed, measured, named and written (see the module docs of
//! [`super`]).
//!
//! Every file is held in memory as 32-bit float while it is processed, so
//! a budget of memory ([`MEMORY_BUDGET`]) holds workers back while the
//! files in hand would pass it (a file larger than the budget runs alone).
//! Names are claimed under a lock as each file is written: two files of
//! the batch never take the same name, and an input is never a
//! destination.

use super::naming::{NameContext, file_stem};
use super::steps::{self, Audio, StepError, Work};
use super::{
    BatchError, BatchRequest, BatchStep, Channels, FileFormat, IfExists, MAX_WORKERS, plugins,
};
use crate::render::files::{Overwrite, Staged, StagedFile, occupied};
use crate::render::{RenderError, Rendered};
use faderframe_analysis::delivery;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Bytes of audio the workers may hold at once (a file counts three
/// times its decoded size: the steps make copies).
pub const MEMORY_BUDGET: u64 = 3 << 30;

/// Frames written at a time.
const WRITE_CHUNK: usize = 65_536;

/// What became of a file.
#[derive(Clone, Debug, PartialEq)]
pub enum FileState {
    Waiting,
    /// `what` is being done (the step's name, "Reading", "Writing");
    /// `fraction` of the whole file's work is done.
    Working {
        what: String,
        fraction: f32,
    },
    Done(FileResult),
    /// Left alone (its output exists and the batch skips such files).
    Skipped(String),
    Failed(String),
    Cancelled,
}

impl FileState {
    pub fn is_over(&self) -> bool {
        !matches!(self, FileState::Waiting | FileState::Working { .. })
    }

    /// In a word or a line, for lists and logs.
    pub fn text(&self) -> String {
        match self {
            FileState::Waiting => "Waiting".into(),
            FileState::Working { what, fraction } => {
                format!("{what} ({:.0} %)", fraction * 100.0)
            }
            FileState::Done(r) => r.summary(),
            FileState::Skipped(why) => format!("Skipped: {why}"),
            FileState::Failed(why) => format!("Failed: {why}"),
            FileState::Cancelled => "Cancelled".into(),
        }
    }

    /// How far it is (1 once over).
    pub fn fraction(&self) -> f32 {
        match self {
            FileState::Waiting => 0.0,
            FileState::Working { fraction, .. } => *fraction,
            _ => 1.0,
        }
    }
}

/// A written file.
#[derive(Clone, Debug, PartialEq)]
pub struct FileResult {
    pub output: PathBuf,
    pub sample_rate: u32,
    pub channels: usize,
    pub seconds: f64,
    /// Integrated loudness (LUFS; `-inf` when too short or silent).
    pub loudness: f64,
    pub true_peak_db: f64,
    pub sample_peak_db: f64,
    /// What the steps did worth saying.
    pub notes: Vec<String>,
}

impl FileResult {
    /// "Kick.wav — 48k stereo, 0:03.2, −16.0 LUFS, −1.0 dBTP".
    pub fn summary(&self) -> String {
        let name = self
            .output
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().to_string());
        let channels = match self.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n} channels"),
        };
        let minutes = (self.seconds / 60.0).floor();
        let seconds = self.seconds - minutes * 60.0;
        let loudness = if self.loudness.is_finite() {
            format!("{:.1} LUFS", self.loudness)
        } else {
            "loudness not measurable".into()
        };
        let peak = if self.true_peak_db.is_finite() {
            format!("{:.1} dBTP", self.true_peak_db)
        } else {
            "silent".into()
        };
        format!(
            "{name} \u{2014} {} {channels}, {minutes:.0}:{seconds:04.1}, {loudness}, {peak}",
            super::rate_text(self.sample_rate)
        )
        .replace('-', "\u{2212}")
    }
}

/// One file of a batch.
#[derive(Clone, Debug, PartialEq)]
pub struct FileStatus {
    pub input: PathBuf,
    pub state: FileState,
}

/// Where a batch is.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchStatus {
    pub files: Vec<FileStatus>,
    /// Every worker has stopped.
    pub finished: bool,
    pub cancelled: bool,
}

impl BatchStatus {
    pub fn done(&self) -> usize {
        self.count(|s| matches!(s, FileState::Done(_)))
    }

    pub fn failed(&self) -> usize {
        self.count(|s| matches!(s, FileState::Failed(_)))
    }

    pub fn skipped(&self) -> usize {
        self.count(|s| matches!(s, FileState::Skipped(_)))
    }

    fn count(&self, f: impl Fn(&FileState) -> bool) -> usize {
        self.files.iter().filter(|s| f(&s.state)).count()
    }

    /// How far the whole batch is (0–1).
    pub fn fraction(&self) -> f64 {
        if self.files.is_empty() {
            return 1.0;
        }
        self.files
            .iter()
            .map(|f| f64::from(f.state.fraction()))
            .sum::<f64>()
            / self.files.len() as f64
    }

    /// "12 files written, 1 failed, 2 skipped".
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} of {} file{} written",
            self.done(),
            self.files.len(),
            if self.files.len() == 1 { "" } else { "s" }
        )];
        if self.failed() > 0 {
            parts.push(format!("{} failed", self.failed()));
        }
        if self.skipped() > 0 {
            parts.push(format!("{} skipped", self.skipped()));
        }
        if self.cancelled {
            parts.push("cancelled".into());
        }
        parts.join(", ")
    }
}

/// Memory the workers hold, waited on.
struct Budget {
    used: Mutex<u64>,
    freed: Condvar,
}

/// A worker's share of the budget, given back when dropped.
struct Held<'a> {
    budget: &'a Budget,
    bytes: u64,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *used = used.saturating_sub(self.bytes);
        self.budget.freed.notify_all();
    }
}

impl Budget {
    /// Wait until `bytes` fit (or nothing else is held); `None` when
    /// cancelled meanwhile.
    fn take<'a>(&'a self, bytes: u64, cancel: &AtomicBool) -> Option<Held<'a>> {
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        while *used > 0 && *used + bytes > MEMORY_BUDGET {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            used = self
                .freed
                .wait_timeout(used, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *used += bytes;
        Some(Held {
            budget: self,
            bytes,
        })
    }
}

struct Shared {
    request: BatchRequest,
    states: Mutex<Vec<FileState>>,
    next: AtomicUsize,
    cancel: AtomicBool,
    running: AtomicUsize,
    /// Output names taken by this batch, and its inputs (never written).
    claimed: Mutex<HashSet<PathBuf>>,
    budget: Budget,
}

impl Shared {
    fn set(&self, index: usize, state: FileState) {
        let mut states = self.states.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = states.get_mut(index) {
            *s = state;
        }
    }
}

/// A batch running (see [`start`]). Dropped, it is cancelled; its
/// workers stop at their next look and leave no files half written.
pub struct BatchRun {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

impl BatchRun {
    /// Every file's state now.
    pub fn status(&self) -> BatchStatus {
        let states = self
            .shared
            .states
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        BatchStatus {
            files: self
                .shared
                .request
                .files
                .iter()
                .zip(states)
                .map(|(input, state)| FileStatus {
                    input: input.clone(),
                    state,
                })
                .collect(),
            finished: self.is_finished(),
            cancelled: self.shared.cancel.load(Ordering::Relaxed),
        }
    }

    /// Stop: files being processed are dropped (nothing of them written),
    /// files waiting are not started.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.budget.freed.notify_all();
    }

    pub fn is_finished(&self) -> bool {
        self.shared.running.load(Ordering::Acquire) == 0
    }

    /// Wait for every worker and say how it went.
    pub fn wait(mut self) -> BatchStatus {
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
        self.status()
    }

    /// The folder the files go to.
    pub fn folder(&self) -> &Path {
        &self.shared.request.folder
    }
}

impl Drop for BatchRun {
    fn drop(&mut self) {
        if !self.handles.is_empty() && !self.is_finished() {
            self.cancel();
        }
    }
}

/// The path an input is known by (its canonical one when it exists).
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Start `request` (see the module docs). Refused before anything runs
/// when there are no files, the chain cannot work or the folder cannot
/// be made.
pub fn start(mut request: BatchRequest) -> Result<BatchRun, BatchError> {
    if request.files.is_empty() {
        return Err(BatchError::NoFiles);
    }
    request.chain.check()?;
    std::fs::create_dir_all(&request.folder).map_err(|source| BatchError::Folder {
        path: request.folder.clone(),
        source,
    })?;
    request.folder = identity(&request.folder);
    let workers = request
        .workers
        .clamp(1, MAX_WORKERS)
        .min(request.files.len());
    let claimed: HashSet<PathBuf> = request.files.iter().map(|f| identity(f)).collect();
    let shared = Arc::new(Shared {
        states: Mutex::new(vec![FileState::Waiting; request.files.len()]),
        request,
        next: AtomicUsize::new(0),
        cancel: AtomicBool::new(false),
        running: AtomicUsize::new(workers),
        claimed: Mutex::new(claimed),
        budget: Budget {
            used: Mutex::new(0),
            freed: Condvar::new(),
        },
    });
    let mut handles = Vec::with_capacity(workers);
    for i in 0..workers {
        let s = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name(format!("faderframe-batch-{i}"))
            .spawn(move || worker(&s));
        match spawned {
            Ok(h) => handles.push(h),
            Err(e) => {
                // The ones started stop; this one never ran.
                shared.cancel.store(true, Ordering::Relaxed);
                shared
                    .running
                    .fetch_sub(workers - handles.len(), Ordering::AcqRel);
                return Err(BatchError::Thread(e));
            }
        }
    }
    Ok(BatchRun { shared, handles })
}

fn worker(shared: &Shared) {
    // Below the UI and the audio: a batch nobody watches frame by frame.
    faderframe_realtime::priority::lower_this_thread();
    loop {
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(input) = shared.request.files.get(index) else {
            break;
        };
        if shared.cancel.load(Ordering::Relaxed) {
            shared.set(index, FileState::Cancelled);
            continue;
        }
        let state = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process(shared, index, input)
        })) {
            Ok(state) => state,
            Err(_) => FileState::Failed("the processing stopped unexpectedly".into()),
        };
        match &state {
            FileState::Done(r) => tracing::info!("batch: {} → {}", input.display(), r.summary()),
            FileState::Failed(why) => tracing::warn!("batch: {}: {why}", input.display()),
            _ => {}
        }
        shared.set(index, state);
    }
    shared.running.fetch_sub(1, Ordering::AcqRel);
}

/// Shares of a file's work: reading, the steps (each alike), writing.
struct Stages {
    steps: usize,
}

impl Stages {
    fn at(&self, stage: usize, fraction: f32) -> f32 {
        let total = (self.steps + 2) as f32;
        ((stage as f32 + fraction.clamp(0.0, 1.0)) / total).clamp(0.0, 1.0)
    }
}

fn process(shared: &Shared, index: usize, input: &Path) -> FileState {
    let r = &shared.request;
    let stages = Stages {
        steps: r.chain.steps.len(),
    };
    let progress = |stage: usize, what: &str, f: f32| {
        shared.set(
            index,
            FileState::Working {
                what: what.to_string(),
                fraction: stages.at(stage, f),
            },
        );
    };
    progress(0, "Reading", 0.0);
    let (mut audio, _held) = match read(shared, input, &mut |f| progress(0, "Reading", f)) {
        Ok(Some(read)) => read,
        Ok(None) => return FileState::Cancelled,
        Err(e) => return FileState::Failed(e),
    };
    // The name, before the work: a file the batch skips costs nothing.
    let (rate, channels) = shape_after(r, &audio);
    let dest = match name(shared, index, input, rate, channels) {
        Ok(Named::Dest(d)) => d,
        Ok(Named::Skip(why)) => return FileState::Skipped(why),
        Err(e) => return FileState::Failed(e),
    };
    let mut notes = Vec::new();
    for (i, step) in r.chain.steps.iter().enumerate() {
        let label = step.label();
        let mut report = |f: f32| progress(i + 1, &label, f);
        let mut work = Work {
            cancel: &shared.cancel,
            progress: &mut report,
            notes: &mut notes,
        };
        work.report(0.0);
        let done = match step {
            BatchStep::Plugin(p) => {
                plugins::process(std::mem::take(&mut audio), p, &mut work).map(|a| audio = a)
            }
            BatchStep::Normalize(n) => steps::normalize(&mut audio, n, &mut work),
            BatchStep::Loudness(l) => steps::loudness(&mut audio, l, &mut work),
            BatchStep::Convert(c) => steps::convert(&mut audio, c, &mut work),
            BatchStep::TrimSilence(t) => steps::trim(&mut audio, t, &mut work),
            BatchStep::Fade(f) => steps::fade(&mut audio, f, &mut work),
        };
        match done {
            Ok(()) => {}
            Err(StepError::Cancelled) => return FileState::Cancelled,
            Err(StepError::Failed(e)) => return FileState::Failed(format!("{label}: {e}")),
        }
    }
    let last = stages.steps + 1;
    progress(last, "Writing", 0.0);
    let measured = delivery::measure(&audio.channels, audio.rate);
    let true_peak = 20.0 * steps::true_peak(&audio).max(1e-12).log10();
    if r.chain.output.depth.is_integer() && measured.sample_peak > 0.0 {
        notes.push(
            format!(
                "clipped: its peak was {:+.1} dBFS, over what {}-bit audio holds",
                measured.sample_peak,
                r.chain.output.depth.bits()
            )
            .replace('-', "\u{2212}"),
        );
    }
    let staged = match write(shared, &audio, &dest, &mut |f| progress(last, "Writing", f)) {
        Ok(Some(staged)) => staged,
        Ok(None) => return FileState::Cancelled,
        Err(e) => return FileState::Failed(e),
    };
    let replace = r.existing == IfExists::Replace;
    let overwrite = if replace {
        Overwrite::Always
    } else {
        Overwrite::Never
    };
    let rendered = Rendered {
        path: dest.clone(),
        finished: None,
        stats: None,
    };
    match Staged::new(vec![(staged, rendered)]).commit(&overwrite) {
        Ok(_) => {}
        Err(RenderError::Exists(_)) => {
            return FileState::Failed(format!(
                "{} appeared while the file was processed: it was left as it is",
                dest.display()
            ));
        }
        Err(e) => return FileState::Failed(e.to_string()),
    }
    FileState::Done(FileResult {
        output: dest,
        sample_rate: audio.rate,
        channels: audio.channels.len(),
        seconds: audio.seconds(),
        loudness: measured.integrated,
        true_peak_db: if measured.sample_peak.is_finite() {
            true_peak
        } else {
            f64::NEG_INFINITY
        },
        sample_peak_db: measured.sample_peak,
        notes,
    })
}

/// Decode `input` at its own rate (the budget's share taken once its size
/// is known). `None`: cancelled.
#[allow(clippy::type_complexity)]
fn read<'a>(
    shared: &'a Shared,
    input: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<Option<(Audio, Option<Held<'a>>)>, String> {
    use faderframe_audio_files::import::ImportError;
    use std::cell::RefCell;
    if !input.is_file() {
        return Err(if input.exists() {
            "not a file".into()
        } else {
            "the file is not there".into()
        });
    }
    let audio = RefCell::new(Audio::default());
    let held: RefCell<Option<Held<'a>>> = RefCell::new(None);
    let total: RefCell<Option<u64>> = RefCell::new(None);
    let size = std::fs::metadata(input).map_or(0, |m| m.len());
    let result = faderframe_audio_files::decode::decode_file(
        input,
        &shared.cancel,
        |info| {
            let mut a = audio.borrow_mut();
            a.rate = info.sample_rate;
            a.channels = vec![Vec::new(); info.channels.max(1)];
            *total.borrow_mut() = info.frames;
            // Decoded size (compressed files without a length: guessed
            // from the file's size), three times over.
            let bytes = info
                .frames
                .map_or(size.saturating_mul(12), |f| f * info.channels as u64 * 4)
                .saturating_mul(3);
            if let Some(f) = info.frames {
                for c in &mut a.channels {
                    c.reserve_exact(f as usize);
                }
            }
            match shared.budget.take(bytes, &shared.cancel) {
                Some(h) => {
                    *held.borrow_mut() = Some(h);
                    Ok(())
                }
                None => Err(ImportError::Cancelled),
            }
        },
        |planes, n| {
            let mut a = audio.borrow_mut();
            for (o, p) in a.channels.iter_mut().zip(planes) {
                o.extend_from_slice(&p[..n.min(p.len())]);
            }
            if let Some(t) = *total.borrow() {
                let done = a.channels.first().map_or(0, Vec::len);
                progress(done as f32 / t.max(1) as f32);
            }
            Ok(())
        },
    );
    match result {
        Ok(_) => {}
        Err(ImportError::Cancelled) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let mut audio = audio.into_inner();
    // Planes of one length.
    let frames = audio.frames();
    for c in &mut audio.channels {
        c.truncate(frames);
    }
    if audio.rate == 0 {
        return Err("the file names no sample rate".into());
    }
    Ok(Some((audio, held.into_inner())))
}

/// The rate and channel count the chain will leave `audio` with (to name
/// it before the work).
fn shape_after(r: &BatchRequest, audio: &Audio) -> (u32, usize) {
    let (mut rate, mut channels) = (audio.rate, audio.channels.len());
    for step in &r.chain.steps {
        match step {
            BatchStep::Convert(c) => {
                if let Some(to) = c.sample_rate {
                    rate = to;
                }
                channels = match c.channels {
                    Channels::Keep => channels,
                    Channels::Mono => 1,
                    Channels::Stereo => 2,
                };
            }
            BatchStep::Plugin(p) if channels == 1 && p.plugin.widens_mono() => channels = 2,
            _ => {}
        }
    }
    (rate, channels)
}

enum Named {
    Dest(PathBuf),
    Skip(String),
}

/// The destination of file `index`, claimed for it.
fn name(
    shared: &Shared,
    index: usize,
    input: &Path,
    rate: u32,
    channels: usize,
) -> Result<Named, String> {
    let r = &shared.request;
    let cx = NameContext {
        input,
        index,
        preset: &r.preset,
        stamp: &r.stamp,
        rate,
        channels,
        output: &r.chain.output,
    };
    let stem = file_stem(&r.chain.output.pattern, &cx)?;
    let ext = r.chain.output.format.extension();
    let mut claimed = shared
        .claimed
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let first = r.folder.join(format!("{stem}.{ext}"));
    let taken_here = |p: &PathBuf, claimed: &HashSet<PathBuf>| claimed.contains(&identity(p));
    let on_disk = occupied(&first);
    if !taken_here(&first, &claimed) {
        match (on_disk, r.existing) {
            (false, _) | (true, IfExists::Replace) => {
                if !(on_disk && std::fs::metadata(&first).is_ok_and(|m| m.is_dir())) {
                    claimed.insert(identity(&first));
                    return Ok(Named::Dest(first));
                }
            }
            (true, IfExists::Skip) => {
                return Ok(Named::Skip(format!(
                    "{} is there already",
                    first
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().to_string())
                )));
            }
            (true, IfExists::Number) => {}
        }
    }
    // Numbered: "Kick 2.wav", "Kick 3.wav" …
    for n in 2..100_000 {
        let p = r.folder.join(format!("{stem} {n}.{ext}"));
        if !occupied(&p) && !taken_here(&p, &claimed) {
            claimed.insert(identity(&p));
            return Ok(Named::Dest(p));
        }
    }
    Err(format!("no free name for {stem}.{ext}"))
}

/// Write `audio` to a temporary file beside `dest`. `None`: cancelled
/// (the temporary goes).
fn write(
    shared: &Shared,
    audio: &Audio,
    dest: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<Option<StagedFile>, String> {
    let output = &shared.request.chain.output;
    let file = StagedFile::new(dest).map_err(|e| e.to_string())?;
    let frames = audio.frames();
    let channels = audio.channels.len();
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    enum Writer {
        Wav(faderframe_audio_files::bw64::Bw64Writer),
        Flac(Box<faderframe_audio_files::flac::FlacWriter>),
    }
    let mut w = match output.format {
        FileFormat::Wav => Writer::Wav(
            faderframe_audio_files::bw64::Bw64Writer::create(
                file.temp(),
                channels as u16,
                audio.rate,
                output.depth,
                output.dither,
                &[],
            )
            .map_err(io)?,
        ),
        FileFormat::Flac => Writer::Flac(Box::new(
            faderframe_audio_files::flac::FlacWriter::create(
                file.temp(),
                channels,
                audio.rate,
                output.depth.bits(),
                output.dither,
            )
            .map_err(io)?,
        )),
    };
    let mut at = 0;
    while at < frames {
        if shared.cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = WRITE_CHUNK.min(frames - at);
        let piece: Vec<&[f32]> = audio.channels.iter().map(|c| &c[at..at + n]).collect();
        match &mut w {
            Writer::Wav(w) => w.write_planar(&piece, n),
            Writer::Flac(w) => w.write_planar(&piece, n),
        }
        .map_err(io)?;
        at += n;
        progress(at as f32 / frames.max(1) as f32);
    }
    match w {
        Writer::Wav(w) => w.finish(&[]).map(drop),
        Writer::Flac(w) => w.finish(),
    }
    .map_err(io)?;
    Ok(Some(file))
}
