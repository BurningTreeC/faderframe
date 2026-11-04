//! The media browser's worker: lists folders, reads what files are from
//! their containers, and analyses their audio (tempo, key, waveform,
//! fingerprint) — one thread, below the UI's priority, started the first
//! time the browser is used. Requests replace what was asked before (a new
//! listing makes the old one's files moot); one file asked to be
//! analysed first (Find Similar) goes ahead of the rest.

use super::index::{Analysis, Facts, FileKind, Stamp, THUMB_POINTS, thumb_text};
use faderframe_audio_files::import::ImportError;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

/// Most files one listing holds (a search through a large library stops
/// there and says so).
pub const MAX_LISTED: usize = 20_000;
/// Audio analysed at most (seconds from the start).
pub const MAX_ANALYSED_SECONDS: f64 = 600.0;
/// Seconds of full-band audio the fingerprint is made from.
pub const PRINT_SECONDS: f64 = 30.0;
/// Least confidence a tempo is kept with (as clips' tempos).
pub const SURE_TEMPO: f64 = 0.1;
/// Least tonal strength a key is kept with (1 − the pitch classes'
/// entropy / ln 12: a held chord ≈ 0.5, noise ≈ 0).
pub const CLEAR_KEY: f64 = 0.08;
/// Rate the tempo and key are found at (about; a whole fraction of the
/// file's).
const LOW_RATE: f64 = 11_025.0;

/// A folder's contents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    pub dir: PathBuf,
    pub deep: bool,
    pub serial: u64,
    /// Folders directly inside (by name).
    pub folders: Vec<PathBuf>,
    /// Media files (inside folders too when deep), by path.
    pub files: Vec<(PathBuf, Stamp, FileKind)>,
    /// More than [`MAX_LISTED`] files: the rest left out.
    pub truncated: bool,
    /// The folder could not be read.
    pub error: Option<String>,
}

/// What the worker found.
#[derive(Debug)]
pub enum Found {
    Listed(Listing),
    Read {
        path: PathBuf,
        stamp: Stamp,
        facts: Result<Facts, String>,
    },
    Analysed {
        path: PathBuf,
        stamp: Stamp,
        analysis: Result<Analysis, String>,
    },
}

#[derive(Default)]
struct Queue {
    list: Option<(PathBuf, bool, u64)>,
    read: VecDeque<(PathBuf, FileKind)>,
    analyse: VecDeque<(PathBuf, FileKind)>,
    busy: bool,
    stop: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Stop the analysis running (its file is no longer wanted).
    cancel: AtomicBool,
}

