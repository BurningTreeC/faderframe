//! One follower direction: the ring between the follower's device thread
//! and the master's callback, both ends, and what they share.
//!
//! Input (a follower records): the device thread pushes its frames into the
//! ring and stamps how many it has written by when ([`Stamp`]); the master's
//! callback estimates the ring's fill at its own moment from that stamp,
//! steers the resampler by the drift loop and reads exactly the frames its
//! buffer needs. Output (a follower plays): the master's callback resamples
//! its frames into the ring, the device thread takes what its buffer needs
//! and stamps how many it has taken by when.
//!
//! A gap (the ring ran dry, a follower stalled or went away) never stops
//! the master: those channels are silent, the gap is counted, and the link
//! starts again at its target once the follower delivers — with a little
//! more safety each time ([`SAFETY_STEP_SECONDS`]). The fill estimate is
//! continuous, so the follower's block size does not saw it.

use super::drift::{DriftLoop, MAX_CORRECTION};
use super::resample::Resampler;
use crate::MAX_BUFFER_SIZE;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

/// Seconds of audio a ring holds (more when blocks are larger).
pub const RING_SECONDS: f64 = 0.5;
/// The fill kept beyond the blocks on both sides (seconds) …
pub const SAFETY_SECONDS: f64 = 0.002;
/// … and added after each gap, up to [`MAX_SAFETY_SECONDS`].
pub const SAFETY_STEP_SECONDS: f64 = 0.002;
pub const MAX_SAFETY_SECONDS: f64 = 0.05;
/// A follower whose last callback is longer ago than this (or than four
/// of its blocks) is away: its channels are silent until it is back.
pub const STALE_SECONDS: f64 = 0.15;
/// Fade-in after a start or a gap (frames of the master).
const RAMP_FRAMES: f32 = 256.0;

/// Frames moved by when, published by one thread and read by another as a
/// pair (a sequence lock over atomics: the reader retries a few times
/// while a write is under way, and keeps what it had otherwise).
#[derive(Debug, Default)]
pub struct Stamp {
    seq: AtomicU64,
    count: AtomicU64,
    ns: AtomicU64,
}

impl Stamp {
    pub fn publish(&self, count: u64, ns: u64) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.count.store(count, Ordering::Relaxed);
        self.ns.store(ns, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// `(count, ns)` as last published; `None` before the first, or while
    /// a write keeps getting in the way.
    pub fn read(&self) -> Option<(u64, u64)> {
        for _ in 0..16 {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let count = self.count.load(Ordering::Relaxed);
            let ns = self.ns.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == s1 {
                return (s1 != 0).then_some((count, ns));
            }
        }
        None
    }
}

/// What a follower's device thread and the master's callback share.
#[derive(Debug, Default)]
pub struct Shared {
    /// Input: frames written into the ring; output: frames taken from it.
    pub stamp: Stamp,
    /// The device's own latency beyond its callbacks (follower frames):
    /// capture to callback for inputs, callback to playback beyond its
    /// buffer for outputs.
    pub device_latency: AtomicU32,
    /// The largest callback seen (frames).
    pub max_block: AtomicU32,
    /// Input: frames lost to a full ring; output: callbacks short of
    /// frames (silence played).
    pub misses: AtomicU64,
    pub callbacks: AtomicU64,
}

/// A link's state for the control side (written by the master's callback).
#[derive(Debug, Default)]
pub struct LinkStatus {
    pub present: AtomicBool,
    pub locked: AtomicBool,
    /// The follower's clock against the master's (f64 bits; a share,
    /// positive: the follower runs fast).
    pub drift: AtomicU64,
    /// The smoothed fill error (f64 bits, follower frames).
    pub error: AtomicU64,
    /// The fill aimed at (follower frames).
    pub target: AtomicU32,
    /// Its channels' latency beyond the master's buffer, in master frames
    /// (see `Joined`): input = capture to the engine, output = the engine
    /// to playback.
    pub latency: AtomicU32,
    /// Gaps: the ring ran dry or overflowed and the link started again.
    pub gaps: AtomicU64,
    /// The follower's nominal rate.
    pub rate: AtomicU32,
    /// Links attached so far (1 + times the device came back).
    pub attached: AtomicU64,
}

fn load_f64(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}

fn store_f64(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl LinkStatus {
    pub fn drift(&self) -> f64 {
        load_f64(&self.drift)
    }

    pub fn error(&self) -> f64 {
        load_f64(&self.error)
    }
}

/// How a link is getting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for the follower (its first frames, or after a gap).
    Waiting,
    Running,
}

/// The ring's capacity in frames for a follower at `rate` with `master`
/// frames a second on the other side.
fn ring_frames(rate: f64, master: f64) -> usize {
    let blocks = 4.0 * (MAX_BUFFER_SIZE as f64 * (rate / master.max(1.0)).max(1.0));
    (rate * RING_SECONDS).max(blocks).ceil() as usize
}

/// The device side of a follower that records: its callback pushes the
/// frames it captured.
#[derive(Debug)]
pub struct FollowerInput {
    tx: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    written: u64,
}

impl FollowerInput {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The device's latency from capture to its callback, beyond the
    /// callback's own frames (frames at its rate).
    pub fn set_device_latency(&self, frames: u32) {
        self.shared.device_latency.store(frames, Ordering::Relaxed);
    }

