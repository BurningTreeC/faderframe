//! Self-freezing: tracks played from a disk cache of their chain's output
//! (the session decides what is cached and when: `faderframe_session::
//! self_freeze`).
//!
//! A cached track's sources, MIDI effects, instrument and inserts are not
//! built. A [`CachedPlayer`](crate::nodes::CachedPlayer) plays the cache
//! file into the track's input in their place and reports the latency the
//! chain has live, so delay compensation and everything after the chain —
//! its strip, sends, the buses it reaches, the sidechains it keys — stay
//! exactly as they were. Its plugins stay loaded: an editor opens at once,
//! and the chain comes back at once.
//!
//! Switching never jumps. A track going to its cache, or back to its chain,
//! passes through a *handover* graph that holds both: the chain and the
//! cache player into a [`Handover`](crate::nodes::Handover) node that
//! crossfades from one to the other — bit-exactly the one side at either
//! end of the fade, and wherever the two agree (a cache of a deterministic
//! chain played on from where the chain was: the switch is sample-exact).
//! [`EngineController::settle_cache`](crate::EngineController::settle_cache)
//! tells the session when a fade is over, and the next graph drops the side
//! no longer heard. The fade to the cache waits until the cache player has
//! its pages (a block played with nothing missing); the fade back waits
//! until the chain, reset before its first block (its plugins held whatever
//! they had when it left), has filled its latency — and, when it comes back
//! gently (nothing changed: an editor opened, the track armed), its
//! devices' tails too, up to [`MAX_WARM_SECONDS`], so its echoes and
//! reverberation have built up again (a reset delay line's first repeat
//! starts with a step) while the cache, still right, is heard. A chain
//! coming back because it was edited fades in once its latency is filled:
//! the edit is heard as soon as it would be live.
//!
//! Realtime: the overlay lives on the control thread; graphs carry what
//! they need (the player's latencies, the handover's shared state, atomics
//! only), and the timeline snapshot carries the cache files' regions (the
//! disk loader keeps their pages resident as it does the clips').

use crate::snapshot::{AudioRegion, Source};
use faderframe_core::TrackId;
use faderframe_realtime::AtomicF32;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Seconds a handover crossfades.
pub const FADE_SECONDS: f64 = 0.02;
/// Seconds a chain coming back plays (after its latency) before the fade
/// to it starts.
pub const WARM_SECONDS: f64 = 0.005;
/// The longest a chain coming back gently plays through its devices'
/// tails before the fade (its repeats and reverberation built up again
/// while the cache, still right, is heard).
pub const MAX_WARM_SECONDS: f64 = 1.0;

/// One track's cache as the engine plays it.
#[derive(Clone, Debug)]
pub struct CachedChain {
    /// The cache file (a stream of its own: never shared with another
    /// reader).
    pub source: Source,
    /// Timeline sample (at the engine's rate) of the file's first frame.
    pub start: i64,
    /// Latency at the front of the file: frame `k` holds what the chain
    /// made of the input played at `start + k − file_latency`.
    pub file_latency: u32,
    /// The latency the chain has live: the cache player reports it, so
    /// delay compensation stays as it was.
    pub latency: u32,
    /// The cache's identity (part of the player's node key).
    pub key: u64,
}

impl CachedChain {
    pub fn channels(&self) -> usize {
        self.source.channels()
    }

    /// Frames in the file.
    pub fn frames(&self) -> i64 {
        self.source.frames()
    }
}

/// Where a cached track is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheState {
    /// Fading from the chain to the cache (both run).
    ToCache,
    /// Playing from the cache (the chain is not built).
    Cached,
    /// Fading back to the chain (both run).
    ToLive,
}

/// What a handover node and the control side share (one per cached
/// track, kept across the graphs of its handover).
#[derive(Debug)]
pub struct HandoverShared {
    /// 0: the chain is heard, 1: the cache.
    gain: AtomicF32,
    /// Where the gain goes (0 or 1).
    target: AtomicF32,
    /// Frames the chain plays before the fade back to it starts.
    wait: AtomicU32,
    /// The cache player played a block with every page it needed.
    ready: AtomicBool,
}

