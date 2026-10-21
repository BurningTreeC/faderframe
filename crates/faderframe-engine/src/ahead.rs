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
//! * **Loop changes** start no sequence: what is rendered stays valid up
//!   to where the anticipators are. The audio thread picks a frame of the
//!   sequence beyond every anticipator (their progress and two blocks) and
//!   hands the new range over as an *amend*; every anticipator and the
//!   audio thread itself take it on at exactly that frame (their blocks
//!   split there), so all follow the same path — no repeat, no gap,
//!   nothing rendered twice. The new range is in force about the
//!   lookahead later (a wrap already rendered still happens once). While
//!   stopped or scrubbing a loop changes nothing that plays: it applies at
//!   once. An anticipator already past the frame (a few frames at most)
//!   keeps what it rendered when no wrap of either range falls in
//!   between, else it says so and the audio thread starts a sequence after
//!   all.
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
use faderframe_transport::{LoopRange, TransportCommand, TransportState};
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
            // Interleaved → planar with counters (no division per sample).
            let (mut frame, mut c) = (done, 0);
            for s in a.iter().chain(b) {
                if c < outs {
                    out.channel_mut(c)[frame] = *s;
                }
                c += 1;
                if c == ch {
                    c = 0;
                    frame += 1;
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

/// A loop change for sequence `seq` from its frame `at` on (`None`: at
/// once — nothing that plays depends on it, the transport is stopped or
/// scrubbing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Amend {
    seq: u64,
    at: Option<usize>,
    range: Option<LoopRange>,
    enabled: bool,
}

/// What the audio thread hands an anticipator.
#[derive(Clone, Debug)]
pub(crate) enum ToAhead {
    /// Render from this transport state from now on.
    Sequence(Sequence),
    /// Change the loop of the sequence being rendered.
    Amend(Amend),
}

/// Loop changes in flight at most (each side keeps them in a fixed
/// array; one more starts a sequence instead).
const MAX_AMENDS: usize = 32;

/// Frames a progress word counts for an anticipator with nothing to
/// render (always ready).
const IDLE_FRAMES: usize = (1 << PROGRESS_FRAMES) - 1;

/// Whether a loop change at frame `at` of a sequence is all the same to
/// what an anticipator has rendered of it up to `primed`, past `at`
/// (`transport` as it is there, `jumped_at`: the frame after its newest
/// wrap or jump): the playhead ran on without a jump since `at`, and the
/// new range would not have wrapped in between either.
fn amend_late_is_equal(
    transport: &TransportState,
    primed: usize,
    jumped_at: usize,
    at: usize,
    range: Option<LoopRange>,
    enabled: bool,
) -> bool {
    if !transport.playing() || transport.scrubbing() {
        return true;
    }
    if jumped_at > at {
        return false;
    }
    let to = transport.position();
    let from = to - (primed - at) as i64;
    !(enabled && range.is_some_and(|r| from < r.end && to >= r.end))
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
    to_ahead: Producer<ToAhead>,
    ready: Arc<AtomicU64>,
    resets: Resets,
    /// Where it is ([`progress`]).
    progress: Arc<AtomicU64>,
    /// The block it renders in.
    block: usize,
    /// The newest sequence a loop change could not be taken on in (it was
    /// past the frame and had rendered a wrap that the change moves).
    amend_failed: Arc<AtomicU64>,
}

/// A panic's resets between the link and an anticipator: how many were
/// asked for, how many it has done, and from which frame of which sequence
/// (a progress word) what it renders is clean — what it had rendered
/// before still carries the tails.
#[derive(Clone)]
struct Resets {
    asked: Arc<AtomicU64>,
    done: Arc<AtomicU64>,
    clean_from: Arc<AtomicU64>,
}

impl Resets {
    fn new() -> Self {
        Self {
            asked: Arc::new(AtomicU64::new(0)),
            done: Arc::new(AtomicU64::new(0)),
            clean_from: Arc::new(AtomicU64::new(0)),
        }
    }
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
    /// Frames of the current sequence played, this callback's included.
    played: usize,
    /// Resets asked of the anticipators; whether what plays still carries
    /// tails from before the last one, and how long that has been.
    resets: u64,
    resetting: bool,
    reset_waited: usize,
    /// Frames of the current sequence played before this callback.
    cb_start: usize,
    /// Loop changes the audio thread takes on at a frame of a sequence, in
    /// order (the first `amend_count`).
    amends: [Scheduled; MAX_AMENDS],
    amend_count: usize,
    /// The loop as last commanded, while the newest sequence's state may
    /// not have it yet: what a further command starts from.
    wanted_loop: Option<(Option<LoopRange>, bool)>,
}

/// A loop change the audio thread takes on at frame `at` of `seq`.
#[derive(Clone, Copy, Debug, Default)]
struct Scheduled {
    seq: u64,
    at: usize,
    range: Option<LoopRange>,
    enabled: bool,
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
            played: 0,
            resets: 0,
            resetting: false,
            reset_waited: 0,
            cb_start: 0,
            amends: [Scheduled::default(); MAX_AMENDS],
            amend_count: 0,
            wanted_loop: None,
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
                .push(ToAhead::Sequence(Sequence {
                    id: seq,
                    transport: next.clone(),
                }))
                .is_ok();
        }
        // The new sequence has the loop as last commanded.
        self.wanted_loop = None;
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
        let base = self.newest(current);
        let mut next = base.clone();
        next.apply(cmd);
        if next == base {
            // Nothing changes (e.g. the same loop range again).
            return;
        }
        if matches!(
            cmd,
            TransportCommand::SetLoopRange(_) | TransportCommand::SetLoopEnabled(_)
        ) && self.amend(current, &next)
        {
            return;
        }
        // It moves the playhead (or the loop change found no room): a new
        // sequence.
        self.hand_over(next);
    }

    /// The transport as last commanded: the pending sequence's (or the
    /// current one) with the loop as last changed.
    fn newest(&self, current: &TransportState) -> TransportState {
        let mut t = self
            .pending
            .as_ref()
            .map_or_else(|| current.clone(), |p| p.transport.clone());
        if let Some((range, enabled)) = self.wanted_loop {
            t.set_loop(range, enabled);
        }
        t
    }

    /// A loop change without a new sequence (realtime-safe): at once when
    /// nothing that plays depends on it, else from a frame beyond every
    /// anticipator. `false`: no room to hand it over.
    fn amend(&mut self, current: &mut TransportState, next: &TransportState) -> bool {
        let (range, enabled) = (next.loop_range(), next.loop_enabled());
        let newest = self.pending.as_ref().map_or(self.seq, |p| p.seq);
        // Single producer: room now is room for the pushes below.
        if self.tiers.iter().any(|t| t.to_ahead.slots() == 0) || self.amend_count == MAX_AMENDS {
            return false;
        }
        let at = (next.playing() && !next.scrubbing()).then(|| self.amend_frame(newest));
        match at {
            None => match self.pending.as_mut() {
                Some(p) => p.transport.set_loop(range, enabled),
                None => current.set_loop(range, enabled),
            },
            Some(at) => {
                self.amends[self.amend_count] = Scheduled {
                    seq: newest,
                    at,
                    range,
                    enabled,
                };
                self.amend_count += 1;
            }
        }
        for tier in &mut self.tiers {
            let _ = tier.to_ahead.push(ToAhead::Amend(Amend {
                seq: newest,
                at,
                range,
                enabled,
            }));
        }
        self.wanted_loop = Some((range, enabled));
        true
    }

    /// The frame of sequence `seq` a loop change is taken on at: beyond
    /// what every anticipator has rendered of it, with two of its blocks to
    /// spare (it may be rendering one), and not before the audio thread.
    fn amend_frame(&self, seq: u64) -> usize {
        let own = if seq == self.seq { self.played } else { 0 };
        let mut at = own;
        for tier in &self.tiers {
            let (tier_seq, frames) = unpack_progress(tier.progress.load(Ordering::Acquire));
            if frames >= IDLE_FRAMES {
                // Nothing to render ahead.
                continue;
            }
            // Not started on it yet: from its first frame.
            let frames = if tier_seq == seq { frames } else { 0 };
            at = at.max(frames + 2 * tier.block);
        }
        at
    }

    /// Before a chunk of the callback at `offset`: take on the loop
    /// changes due there; frames until the next one in the sequence
    /// playing (the chunk ends there).
    pub(crate) fn amend_due(
        &mut self,
        current: &mut TransportState,
        offset: usize,
    ) -> Option<usize> {
        let frame = self.cb_start + offset;
        let mut done = 0;
        let mut next = None;
        for a in &self.amends[..self.amend_count] {
            if a.seq < self.seq {
                // For a sequence handed over and passed.
                done += 1;
                continue;
            }
            if a.seq == self.seq && a.at <= frame {
                current.set_loop(a.range, a.enabled);
                done += 1;
                continue;
            }
            if a.seq == self.seq {
                next = Some(a.at - frame);
            }
            break;
        }
        if done > 0 {
            self.amends.copy_within(done..self.amend_count, 0);
            self.amend_count -= done;
        }
        next
    }

    /// Before another link (or none) takes over: what this one was waiting
    /// for is in force from now on.
    pub(crate) fn settle(&mut self, current: &mut TransportState) {
        *current = self.newest(current);
        self.pending = None;
        self.amend_count = 0;
        self.wanted_loop = None;
    }

    /// Whether what the audio thread plays still carries tails from before
    /// the last [`Self::reset`] (as of the end of this callback).
    pub(crate) fn restarting(&self) -> bool {
        self.resetting
    }

    /// The panic (realtime-safe): every anticipator resets its processors
    /// on its own thread, where it is in its sequence; the transport goes
    /// on as it was (no new sequence, no jump). What they rendered before
    /// — up to the lookahead — still carries the tails: [`Self::restarting`]
    /// says so until the audio thread has played past it.
    pub(crate) fn reset(&mut self) {
        self.resets += 1;
        for tier in &self.tiers {
            tier.resets.asked.store(self.resets, Ordering::Release);
        }
        self.resetting = true;
        self.reset_waited = 0;
    }

    /// After the callback's frames: whether they are still from before the
    /// anticipators' resets.
    fn still_resetting(&mut self, frames: usize) -> bool {
        let now = progress(self.seq, self.played);
        let mut waiting = false;
        for tier in &self.tiers {
            if tier.resets.done.load(Ordering::Acquire) < self.resets {
                // Not done yet (or that anticipator is stuck: give up after
                // a while rather than stay silent).
                waiting = true;
                continue;
            }
            if now < tier.resets.clean_from.load(Ordering::Acquire) {
                return true;
            }
        }
        if waiting {
            self.reset_waited += frames;
            return self.reset_waited < self.max_wait * 4;
        }
        false
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
        // An anticipator could not take a loop change on where it was
        // asked to (it had rendered a wrap the change moves before it saw
        // the change — rare): a sequence after all.
        let newest = self.pending.as_ref().map_or(self.seq, |p| p.seq);
        if self.wanted_loop.is_some()
            && self
                .tiers
                .iter()
                .any(|t| t.amend_failed.load(Ordering::Acquire) == newest)
        {
            let next = self.newest(current);
            self.hand_over(next);
        }
        let mut switched = false;
        if let Some(p) = &mut self.pending {
            let ready = self
                .tiers
                .iter()
                .all(|t| t.ready.load(Ordering::Acquire) >= p.seq);
            if ready || p.waited >= self.max_wait {
                *current = p.transport.clone();
                self.seq = p.seq;
                self.pending = None;
                switched = true;
            } else {
                p.waited += frames;
            }
        }
        // This callback plays the sequence's next `frames` (the
        // anticipators count its frames from the same start).
        self.cb_start = if switched { 0 } else { self.played };
        self.played = self.cb_start + frames;
        if self.resetting {
            self.resetting = self.still_resetting(frames);
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
    sequences: Consumer<ToAhead>,
    ready: Arc<AtomicU64>,
    resets: Resets,
    amend_failed: Arc<AtomicU64>,
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
    /// This thread's place among the readers of streamed pages: pages are
    /// freed only after a cycle reading them is over (`PageTable`).
    reader: Option<faderframe_realtime::Reader>,
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
    /// The epoch guarding the streamed pages its graphs read.
    pub epoch: Arc<faderframe_realtime::Epoch>,
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
    let amend_failed = Arc::new(AtomicU64::new(0));
    let resets = Resets::new();
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
        resets: resets.clone(),
        amend_failed: Arc::clone(&amend_failed),
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
        reader: {
            let r = options.epoch.reader();
            if r.is_none() {
                tracing::warn!("render ahead: no reader slot left on the page epoch");
            }
            r
        },
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
            progress: Arc::clone(&progress_word),
            target,
        },
        Tier {
            to_ahead: seq_tx,
            ready,
            resets,
            progress: progress_word,
            block,
            amend_failed,
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
        // Loop changes of the current sequence waiting for their frame (in
        // order), and the frame after its newest wrap or jump.
        let mut amends: std::collections::VecDeque<Scheduled> =
            std::collections::VecDeque::with_capacity(MAX_AMENDS);
        let mut jumped_at = 0usize;
        let startup = STARTUP_BLOCKS * self.block;
        let idle = self.idle;
        let mut adopted = 0u64;
        let mut resets_done = 0u64;
        // The deep tier hasn't the shallow one's urgency: further below
        // the audio thread (`faderframe_realtime::priority`).
        let helper = if self.scheduling.is_some() {
            faderframe_realtime::priority::Helper::Near
        } else {
            faderframe_realtime::priority::Helper::Ahead
        };
        while !self.stop.load(Ordering::Relaxed) {
            match &self.scheduling {
                // macOS: the audio thread's time constraint, as it is.
                Some(s) if cfg!(target_os = "macos") => {
                    let packed = s.packed.load(Ordering::Acquire);
                    if packed != adopted {
                        adopted = packed;
                        faderframe_realtime::apply_thread_scheduling(
                            packed,
                            s.extra.load(Ordering::Relaxed),
                        );
                    }
                }
                _ => faderframe_realtime::priority::follow_audio(helper),
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
            // The tier whose rings this one reads, as far as it is: read
            // before the messages, so that a loop change it has taken on
            // is among them (the audio thread hands it over here first).
            let upstream = self
                .upstream
                .as_ref()
                .map(|up| unpack_progress(up.load(Ordering::Acquire)));
            while let Ok(m) = self.sequences.pop() {
                match m {
                    ToAhead::Sequence(s) => {
                        current = Some((s.id, s.transport, 0, true));
                        amends.clear();
                        jumped_at = 0;
                    }
                    ToAhead::Amend(a) => {
                        let Some((seq, transport, primed, _)) = current.as_mut() else {
                            continue;
                        };
                        if *seq != a.seq {
                            continue;
                        }
                        match a.at {
                            Some(at) if at > *primed && amends.len() < MAX_AMENDS => {
                                amends.push_back(Scheduled {
                                    seq: a.seq,
                                    at,
                                    range: a.range,
                                    enabled: a.enabled,
                                });
                            }
                            Some(at)
                                if at > *primed
                                    || !amend_late_is_equal(
                                        transport, *primed, jumped_at, at, a.range, a.enabled,
                                    ) =>
                            {
                                // What it rendered went another way: the
                                // audio thread starts a sequence.
                                self.amend_failed.fetch_max(a.seq, Ordering::AcqRel);
                            }
                            // Now (as good as at its frame). Frames only
                            // grow from one change to the next: none
                            // waits before it.
                            _ if amends.is_empty() => transport.set_loop(a.range, a.enabled),
                            _ => {
                                self.amend_failed.fetch_max(a.seq, Ordering::AcqRel);
                            }
                        }
                    }
                }
            }
            // A panic: reset here (this thread's time, not the audio
            // thread's) and say from which frame on what it renders is
            // clean.
            let asked = self.resets.asked.load(Ordering::Acquire);
            if asked != resets_done {
                if let Some(g) = graph.as_mut() {
                    g.graph.reset_all();
                }
                let from = current
                    .as_ref()
                    .map_or(0, |(seq, _, primed, _)| progress(*seq, *primed));
                self.resets.clean_from.store(from, Ordering::Release);
                self.resets.done.store(asked, Ordering::Release);
                resets_done = asked;
            }
            let Some((seq, transport, primed, first)) = current.as_mut() else {
                std::thread::sleep(idle);
                continue;
            };
            let Some(g) = graph.as_mut().filter(|g| !g.rings.is_empty()) else {
                // Nothing to render ahead: always ready; loop changes need
                // not wait for a frame.
                for a in amends.drain(..) {
                    transport.set_loop(a.range, a.enabled);
                }
                self.ready.fetch_max(*seq, Ordering::AcqRel);
                self.progress
                    .store(progress(*seq, usize::MAX), Ordering::Release);
                std::thread::sleep(idle.max(Duration::from_millis(2)));
                continue;
            };
            // Loop changes due here; a block ends where the next one is.
            while let Some(a) = amends.front().filter(|a| a.at <= *primed) {
                transport.set_loop(a.range, a.enabled);
                amends.pop_front();
            }
            let capacity = g.rings.iter().map(|r| r.capacity()).min().unwrap_or(0);
            let free = g.rings.iter().map(|r| r.free_frames()).min().unwrap_or(0);
            let mut n = transport.frames_until_wrap(self.block);
            if let Some(a) = amends.front() {
                n = n.min(a.at - *primed);
            }
            let buffered = capacity.saturating_sub(free);
            let lookahead = self.lookahead.load(Ordering::Relaxed);
            if free < n || (buffered >= lookahead && *primed >= startup) {
                std::thread::sleep(idle);
                continue;
            }
            // Reading another tier's rings: only what it has rendered.
            if let Some((up_seq, up_frames)) = upstream
                && (up_seq < *seq || (up_seq == *seq && up_frames < *primed + n))
            {
                std::thread::sleep(idle);
                continue;
            }
            let info = transport.info(&self.ctx.timeline.timeline, self.rate);
            self.ctx.transport = info;
            // What this graph played, as the audio thread keeps it for its
            // own: nodes behind latency read their automation (and
            // modulation) where their audio was played
            // (`nodes::automation_at`), not at position 0 of an empty
            // history.
            self.ctx
                .history
                .record(info.sample_position, info.playing, transport.jumps(), n);
            self.ctx.discontinuity = transport.take_discontinuity() || std::mem::take(first);
            self.ctx.ahead_seq = *seq;
            let cx = ProcessContext {
                frames: n,
                sample_rate: self.rate,
                data: &self.ctx,
            };
            // Streamed pages read in this cycle stay until it is over.
            if let Some(r) = &self.reader {
                r.begin();
            }
            match &self.pool {
                Some(pool) => g.graph.process_parallel(&cx, pool),
                None => g.graph.process(&cx),
            }
            if let Some(r) = &self.reader {
                r.end();
            }
            let (moving, before) = (transport.playing(), transport.position());
            transport.advance(n);
            *primed += n;
            if moving && transport.position() != before + n as i64 {
                // Wrapped (or a snippet jumped).
                jumped_at = *primed;
            }
            self.progress
                .store(progress(*seq, *primed), Ordering::Release);
            if *primed >= startup {
                self.ready.fetch_max(*seq, Ordering::AcqRel);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tier(block: usize) -> (Tier, Consumer<ToAhead>) {
        let (tx, rx) = RingBuffer::new(64);
        let tier = Tier {
            to_ahead: tx,
            ready: Arc::new(AtomicU64::new(0)),
            resets: Resets::new(),
            progress: Arc::new(AtomicU64::new(0)),
            block,
            amend_failed: Arc::new(AtomicU64::new(0)),
        };
        (tier, rx)
    }

    /// A link to one anticipator in 256-frame blocks, playing sequence 1
    /// from `pos` (one callback of `frames` in).
    fn playing(pos: i64, frames: usize) -> (AheadLink, TransportState, Consumer<ToAhead>) {
        let (t, mut rx) = tier(256);
        let ready = Arc::clone(&t.ready);
        let mut link = AheadLink::new(vec![t], 48_000.0, Scheduling::new());
        let mut current = TransportState::default();
        current.apply(TransportCommand::Locate(pos));
        current.apply(TransportCommand::Play);
        link.begin(&current);
        ready.store(1, Ordering::Release);
        link.poll(&mut current, frames);
        assert_eq!(link.seq(), 1);
        assert!(matches!(rx.pop(), Ok(ToAhead::Sequence(_))));
        (link, current, rx)
    }

    fn range(a: i64, b: i64) -> Option<LoopRange> {
        LoopRange::new(a, b)
    }

    #[test]
    fn a_loop_change_while_playing_lands_beyond_the_anticipator() {
        let (mut link, mut current, mut rx) = playing(0, 256);
        // The anticipator has rendered 10 000 frames of the sequence.
        link.tiers[0]
            .progress
            .store(progress(1, 10_000), Ordering::Release);
        link.command(
            &mut current,
            TransportCommand::SetLoopRange(range(5_000, 20_000)),
        );
        // No new sequence: an amend two blocks beyond it.
        assert!(link.pending.is_none());
        let Ok(ToAhead::Amend(a)) = rx.pop() else {
            panic!("an amend")
        };
        assert_eq!(a.seq, 1);
        assert_eq!(a.at, Some(10_000 + 512));
        assert_eq!(a.range, range(5_000, 20_000));
        // Not yet on the audio thread; the chunk ends at its frame.
        assert_eq!(current.loop_range(), None);
        link.poll(&mut current, 256);
        assert_eq!(link.amend_due(&mut current, 0), Some(10_512 - 256));
        assert_eq!(current.loop_range(), None);
        assert_eq!(link.amend_due(&mut current, 10_512 - 256), None);
        assert_eq!(current.loop_range(), range(5_000, 20_000));
        // The same range again: nothing.
        link.command(
            &mut current,
            TransportCommand::SetLoopRange(range(5_000, 20_000)),
        );
        assert!(rx.pop().is_err());
    }

    #[test]
    fn stopped_a_loop_change_holds_at_once() {
        let (t, mut rx) = tier(256);
        let ready = Arc::clone(&t.ready);
        let mut link = AheadLink::new(vec![t], 48_000.0, Scheduling::new());
        let mut current = TransportState::default();
        link.begin(&current);
        ready.store(1, Ordering::Release);
        link.poll(&mut current, 256);
        let _ = rx.pop();
        link.command(
            &mut current,
            TransportCommand::SetLoopRange(range(0, 48_000)),
        );
        link.command(&mut current, TransportCommand::SetLoopEnabled(true));
        assert!(current.looping());
        for _ in 0..2 {
            let Ok(ToAhead::Amend(a)) = rx.pop() else {
                panic!("an amend")
            };
            assert_eq!(a.at, None);
        }
        assert!(link.pending.is_none());
    }

    #[test]
    fn a_locate_after_a_loop_change_carries_it() {
        let (mut link, mut current, mut rx) = playing(0, 256);
        link.tiers[0]
            .progress
            .store(progress(1, 4_096), Ordering::Release);
        link.command(
            &mut current,
            TransportCommand::SetLoopRange(range(0, 30_000)),
        );
        link.command(&mut current, TransportCommand::Locate(1_000));
        let _ = rx.pop();
        let Ok(ToAhead::Sequence(s)) = rx.pop() else {
            panic!("a sequence")
        };
        assert_eq!(s.transport.loop_range(), range(0, 30_000));
        assert_eq!(s.transport.position(), 1_000);
        // The old sequence still takes it on at its frame until the switch.
        link.poll(&mut current, 256);
        assert_eq!(link.seq(), 1);
        link.amend_due(&mut current, 4_608 - 256);
        assert_eq!(current.loop_range(), range(0, 30_000));
    }

    #[test]
    fn an_anticipator_past_the_frame_makes_a_sequence_after_all() {
        let (mut link, mut current, mut rx) = playing(0, 256);
        link.command(
            &mut current,
            TransportCommand::SetLoopRange(range(0, 30_000)),
        );
        let _ = rx.pop();
        link.tiers[0].amend_failed.store(1, Ordering::Release);
        link.poll(&mut current, 256);
        let Ok(ToAhead::Sequence(s)) = rx.pop() else {
            panic!("a sequence")
        };
        assert_eq!(s.id, 2);
        assert_eq!(s.transport.loop_range(), range(0, 30_000));
        // Once.
        link.poll(&mut current, 256);
        assert!(rx.pop().is_err());
    }

    #[test]
    fn a_late_loop_change_keeps_what_was_rendered_unless_a_wrap_moves() {
        let mut t = TransportState::default();
        t.apply(TransportCommand::Play);
        t.advance(10_000);
        // Rendered to 10 000 frames (from 0), asked for frame 9 500.
        assert!(amend_late_is_equal(
            &t,
            10_000,
            0,
            9_500,
            range(0, 20_000),
            true
        ));
        assert!(amend_late_is_equal(
            &t,
            10_000,
            0,
            9_500,
            range(0, 9_000),
            true
        ));
        // The new end inside what was rendered past the frame.
        assert!(!amend_late_is_equal(
            &t,
            10_000,
            0,
            9_500,
            range(0, 9_800),
            true
        ));
        assert!(amend_late_is_equal(
            &t,
            10_000,
            0,
            9_500,
            range(0, 9_800),
            false
        ));
        // A wrap rendered after the frame.
        assert!(!amend_late_is_equal(
            &t,
            10_000,
            9_700,
            9_500,
            range(0, 20_000),
            true
        ));
        // Stopped: never.
        t.apply(TransportCommand::Stop);
        assert!(amend_late_is_equal(
            &t,
            10_000,
            9_700,
            9_500,
            range(0, 9_800),
            true
        ));
    }
}