    /// One callback: `frames` frames captured, sample `(frame, channel)`
    /// from `sample`, the callback at `now_ns` ([`super::now_ns`]).
    /// Realtime safe.
    pub fn capture_with(
        &mut self,
        frames: usize,
        now_ns: u64,
        mut sample: impl FnMut(usize, usize) -> f32,
    ) {
        let ch = self.channels.max(1);
        let room = self.tx.slots() / ch;
        let n = frames.min(room);
        if n > 0
            && let Ok(mut chunk) = self.tx.write_chunk(n * ch)
        {
            let (a, b) = chunk.as_mut_slices();
            for (s, v) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                *v = sample(s / ch, s % ch);
            }
            chunk.commit_all();
        }
        if n < frames {
            self.shared
                .misses
                .fetch_add((frames - n) as u64, Ordering::Relaxed);
        }
        self.written += n as u64;
        self.shared
            .max_block
            .fetch_max(frames as u32, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        self.shared.stamp.publish(self.written, now_ns);
    }

    /// One callback of interleaved frames, converted by `f`.
    pub fn capture<T: Copy>(&mut self, data: &[T], now_ns: u64, f: impl Fn(T) -> f32) {
        let ch = self.channels.max(1);
        let frames = data.len() / ch;
        self.capture_with(frames, now_ns, |i, c| {
            data.get(i * ch + c).map_or(0.0, |v| f(*v))
        });
    }
}

/// The device side of a follower that plays: its callback takes the frames
/// its buffer needs.
#[derive(Debug)]
pub struct FollowerOutput {
    rx: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    taken: u64,
    /// Frames have come (a dry ring after that is a gap).
    started: bool,
}

impl FollowerOutput {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The device's latency from its callback to playback, beyond the
    /// callback's own frames (frames at its rate).
    pub fn set_device_latency(&self, frames: u32) {
        self.shared.device_latency.store(frames, Ordering::Relaxed);
    }