impl HandoverShared {
    fn new(gain: f32, target: f32) -> Arc<Self> {
        Arc::new(Self {
            gain: AtomicF32::new(gain),
            target: AtomicF32::new(target),
            wait: AtomicU32::new(0),
            ready: AtomicBool::new(false),
        })
    }

    pub fn gain(&self) -> f32 {
        self.gain.load(Ordering::Acquire)
    }

    pub(crate) fn set_gain(&self, g: f32) {
        self.gain.store(g, Ordering::Release);
    }

    pub fn target(&self) -> f32 {
        self.target.load(Ordering::Acquire)
    }

    /// Frames left before the fade back may start.
    pub fn wait(&self) -> u32 {
        self.wait.load(Ordering::Acquire)
    }

    /// `n` frames played towards the wait.
    pub(crate) fn waited(&self, n: u32) {
        let left = self.wait().saturating_sub(n);
        self.wait.store(left, Ordering::Release);
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn set_ready(&self) {
        if !self.ready.load(Ordering::Relaxed) {
            self.ready.store(true, Ordering::Release);
        }
    }

    /// The fade arrived where it was going.
    fn done(&self) -> bool {
        self.gain() == self.target() && self.wait() == 0
    }
}

/// How a track is built (see the module docs).
pub(crate) enum Plays<'a> {
    /// From the cache alone.
    Cache(&'a CachedChain),
    /// The chain and the cache, crossfaded.
    Both(&'a CachedChain, &'a Arc<HandoverShared>),
}

struct Entry {
    chain: Arc<CachedChain>,
    state: CacheState,
    handover: Arc<HandoverShared>,
}

/// The cached tracks of an engine (control thread).
#[derive(Default)]
pub struct CacheOverlay {
    entries: BTreeMap<TrackId, Entry>,
    /// Tracks whose chains come back: their plugins reset before their
    /// next block (asked when the next graph is built).
    resets: HashSet<TrackId>,
}

impl CacheOverlay {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn plays(&self, track: TrackId) -> Option<Plays<'_>> {
        let e = self.entries.get(&track)?;
        Some(match e.state {
            CacheState::Cached => Plays::Cache(&e.chain),
            CacheState::ToCache | CacheState::ToLive => Plays::Both(&e.chain, &e.handover),
        })
    }

    pub fn state(&self, track: TrackId) -> Option<CacheState> {
        self.entries.get(&track).map(|e| e.state)
    }

    pub fn states(&self) -> Vec<(TrackId, CacheState)> {
        self.entries.iter().map(|(t, e)| (*t, e.state)).collect()
    }

    pub fn chain(&self, track: TrackId) -> Option<Arc<CachedChain>> {
        self.entries.get(&track).map(|e| Arc::clone(&e.chain))
    }

    /// The handover's state of a track (tests, diagnostics).
    pub fn handover(&self, track: TrackId) -> Option<Arc<HandoverShared>> {
        self.entries.get(&track).map(|e| Arc::clone(&e.handover))
    }

