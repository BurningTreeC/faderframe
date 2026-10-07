//! Anticipative processing ("render ahead"): the sources and plugin chains
//! of tracks nobody plays live — not armed, no input monitoring, no live
//! MIDI — are rendered ahead of the playhead on a thread of their own, in
//! the engine's block size, into a ring per track. The audio thread's
//! graph keeps a reader for them in front of their strip (faders, pan,
//! mute and sends stay immediate) and everything live. A heavy plugin chain
//! then no longer has to finish within one device buffer: spikes are
//! absorbed by the lookahead.
//!
//! * **Prediction.** The anticipator runs its own copy of the transport
//!   ([`TransportState`] is plain data), so it produces exactly the
//!   positions the audio thread will ask for, loop wraps and scrub snippets
//!   included. A transport command that changes them starts a new
//!   *sequence*: the audio thread hands the commanded state over
//!   ([`AheadLink::command`]) and carries on until the rings hold the new
//!   sequence's first blocks (or a deadline passed), then switches — play,
//!   stop and locate take effect ~20 ms later, without a gap.
//! * **Rings** tag their audio with (sequence, position, playing); the
//!   reader skips audio of older sequences, waits for audio that is not
//!   there yet (silence, counted as a miss) and never reads audio of a
//!   sequence it has not switched to.
//! * **Graphs.** `build_graph` puts an anticipated track's chain into a
//!   second graph ending in an [`AheadWriter`] and gives the realtime graph
//!   an [`AheadReader`] with the chain's latency (delay compensation is
//!   unchanged). The anticipator processes that graph (in parallel on a
//!   pool of its own) and adopts processor state across rebuilds like the
//!   audio thread does; rings persist across rebuilds.
//! * **Tiers.** Buses rendered ahead (`build::ahead_sets`) take a second,
//!   shallow anticipator: it reads the deep one's rings for the tracks that
//!   reach them, runs their strips and sends (faders, pan, mute and send
//!   levels), the buses' sums and devices, in small blocks only a device
//!   callback and two blocks ahead of the audio thread
//!   ([`shallow_lookahead`]), at the audio thread's scheduling. A block is
//!   rendered there once the deep tier has rendered it (its progress). Fader
//!   moves are heard after that much, bus devices still leave the audio
//!   thread. The audio thread hands every sequence to both tiers and
//!   switches when both are ready.
//! * **Limits.** Changes to an anticipated track's plugins (from their
//!   editors, inserted plugins) and to its clips are heard after the
//!   lookahead. A track that becomes live moves to the realtime graph at
//!   once (its plugins jump back by up to the lookahead); a track that
//!   stops being live stays there until playback stops.

use crate::context::EngineContext;
use crate::snapshot::TimelineSnapshot;
use faderframe_audio_graph::{CompiledGraph, NodeIo, ProcessContext, Processor};
use faderframe_realtime::{MailboxReceiver, MailboxSender, TryCell, WorkerPool};
use faderframe_transport::{TransportCommand, TransportState};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Frames of a new sequence rendered before the audio thread switches to
/// it.
const STARTUP_BLOCKS: usize = 2;

/// A run of frames in a ring: which sequence, from which position, and
/// whether the transport was playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Segment {
    seq: u64,
    pos: i64,
    frames: u32,
    playing: bool,
}

struct RingWriter {
    samples: Producer<f32>,
    segments: Producer<Segment>,
}

struct RingReader {
    samples: Consumer<f32>,
    segments: Consumer<Segment>,
    /// The segment being read (what is left of it).
    current: Option<Segment>,
}

/// One anticipated track's rendered audio on its way to the audio thread
/// (interleaved frames and their segments).
pub struct AheadRing {
    channels: usize,
    capacity: usize,
    writer: TryCell<RingWriter>,
    reader: TryCell<RingReader>,
}