    /// One callback: `frames` frames to play, sample `(frame, channel)`
    /// given to `put` (silence where the ring is dry), the callback at
    /// `now_ns`. Realtime safe.
    pub fn play_with(
        &mut self,
        frames: usize,
        now_ns: u64,
        mut put: impl FnMut(usize, usize, f32),
    ) {
        let ch = self.channels.max(1);
        let n = frames.min(self.rx.slots() / ch);
        if n > 0
            && let Ok(chunk) = self.rx.read_chunk(n * ch)
        {
            let (a, b) = chunk.as_slices();
            for (s, v) in a.iter().chain(b).enumerate() {
                put(s / ch, s % ch, *v);
            }
            chunk.commit_all();
            self.started = true;
        }
        for i in n..frames {
            for c in 0..ch {
                put(i, c, 0.0);
            }
        }
        if n < frames && self.started {
            self.shared.misses.fetch_add(1, Ordering::Relaxed);
        }
        self.taken += n as u64;
        self.shared
            .max_block
            .fetch_max(frames as u32, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        self.shared.stamp.publish(self.taken, now_ns);
    }

    /// One callback into interleaved frames, converted by `f`.
    pub fn play<T: Copy>(&mut self, data: &mut [T], now_ns: u64, f: impl Fn(f32) -> T) {
        let ch = self.channels.max(1);
        let frames = data.len() / ch;
        self.play_with(frames, now_ns, |i, c, v| {
            if let Some(s) = data.get_mut(i * ch + c) {
                *s = f(v);
            }
        });
    }
}

/// What both directions keep on the master's side.
#[derive(Debug)]
struct Common {
    shared: Arc<Shared>,
    status: Arc<LinkStatus>,
    channels: usize,
    /// The follower's nominal rate and the master's (frames a second).
    rate: f64,
    master: f64,
    /// +1 reading a follower, −1 feeding one: the loop's correction
    /// settles on the drift or against it.
    sign: f64,
    resampler: Resampler,
    drift: DriftLoop,
    state: State,
    /// Extra fill kept (follower frames), grown by gaps.
    safety: f64,
    /// The fill aimed at (follower frames).
    target: f64,
    /// Fade-in gain after a start or a gap.
    ramp: f32,
    /// Misses the device had counted when last looked at.
    misses: u64,
    /// Gaps counted.
    gaps: u64,
}

impl Common {
    /// `step` = the resampler's nominal input frames per output frame;
    /// up to `max_input` input frames a call.
    #[allow(clippy::too_many_arguments)]
    fn new(
        shared: Arc<Shared>,
        status: Arc<LinkStatus>,
        channels: usize,
        rate: f64,
        master: f64,
        sign: f64,
        step: f64,
        max_input: usize,
        drift: f64,
    ) -> Self {
        status.rate.store(rate as u32, Ordering::Relaxed);
        status.attached.fetch_add(1, Ordering::Relaxed);
        Self {
            shared,
            gaps: status.gaps.load(Ordering::Relaxed),
            status,
            channels,
            rate,
            master,
            sign,
            resampler: Resampler::new(channels, step, max_input),
            drift: DriftLoop::new(rate, sign * drift),
            state: State::Waiting,
            safety: SAFETY_SECONDS * rate,
            target: 0.0,
            ramp: 0.0,
            misses: 0,
        }
    }

    /// The last stamp, if the follower is there (not stale at `now`).
    fn stamp(&self, now: u64) -> Option<(u64, u64)> {
        let (count, at) = self.shared.stamp.read()?;
        let block = f64::from(self.shared.max_block.load(Ordering::Relaxed)) / self.rate;
        let stale = STALE_SECONDS.max(4.0 * block);
        let since = now.saturating_sub(at) as f64 * 1e-9;
        (since <= stale).then_some((count, at))
    }

    /// A gap: count it, keep more fill from now on, start again.
    fn gap(&mut self) {
        self.gaps += 1;
        self.status.gaps.store(self.gaps, Ordering::Relaxed);
        self.safety =
            (self.safety + SAFETY_STEP_SECONDS * self.rate).min(MAX_SAFETY_SECONDS * self.rate);
        self.state = State::Waiting;
    }

    fn away(&mut self) {
        self.status.present.store(false, Ordering::Relaxed);
        self.status.locked.store(false, Ordering::Relaxed);
        self.state = State::Waiting;
    }

    fn publish(&self, latency: f64) {
        let s = &self.status;
        s.present.store(true, Ordering::Relaxed);
        self.publish_latency(latency);
        store_f64(&s.drift, self.sign * self.drift.drift());
        store_f64(&s.error, self.drift.average_error());
        // Settled: past the wide start, the fill on its target within a
        // few frames (jitter averaged out).
        let locked = self.state == State::Running
            && self.drift.seconds() > super::drift::BW_HOLD
            && self.drift.average_error().abs() < (0.005 * self.target).max(3.0);
        s.locked.store(locked, Ordering::Relaxed);
    }

    /// The fill as it runs: the target, and how far off it the loop has
    /// it lately (while it settles).
    fn fill(&self) -> f64 {
        let off = if self.state == State::Running {
            self.drift
                .average_error()
                .clamp(-0.5 * self.target, 0.5 * self.target)
        } else {
            0.0
        };
        self.target + off
    }

