//! The frame service: decoders on their own threads, a frame cache within a
//! memory budget, and answers that never wait. Whoever shows pictures asks
//! for the frame at a time and gets the best one there is now (exact, or
//! the nearest decoded so far); asking also tells the threads what to
//! decode next:
//!
//! - the player reads ahead in order while playing (never seeking unless
//!   the position jumps);
//! - the seeker serves the newest locate or scrub only, a keyframe first
//!   where the exact frame is far from one (long GOPs without a proxy);
//! - the thumbnailer makes filmstrip pictures (keyframes where that is
//!   faster).
//!
//! Playing and scrubbing read the proxy when there is one; a still picture
//! larger than the proxy is decoded from the original once the seeker is
//! free ("sharp when stopped").

use crate::decode::{Decoder, Frame};
use crate::index::FrameIndex;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The caller's name for a video (a project's source id).
pub type Key = u64;

/// A video the service decodes.
#[derive(Clone, Debug)]
pub struct Media {
    pub original: PathBuf,
    /// The original's frame times.
    pub index: Arc<FrameIndex>,
    /// The original's stored size and pixel aspect.
    pub size: (u32, u32),
    pub par: (u32, u32),
    /// Its proxy and the proxy's size, once made (same frame times).
    pub proxy: Option<(PathBuf, (u32, u32))>,
}

impl Media {
    /// The size the picture is shown at within `max` (aspect kept).
    pub fn fit(&self, max: (u32, u32)) -> (u32, u32) {
        crate::fit(self.size.0, self.size.1, self.par, max.0, max.1)
    }
}

/// A frame to show.
#[derive(Clone, Debug)]
pub struct Picture {
    pub frame: Arc<Frame>,
    /// Which frame of the video it is.
    pub number: usize,
    /// Whether it is the frame asked for (else the nearest so far).
    pub exact: bool,
}

/// What the picture is wanted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Playing: frames in order.
    Play,
    /// Still or scrubbing: this frame.
    Still,
}

/// Cache key: video, frame, width.
type Slot = (Key, u32, u32);

struct Cached {
    frame: Arc<Frame>,
    used: u64,
}

#[derive(Default)]
struct Cache {
    frames: BTreeMap<Slot, Cached>,
    bytes: usize,
    clock: u64,
    /// Frames in video memory (kept to `MAX_GPU_FRAMES`).
    gpu: usize,
}

/// Dmabuf frames kept at most (video memory: a second or two of picture).
const MAX_GPU_FRAMES: usize = 48;

impl Cache {
    fn put(&mut self, slot: Slot, frame: Arc<Frame>, budget: usize) {
        self.clock += 1;
        let size = frame.bytes();
        let gpu = frame.gpu.is_some();
        if let Some(old) = self.frames.insert(
            slot,
            Cached {
                frame,
                used: self.clock,
            },
        ) {
            self.forget(&old);
        }
        self.bytes += size;
        self.gpu += usize::from(gpu);
        while self.bytes > budget && self.frames.len() > 1 {
            let Some((&oldest, _)) = self.frames.iter().min_by_key(|(_, c)| c.used) else {
                break;
            };
            if let Some(c) = self.frames.remove(&oldest) {
                self.forget(&c);
            }
        }
        while self.gpu > MAX_GPU_FRAMES {
            let Some((&oldest, _)) = self
                .frames
                .iter()
                .filter(|(_, c)| c.frame.gpu.is_some())
                .min_by_key(|(_, c)| c.used)
            else {
                break;
            };
            if let Some(c) = self.frames.remove(&oldest) {
                self.forget(&c);
            }
        }
    }

    /// Account for a frame that left.
    fn forget(&mut self, c: &Cached) {
        self.bytes -= c.frame.bytes();
        self.gpu -= usize::from(c.frame.gpu.is_some());
    }

    fn get(&mut self, slot: Slot) -> Option<Arc<Frame>> {
        self.clock += 1;
        let clock = self.clock;
        self.frames.get_mut(&slot).map(|c| {
            c.used = clock;
            Arc::clone(&c.frame)
        })
    }

    /// Any width of frame `n`, the widest first.
    fn any_width(&mut self, key: Key, n: u32) -> Option<Arc<Frame>> {
        let found = self
            .frames
            .range((key, n, 0)..=(key, n, u32::MAX))
            .next_back()
            .map(|(s, _)| *s)?;
        self.get(found)
    }

    /// The nearest frame at or before `n` (any width) after `from`.
    fn before(&mut self, key: Key, from: u32, n: u32) -> Option<(u32, Arc<Frame>)> {
        let found = self
            .frames
            .range((key, from, 0)..=(key, n, u32::MAX))
            .next_back()
            .map(|(s, _)| *s)?;
        self.get(found).map(|f| (found.1, f))
    }