    /// Play `track` from `chain`, or (`None`) from its chain again after
    /// it has played `warm` frames beyond its latency and [`WARM_SECONDS`]
    /// (its tails); `rate` is the engine's. Returns whether anything
    /// changed (the graph is to be rebuilt).
    pub(crate) fn set(
        &mut self,
        track: TrackId,
        chain: Option<CachedChain>,
        rate: u32,
        warm: u32,
    ) -> bool {
        let wait_for = |c: &CachedChain| -> u32 {
            let fill = c.latency as f64 + WARM_SECONDS * f64::from(rate.max(1));
            (fill.ceil() as u32).saturating_add(warm)
        };
        match (chain, self.entries.get_mut(&track)) {
            (Some(chain), None) => {
                self.entries.insert(
                    track,
                    Entry {
                        chain: Arc::new(chain),
                        state: CacheState::ToCache,
                        handover: HandoverShared::new(0.0, 1.0),
                    },
                );
                true
            }
            (Some(chain), Some(e)) => {
                let same = e.chain.key == chain.key
                    && e.chain.latency == chain.latency
                    && e.chain.file_latency == chain.file_latency
                    && e.chain.start == chain.start;
                if !same {
                    e.chain = Arc::new(chain);
                }
                match e.state {
                    // Fading back: the chain still runs, the fade turns.
                    CacheState::ToLive => {
                        e.handover.wait.store(0, Ordering::Release);
                        e.handover.target.store(1.0, Ordering::Release);
                        e.state = CacheState::ToCache;
                        true
                    }
                    CacheState::ToCache | CacheState::Cached => !same,
                }
            }
            (None, Some(e)) => match e.state {
                CacheState::Cached => {
                    // The chain comes back from nothing: reset, then its
                    // latency (and tails) filled before the fade.
                    e.handover.wait.store(wait_for(&e.chain), Ordering::Release);
                    e.handover.target.store(0.0, Ordering::Release);
                    e.state = CacheState::ToLive;
                    self.resets.insert(track);
                    true
                }
                CacheState::ToCache => {
                    e.handover.target.store(0.0, Ordering::Release);
                    e.state = CacheState::ToLive;
                    true
                }
                // Coming back already: sooner, if this asks for less.
                CacheState::ToLive => {
                    let w = wait_for(&e.chain);
                    if w < e.handover.wait() {
                        e.handover.wait.store(w, Ordering::Release);
                    }
                    false
                }
            },
            (None, None) => false,
        }
    }

    /// Forget a track at once (it is gone, or the engine starts over):
    /// whatever plays it is rebuilt from its chain.
    pub(crate) fn remove(&mut self, track: TrackId) -> bool {
        let was = self.entries.remove(&track);
        if matches!(&was, Some(e) if e.state == CacheState::Cached) {
            self.resets.insert(track);
        }
        was.is_some()
    }

    /// Fades that are over: to the cache → cached, back → gone. Returns
    /// whether any ended (the graph is to be rebuilt without the side no
    /// longer heard).
    pub(crate) fn settle(&mut self) -> bool {
        let mut changed = false;
        self.entries.retain(|_, e| match e.state {
            CacheState::ToCache if e.handover.done() => {
                e.state = CacheState::Cached;
                changed = true;
                true
            }
            CacheState::ToLive if e.handover.done() => {
                changed = true;
                false
            }
            _ => true,
        });
        changed
    }

    /// End every fade at once (nothing plays: no audio device runs, so no
    /// fade would ever move).
    pub(crate) fn skip_fades(&mut self) {
        for e in self.entries.values() {
            let h = &e.handover;
            h.wait.store(0, Ordering::Release);
            h.set_gain(h.target());
        }
    }

    /// Tracks whose plugins are to reset before their next block.
    pub(crate) fn take_resets(&mut self) -> Vec<TrackId> {
        self.resets.drain().collect()
    }

    /// The cache files' regions for the timeline snapshot (sorted by
    /// track).
    pub(crate) fn regions(&self) -> Vec<(TrackId, AudioRegion)> {
        self.entries
            .iter()
            .map(|(t, e)| {
                let c = &e.chain;
                (
                    *t,
                    AudioRegion {
                        start: c.start,
                        end: c.start + c.frames(),
                        source: c.source.clone(),
                        source_start: 0,
                        step: 1.0,
                        gain: 1.0,
                        fade_in: 0,
                        fade_out: 0,
                        fade_in_shape: faderframe_project::FadeShape::Linear,
                        fade_out_shape: faderframe_project::FadeShape::Linear,
                        fade_in_bend: 0.0,
                        fade_out_bend: 0.0,
                        reversed: false,
                        envelope: None,
                    },
                )
            })
            .collect()
    }
}