    fn publish_latency(&self, latency: f64) {
        let s = &self.status;
        s.target
            .store(self.target.round() as u32, Ordering::Relaxed);
        s.latency
            .store(latency.max(0.0).round() as u32, Ordering::Relaxed);
    }

    /// Apply the fade-in to `n` frames of `bufs`.
    fn fade_in(&mut self, bufs: &mut [Box<[f32]>], n: usize) {
        if self.ramp >= 1.0 {
            return;
        }
        let start = self.ramp;
        let mut g = start;
        for b in bufs.iter_mut() {
            g = start;
            for s in b.iter_mut().take(n) {
                *s *= g;
                g = (g + 1.0 / RAMP_FRAMES).min(1.0);
            }
        }
        self.ramp = g;
    }
}

/// The master's side of a follower that records.
#[derive(Debug)]
pub struct InLink {
    rx: rtrb::Consumer<f32>,
    common: Common,
    /// Frames taken from the ring.
    taken: u64,
}

impl InLink {
    /// A link and its device end, for `channels` at `rate` following a
    /// master at `master` (frames a second), the follower's callbacks
    /// about `block` frames, starting from `drift`.
    pub fn new(
        channels: usize,
        rate: f64,
        master: f64,
        block: u32,
        status: Arc<LinkStatus>,
        drift: f64,
    ) -> (Self, FollowerInput) {
        let shared = Arc::new(Shared::default());
        shared.max_block.store(block, Ordering::Relaxed);
        let (tx, rx) = rtrb::RingBuffer::new(ring_frames(rate, master) * channels.max(1));
        let step = rate / master.max(1.0);
        // A read takes up to a whole master buffer's worth of its frames.
        let most = (MAX_BUFFER_SIZE as f64 * step * (1.0 + 2.0 * MAX_CORRECTION)).ceil() as usize;
        (
            Self {
                rx,
                common: Common::new(
                    Arc::clone(&shared),
                    status,
                    channels,
                    rate,
                    master,
                    1.0,
                    step,
                    most + 2,
                    drift,
                ),
                taken: 0,
            },
            FollowerInput {
                tx,
                shared,
                channels,
                written: 0,
            },
        )
    }

    pub fn channels(&self) -> usize {
        self.common.channels
    }

    /// The fill to keep for the master taking `block` frames at a time.
    fn target(&self, block: usize) -> f64 {
        let c = &self.common;
        let theirs = f64::from(c.shared.max_block.load(Ordering::Relaxed));
        let step = c.rate / c.master;
        theirs + block as f64 * step + c.resampler.delay() as f64 + c.safety + 0.25 * theirs
    }

    /// `n` master frames of the follower's channels into `out[c][..n]`
    /// (silence where there is none), at `now` ns; the master's buffer is
    /// `block` frames. Realtime safe.
    pub fn pull(&mut self, n: usize, block: usize, now: u64, out: &mut [Box<[f32]>]) {
        let n = n.min(MAX_BUFFER_SIZE as usize);
        let silence = |out: &mut [Box<[f32]>]| {
            for b in out.iter_mut() {
                let m = n.min(b.len());
                b[..m].fill(0.0);
            }
        };
        let Some((written, at)) = self.common.stamp(now) else {
            self.common.away();
            silence(out);
            return;
        };
        let ch = self.common.channels.max(1);
        self.common.target = self.target(block);
        let target = self.common.target;
        let rate = self.common.rate;
        let estimate =
            |taken: u64| written as f64 + (now as f64 - at as f64) * 1e-9 * rate - taken as f64;
        if self.common.state == State::Waiting {
            // Start with the fill at its target: what is beyond it goes.
            let held = self.rx.slots() / ch;
            let surplus = estimate(self.taken) - target;
            if surplus < 0.0 {
                self.common.publish(self.latency(block));
                silence(out);
                return;
            }
            let drop = (surplus.floor() as usize).min(held);
            if drop > 0
                && let Ok(chunk) = self.rx.read_chunk(drop * ch)
            {
                chunk.commit_all();
            }
            self.taken += drop as u64;
            self.common.resampler.reset();
            self.common.drift.restart();
            self.common.state = State::Running;
            self.common.ramp = 0.0;
        }
        let fill = estimate(self.taken) + self.common.resampler.ahead();
        let error = fill - target;
        if error > target.max(0.25 * rate) {
            // Far too much (the master stalled): start again.
            self.common.gap();
            silence(out);
            return;
        }
        let dt = n as f64 / self.common.master;
        let correction = self.common.drift.update(error, dt);
        let nominal = rate / self.common.master;
        self.common.resampler.set_step(nominal * (1.0 + correction));
        let need = self.common.resampler.needed(n);
        let held = self.rx.slots() / ch;
        if need > held {
            self.common.gap();
            silence(out);
            return;
        }
        if need > 0
            && let Ok(chunk) = self.rx.read_chunk(need * ch)
        {
            let (a, b) = chunk.as_slices();
            self.common.resampler.push_interleaved(a, b);
            chunk.commit_all();
            self.taken += need as u64;
        }
        let got = self.common.resampler.read(n, out, 0);
        for b in out.iter_mut() {
            let m = n.min(b.len());
            if got < m {
                b[got..m].fill(0.0);
            }
        }
        self.common.fade_in(out, n);
        self.common.publish(self.latency(block));
    }