    /// The nearest frame after `n` (any width) up to `to`.
    fn after(&mut self, key: Key, n: u32, to: u32) -> Option<(u32, Arc<Frame>)> {
        let found = self
            .frames
            .range((key, n.saturating_add(1), 0)..=(key, to, u32::MAX))
            .next()
            .map(|(s, _)| *s)?;
        self.get(found).map(|f| (found.1, f))
    }

    fn drop_video(&mut self, key: Key) {
        let gone: Vec<Slot> = self
            .frames
            .range((key, 0, 0)..=(key, u32::MAX, u32::MAX))
            .map(|(s, _)| *s)
            .collect();
        for s in gone {
            if let Some(c) = self.frames.remove(&s) {
                self.forget(&c);
            }
        }
    }
}

/// A frame wanted of a video (the newest asked).
#[derive(Clone, Copy, Debug)]
struct Target {
    n: u32,
    size: (u32, u32),
    at: Instant,
}

/// A target nobody asked for this long is not shown any more.
const STALE: Duration = Duration::from_millis(400);

impl Target {
    fn fresh(&self) -> bool {
        self.at.elapsed() < STALE
    }
}

/// What the threads are asked to do.
#[derive(Default)]
struct Asked {
    media: HashMap<Key, Media>,
    /// Playing, per video (several play at once side by side).
    play: HashMap<Key, Target>,
    /// Still or scrubbing, per video.
    still: HashMap<Key, Target>,
    /// Thumbnails wanted: video, frame, height (newest last).
    thumbs: Vec<(Key, u32, u32)>,
    errors: HashMap<Key, String>,
    /// Bumped on every change, so threads see news.
    generation: u64,
}

struct Shared {
    cache: Mutex<Cache>,
    thumbs: Mutex<Cache>,
    asked: Mutex<Asked>,
    /// Files whose picture does not decode into dmabufs.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    no_dmabuf: Mutex<std::collections::HashSet<PathBuf>>,
    wake: Condvar,
    stop: AtomicBool,
    budget: usize,
    thumb_budget: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Frames read ahead of the one showing while playing.
const AHEAD: u32 = 12;

pub struct FrameService {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl FrameService {
    /// A service keeping up to `budget` bytes of frames (and a tenth of it
    /// of thumbnails).
    pub fn new(budget: usize) -> Self {
        let shared = Arc::new(Shared {
            cache: Mutex::default(),
            thumbs: Mutex::default(),
            asked: Mutex::default(),
            no_dmabuf: Mutex::default(),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            budget,
            thumb_budget: budget / 10,
        });
        let mut threads = Vec::new();
        for (name, job) in [
            ("faderframe-video-play", player as fn(&Shared)),
            ("faderframe-video-seek", seeker),
            ("faderframe-video-thumbs", thumbnailer),
        ] {
            let s = Arc::clone(&shared);
            match std::thread::Builder::new()
                .name(name.into())
                .spawn(move || job(&s))
            {
                Ok(t) => threads.push(t),
                Err(e) => tracing::error!("cannot start {name}: {e}"),
            }
        }
        Self { shared, threads }
    }

    fn ask(&self, f: impl FnOnce(&mut Asked)) {
        let mut a = lock(&self.shared.asked);
        f(&mut a);
        a.generation += 1;
        drop(a);
        self.shared.wake.notify_all();
    }

    /// Decode `media` as `key` (replacing what was there, e.g. once its
    /// proxy is made).
    pub fn set_media(&self, key: Key, media: Media) {
        let proxy_changed = lock(&self.shared.asked)
            .media
            .get(&key)
            .is_none_or(|m| m.proxy.is_some() != media.proxy.is_some());
        self.ask(|a| {
            a.errors.remove(&key);
            a.media.insert(key, media);
        });
        if proxy_changed {
            // Frames stay valid (same times); thumbnails may sharpen.
            lock(&self.shared.thumbs).drop_video(key);
        }
    }

    pub fn remove(&self, key: Key) {
        self.ask(|a| {
            a.media.remove(&key);
            a.errors.remove(&key);
        });
        lock(&self.shared.cache).drop_video(key);
        lock(&self.shared.thumbs).drop_video(key);
    }

    /// Why `key` cannot be decoded, if it cannot.
    pub fn error(&self, key: Key) -> Option<String> {
        lock(&self.shared.asked).errors.get(&key).cloned()
    }

