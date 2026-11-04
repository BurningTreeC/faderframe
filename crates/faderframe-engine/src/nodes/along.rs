//! Listen Along: another session's mix, streamed in a live session (the
//! host's, decoded on the control side), heard here in place of this
//! session's own.
//!
//! It lives in the master's listening node ([`super::ListenOut`]) first of
//! all, before the compare source and the codec monitor, and like them it
//! is monitoring only: nothing of it reaches a render, a file or a meter.
//!
//! * **The stream** reaches the audio thread as the reading end of a
//!   wait-free ring of interleaved stereo frames at the engine's rate
//!   ([`AlongLink::open`]); the control side writes what it decoded into
//!   the other end ([`AlongFeed`]). An old ring goes back through a
//!   mailbox, never freed here.
//! * **Jitter.** Playing starts once [`AlongShared::set_prime`] frames
//!   wait (until then the own mix plays on); a ring that runs dry is an
//!   underrun (counted, silence until it is primed again — not the own
//!   mix back); one that holds more than twice the prime (the host's clock
//!   faster than this one's) drops the surplus.
//! * **Switching** crossfades over [`FADE_SECONDS`] between the own mix
//!   and the stream (left and right on the first two channels; a mono
//!   master hears their mean).

use faderframe_audio_graph::AudioBuffer;
use faderframe_core::ChannelLayout;
use faderframe_realtime::{MailboxReceiver, MailboxSender, mailbox};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// How long switching between the own mix and the stream takes.
pub const FADE_SECONDS: f64 = 0.03;

/// The reading end of a stream (the audio thread's).
pub struct AlongStream {
    ring: rtrb::Consumer<f32>,
}

/// The writing end (the control side's).
pub struct AlongFeed {
    ring: rtrb::Producer<f32>,
}

impl AlongFeed {
    /// Write stereo frames; returns how many fit (the rest is dropped:
    /// the stream is that far ahead).
    pub fn push(&mut self, left: &[f32], right: &[f32]) -> usize {
        let frames = left.len().min(right.len()).min(self.ring.slots() / 2);
        if let Ok(mut chunk) = self.ring.write_chunk(frames * 2) {
            let (a, b) = chunk.as_mut_slices();
            for (i, v) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                *v = if i % 2 == 0 {
                    left[i / 2]
                } else {
                    right[i / 2]
                };
            }
            chunk.commit_all();
        }
        frames
    }

    /// Frames waiting to be heard.
    pub fn waiting(&self) -> usize {
        (self.ring.buffer().capacity() - self.ring.slots()) / 2
    }
}

/// What the control side and the node share.
#[derive(Debug, Default)]
pub struct AlongShared {
    hear: AtomicBool,
    /// Frames that must wait before playing starts.
    prime: AtomicU32,
    /// Frames waiting in the ring (as of the last block).
    buffered: AtomicU32,
    underruns: AtomicU64,
    /// The stream is heard now (faded in).
    playing: AtomicBool,
}

impl AlongShared {
    /// Hear the stream (or the own mix again).
    pub fn set_hear(&self, on: bool) {
        self.hear.store(on, Ordering::Relaxed);
    }

    pub fn hear(&self) -> bool {
        self.hear.load(Ordering::Relaxed)
    }

    pub fn set_prime(&self, frames: u32) {
        self.prime.store(frames.max(1), Ordering::Relaxed);
    }

    pub fn buffered(&self) -> u32 {
        self.buffered.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

/// The way a stream reaches the node and goes back (one per engine).
pub struct AlongLink {
    to_audio: (MailboxSender<AlongStream>, MailboxReceiver<AlongStream>),
    back: (MailboxSender<AlongStream>, MailboxReceiver<AlongStream>),
    shared: Arc<AlongShared>,
}

impl AlongLink {
    pub fn new() -> Arc<Self> {
        let shared = Arc::new(AlongShared::default());
        shared.set_prime(4800);
        Arc::new(Self {
            to_audio: mailbox(),
            back: mailbox(),
            shared,
        })
    }

    pub fn shared(&self) -> &Arc<AlongShared> {
        &self.shared
    }

    /// Control thread: a new ring of `frames` stereo frames; the node takes
    /// its reading end, the feed is returned.
    pub fn open(&self, frames: usize) -> AlongFeed {
        let (ring, read) = rtrb::RingBuffer::new(frames.max(64) * 2);
        // One the node had not taken yet comes back: dropped here.
        drop(self.to_audio.0.send(Box::new(AlongStream { ring: read })));
        AlongFeed { ring }
    }

    /// Control thread: drop what the node gave back.
    pub fn collect(&self) {
        while self.back.1.take().is_some() {}
    }
}

impl std::fmt::Debug for AlongLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AlongLink")
    }
}