    /// Work out the target and latency for a master taking `block` frames
    /// at a time, before the first callback (control side, once).
    pub fn note_block(&mut self, block: usize) {
        self.common.target = self.target(block);
        self.common.publish_latency(self.latency(block));
    }

    /// Its channels' latency beyond the master's buffer (master frames):
    /// a frame read now was captured `fill + device latency` follower
    /// frames before the callback, the master's own input `block` frames
    /// before it (see `follow`'s docs).
    fn latency(&self, block: usize) -> f64 {
        let c = &self.common;
        let device = f64::from(c.shared.device_latency.load(Ordering::Relaxed));
        (c.fill() + device) * c.master / c.rate - block as f64
    }
}

/// The master's side of a follower that plays.
#[derive(Debug)]
pub struct OutLink {
    tx: rtrb::Producer<f32>,
    common: Common,
    /// Frames written into the ring.
    written: u64,
    /// Resampled frames before they go into the ring.
    scratch: Box<[Box<[f32]>]>,
}

impl OutLink {
    /// A link and its device end, for `channels` at `rate` following a
    /// master at `master` (frames a second), the follower's callbacks
    /// about `block` frames, starting from `drift`.
    pub fn new(
        channels: usize,
        rate: f64,
        master: f64,
        block: u32,
        status: Arc<LinkStatus>,
        drift: f64,
    ) -> (Self, FollowerOutput) {
        let shared = Arc::new(Shared::default());
        shared.max_block.store(block, Ordering::Relaxed);
        let (tx, rx) = rtrb::RingBuffer::new(ring_frames(rate, master) * channels.max(1));
        let step = master / rate.max(1.0);
        let most =
            (MAX_BUFFER_SIZE as f64 / (step * (1.0 - 2.0 * MAX_CORRECTION))).ceil() as usize + 8;
        (
            Self {
                tx,
                common: Common::new(
                    Arc::clone(&shared),
                    status,
                    channels,
                    rate,
                    master,
                    -1.0,
                    step,
                    MAX_BUFFER_SIZE as usize,
                    drift,
                ),
                written: 0,
                scratch: (0..channels)
                    .map(|_| vec![0.0f32; most].into_boxed_slice())
                    .collect(),
            },
            FollowerOutput {
                rx,
                shared,
                channels,
                taken: 0,
                started: false,
            },
        )
    }

    pub fn channels(&self) -> usize {
        self.common.channels
    }

    fn target(&self, block: usize) -> f64 {
        let c = &self.common;
        let theirs = f64::from(c.shared.max_block.load(Ordering::Relaxed));
        let ours = block as f64 * c.rate / c.master;
        theirs + ours + c.safety + 0.25 * theirs
    }