    /// The frame of `key` showing at `t` (ns in the file's timeline), at
    /// `max` size at most: the best there is now; asking also has it
    /// decoded. `None` outside the video or before anything is decoded.
    pub fn picture(&self, key: Key, t: i64, max: (u32, u32), want: Want) -> Option<Picture> {
        let (n, size) = {
            let a = lock(&self.shared.asked);
            let m = a.media.get(&key)?;
            (m.index.frame_at(t)? as u32, m.fit(max))
        };
        let exact = {
            let mut c = lock(&self.shared.cache);
            c.get((key, n, size.0)).or_else(|| {
                // A proxy-size frame stands in until the sharp one.
                c.any_width(key, n).filter(|_| want == Want::Play)
            })
        };
        self.ask(|a| {
            let t = Target {
                n,
                size,
                at: Instant::now(),
            };
            match want {
                Want::Play => {
                    a.play.insert(key, t);
                    a.still.remove(&key);
                }
                Want::Still => {
                    a.still.insert(key, t);
                    a.play.remove(&key);
                }
            }
        });
        if let Some(frame) = exact {
            let sharp = frame.width >= size.0;
            return Some(Picture {
                frame,
                number: n as usize,
                exact: sharp || want == Want::Play,
            });
        }
        let mut c = lock(&self.shared.cache);
        // The nearest frame there is: before it, or (running in reverse)
        // after it.
        let near = c.any_width(key, n).map(|f| (n, f)).or_else(|| {
            let before = c.before(key, n.saturating_sub(250), n);
            let after = c.after(key, n, n.saturating_add(250));
            match (before, after) {
                (Some(b), Some(a)) => Some(if n - b.0 <= a.0 - n { b } else { a }),
                (b, a) => b.or(a),
            }
        });
        near.map(|(m, frame)| Picture {
            frame,
            number: m as usize,
            exact: false,
        })
    }

    /// A filmstrip picture of `key` at frame `n`, `height` pixels high
    /// (`None` until made; asking has it made).
    pub fn thumbnail(&self, key: Key, n: usize, height: u32) -> Option<Arc<Frame>> {
        let slot = (key, n as u32, height);
        if let Some(f) = lock(&self.shared.thumbs).get(slot) {
            return Some(f);
        }
        self.ask(|a| {
            if !a.thumbs.contains(&(key, n as u32, height)) {
                a.thumbs.push((key, n as u32, height));
                // The newest views matter: drop what scrolled away.
                if a.thumbs.len() > 256 {
                    a.thumbs.remove(0);
                }
            }
        });
        None
    }

    /// Forget what was asked (the transport stopped showing pictures).
    pub fn idle(&self) {
        self.ask(|a| {
            a.play.clear();
            a.still.clear();
        });
    }
}

impl Drop for FrameService {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// An open decoder: which video, which file, what size.
struct Open {
    key: Key,
    file: PathBuf,
    size: (u32, u32),
    /// Decoding into dmabufs.
    gpu: bool,
    decoder: Decoder,
}

/// Wait for news (or 200 ms); `None` when stopping.
fn wait<'a>(s: &'a Shared, seen: u64) -> Option<MutexGuard<'a, Asked>> {
    let mut a = lock(&s.asked);
    while a.generation == seen && !s.stop.load(Ordering::Relaxed) {
        let (next, _) = s
            .wake
            .wait_timeout(a, Duration::from_millis(200))
            .unwrap_or_else(|p| p.into_inner());
        a = next;
        if a.generation == seen {
            // Timed out: let the caller look again anyway.
            break;
        }
    }
    (!s.stop.load(Ordering::Relaxed)).then_some(a)
}

/// The decoder for `file` at `size`, reusing `open` when it is that one;
/// for playback (`play`) into dmabufs where the display takes them.
fn decoder_for<'a>(
    open: &'a mut Option<Open>,
    key: Key,
    file: &PathBuf,
    size: (u32, u32),
    play: bool,
    s: &Shared,
) -> Option<&'a mut Decoder> {
    let gpu = play && zero_copy_for(file, s);
    let same = open
        .as_ref()
        .is_some_and(|o| o.key == key && &o.file == file && o.size == size && o.gpu == gpu);
    if !same {
        *open = None;
        let opened = if gpu {
            open_dmabuf(file, size, s).map(|d| (d, true))
        } else {
            None
        };
        let opened = match opened {
            Some(d) => Ok(d),
            None => Decoder::open(file, size.0, size.1).map(|d| (d, false)),
        };
        match opened {
            Ok((decoder, gpu)) => {
                *open = Some(Open {
                    key,
                    file: file.clone(),
                    size,
                    gpu,
                    decoder,
                });
            }
            Err(e) => {
                tracing::warn!("{}: {e}", file.display());
                lock(&s.asked).errors.insert(key, e.to_string());
                return None;
            }
        }
    }
    open.as_mut().map(|o| &mut o.decoder)
}

