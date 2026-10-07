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
use std::time::Duration;

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
}

impl Cache {
    fn put(&mut self, slot: Slot, frame: Arc<Frame>, budget: usize) {
        self.clock += 1;
        let size = frame.rgba.len();
        if let Some(old) = self.frames.insert(
            slot,
            Cached {
                frame,
                used: self.clock,
            },
        ) {
            self.bytes -= old.frame.rgba.len();
        }
        self.bytes += size;
        while self.bytes > budget && self.frames.len() > 1 {
            let Some((&oldest, _)) = self.frames.iter().min_by_key(|(_, c)| c.used) else {
                break;
            };
            if let Some(c) = self.frames.remove(&oldest) {
                self.bytes -= c.frame.rgba.len();
            }
        }
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

    fn drop_video(&mut self, key: Key) {
        let gone: Vec<Slot> = self
            .frames
            .range((key, 0, 0)..=(key, u32::MAX, u32::MAX))
            .map(|(s, _)| *s)
            .collect();
        for s in gone {
            if let Some(c) = self.frames.remove(&s) {
                self.bytes -= c.frame.rgba.len();
            }
        }
    }
}

/// What the threads are asked to do.
#[derive(Default)]
struct Asked {
    media: HashMap<Key, Media>,
    /// Playing: video, frame, size (latest only).
    play: Option<(Key, u32, (u32, u32))>,
    /// Still: video, frame, size (latest only).
    still: Option<(Key, u32, (u32, u32))>,
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
        self.ask(|a| match want {
            Want::Play => {
                a.play = Some((key, n, size));
                a.still = None;
            }
            Want::Still => {
                a.still = Some((key, n, size));
                a.play = None;
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
        let near = c
            .any_width(key, n)
            .map(|f| (n, f))
            .or_else(|| c.before(key, n.saturating_sub(250), n));
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
            a.play = None;
            a.still = None;
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

/// The decoder for `file` at `size`, reusing `open` when it is that one.
fn decoder_for<'a>(
    open: &'a mut Option<Open>,
    key: Key,
    file: &PathBuf,
    size: (u32, u32),
    s: &Shared,
) -> Option<&'a mut Decoder> {
    let same = open
        .as_ref()
        .is_some_and(|o| o.key == key && &o.file == file && o.size == size);
    if !same {
        *open = None;
        match Decoder::open(file, size.0, size.1) {
            Ok(decoder) => {
                *open = Some(Open {
                    key,
                    file: file.clone(),
                    size,
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

fn player(s: &Shared) {
    let mut open: Option<Open> = None;
    // Next frame the decoder gives, of which video.
    let mut cursor: Option<(Key, u32)> = None;
    let mut seen = 0;
    loop {
        let Some(a) = wait(s, seen) else { return };
        seen = a.generation;
        let Some((key, _, size)) = a.play else {
            cursor = None;
            continue;
        };
        let Some(m) = a.media.get(&key).cloned() else {
            continue;
        };
        drop(a);
        let (file, dsize) = moving_source(&m, size);
        let cached = |n: u32| lock(&s.cache).get((key, n, dsize.0)).is_some();
        // Read ahead while what is wanted is near.
        loop {
            if s.stop.load(Ordering::Relaxed) {
                return;
            }
            let now = lock(&s.asked).play;
            let Some((k, want_now, _)) = now else { break };
            if k != key {
                break;
            }
            let at = match cursor {
                Some((ck, c)) if ck == key && c >= want_now && c <= want_now + AHEAD + 2 => c,
                Some((ck, c)) if ck == key && c < want_now && want_now - c <= 2 => c,
                _ => {
                    // A jump (or the start): play from the frame wanted,
                    // unless it and the next are already there.
                    if cached(want_now) && cached(want_now + 1) {
                        let mut n = want_now;
                        while n < want_now + AHEAD && cached(n + 1) {
                            n += 1;
                        }
                        if n >= want_now + AHEAD {
                            break;
                        }
                    }
                    let Some(t) = m.index.times.get(want_now as usize).copied() else {
                        break;
                    };
                    let Some(d) = decoder_for(&mut open, key, &file, dsize, s) else {
                        break;
                    };
                    if let Err(e) = d.play_from(t) {
                        tracing::warn!("{}: {e}", file.display());
                        break;
                    }
                    cursor = Some((key, want_now));
                    want_now
                }
            };
            if at > want_now + AHEAD {
                break;
            }
            let Some(d) = decoder_for(&mut open, key, &file, dsize, s) else {
                break;
            };
            match d.next_frame() {
                Ok(Some(f)) => {
                    lock(&s.cache).put((key, at, dsize.0), Arc::new(f), s.budget);
                    cursor = Some((key, at + 1));
                }
                Ok(None) => {
                    cursor = None;
                    break;
                }
                Err(e) => {
                    tracing::warn!("{}: {e}", file.display());
                    cursor = None;
                    open = None;
                    break;
                }
            }
        }
    }
}

fn seeker(s: &Shared) {
    let mut moving: Option<Open> = None;
    let mut sharp: Option<Open> = None;
    let mut seen = 0;
    loop {
        let Some(a) = wait(s, seen) else { return };
        seen = a.generation;
        let Some((key, n, size)) = a.still else {
            continue;
        };
        let Some(m) = a.media.get(&key).cloned() else {
            continue;
        };
        drop(a);
        let Some(t) = m.index.times.get(n as usize).copied() else {
            continue;
        };
        let (file, dsize) = moving_source(&m, size);
        let have = |w: u32| lock(&s.cache).get((key, n, w)).is_some();
        // The frame from what scrubbing reads (the proxy, or the original
        // with a keyframe first where that is far).
        if !have(dsize.0) {
            let far = m.proxy.is_none() && n as usize - m.index.key_before(n as usize) > 2;
            let k = m.index.key_before(n as usize);
            if far
                && !have_frame(s, key, k as u32, dsize.0)
                && let Some(&kt) = m.index.times.get(k)
                && let Some(d) = decoder_for(&mut moving, key, &file, dsize, s)
                && let Ok(Some(f)) = d.frame_at(kt, false)
            {
                lock(&s.cache).put((key, k as u32, dsize.0), Arc::new(f), s.budget);
            }
            if newer(s, key, n) {
                continue;
            }
            if let Some(d) = decoder_for(&mut moving, key, &file, dsize, s) {
                match d.frame_at(t, true) {
                    Ok(Some(f)) => lock(&s.cache).put((key, n, dsize.0), Arc::new(f), s.budget),
                    Ok(None) => {}
                    Err(e) => tracing::warn!("{}: {e}", file.display()),
                }
            }
        }
        // Sharp when stopped: the original at the full size wanted.
        if dsize != size && !have(size.0) && !newer(s, key, n) {
            let original = m.original.clone();
            if let Some(d) = decoder_for(&mut sharp, key, &original, size, s) {
                match d.frame_at(t, true) {
                    Ok(Some(f)) => lock(&s.cache).put((key, n, size.0), Arc::new(f), s.budget),
                    Ok(None) => {}
                    Err(e) => tracing::warn!("{}: {e}", original.display()),
                }
            }
        }
    }
}

fn have_frame(s: &Shared, key: Key, n: u32, width: u32) -> bool {
    lock(&s.cache).get((key, n, width)).is_some()
}

/// Whether a newer still frame than `n` of `key` is wanted.
fn newer(s: &Shared, key: Key, n: u32) -> bool {
    lock(&s.asked)
        .still
        .is_some_and(|(k, m, _)| k != key || m != n)
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
                if let Some(d) = decoder_for(&mut open, key, &file, size, s) {
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