    /// Write `frames` frames of silence into the ring (as many as fit).
    fn silence(&mut self, frames: usize) {
        let ch = self.common.channels.max(1);
        let n = frames.min(self.tx.slots() / ch);
        if n > 0
            && let Ok(mut chunk) = self.tx.write_chunk(n * ch)
        {
            let (a, b) = chunk.as_mut_slices();
            a.fill(0.0);
            b.fill(0.0);
            chunk.commit_all();
            self.written += n as u64;
        }
    }

    /// `n` master frames of the follower's channels from `src[c][..n]`,
    /// at `now` ns; the master's buffer is `block` frames. Realtime safe.
    pub fn push(&mut self, n: usize, block: usize, now: u64, src: &[Box<[f32]>]) {
        let n = n.min(MAX_BUFFER_SIZE as usize);
        let ch = self.common.channels.max(1);
        let target = self.target(block);
        self.common.target = target;
        let Some((taken, at)) = self.common.stamp(now) else {
            if self.common.shared.stamp.read().is_none() && self.written == 0 {
                // Not started yet: the ring waits for it at its target.
                self.silence(target.ceil() as usize);
            } else {
                self.common.away();
            }
            return;
        };
        let rate = self.common.rate;
        let misses = self.common.shared.misses.load(Ordering::Relaxed);
        let ran_dry = misses != self.common.misses;
        self.common.misses = misses;
        let fill =
            |written: u64| written as f64 - taken as f64 - (now as f64 - at as f64) * 1e-9 * rate;
        if ran_dry && self.common.state == State::Running {
            self.common.gap();
        }
        if self.common.state == State::Waiting {
            let short = target - fill(self.written);
            if short > 0.0 {
                self.silence(short.ceil() as usize);
            }
            self.common.resampler.reset();
            self.common.drift.restart();
            self.common.state = State::Running;
            self.common.ramp = 0.0;
        }
        let step = self.common.resampler.step();
        let pending = (self.common.resampler.ahead() - self.common.resampler.delay() as f64) / step;
        let error = fill(self.written) + pending - target;
        let dt = n as f64 / self.common.master;
        let correction = self.common.drift.update(error, dt);
        let nominal = self.common.master / rate;
        self.common.resampler.set_step(nominal * (1.0 + correction));
        self.common
            .resampler
            .push(n, |c| src.get(c).map_or(&[][..], |s| &s[..n.min(s.len())]));
        let k = self
            .common
            .resampler
            .readable()
            .min(self.scratch.first().map_or(0, |s| s.len()));
        let k = self.common.resampler.read(k, &mut self.scratch, 0);
        self.common.fade_in(&mut self.scratch, k);
        let room = self.tx.slots() / ch;
        let m = k.min(room);
        if m > 0
            && let Ok(mut chunk) = self.tx.write_chunk(m * ch)
        {
            let (a, b) = chunk.as_mut_slices();
            for (s, v) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                *v = self.scratch[s % ch][s / ch];
            }
            chunk.commit_all();
            self.written += m as u64;
        }
        if m < k {
            // The follower does not take its frames (stalled): start again
            // once it does.
            self.common.gap();
        }
        self.common.publish(self.latency(block));
    }

    /// See [`InLink::note_block`].
    pub fn note_block(&mut self, block: usize) {
        self.common.target = self.target(block);
        self.common.publish_latency(self.latency(block));
    }

    /// Its channels' latency beyond the master's buffer (master frames): a
    /// frame written now plays `fill` follower frames after the callback,
    /// plus the resampler's delay and the device's own, against the
    /// master's `block` frames.
    fn latency(&self, block: usize) -> f64 {
        let c = &self.common;
        let device = f64::from(c.shared.device_latency.load(Ordering::Relaxed));
        (c.fill() + device) * c.master / c.rate + c.resampler.delay() as f64 - block as f64
    }
}

/// One link, either way.
#[derive(Debug)]
pub enum Link {
    In(InLink),
    Out(OutLink),
}

impl Link {
    /// What its device end shares with it.
    pub fn shared(&self) -> Arc<Shared> {
        match self {
            Link::In(l) => Arc::clone(&l.common.shared),
            Link::Out(l) => Arc::clone(&l.common.shared),
        }
    }

    pub fn note_block(&mut self, block: usize) {
        match self {
            Link::In(l) => l.note_block(block),
            Link::Out(l) => l.note_block(block),
        }
    }
}