/// Whether `file` plays into dmabufs (zero-copy on, and it has not failed
/// for this file).
fn zero_copy_for(file: &PathBuf, s: &Shared) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::zero_copy::enabled() && !lock(&s.no_dmabuf).contains(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, s);
        false
    }
}

/// A dmabuf decoder of `file`, or `None` (then never again for it).
fn open_dmabuf(file: &PathBuf, size: (u32, u32), s: &Shared) -> Option<Decoder> {
    #[cfg(target_os = "linux")]
    match Decoder::open_dmabuf(file, size.0, size.1) {
        Ok(d) => return Some(d),
        Err(e) => {
            tracing::info!("{}: frames through memory ({e})", file.display());
            lock(&s.no_dmabuf).insert(file.clone());
        }
    }
    let _ = (file, size, s);
    None
}

/// Which file and size playing and scrubbing read: the proxy when there is
/// one (at its size, or smaller), else the original at the size wanted.
fn moving_source(m: &Media, size: (u32, u32)) -> (PathBuf, (u32, u32)) {
    match &m.proxy {
        Some((path, proxy)) => {
            let fit = crate::fit(proxy.0, proxy.1, (1, 1), size.0, size.1);
            (path.clone(), fit)
        }
        None => (m.original.clone(), size),
    }
}

/// A video the player reads ahead: its decoder and the next frame it
/// gives.
#[derive(Default)]
struct Lane {
    open: Option<Open>,
    cursor: Option<u32>,
}

/// The frame wanted of `key` now (fresh targets only).
fn wanted(s: &Shared, key: Key, play: bool) -> Option<(u32, (u32, u32))> {
    let a = lock(&s.asked);
    let map = if play { &a.play } else { &a.still };
    map.get(&key).filter(|t| t.fresh()).map(|t| (t.n, t.size))
}

/// One step of reading `key` ahead: a frame decoded (true), or nothing to
/// do now.
fn play_step(s: &Shared, lane: &mut Lane, key: Key, m: &Media) -> bool {
    let Some((want, size)) = wanted(s, key, true) else {
        lane.cursor = None;
        return false;
    };
    let (file, dsize) = moving_source(m, size);
    let cached = |n: u32| lock(&s.cache).get((key, n, dsize.0)).is_some();
    let at = match lane.cursor {
        Some(c) if c >= want && c <= want + AHEAD + 2 => c,
        Some(c) if c < want && want - c <= 2 => c,
        _ => {
            // A jump (or the start): play from the frame wanted, unless the
            // frames ahead are there already.
            let mut n = want;
            while n < want + AHEAD && cached(n) {
                n += 1;
            }
            if n >= want + AHEAD {
                return false;
            }
            let Some(t) = m.index.times.get(want as usize).copied() else {
                return false;
            };
            let Some(d) = decoder_for(&mut lane.open, key, &file, dsize, true, s) else {
                return false;
            };
            if let Err(e) = d.play_from(t) {
                tracing::warn!("{}: {e}", file.display());
                return false;
            }
            lane.cursor = Some(want);
            want
        }
    };
    if at > want + AHEAD {
        return false;
    }
    let Some(d) = decoder_for(&mut lane.open, key, &file, dsize, true, s) else {
        return false;
    };
    match d.next_frame() {
        Ok(Some(f)) => {
            lock(&s.cache).put((key, at, dsize.0), Arc::new(f), s.budget);
            lane.cursor = Some(at + 1);
            true
        }
        Ok(None) => {
            lane.cursor = None;
            false
        }
        Err(e) => {
            tracing::warn!("{}: {e}", file.display());
            lane.cursor = None;
            lane.open = None;
            false
        }
    }
}

/// The videos with fresh targets in `map`, with their media.
fn targets(a: &Asked, play: bool) -> Vec<(Key, Media)> {
    let map = if play { &a.play } else { &a.still };
    map.iter()
        .filter(|(_, t)| t.fresh())
        .filter_map(|(k, _)| a.media.get(k).map(|m| (*k, m.clone())))
        .collect()
}