/// The worker and the way back from it.
pub struct Scanner {
    shared: Arc<Shared>,
    found: Receiver<Found>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Scanner {
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let (tx, found) = channel();
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("faderframe-browser".into())
            .spawn(move || work(&s, &tx))
            .ok();
        Self {
            shared,
            found,
            thread,
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// List `dir` (and the folders inside when `deep`), replacing a
    /// listing asked for before.
    pub fn list(&self, dir: PathBuf, deep: bool, serial: u64) {
        self.queue().list = Some((dir, deep, serial));
        self.shared.wake.notify_all();
    }

    /// Read and analyse these files, in this order, instead of those asked
    /// for before (the one analysing goes on unless it is not among them).
    pub fn want(&self, read: Vec<(PathBuf, FileKind)>, analyse: Vec<(PathBuf, FileKind)>) {
        let mut q = self.queue();
        q.read = read.into();
        q.analyse = analyse.into();
        drop(q);
        self.shared.wake.notify_all();
    }

    /// Analyse `path` before anything else (and read it, if needed).
    pub fn first(&self, path: PathBuf, kind: FileKind, read: bool) {
        let mut q = self.queue();
        q.analyse.retain(|(p, _)| *p != path);
        q.analyse.push_front((path.clone(), kind));
        if read {
            q.read.retain(|(p, _)| *p != path);
            q.read.push_front((path, kind));
        }
        drop(q);
        self.shared.wake.notify_all();
    }

    /// What the worker found since the last call.
    pub fn take(&self) -> Vec<Found> {
        self.found.try_iter().collect()
    }

    /// Nothing asked for is left (and nothing is being worked on).
    pub fn idle(&self) -> bool {
        let q = self.queue();
        !q.busy && q.list.is_none() && q.read.is_empty() && q.analyse.is_empty()
    }

    /// Files still to read and to analyse.
    pub fn pending(&self) -> (usize, usize) {
        let q = self.queue();
        (q.read.len(), q.analyse.len() + usize::from(q.busy))
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.queue().stop = true;
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

enum Job {
    List(PathBuf, bool, u64),
    Read(Vec<(PathBuf, FileKind)>),
    Analyse(PathBuf, FileKind),
}

fn work(shared: &Shared, tx: &Sender<Found>) {
    faderframe_realtime::priority::lower_this_thread();
    loop {
        let job = {
            let mut q = shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.busy = false;
            loop {
                if q.stop {
                    return;
                }
                if let Some((d, deep, serial)) = q.list.take() {
                    break Job::List(d, deep, serial);
                }
                if !q.read.is_empty() {
                    let n = q.read.len().min(32);
                    break Job::Read(q.read.drain(..n).collect());
                }
                if let Some((p, k)) = q.analyse.pop_front() {
                    break Job::Analyse(p, k);
                }
                q = shared
                    .wake
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if let Ok(mut q) = shared.queue.lock() {
            q.busy = true;
        }
        shared.cancel.store(false, Ordering::Relaxed);
        let sent = match job {
            Job::List(dir, deep, serial) => {
                tx.send(Found::Listed(list(&dir, deep, serial))).is_ok()
            }
            Job::Read(files) => files.into_iter().all(|(path, kind)| {
                let stamp = Stamp::read(&path).unwrap_or_default();
                let facts = read(&path, kind);
                tx.send(Found::Read { path, stamp, facts }).is_ok()
            }),
            Job::Analyse(path, kind) => {
                let stamp = Stamp::read(&path).unwrap_or_default();
                let analysis = analyse(&path, kind, &shared.cancel);
                tx.send(Found::Analysed {
                    path,
                    stamp,
                    analysis,
                })
                .is_ok()
            }
        };
        if !sent {
            return;
        }
    }
}

fn hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// The media in `dir` (and below it when `deep`), and its folders.
pub fn list(dir: &Path, deep: bool, serial: u64) -> Listing {
    let mut out = Listing {
        dir: dir.to_path_buf(),
        deep,
        serial,
        ..Listing::default()
    };
    let mut todo = vec![dir.to_path_buf()];
    let mut first = true;
    while let Some(d) = todo.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) => {
                if first {
                    out.error = Some(format!("{}: {e}", d.display()));
                }
                continue;
            }
        };
        let mut inner = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            if hidden(&path) {
                continue;
            }
            // Links to folders are listed but never followed down (no
            // loops); links to files are their files.
            let link = e.file_type().is_ok_and(|t| t.is_symlink());
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if first {
                    out.folders.push(path.clone());
                }
                if deep && !link {
                    inner.push(path);
                }
            } else if let Some(kind) = FileKind::of(&path) {
                if out.files.len() >= MAX_LISTED {
                    out.truncated = true;
                    break;
                }
                out.files.push((path, Stamp::of(&meta), kind));
            }
        }
        first = false;
        if out.truncated {
            break;
        }
        inner.sort();
        // Depth first, in name order.
        todo.extend(inner.into_iter().rev());
    }
    out.folders.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    out
}

/// The facts of a file.
pub fn read(path: &Path, kind: FileKind) -> Result<Facts, String> {
    match kind {
        FileKind::Audio => {
            let f = faderframe_audio_files::decode::probe(path).map_err(|e| e.to_string())?;
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_uppercase();
            let format = match ext.as_str() {
                "WAVE" => "WAV".to_string(),
                "AIF" | "AIFC" => "AIFF".to_string(),
                "OGA" => "OGG".to_string(),
                _ => ext,
            };
            Ok(Facts {
                format,
                bits: f.bits,
                channels: f.channels as u16,
                rate: f.sample_rate,
                seconds: f
                    .frames
                    .map_or(0.0, |n| n as f64 / f64::from(f.sample_rate.max(1))),
            })
        }
        FileKind::Midi => {
            let s = super::smf::read(path)?;
            Ok(Facts {
                format: format!("MIDI {}", s.format),
                bits: None,
                channels: s.channels,
                rate: 0,
                seconds: s.seconds(),
            })
        }
    }
}

/// How clearly a pitch-class profile holds a key (0 none … 1 one note).
pub fn tonal_strength(profile: &[f64; 12]) -> f64 {
    let sum: f64 = profile.iter().sum();
    if sum <= 0.0 {
        return 0.0;
    }
    let h: f64 = profile
        .iter()
        .map(|p| p / sum)
        .filter(|p| *p > 0.0)
        .map(|p| -p * p.ln())
        .sum();
    1.0 - h / 12f64.ln()
}

/// What a file is found to be.
pub fn analyse(path: &Path, kind: FileKind, cancel: &AtomicBool) -> Result<Analysis, String> {
    match kind {
        FileKind::Audio => analyse_audio(path, cancel),
        FileKind::Midi => {
            let s = super::smf::read(path)?;
            Ok(Analysis {
                bpm: Some(s.bpm()),
                key: s.key(),
                thumb: thumb_text(&s.density(THUMB_POINTS)),
                print: Vec::new(),
                seconds: Some(s.seconds()),
            })
        }
    }
}

/// Sounds shorter than this are heard round as loops for their tempo.
const TILE_UNDER: f64 = 8.0;
/// Shorter than this: no tempo (a hit).
const SHORTEST_LOOP: f64 = 1.5;

/// The tempo of mono `x` at `rate`: a long recording's as it is heard; a
/// short one (a loop of a bar or two is shorter than the detector needs)
/// heard round and round, and kept only when its length is then an even
/// number of beats — so a hit is not given the tempo of its own length.
pub fn tempo_of(x: &[f32], rate: f64) -> Option<f64> {
    let seconds = x.len() as f64 / rate.max(1.0);
    let sure = |t: faderframe_analysis::tempo::Tempo| (t.confidence >= SURE_TEMPO).then_some(t.bpm);
    if seconds >= TILE_UNDER {
        return faderframe_analysis::tempo::detect(x, rate).and_then(sure);
    }
    if seconds < SHORTEST_LOOP {
        return None;
    }
    let copies = (TILE_UNDER / seconds).ceil() as usize + 1;
    let round: Vec<f32> = x.iter().copied().cycle().take(x.len() * copies).collect();
    // The beat must be in the sound itself (a held pad heard round has a
    // "beat" where it wraps): an onset every other beat at least.
    let onsets = faderframe_analysis::sound_print::onset_count(x, rate);
    faderframe_analysis::tempo::detect(&round, rate)
        .and_then(sure)
        .filter(|bpm| super::is_loop(Some(*bpm), Some(seconds)))
        .filter(|bpm| {
            let beats = seconds * bpm / 60.0;
            onsets >= 2 && onsets as f64 >= (beats / 2.0).floor()
        })
}

/// Peaks are kept a block of this many frames.
const PEAK_BLOCK: usize = 256;

fn analyse_audio(path: &Path, cancel: &AtomicBool) -> Result<Analysis, String> {
    use std::cell::{Cell, RefCell};
    let rate = Cell::new(0u32);
    let channels = Cell::new(1usize);
    let decimate = Cell::new(1usize);
    // Full band (the start), decimated (all), block peaks (all).
    let head: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    let low: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    let peaks: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    let acc = Cell::new((0.0f32, 0usize, 0.0f32, 0usize));
    let frames = Cell::new(0u64);
    let full = Cell::new(false);
    let result = faderframe_audio_files::decode::decode_file(
        path,
        cancel,
        |info| {
            rate.set(info.sample_rate.max(1));
            channels.set(info.channels.max(1));
            decimate.set(((f64::from(info.sample_rate) / LOW_RATE).floor() as usize).max(1));
            Ok(())
        },
        |planes, n| {
            let r = f64::from(rate.get());
            let head_max = (r * PRINT_SECONDS) as usize;
            let limit = (r * MAX_ANALYSED_SECONDS) as u64;
            let d = decimate.get();
            let c = channels.get().min(planes.len()).max(1);
            let (mut sum, mut count, mut peak, mut in_block) = acc.get();
            let mut head = head.borrow_mut();
            let mut low = low.borrow_mut();
            let mut peaks = peaks.borrow_mut();
            for i in 0..n {
                let v = planes[..c]
                    .iter()
                    .map(|p| p.get(i).copied().unwrap_or(0.0))
                    .sum::<f32>()
                    / c as f32;
                if head.len() < head_max {
                    head.push(v);
                }
                sum += v;
                count += 1;
                if count == d {
                    low.push(sum / d as f32);
                    sum = 0.0;
                    count = 0;
                }
                peak = peak.max(v.abs());
                in_block += 1;
                if in_block == PEAK_BLOCK {
                    peaks.push(peak);
                    peak = 0.0;
                    in_block = 0;
                }
            }
            acc.set((sum, count, peak, in_block));
            frames.set(frames.get() + n as u64);
            if frames.get() >= limit {
                full.set(true);
                // Enough: the rest is not analysed.
                return Err(ImportError::Cancelled);
            }
            Ok(())
        },
    );
    match result {
        Ok(_) => {}
        Err(ImportError::Cancelled) if full.get() => {}
        Err(e) => return Err(e.to_string()),
    }
    let (_, _, peak, in_block) = acc.get();
    let mut peaks = peaks.into_inner();
    if in_block > 0 {
        peaks.push(peak);
    }
    let r = f64::from(rate.get().max(1));
    let low = low.into_inner();
    let low_rate = r / decimate.get() as f64;
    let bpm = tempo_of(&low, low_rate);
    let profile = faderframe_analysis::chroma::profile(&low, low_rate);
    let key = (tonal_strength(&profile) >= CLEAR_KEY)
        .then(|| faderframe_midi::theory::detect_key(&profile))
        .flatten();
    let print = faderframe_analysis::sound_print::analyse(&head.into_inner(), r)
        .map(|p| p.to_values())
        .unwrap_or_default()
        .into_iter()
        // Two decimals are plenty (a smaller index).
        .map(|v| (v * 100.0).round() / 100.0)
        .collect();
    Ok(Analysis {
        bpm,
        key,
        thumb: thumb_text(&thumbnail(&peaks)),
        print,
        seconds: (!full.get()).then(|| frames.get() as f64 / r),
    })
}

/// Block peaks as [`THUMB_POINTS`] levels, the loudest at 1.
fn thumbnail(peaks: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; THUMB_POINTS];
    if peaks.is_empty() {
        return out;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let a = i * peaks.len() / THUMB_POINTS;
        let b = ((i + 1) * peaks.len() / THUMB_POINTS)
            .max(a + 1)
            .min(peaks.len());
        *o = peaks[a.min(peaks.len() - 1)..b]
            .iter()
            .copied()
            .fold(0.0, f32::max);
    }
    let top = out.iter().copied().fold(0.0f32, f32::max);
    if top > 0.0 {
        for v in &mut out {
            *v /= top;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_chord_holds_a_key_and_noise_does_not() {
        let rate = 44_100.0;
        let chord: Vec<f32> = (0..(rate * 4.0) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                [261.63, 329.63, 392.0]
                    .iter()
                    .map(|f| (std::f64::consts::TAU * f * t).sin() * 0.2)
                    .sum::<f64>() as f32
            })
            .collect();
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let noise: Vec<f32> = (0..(rate * 4.0) as usize)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32 * 0.3
            })
            .collect();
        let c = tonal_strength(&faderframe_analysis::chroma::profile(&chord, rate));
        let n = tonal_strength(&faderframe_analysis::chroma::profile(&noise, rate));
        assert!(c > 0.3, "the chord: {c}");
        assert!(n < CLEAR_KEY, "noise: {n}");
    }

    /// Kicks on the beats at `bpm` for `beats` (or a single hit).
    fn kicks(rate: f64, bpm: f64, beats: usize) -> Vec<f32> {
        let beat = (rate * 60.0 / bpm) as usize;
        (0..beat * beats)
            .map(|i| {
                let t = (i % beat) as f64 / rate;
                let f = 55.0 * (1.0 + 2.0 * (-t * 40.0).exp());
                ((std::f64::consts::TAU * f * t).sin() * (-t * 14.0).exp() * 0.8) as f32
            })
            .collect()
    }

    #[test]
    fn short_loops_have_a_tempo_and_hits_do_not() {
        let rate = 11_025.0;
        // A bar and two bars at 120, a bar at 96.
        for (bpm, beats) in [(120.0, 4), (120.0, 8), (96.0, 4)] {
            let t = tempo_of(&kicks(rate, bpm, beats), rate).unwrap();
            assert!((t - bpm).abs() < 0.5, "{bpm} × {beats}: {t}");
        }
        // A hit of a second and a half: no tempo of its own length.
        let mut hit = kicks(rate, 40.0, 1);
        hit.truncate((rate * 1.5) as usize);
        assert_eq!(tempo_of(&hit, rate), None);
        assert_eq!(tempo_of(&kicks(rate, 120.0, 1), rate), None);
        // A held chord heard round wraps on a "beat": not a loop.
        let pad: Vec<f32> = (0..(rate * 4.0) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                [293.66, 369.99, 440.0]
                    .iter()
                    .map(|f| (std::f64::consts::TAU * f * t).sin() * 0.2)
                    .sum::<f64>() as f32
            })
            .collect();
        assert_eq!(tempo_of(&pad, rate), None);
    }

    #[test]
    fn a_deep_listing_finds_media_below_and_leaves_the_rest() {
        let dir = std::env::temp_dir().join(format!("ff-browser-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Drums/Kicks")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        for f in [
            "a.wav",
            "notes.txt",
            "Drums/b.flac",
            "Drums/Kicks/c.mid",
            ".hidden/d.wav",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let flat = list(&dir, false, 1);
        assert_eq!(flat.folders, vec![dir.join("Drums")]);
        assert_eq!(flat.files.len(), 1);
        let deep = list(&dir, true, 2);
        let mut names: Vec<_> = deep.files.iter().map(|f| f.0.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                dir.join("Drums/Kicks/c.mid"),
                dir.join("Drums/b.flac"),
                dir.join("a.wav")
            ]
        );
        assert_eq!(
            deep.files.iter().filter(|f| f.2 == FileKind::Midi).count(),
            1
        );
        assert!(list(&dir.join("nowhere"), false, 3).error.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