impl AheadRing {
    /// Room for `frames` frames of `channels` channels (control thread).
    pub fn new(channels: usize, frames: usize) -> Arc<Self> {
        let channels = channels.max(1);
        let (samples_tx, samples_rx) = RingBuffer::new(frames * channels);
        let (segments_tx, segments_rx) = RingBuffer::new(frames / 16 + 64);
        Arc::new(Self {
            channels,
            capacity: frames,
            writer: TryCell::new(RingWriter {
                samples: samples_tx,
                segments: segments_tx,
            }),
            reader: TryCell::new(RingReader {
                samples: samples_rx,
                segments: segments_rx,
                current: None,
            }),
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Tells rings apart in node keys: a new ring needs new nodes (an
    /// adopted reader would keep reading the old one).
    pub fn identity(self: &Arc<Self>) -> u64 {
        Arc::as_ptr(self) as usize as u64
    }

    /// Frames the writer can add now (0 while it is busy).
    fn free_frames(&self) -> usize {
        self.writer.try_lock().map_or(0, |w| {
            if w.segments.slots() == 0 {
                0
            } else {
                w.samples.slots() / self.channels
            }
        })
    }
}

/// End of an anticipated chain (in the ahead graph): its audio into the
/// track's ring.
pub struct AheadWriter {
    ring: Arc<AheadRing>,
}

impl AheadWriter {
    pub fn new(ring: Arc<AheadRing>) -> Self {
        Self { ring }
    }
}

impl Processor<EngineContext> for AheadWriter {
    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(input) = io.audio_in.first() else {
            return;
        };
        let Some(mut w) = self.ring.writer.try_lock() else {
            return;
        };
        let (n, ch) = (io.frames, self.ring.channels);
        if w.segments.slots() == 0 {
            return;
        }
        let Ok(chunk) = w.samples.write_chunk_uninit(n * ch) else {
            // No room: the anticipator waits for room before a block.
            return;
        };
        let have = input.num_channels();
        chunk.fill_from_iter((0..n).flat_map(|i| {
            (0..ch).map(move |c| {
                if have == 0 {
                    0.0
                } else {
                    input.channel(c.min(have - 1))[i]
                }
            })
        }));
        let t = &cx.data.transport;
        let _ = w.segments.push(Segment {
            seq: cx.data.ahead_seq,
            pos: t.sample_position,
            frames: n as u32,
            playing: t.playing,
        });
    }
}

/// An anticipated track in the realtime graph: its rendered audio, or
/// silence when it is not there (a miss).
pub struct AheadReader {
    ring: Arc<AheadRing>,
    latency: u32,
    misses: Arc<AtomicU64>,
    /// Has played audio: from then on missing audio is late (before, the
    /// anticipator is still starting on this track).
    started: bool,
}

impl AheadReader {
    pub fn new(ring: Arc<AheadRing>, latency: u32, misses: Arc<AtomicU64>) -> Self {
        Self {
            ring,
            latency,
            misses,
            started: false,
        }
    }
}

/// Drop `samples` samples of the ring (they belong to nobody now).
fn skip(r: &mut RingReader, samples: usize) {
    let n = samples.min(r.samples.slots());
    if let Ok(chunk) = r.samples.read_chunk(n) {
        chunk.commit_all();
    }
}

impl Processor<EngineContext> for AheadReader {
    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let Some(out) = io.audio_out.first_mut() else {
            return;
        };
        let n = io.frames;
        let ch = self.ring.channels;
        let Some(mut guard) = self.ring.reader.try_lock() else {
            out.clear();
            return;
        };
        let r = &mut *guard;
        let seq = cx.data.ahead_seq;
        let t = &cx.data.transport;
        let mut done = 0usize;
        while done < n {
            let seg = match r.current {
                Some(s) => s,
                None => match r.segments.pop() {
                    Ok(s) => s,
                    Err(_) => break,
                },
            };
            r.current = Some(seg);
            if seg.seq > seq {
                // A sequence the audio thread has not switched to yet.
                break;
            }
            let expected = if t.playing {
                t.sample_position + done as i64
            } else {
                t.sample_position
            };
            if seg.seq < seq || seg.playing != t.playing {
                skip(r, seg.frames as usize * ch);
                r.current = None;
                continue;
            }
            if seg.playing && seg.pos > expected {
                // Ahead of us (a track just added mid-sequence): wait.
                break;
            }
            if seg.playing && seg.pos < expected {
                // Behind: drop what we have passed.
                let behind = ((expected - seg.pos) as usize).min(seg.frames as usize);
                skip(r, behind * ch);
                let left = seg.frames as usize - behind;
                r.current = (left > 0).then_some(Segment {
                    pos: seg.pos + behind as i64,
                    frames: left as u32,
                    ..seg
                });
                continue;
            }
            if !seg.playing && seg.pos != expected {
                skip(r, seg.frames as usize * ch);
                r.current = None;
                continue;
            }
            let k = (n - done).min(seg.frames as usize);
            let Ok(chunk) = r.samples.read_chunk(k * ch) else {
                // Samples not there (cannot happen: segments follow them).
                r.current = None;
                break;
            };
            let (a, b) = chunk.as_slices();
            let outs = out.num_channels();
            for (j, s) in a.iter().chain(b).enumerate() {
                let (frame, c) = (j / ch, j % ch);
                if c < outs {
                    out.channel_mut(c)[done + frame] = *s;
                }
            }
            chunk.commit_all();
            self.started = true;
            // More output channels than the ring has: repeat them.
            for c in ch..outs {
                for i in done..done + k {
                    let s = out.channel(c % ch)[i];
                    out.channel_mut(c)[i] = s;
                }
            }
            let left = seg.frames as usize - k;
            r.current = (left > 0).then_some(Segment {
                pos: if seg.playing {
                    seg.pos + k as i64
                } else {
                    seg.pos
                },
                frames: left as u32,
                ..seg
            });
            done += k;
        }
        if done < n {
            for c in 0..out.num_channels() {
                out.channel_mut(c)[done..n].fill(0.0);
            }
            // Sequence 0 or no audio yet: rendering ahead is only starting.
            if seq > 0 && self.started {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// A transport state the anticipator renders from, under a sequence id.
#[derive(Clone, Debug)]
pub(crate) struct Sequence {
    id: u64,
    transport: TransportState,
}

struct Pending {
    seq: u64,
    transport: TransportState,
    waited: usize,
}

/// The audio thread's scheduling, published once by the link for
/// anticipators that keep pace with it (the shallow tier adopts it).
pub(crate) struct Scheduling {
    /// As `faderframe_realtime::thread_scheduling` packs it; 0: not yet.
    packed: AtomicU64,
    extra: AtomicU64,
}

impl Scheduling {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            packed: AtomicU64::new(0),
            extra: AtomicU64::new(0),
        })
    }
}

/// One anticipator as the audio thread's link sees it.
pub(crate) struct Tier {
    to_ahead: Producer<Sequence>,
    ready: Arc<AtomicU64>,
}

/// The audio thread's side: the current sequence and transport changes on
/// their way to the anticipators.
pub(crate) struct AheadLink {
    seq: u64,
    /// The shallow tier first: it is handed a sequence before the deep
    /// one, so that it never sees the deep one's progress in a sequence it
    /// has not been given.
    tiers: Vec<Tier>,
    pending: Option<Pending>,
    /// Frames to wait for the anticipators before switching anyway.
    max_wait: usize,
    scheduling: Arc<Scheduling>,
    scheduling_read: bool,
}

impl AheadLink {
    /// A link to `tiers` (shallow first) at `rate` (control thread).
    pub(crate) fn new(tiers: Vec<Tier>, rate: f64, scheduling: Arc<Scheduling>) -> Self {
        Self {
            seq: 0,
            tiers,
            pending: None,
            max_wait: (rate * 0.25) as usize,
            scheduling,
            scheduling_read: false,
        }
    }

    /// The sequence the audio thread plays.
    pub(crate) fn seq(&self) -> u64 {
        self.seq
    }

    /// Start: the anticipators render the current state as sequence 1.
    pub(crate) fn begin(&mut self, current: &TransportState) {
        self.hand_over(current.clone());
    }

    /// Hand `next` to the anticipators as a new sequence; the audio thread
    /// switches once they are ready (realtime-safe).
    fn hand_over(&mut self, next: TransportState) {
        let seq = self.pending.as_ref().map_or(self.seq, |p| p.seq) + 1;
        let mut sent = true;
        for tier in &mut self.tiers {
            sent &= tier
                .to_ahead
                .push(Sequence {
                    id: seq,
                    transport: next.clone(),
                })
                .is_ok();
        }
        self.pending = Some(Pending {
            seq,
            transport: next,
            // Not sent (an anticipator is far behind): switch at once and
            // live with misses.
            waited: if sent { 0 } else { self.max_wait },
        });
    }

    /// A transport command (audio thread). Commands that change what plays
    /// when are deferred until the anticipators have rendered ahead.
    pub(crate) fn command(&mut self, current: &mut TransportState, cmd: TransportCommand) {
        if let TransportCommand::SetRecording(_) = cmd {
            // Does not move the playhead.
            current.apply(cmd);
            if let Some(p) = &mut self.pending {
                p.transport.apply(cmd);
            }
            return;
        }
        let base = self
            .pending
            .as_ref()
            .map_or_else(|| current.clone(), |p| p.transport.clone());
        let mut next = base.clone();
        next.apply(cmd);
        if next == base {
            // Nothing changes (e.g. the same loop range again).
            return;
        }
        self.hand_over(next);
    }

    /// Before a callback: switch to the pending sequence when every
    /// anticipator has it ready (or it took too long).
    pub(crate) fn poll(&mut self, current: &mut TransportState, frames: usize) {
        if !self.scheduling_read {
            // Once, on the audio thread (a system call, no allocation).
            self.scheduling_read = true;
            let (packed, extra) = faderframe_realtime::thread_scheduling();
            self.scheduling.extra.store(extra, Ordering::Relaxed);
            self.scheduling.packed.store(packed, Ordering::Release);
        }
        let Some(p) = &mut self.pending else { return };
        let ready = self
            .tiers
            .iter()
            .all(|t| t.ready.load(Ordering::Acquire) >= p.seq);
        if ready || p.waited >= self.max_wait {
            *current = p.transport.clone();
            self.seq = p.seq;
            self.pending = None;
        } else {
            p.waited += frames;
        }
    }
}

/// The ahead graph and the rings it fills.
pub(crate) struct AheadGraph {
    pub graph: Box<CompiledGraph<EngineContext>>,
    pub rings: Vec<Arc<AheadRing>>,
}

/// What the anticipator hands back to be dropped on the control thread.
pub(crate) enum AheadGarbage {
    Graph(#[allow(dead_code)] Box<AheadGraph>),
    // The mailbox's box, handed back whole: nothing is freed off the
    // control thread.
    #[allow(clippy::redundant_allocation)]
    Timeline(#[allow(dead_code)] Box<Arc<TimelineSnapshot>>),
    #[allow(clippy::redundant_allocation)]
    Modulation(#[allow(dead_code)] Box<Arc<crate::modulation::ModulationSet>>),
}

/// The control thread's handle on the anticipator.
pub(crate) struct Anticipator {
    pub graph_tx: MailboxSender<AheadGraph>,
    pub timeline_tx: MailboxSender<Arc<TimelineSnapshot>>,
    pub modulation_tx: MailboxSender<Arc<crate::modulation::ModulationSet>>,
    pub garbage: Consumer<AheadGarbage>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Lookahead in frames (at most: see [`Self::set_lookahead`]).
    pub lookahead: usize,
    /// The block it renders in.
    pub block: usize,
    /// Its progress: [`progress`] packs the sequence it renders and the
    /// frames of it rendered.
    pub progress: Arc<AtomicU64>,
    target: Arc<AtomicUsize>,
}

impl Anticipator {
    /// Render this far ahead from now on (at most the lookahead it was
    /// started with: its rings are sized for that).
    pub fn set_lookahead(&self, frames: usize) {
        self.target.store(
            frames.clamp(self.block * 2, self.lookahead),
            Ordering::Relaxed,
        );
    }
}

/// Bits of a progress word that count frames.
const PROGRESS_FRAMES: u32 = 40;

/// A progress word: `seq` rendered for `frames`.
fn progress(seq: u64, frames: usize) -> u64 {
    (seq << PROGRESS_FRAMES) | (frames as u64).min((1 << PROGRESS_FRAMES) - 1)
}

fn unpack_progress(word: u64) -> (u64, usize) {
    (
        word >> PROGRESS_FRAMES,
        (word & ((1 << PROGRESS_FRAMES) - 1)) as usize,
    )
}

/// How far ahead the shallow tier renders, in frames, with device
/// callbacks of `device_block` (0: unknown) and blocks of `block`: a
/// callback and two blocks, so the audio thread always finds what it needs
/// and the tier has a block's time to spare. Fader moves on strips it
/// renders are heard this much later.
pub fn shallow_lookahead(device_block: usize, block: usize) -> usize {
    device_block.max(block) + 2 * block
}

/// The shallow tier's block.
pub const SHALLOW_BLOCK: usize = 128;

/// Most frames the shallow tier ever renders ahead (its rings' size).
pub const SHALLOW_MAX: usize = 4096 + 2 * SHALLOW_BLOCK;

impl Drop for Anticipator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        while self.garbage.pop().is_ok() {}
    }
}

/// Everything the anticipator thread owns.
struct Worker {
    graph_rx: MailboxReceiver<AheadGraph>,
    timeline_rx: MailboxReceiver<Arc<TimelineSnapshot>>,
    modulation_rx: MailboxReceiver<Arc<crate::modulation::ModulationSet>>,
    sequences: Consumer<Sequence>,
    ready: Arc<AtomicU64>,
    garbage: Producer<AheadGarbage>,
    stop: Arc<AtomicBool>,
    ctx: EngineContext,
    rate: f64,
    block: usize,
    lookahead: Arc<AtomicUsize>,
    pool: Option<WorkerPool>,
    progress: Arc<AtomicU64>,
    /// The tier whose rings this one reads (it renders a block once that
    /// one has).
    upstream: Option<Arc<AtomicU64>>,
    /// The audio thread's scheduling, to adopt.
    scheduling: Option<Arc<Scheduling>>,
    idle: Duration,
}

/// How an anticipator runs.
pub(crate) struct Options {
    pub block: usize,
    pub lookahead: Duration,
    pub threads: usize,
    /// Read the rings of the anticipator with this progress.
    pub upstream: Option<Arc<AtomicU64>>,
    /// Keep pace with the audio thread: adopt its scheduling, sleep briefly.
    pub follow: Option<Arc<Scheduling>>,
}

/// Start an anticipator as `options` say; returns its handle and its tier
/// for the audio thread's link. `ctx` is a context for its graph (shared
/// tables, an empty MIDI input).
pub(crate) fn start(ctx: EngineContext, rate: f64, options: Options) -> (Anticipator, Tier) {
    let block = options.block.max(16);
    let lookahead = ((options.lookahead.as_secs_f64() * rate) as usize).max(block * 2);
    let (graph_tx, graph_rx) = faderframe_realtime::mailbox();
    let (timeline_tx, timeline_rx) = faderframe_realtime::mailbox();
    let (modulation_tx, modulation_rx) = faderframe_realtime::mailbox();
    let (seq_tx, seq_rx) = RingBuffer::new(64);
    let (garbage_tx, garbage_rx) = RingBuffer::new(64);
    let ready = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let progress_word = Arc::new(AtomicU64::new(0));
    let target = Arc::new(AtomicUsize::new(lookahead));
    let idle = if options.follow.is_some() {
        Duration::from_micros(250)
    } else {
        Duration::from_millis(1)
    };
    let worker = Worker {
        graph_rx,
        timeline_rx,
        modulation_rx,
        sequences: seq_rx,
        ready: Arc::clone(&ready),
        garbage: garbage_tx,
        stop: Arc::clone(&stop),
        ctx,
        rate,
        block,
        lookahead: Arc::clone(&target),
        pool: (options.threads > 0)
            .then(|| WorkerPool::new(faderframe_realtime::PoolConfig::new(options.threads))),
        progress: Arc::clone(&progress_word),
        upstream: options.upstream,
        scheduling: options.follow,
        idle,
    };
    let name = if worker.scheduling.is_some() {
        "ff-ahead-bus"
    } else {
        "ff-ahead"
    };
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || worker.run())
        .ok();
    (
        Anticipator {
            graph_tx,
            timeline_tx,
            modulation_tx,
            garbage: garbage_rx,
            stop,
            thread,
            lookahead,
            block,
            progress: progress_word,
            target,
        },
        Tier {
            to_ahead: seq_tx,
            ready,
        },
    )
}

/// Ring room for `lookahead` frames plus a new sequence's start.
pub(crate) fn ring_frames(lookahead: usize, block: usize) -> usize {
    lookahead + (STARTUP_BLOCKS + 4) * block.max(16)
}

impl Worker {
    fn retire(&mut self, g: AheadGarbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // The control thread has not collected for a while: keep it.
            std::mem::forget(g);
        }
    }

    fn run(mut self) {
        let _ftz = faderframe_realtime::ScopedFlushDenormals::new();
        let mut graph: Option<Box<AheadGraph>> = None;
        // (sequence, its transport, frames rendered, first block pending)
        let mut current: Option<(u64, TransportState, usize, bool)> = None;
        let startup = STARTUP_BLOCKS * self.block;
        let idle = self.idle;
        let mut adopted = 0u64;
        while !self.stop.load(Ordering::Relaxed) {
            if let Some(s) = &self.scheduling {
                let packed = s.packed.load(Ordering::Acquire);
                if packed != adopted {
                    adopted = packed;
                    faderframe_realtime::apply_thread_scheduling(
                        packed,
                        s.extra.load(Ordering::Relaxed),
                    );
                }
            }
            if let Some(mut new) = self.graph_rx.take() {
                if let Some(mut old) = graph.take() {
                    new.graph.adopt_state_from(&mut old.graph);
                    self.retire(AheadGarbage::Graph(old));
                }
                graph = Some(new);
            }
            if let Some(mut t) = self.timeline_rx.take() {
                std::mem::swap(&mut self.ctx.timeline, &mut *t);
                self.retire(AheadGarbage::Timeline(t));
            }
            if let Some(mut m) = self.modulation_rx.take() {
                std::mem::swap(&mut self.ctx.modulation, &mut *m);
                self.retire(AheadGarbage::Modulation(m));
            }
            while let Ok(s) = self.sequences.pop() {
                current = Some((s.id, s.transport, 0, true));
            }
            let Some((seq, transport, primed, first)) = current.as_mut() else {
                std::thread::sleep(idle);
                continue;
            };
            let Some(g) = graph.as_mut().filter(|g| !g.rings.is_empty()) else {
                // Nothing to render ahead: always ready.
                self.ready.fetch_max(*seq, Ordering::AcqRel);
                self.progress
                    .store(progress(*seq, usize::MAX), Ordering::Release);
                std::thread::sleep(idle.max(Duration::from_millis(2)));
                continue;
            };
            let capacity = g.rings.iter().map(|r| r.capacity()).min().unwrap_or(0);
            let free = g.rings.iter().map(|r| r.free_frames()).min().unwrap_or(0);
            let n = transport.frames_until_wrap(self.block);
            let buffered = capacity.saturating_sub(free);
            let lookahead = self.lookahead.load(Ordering::Relaxed);
            if free < n || (buffered >= lookahead && *primed >= startup) {
                std::thread::sleep(idle);
                continue;
            }
            // Reading another tier's rings: only what it has rendered.
            if let Some(up) = &self.upstream {
                let (up_seq, up_frames) = unpack_progress(up.load(Ordering::Acquire));
                if up_seq < *seq || (up_seq == *seq && up_frames < *primed + n) {
                    std::thread::sleep(idle);
                    continue;
                }
            }
            self.ctx.transport = transport.info(&self.ctx.timeline.timeline, self.rate);
            self.ctx.discontinuity = transport.take_discontinuity() || std::mem::take(first);
            self.ctx.ahead_seq = *seq;
            let cx = ProcessContext {
                frames: n,
                sample_rate: self.rate,
                data: &self.ctx,
            };
            match &self.pool {
                Some(pool) => g.graph.process_parallel(&cx, pool),
                None => g.graph.process(&cx),
            }
            transport.advance(n);
            *primed += n;
            self.progress
                .store(progress(*seq, *primed), Ordering::Release);
            if *primed >= startup {
                self.ready.fetch_max(*seq, Ordering::AcqRel);
            }
        }
    }
}