fn player(s: &Shared) {
    let mut lanes: HashMap<Key, Lane> = HashMap::new();
    let mut seen = 0;
    loop {
        let Some(a) = wait(s, seen) else { return };
        seen = a.generation;
        let playing = targets(&a, true);
        drop(a);
        lanes.retain(|k, _| playing.iter().any(|(p, _)| p == k));
        // Every video playing read ahead in turn, a frame each, until all
        // are far enough ahead.
        loop {
            if s.stop.load(Ordering::Relaxed) {
                return;
            }
            let mut any = false;
            for (key, m) in &playing {
                let lane = lanes.entry(*key).or_default();
                any |= play_step(s, lane, *key, m);
            }
            if !any {
                break;
            }
        }
    }
}

/// The decoders a still picture of one video uses: what scrubbing reads,
/// and the original for the sharp picture.
#[derive(Default)]
struct Stills {
    moving: Option<Open>,
    sharp: Option<Open>,
}

fn seeker(s: &Shared) {
    let mut decoders: HashMap<Key, Stills> = HashMap::new();
    let mut seen = 0;
    loop {
        let Some(a) = wait(s, seen) else { return };
        seen = a.generation;
        let stills = targets(&a, false);
        drop(a);
        decoders.retain(|k, _| stills.iter().any(|(p, _)| p == k));
        for (key, m) in &stills {
            let d = decoders.entry(*key).or_default();
            still_step(s, d, *key, m);
        }
    }
}

/// The still frame wanted of `key`: from what scrubbing reads (a keyframe
/// first where the exact one is far), then sharp from the original.
fn still_step(s: &Shared, d: &mut Stills, key: Key, m: &Media) {
    let Some((n, size)) = wanted(s, key, false) else {
        return;
    };
    let Some(t) = m.index.times.get(n as usize).copied() else {
        return;
    };
    let (file, dsize) = moving_source(m, size);
    let have = |w: u32| lock(&s.cache).get((key, n, w)).is_some();
    let newer = || wanted(s, key, false).is_none_or(|(m2, _)| m2 != n);
    if !have(dsize.0) {
        let far = m.proxy.is_none() && n as usize - m.index.key_before(n as usize) > 2;
        let k = m.index.key_before(n as usize);
        if far
            && !have_frame(s, key, k as u32, dsize.0)
            && let Some(&kt) = m.index.times.get(k)
            && let Some(dec) = decoder_for(&mut d.moving, key, &file, dsize, false, s)
            && let Ok(Some(f)) = dec.frame_at(kt, false)
        {
            lock(&s.cache).put((key, k as u32, dsize.0), Arc::new(f), s.budget);
        }
        if newer() {
            return;
        }
        if let Some(dec) = decoder_for(&mut d.moving, key, &file, dsize, false, s) {
            match dec.frame_at(t, true) {
                Ok(Some(f)) => lock(&s.cache).put((key, n, dsize.0), Arc::new(f), s.budget),
                Ok(None) => {}
                Err(e) => tracing::warn!("{}: {e}", file.display()),
            }
        }
    }
    // Sharp when stopped: the original at the full size wanted.
    if dsize != size && !have(size.0) && !newer() {
        let original = m.original.clone();
        if let Some(dec) = decoder_for(&mut d.sharp, key, &original, size, false, s) {
            match dec.frame_at(t, true) {
                Ok(Some(f)) => lock(&s.cache).put((key, n, size.0), Arc::new(f), s.budget),
                Ok(None) => {}
                Err(e) => tracing::warn!("{}: {e}", original.display()),
            }
        }
    }
}

fn have_frame(s: &Shared, key: Key, n: u32, width: u32) -> bool {
    lock(&s.cache).get((key, n, width)).is_some()
}

fn thumbnailer(s: &Shared) {
    let mut open: Option<Open> = None;
    let mut seen = 0;
    loop {
        let Some(mut a) = wait(s, seen) else { return };
        seen = a.generation;
        while let Some((key, n, height)) = a.thumbs.pop() {
            let Some(m) = a.media.get(&key).cloned() else {
                continue;
            };
            drop(a);
            let slot = (key, n, height);
            if lock(&s.thumbs).get(slot).is_none()
                && let Some(&t) = m.index.times.get(n as usize)
            {
                let (file, exact) = match &m.proxy {
                    Some((p, _)) => (p.clone(), true),
                    None => (m.original.clone(), m.index.longest_gop() <= 1),
                };
                let size = m.fit((u32::MAX, height));
                if let Some(d) = decoder_for(&mut open, key, &file, size, false, s) {
                    match d.frame_at(t, exact) {
                        Ok(Some(f)) => {
                            lock(&s.thumbs).put(slot, Arc::new(f), s.thumb_budget);
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("{}: {e}", file.display()),
                    }
                }
            }
            if s.stop.load(Ordering::Relaxed) {
                return;
            }
            a = lock(&s.asked);
        }
    }
}