/// The stage inside the listening node.
pub(crate) struct Along {
    link: Arc<AlongLink>,
    now: Option<Box<AlongStream>>,
    retired: Option<Box<AlongStream>>,
    primed: bool,
    /// Primed once since it was asked for: heard from then on (a ring
    /// that runs dry is silence, not the own mix back).
    started: bool,
    /// 0: the own mix, 1: the stream.
    x: f32,
    fade: f32,
    out: AudioBuffer,
    /// The block's stream frames (left, right).
    left: Box<[f32]>,
    right: Box<[f32]>,
}

impl Along {
    pub(crate) fn new(
        link: &Arc<AlongLink>,
        layout: ChannelLayout,
        rate: u32,
        block: usize,
    ) -> Self {
        let block = block.max(1);
        let mut out = AudioBuffer::new(layout, block);
        out.set_len(block);
        Self {
            link: Arc::clone(link),
            now: None,
            retired: None,
            primed: false,
            started: false,
            x: 0.0,
            fade: 1.0 / ((f64::from(rate) * FADE_SECONDS) as f32).max(1.0),
            out,
            left: vec![0.0; block].into_boxed_slice(),
            right: vec![0.0; block].into_boxed_slice(),
        }
    }

    fn give_back(&mut self) {
        if let Some(old) = self.retired.take() {
            self.retired = self.link.back.0.send(old);
        }
    }

    /// Audio thread: what is heard instead of `input` (`input` itself
    /// while no stream is heard).
    pub(crate) fn process<'b>(&'b mut self, input: &'b AudioBuffer, n: usize) -> &'b AudioBuffer {
        self.give_back();
        if self.retired.is_none()
            && let Some(new) = self.link.to_audio.1.take()
        {
            self.retired = self.now.replace(new);
            self.primed = false;
            self.give_back();
        }
        let shared = &*self.link.shared;
        let hear = shared.hear();
        let Some(stream) = self.now.as_mut() else {
            self.x = 0.0;
            shared.playing.store(false, Ordering::Relaxed);
            shared.buffered.store(0, Ordering::Relaxed);
            return input;
        };
        let n = n.min(self.left.len());
        // What waits, the surplus dropped.
        let prime = shared.prime.load(Ordering::Relaxed) as usize;
        let mut waiting = stream.ring.slots() / 2;
        if waiting > prime * 2 + n
            && let Ok(chunk) = stream.ring.read_chunk((waiting - prime) * 2)
        {
            chunk.commit_all();
            waiting = prime;
        }
        if !self.primed && waiting >= prime {
            self.primed = true;
        }
        // The block's frames (silence when not playing or dry).
        let mut got = 0;
        if self.primed {
            let take = waiting.min(n);
            if let Ok(chunk) = stream.ring.read_chunk(take * 2) {
                let (a, b) = chunk.as_slices();
                for (i, v) in a.iter().chain(b.iter()).enumerate() {
                    if i % 2 == 0 {
                        self.left[i / 2] = *v;
                    } else {
                        self.right[i / 2] = *v;
                    }
                }
                chunk.commit_all();
                got = take;
            }
            if got < n && hear {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.primed = false;
            }
        }
        self.left[got..n].fill(0.0);
        self.right[got..n].fill(0.0);
        shared
            .buffered
            .store((stream.ring.slots() / 2) as u32, Ordering::Relaxed);
        self.started = hear && (self.started || self.primed);
        let target = if self.started { 1.0 } else { 0.0 };
        if self.x == 0.0 && target == 0.0 {
            shared.playing.store(false, Ordering::Relaxed);
            return input;
        }
        let channels = self.out.num_channels().min(input.num_channels());
        let mut x = self.x;
        for i in 0..n {
            x = if target > x {
                (x + self.fade).min(1.0)
            } else {
                (x - self.fade).max(0.0)
            };
            let (l, r) = (self.left[i], self.right[i]);
            for c in 0..channels {
                let own = input.channel(c)[i];
                let theirs = match (channels, c) {
                    (1, _) => 0.5 * (l + r),
                    (_, 0) => l,
                    (_, 1) => r,
                    _ => 0.0,
                };
                self.out.channel_mut(c)[i] = own * (1.0 - x) + theirs * x;
            }
        }
        self.x = x;
        shared.playing.store(x >= 1.0, Ordering::Relaxed);
        &self.out
    }
}
