use crate::EngineError;
use crate::build::build_graph;
use crate::click::{Click, ClickSound, ClickSounds, MetronomeMode, MetronomeShared};
use crate::context::EngineContext;
use crate::modulation::ModulationSet;
use crate::plugins::PluginHost;
use crate::record::{RecordStreams, RecordTarget, Recorder};
use crate::slots::SlotRegistry;
use crate::snapshot::{SourceMap, StreamPlan, TimelineSnapshot};
use faderframe_audio::{AudioCallback, DeviceBuffers, StreamInfo};
use faderframe_audio_graph::{
    CompiledGraph, GraphStats, NodeTimings, PrepareConfig, ProcessContext,
};
use faderframe_core::TrackId;
use faderframe_project::{Impact, Project};
use faderframe_realtime::{
    CallbackMetrics, Epoch, MailboxReceiver, MailboxSender, MeterBank, MeterRange, MeterReading,
    MetricsSnapshot, ParamTable, ScopedFlushDenormals, WorkerPool, mailbox,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{
    LoopRange, TransportCommand, TransportShared, TransportSnapshot, TransportState,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Frames the analysis scope holds (~10 s at 48 kHz).
const SCOPE_FRAMES: usize = 1 << 19;

/// Static engine configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    /// Rate the graph is prepared for; follows the audio stream.
    pub sample_rate: u32,
    /// Internal block size; device callbacks are split into chunks of at
    /// most this many frames (any device buffer size works).
    pub max_block_size: usize,
    pub param_capacity: u32,
    pub meter_capacity: u32,
    /// Control → RT message queue length.
    pub queue_capacity: usize,
    pub measure_nodes: bool,
    /// Graphs doing less work per cycle than this stay on the audio thread
    /// even with a worker pool (see `PrepareConfig::parallel_min_ns`).
    pub parallel_min_ns: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            max_block_size: 512,
            param_capacity: 8192,
            meter_capacity: 16_384,
            queue_capacity: 256,
            measure_nodes: true,
            parallel_min_ns: 40_000,
        }
    }
}

/// Small, ordered control → RT commands. Graphs and timeline snapshots do
/// not use this queue: they go through latest-value mailboxes, so a burst of
/// edits can never overflow it with stale structures.
enum Message {
    Transport(TransportCommand),
    /// Render ahead from now on (the audio thread's end of it), or no more.
    Ahead(Box<crate::ahead::AheadLink>),
    AheadOff,
    ResetProcessors,
    /// Stop when playback reaches this position (once), or never.
    StopAt(Option<i64>),
    BeginRecord(Box<Recorder>),
    EndRecord,
    MidiInput(Box<faderframe_midi::MidiInputQueue>),
    MidiOutput(Box<faderframe_midi::MidiOutputQueue>),
    BeginMidiRecord(Box<crate::midi::MidiRecorder>),
    EndMidiRecord,
    /// Listen to this voice only, or to none.
    Voice(Option<Box<crate::voice::VoiceRun>>),
    /// Listen to one more voice (replacing one on the same input and port).
    VoiceAdd(Box<crate::voice::VoiceRun>),
    /// No more listening to an input on a port.
    VoiceRemove {
        input: u16,
        port: u16,
    },
    VoiceConfig(faderframe_analysis::voice::VoiceConfig, crate::voice::Glide),
    /// Play this file instead of the project (album playback), or no more.
    Preview(Option<Box<crate::preview::Preview>>),
    /// Locate so that `position` is where playback would be at `at_ns` (on
    /// the MIDI clock), compensating for the time until the command is
    /// applied, and play or stop.
    Chase {
        position: i64,
        at_ns: u64,
        play: bool,
    },
    Launch(crate::launch::LaunchCommand),
    /// Play at a speed (see [`crate::varispeed`]), or no more.
    Varispeed(Option<Box<crate::varispeed::Varispeed>>),
}

/// Objects retired by the audio thread, dropped on the control thread.
enum Garbage {
    Graph(#[allow(dead_code)] Box<CompiledGraph<EngineContext>>),
    // The mailbox's box, handed back whole: the audio thread frees nothing.
    #[allow(clippy::redundant_allocation)]
    Timeline(#[allow(dead_code)] Box<Arc<TimelineSnapshot>>),
    #[allow(clippy::redundant_allocation)]
    Modulation(#[allow(dead_code)] Box<Arc<ModulationSet>>),
    AheadLink(#[allow(dead_code)] Box<crate::ahead::AheadLink>),
    Recorder(#[allow(dead_code)] Box<Recorder>),
    MidiQueue(#[allow(dead_code)] Box<faderframe_midi::MidiInputQueue>),
    MidiOutQueue(#[allow(dead_code)] Box<faderframe_midi::MidiOutputQueue>),
    MidiRecorder(#[allow(dead_code)] Box<crate::midi::MidiRecorder>),
    Preview(#[allow(dead_code)] Box<crate::preview::Preview>),
    Varispeed(#[allow(dead_code)] Box<crate::varispeed::Varispeed>),
    Voice(#[allow(dead_code)] Box<crate::voice::VoiceRun>),
    #[allow(clippy::redundant_allocation)]
    ClickSounds(#[allow(dead_code)] Box<Arc<ClickSounds>>),
}

/// When a callback started (MIDI clock) and the transport position then,
/// written together by the audio thread and read as a consistent pair: a
/// sequence lock over atomics (a torn pair would put the position one
/// callback off).
#[derive(Debug, Default)]
struct CallbackStamp {
    seq: AtomicU32,
    ns: AtomicU64,
    position: AtomicI64,
    /// The transport's jumps by then (`TransportState::jumps`), with the
    /// position they put it at.
    jumps: AtomicU32,
}

impl CallbackStamp {
    /// The audio thread (the only writer); wait-free.
    fn store(&self, ns: u64, position: i64, jumps: u32) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.ns.store(ns, Ordering::Relaxed);
        self.position.store(position, Ordering::Relaxed);
        self.jumps.store(jumps, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    fn load(&self) -> (u64, i64, u32) {
        loop {
            let s = self.seq.load(Ordering::Acquire);
            if s & 1 == 0 {
                let ns = self.ns.load(Ordering::Relaxed);
                let position = self.position.load(Ordering::Relaxed);
                let jumps = self.jumps.load(Ordering::Relaxed);
                std::sync::atomic::fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == s {
                    return (ns, position, jumps);
                }
            }
            std::hint::spin_loop();
        }
    }
}

/// State shared between the processor and the controller (atomics only).
#[derive(Debug, Default)]
pub struct EngineShared {
    pub transport: TransportShared,
    /// The clip launcher's state, published after every callback.
    pub launch: crate::launch::LaunchStatus,
    pub metrics: CallbackMetrics,
    stream_sample_rate: AtomicU32,
    stream_buffer_size: AtomicU32,
    stream_inputs: AtomicU32,
    stream_outputs: AtomicU32,
    /// Varispeed: the playback speed (f64 bits; 0 = 1).
    speed: AtomicU64,
    /// The running graph was prepared for a different sample rate.
    rate_mismatch: AtomicBool,
    /// Objects that could not be returned (garbage queue full) and were leaked.
    leaked: AtomicU64,
    graphs_installed: AtomicU64,
    /// Advanced after every callback; lets disk loaders free evicted pages.
    /// Shared by every engine a session creates (only one processes at a
    /// time), so pages retired under one engine are reclaimed safely after
    /// it is replaced.
    pub epoch: Arc<Epoch>,
    /// The last locate's target and when it was asked for (`ns_since_epoch`):
    /// the disk loader reads there first, before the transport (deferred
    /// while rendering ahead) gets there.
    locate_target: AtomicI64,
    locate_at: AtomicU64,
    pub metronome: MetronomeShared,
    /// Frames the recorder had to drop because the writer fell behind.
    record_overruns: AtomicU64,
    /// Frames captured since the engine started.
    recorded_frames: AtomicU64,
    /// MIDI input events dropped (block full) and recorded events lost
    /// (session too slow).
    midi_dropped: AtomicU64,
    midi_record_overruns: AtomicU64,
    /// MIDI output messages lost (sender thread too slow).
    midi_out_dropped: AtomicU64,
    /// Wall-clock time spent processing the graph (all threads at once),
    /// since the engine started.
    graph_wall_ns: AtomicU64,
    /// Start of the last callback on the MIDI clock (0: no MIDI clock) and
    /// the transport position then: where playback is at any moment.
    callback: CallbackStamp,
    /// Frames between processing and hearing (buffer + device).
    output_latency: AtomicU32,
    /// The installed graph's output latency (frames): how late its outputs
    /// are behind the timeline it plays.
    graph_latency: AtomicU32,
    /// The play history around what is heard (the playhead's clock).
    heard: crate::history::HeardClock,
    /// Auditioning, mapped controls, clock outputs.
    pub midi: Arc<crate::midi::MidiShared>,
    /// Album playback (see [`crate::preview`]).
    pub preview: crate::preview::PreviewShared,
    /// A ping asked for: `1 << 63 | output << 16 | input` (0: none).
    ping_request: AtomicU64,
    /// Its answer: the round trip (frames), `PING_PENDING` or
    /// `PING_LOST`.
    ping_result: std::sync::atomic::AtomicI64,
}

/// How long the output fades out and in around a latency change.
const DIP_SECONDS: f64 = 0.004;

/// The output around a graph whose latencies differ from the running one
/// (a plugin's latency changed, one with latency came or went): the
/// paths through it would jump — its delay lines start empty — so the
/// output fades out ([`DIP_SECONDS`]), the graph goes in, the output
/// stays silent until its delay lines are full (its output latency), then
/// fades back in. A short gap instead of clicks; bounded, no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dip {
    None,
    Out {
        done: usize,
    },
    /// Faded out: the graph goes in at the next callback.
    Silent,
    /// The new graph's delay lines filling (frames left).
    Hold {
        left: usize,
    },
    In {
        done: usize,
    },
}

/// The shortest loop the transport plays (shorter ones are lengthened).
pub const MIN_LOOP_SECONDS: f64 = 0.01;

/// No answer yet.
const PING_PENDING: i64 = -1;
/// Nothing came back within a second.
const PING_LOST: i64 = -2;

/// A ping on its way (audio thread).
#[derive(Clone, Copy, Debug)]
struct PingRun {
    output: usize,
    input: usize,
    /// Frames since the impulse went out (`None`: not sent yet).
    elapsed: Option<u64>,
}

/// Create a connected controller/processor pair.
impl EngineShared {
    /// Where a locate asked for within `recent` is going (disk loader).
    pub fn recent_locate(&self, recent: std::time::Duration) -> Option<i64> {
        let at = self.locate_at.load(Ordering::Acquire);
        let now = crate::history::ns_since_epoch(Instant::now());
        (at != 0 && now.saturating_sub(at) < recent.as_nanos() as u64)
            .then(|| self.locate_target.load(Ordering::Relaxed))
    }

    /// The varispeed speed (1: none).
    pub fn speed(&self) -> f64 {
        match f64::from_bits(self.speed.load(Ordering::Relaxed)) {
            s if s > 0.0 && s.is_finite() => s,
            _ => 1.0,
        }
    }
}

pub fn create(config: EngineConfig) -> (EngineController, EngineProcessor) {
    create_with_epoch(config, Epoch::new())
}

/// Like [`create`], reading streamed pages under an existing epoch.
pub fn create_with_epoch(
    config: EngineConfig,
    epoch: Arc<Epoch>,
) -> (EngineController, EngineProcessor) {
    let (tx, rx) = RingBuffer::new(config.queue_capacity);
    // At most two objects (graph + timeline) are retired per callback; the
    // headroom covers the control side being slow to collect.
    let (gtx, grx) = RingBuffer::new(config.queue_capacity.max(64));
    let (graph_tx, graph_rx) = mailbox();
    let (timeline_tx, timeline_rx) = mailbox();
    let (modulation_tx, modulation_rx) = mailbox();
    let (click_tx, click_rx) = mailbox();
    let shared_midi = Arc::new(crate::midi::MidiShared::default());
    // The clock the heard positions are stamped on starts now (not on the
    // audio thread).
    crate::history::now_ns();
    let shared = Arc::new(EngineShared {
        epoch,
        locate_target: AtomicI64::new(0),
        locate_at: AtomicU64::new(0),
        midi: Arc::clone(&shared_midi),

        preview: Default::default(),
        ..EngineShared::default()
    });
    shared
        .stream_sample_rate
        .store(config.sample_rate, Ordering::Relaxed);
    let params = Arc::new(ParamTable::new(config.param_capacity));
    let readback = Arc::new(ParamTable::new(config.param_capacity));
    let meters = Arc::new(MeterBank::new(config.meter_capacity));
    meters.set_rate(config.sample_rate as f32);
    let scope = Arc::new(faderframe_realtime::ScopeRing::new(SCOPE_FRAMES));
    let (midi_input, live_sysex) = crate::midi::MidiInputState::new();
    let processor = EngineProcessor {
        rx,
        graph_rx,
        timeline_rx,
        modulation_rx,
        click_rx,
        garbage: gtx,
        graph: None,
        ctx: EngineContext {
            worker_underruns: AtomicU64::new(0),
            callback_deadline: None,
            transport: Default::default(),
            discontinuity: false,
            timeline: Arc::new(TimelineSnapshot::empty(config.sample_rate as f64)),
            modulation: Arc::default(),
            params: Arc::clone(&params),
            readback: Arc::clone(&readback),
            meters: Arc::clone(&meters),
            scope: Arc::clone(&scope),
            midi_input: crate::midi::MidiInputBlock::with_capacity(
                crate::midi::MIDI_INPUT_CAPACITY,
            ),
            ahead_seq: 0,
            preview_active: false,
            launch: crate::launch::LaunchState::new(),
            history: Default::default(),
            recording: Default::default(),
        },
        transport: TransportState::default(),
        shared: Arc::clone(&shared),
        stream_rate: config.sample_rate,
        recorder: None,
        click: Click::new(Arc::new(ClickSounds::synth(
            ClickSound::default(),
            config.sample_rate,
        ))),
        midi: midi_input,
        midi_recorder: None,
        voices: Vec::with_capacity(crate::voice::MAX_VOICES),
        midi_out: None,
        clock: crate::midi::ClockGen::default(),
        clock_due: 0,
        stop_at: None,
        mtc_due: 0,
        output_latency: 0,
        graph_latency: 0,
        pending_graph: None,
        reset_from: None,
        dip: Dip::None,
        pool: None,
        ahead: None,
        preview: None,
        varispeed: None,
        ping: None,
        callbacks: 0,
        count_in_end: i64::MIN,
        counted_in: CountedIn::No,
    };
    let controller = EngineController {
        config,
        live_sysex,
        readback,
        tx,
        graph_tx,
        timeline_tx,
        modulation_tx,
        click_tx,
        garbage: grx,
        shared,
        params,
        meters,
        scope,
        slots: SlotRegistry::new(config.param_capacity, config.meter_capacity),
        plugins: PluginHost::default(),
        profile: None,
        node_timing: false,
        stats: GraphStats::default(),
        warnings: Vec::new(),
        voices: Vec::new(),
        launch_tracks: Vec::new(),
        param_shape: crate::build::ParamShape::default(),
        meter_delays: std::collections::HashMap::new(),
        track_latencies: std::collections::HashMap::new(),
        low_latency: false,
        low_latency_limit: 0,
        low_latency_tracks: std::collections::HashSet::new(),
        bypassed_for_monitoring: Vec::new(),
        plan: Arc::new(StreamPlan::default()),
        suspended_lanes: Default::default(),
        take_audition: None,
        midi_routing: crate::build::MidiRouting {
            shared: Arc::clone(&shared_midi),
            ..Default::default()
        },
        midi_live: Default::default(),
        edited: Default::default(),
        album_monitor: None,
        listen: crate::build::Listen::default(),
        timeline: Arc::new(TimelineSnapshot::empty(config.sample_rate as f64)),
        modulation: Arc::default(),
        mod_buses: Default::default(),
        mod_shape: Vec::new(),
        ahead: None,
        ahead_setting: None,
        bus_rings: Default::default(),
        cue_rings: Default::default(),
        bus_ahead: None,
        ahead_threads: 0,
        talkback: crate::slots::Talkback::default(),
        ahead_rings: Default::default(),
        ahead_tracks: Default::default(),
        ahead_strips: Default::default(),
        ahead_buses: false,
        graph_device_block: 0,
        graph_device_outputs: 0,
        ahead_misses: Arc::new(AtomicU64::new(0)),
        varispeed: false,
        sent_loop: None,
    };
    (controller, processor)
}

/// The realtime half. Implements [`AudioCallback`]; also driven directly by
/// the offline renderer.
pub struct EngineProcessor {
    rx: Consumer<Message>,
    graph_rx: MailboxReceiver<CompiledGraph<EngineContext>>,
    timeline_rx: MailboxReceiver<Arc<TimelineSnapshot>>,
    click_rx: MailboxReceiver<Arc<ClickSounds>>,
    modulation_rx: MailboxReceiver<Arc<ModulationSet>>,
    garbage: Producer<Garbage>,
    graph: Option<Box<CompiledGraph<EngineContext>>>,
    ctx: EngineContext,
    transport: TransportState,
    shared: Arc<EngineShared>,
    stream_rate: u32,
    recorder: Option<Box<Recorder>>,
    click: Click,
    midi: crate::midi::MidiInputState,
    midi_recorder: Option<Box<crate::midi::MidiRecorder>>,
    /// Voice to MIDI: the listeners, on this thread (room for
    /// `MAX_VOICES`, made on the control side). Boxed as they arrive and
    /// leave: unboxing here would free the box on the audio thread.
    #[allow(clippy::vec_box)]
    voices: Vec<Box<crate::voice::VoiceRun>>,
    midi_out: Option<Box<faderframe_midi::MidiOutputQueue>>,
    clock: crate::midi::ClockGen,
    /// The last MIDI clock and MTC messages' due times: callbacks run late
    /// now and then, so later messages are kept after earlier ones.
    clock_due: u64,
    /// Stop when playback reaches this position (a setlist song's end).
    stop_at: Option<i64>,
    mtc_due: u64,
    /// Frames from a callback to its audio being heard (buffer + device).
    output_latency: u32,
    /// The installed graph's output latency.
    graph_latency: u32,
    /// A graph whose latencies differ, waiting for the output to fade out
    /// (see [`Dip`]).
    pending_graph: Option<Box<CompiledGraph<EngineContext>>>,
    dip: Dip,
    /// A panic's resets under way: the next node of the graph (done while
    /// the output is silent, within a share of each callback).
    reset_from: Option<usize>,
    /// DSP worker threads for the graph (none: serial on the audio thread).
    pool: Option<Arc<WorkerPool>>,
    /// Render-ahead: the current sequence and transport changes on their
    /// way to the anticipator.
    ahead: Option<Box<crate::ahead::AheadLink>>,
    /// Album playback's file.
    preview: Option<Box<crate::preview::Preview>>,
    varispeed: Option<Box<crate::varispeed::Varispeed>>,
    ping: Option<PingRun>,
    /// Device callbacks so far (the thread's scheduling is published on
    /// the first and every few thousand after:
    /// `faderframe_realtime::priority`).
    callbacks: u32,
    /// The play history's frame after the newest count-in frame (the click
    /// counts in until it is heard).
    count_in_end: i64,
    /// Whether playback began with a count-in (the metronome may keep
    /// quiet after it: [`MetronomeShared::count_in_only`]).
    counted_in: CountedIn,
}

/// Playback after a count-in, until the next start without one (the
/// clicks after a stop still sound for the graph's latency: as the run
/// they belong to).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CountedIn {
    No,
    Playing,
    Stopped,
}

impl EngineProcessor {
    /// Upper bound of messages handled per callback (bounds callback time).
    const MAX_MESSAGES_PER_CALLBACK: usize = 64;

    fn retire(&mut self, g: Garbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Never free on the audio thread: leak instead and count it.
            std::mem::forget(g);
            self.shared.leaked.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Take a new graph: at once, or — when a path through it would jump
    /// (a latency changed while sound is going out) — after the output has
    /// faded out ([`Dip`]).
    fn drain_messages(&mut self, audible: bool, reset_until: Instant) {
        if let Some(new) = self.graph_rx.take() {
            let jumps = self
                .graph
                .as_deref()
                .is_some_and(|old| new.timing_changed_from(old));
            if jumps && audible {
                if let Some(older) = self.pending_graph.replace(new) {
                    self.retire(Garbage::Graph(older));
                }
                self.dip = match self.dip {
                    Dip::None => Dip::Out { done: 0 },
                    // Fading in: out again from the gain reached.
                    Dip::In { done } => Dip::Out {
                        done: self.dip_fade().saturating_sub(done),
                    },
                    // Silent already (filling the last graph's delay
                    // lines): this one goes in now instead.
                    Dip::Hold { .. } => Dip::Silent,
                    d @ (Dip::Out { .. } | Dip::Silent) => d,
                };
            } else {
                if let Some(older) = self.pending_graph.take() {
                    self.retire(Garbage::Graph(older));
                }
                self.install_graph(new);
            }
        }
        // A panic's resets, while silent, a share of the callback at a
        // time (they clear megabytes of delay lines).
        if self.dip == Dip::Silent
            && let Some(from) = self.reset_from
        {
            self.reset_from = match self.graph.as_deref_mut() {
                Some(g) => g.reset_nodes(from, reset_until),
                None => None,
            };
            if self.reset_from.is_none() {
                self.click.reset();
            }
        }
        // Faded out: the new graph, silent until its delay lines are full.
        if self.dip == Dip::Silent
            && self.reset_from.is_none()
            && let Some(new) = self.pending_graph.take()
        {
            let fill = new.stats().output_latency as usize;
            self.install_graph(new);
            self.dip = Dip::Hold { left: fill };
        }
        if let Some(mut s) = self.click_rx.take() {
            // New metronome sounds; the old ones go back.
            std::mem::swap(&mut self.click.sounds, &mut *s);
            self.click.reset();
            self.retire(Garbage::ClickSounds(s));
        }
        if let Some(mut t) = self.timeline_rx.take() {
            // The box goes back holding the old snapshot: nothing is freed
            // here.
            std::mem::swap(&mut self.ctx.timeline, &mut *t);
            self.retire(Garbage::Timeline(t));
            // Launched clips whose slots went stop; follow actions anew.
            self.ctx.launch.timeline_changed(&self.ctx.timeline);
        }
        if let Some(mut m) = self.modulation_rx.take() {
            std::mem::swap(&mut self.ctx.modulation, &mut *m);
            self.retire(Garbage::Modulation(m));
        }
        for _ in 0..Self::MAX_MESSAGES_PER_CALLBACK {
            let Ok(msg) = self.rx.pop() else { break };
            match msg {
                Message::Transport(cmd) => self.transport_command(cmd),
                Message::Ahead(mut link) => {
                    // What the old link was waiting for (a command, a loop
                    // change) is in force now: the new one starts there.
                    if let Some(old) = self.ahead.as_deref_mut() {
                        old.settle(&mut self.transport);
                    }
                    link.begin(&self.transport);
                    if let Some(old) = self.ahead.replace(link) {
                        self.retire(Garbage::AheadLink(old));
                    }
                }
                Message::Preview(p) => {
                    if let Some(old) = std::mem::replace(&mut self.preview, p) {
                        self.retire(Garbage::Preview(old));
                    }
                }
                Message::AheadOff => {
                    if let Some(mut old) = self.ahead.take() {
                        old.settle(&mut self.transport);
                        self.retire(Garbage::AheadLink(old));
                    }
                }
                Message::StopAt(at) => self.stop_at = at,
                Message::ResetProcessors => {
                    // Rendered ahead: each anticipator resets on its own
                    // thread; the transport goes on.
                    if let Some(link) = self.ahead.as_deref_mut() {
                        link.reset();
                    }
                    if audible {
                        // Out, the resets while silent, back in.
                        self.reset_from = Some(0);
                        self.dip = match self.dip {
                            Dip::None => Dip::Out { done: 0 },
                            Dip::In { done } => Dip::Out {
                                done: self.dip_fade().saturating_sub(done),
                            },
                            Dip::Hold { .. } => Dip::Silent,
                            d @ (Dip::Out { .. } | Dip::Silent) => d,
                        };
                    } else {
                        if let Some(g) = &mut self.graph {
                            g.reset_all();
                        }
                        self.click.reset();
                    }
                }
                Message::BeginRecord(r) => {
                    self.ctx.recording = r.now();
                    if let Some(old) = self.recorder.replace(r) {
                        self.retire(Garbage::Recorder(old));
                    }
                }
                Message::EndRecord => {
                    self.ctx.recording = Default::default();
                    if let Some(old) = self.recorder.take() {
                        self.retire(Garbage::Recorder(old));
                    }
                }
                Message::MidiInput(q) => {
                    // Pointer swap; the old queue is dropped on the control
                    // thread.
                    if let Some(old) = self.midi.replace_queue(Some(q)) {
                        self.retire(Garbage::MidiQueue(old));
                    }
                }
                Message::MidiOutput(q) => {
                    if let Some(old) = self.midi_out.replace(q) {
                        self.retire(Garbage::MidiOutQueue(old));
                    }
                }
                Message::BeginMidiRecord(r) => {
                    if let Some(old) = self.midi_recorder.replace(r) {
                        self.retire(Garbage::MidiRecorder(old));
                    }
                }
                Message::Voice(run) => {
                    while let Some(mut old) = self.voices.pop() {
                        old.release(&mut self.midi);
                        self.retire(Garbage::Voice(old));
                    }
                    if let Some(run) = run {
                        self.voices.push(run);
                    }
                }
                Message::VoiceAdd(run) => {
                    if let Some(i) = self
                        .voices
                        .iter()
                        .position(|v| v.input() == run.input() && v.port() == run.port())
                    {
                        let mut old = self.voices.swap_remove(i);
                        old.release(&mut self.midi);
                        self.retire(Garbage::Voice(old));
                    }
                    if self.voices.len() < self.voices.capacity() {
                        self.voices.push(run);
                    } else {
                        // No room (never grown here): not listened to.
                        self.retire(Garbage::Voice(run));
                    }
                }
                Message::VoiceRemove { input, port } => {
                    if let Some(i) = self
                        .voices
                        .iter()
                        .position(|v| v.input() == input && v.port() == port)
                    {
                        let mut old = self.voices.swap_remove(i);
                        old.release(&mut self.midi);
                        self.retire(Garbage::Voice(old));
                    }
                }
                Message::VoiceConfig(config, glide) => {
                    for v in &mut self.voices {
                        v.set(config, glide, &mut self.midi);
                    }
                }
                Message::EndMidiRecord => {
                    if let Some(old) = self.midi_recorder.take() {
                        self.retire(Garbage::MidiRecorder(old));
                    }
                }
                Message::Chase {
                    position,
                    at_ns,
                    play,
                } => {
                    let now = self.midi.clock().map_or(at_ns, |c| c.now_ns());
                    let late = now as f64 - at_ns as f64;
                    let rate = self.stream_rate as f64;
                    let adjusted = position + if play { (late * rate / 1e9) as i64 } else { 0 };
                    self.transport_command(TransportCommand::Locate(adjusted));
                    self.transport_command(if play {
                        TransportCommand::Play
                    } else {
                        TransportCommand::Stop
                    });
                }
                Message::Varispeed(v) => {
                    if let Some(old) = std::mem::replace(&mut self.varispeed, v) {
                        self.retire(Garbage::Varispeed(old));
                    }
                }
                Message::Launch(cmd) => {
                    let rate = self.stream_rate as f64;
                    self.ctx.launch.command(
                        cmd,
                        self.transport.position(),
                        self.transport.playing(),
                        &self.ctx.timeline.timeline,
                        rate,
                    );
                    self.ctx.launch.follow(&self.ctx.timeline);
                }
            }
        }
    }

    fn install_graph(&mut self, mut new: Box<CompiledGraph<EngineContext>>) {
        // A panic's resets under way: the new graph's nodes from the first.
        if self.reset_from.is_some() {
            self.reset_from = Some(0);
        }
        if let Some(mut old) = self.graph.take() {
            new.adopt_state_from(&mut old);
            self.retire(Garbage::Graph(old));
        }
        self.graph_latency = new.stats().output_latency;
        self.shared
            .graph_latency
            .store(self.graph_latency, Ordering::Relaxed);
        self.graph = Some(new);
        self.shared.graphs_installed.fetch_add(1, Ordering::Relaxed);
    }

    /// Frames a [`Dip`]'s fades take.
    fn dip_fade(&self) -> usize {
        ((f64::from(self.stream_rate) * DIP_SECONDS) as usize).max(16)
    }

    /// The output's gain through a [`Dip`] (after the callback's audio).
    fn apply_dip(&mut self, io: &mut dyn DeviceBuffers) {
        let fade = self.dip_fade();
        let n = io.frames();
        let gain_at = |dip: Dip, i: usize| -> f32 {
            match dip {
                Dip::None => 1.0,
                Dip::Out { done } => 1.0 - ((done + i) as f32 / fade as f32).min(1.0),
                Dip::Silent => 0.0,
                Dip::Hold { left } => {
                    if i < left {
                        0.0
                    } else {
                        ((i - left) as f32 / fade as f32).min(1.0)
                    }
                }
                Dip::In { done } => ((done + i) as f32 / fade as f32).min(1.0),
            }
        };
        if self.dip == Dip::None {
            return;
        }
        let dip = self.dip;
        for c in 0..io.output_channels() {
            for (i, s) in io.output(c).iter_mut().enumerate() {
                *s *= gain_at(dip, i);
            }
        }
        self.dip = match dip {
            Dip::None => Dip::None,
            Dip::Out { done } if done + n >= fade => Dip::Silent,
            Dip::Out { done } => Dip::Out { done: done + n },
            // Waiting for the next callback to take the graph (or none
            // came: a newer one replaced it and went in at once), or for a
            // panic's resets (here and by the anticipators: what they
            // rendered before still carries the tails).
            Dip::Silent
                if self.pending_graph.is_none()
                    && self.reset_from.is_none()
                    && !self
                        .ahead
                        .as_deref()
                        .is_some_and(crate::ahead::AheadLink::restarting) =>
            {
                Dip::In { done: 0 }
            }
            Dip::Silent => Dip::Silent,
            Dip::Hold { left } if left > n => Dip::Hold { left: left - n },
            Dip::Hold { left } if n - left >= fade => Dip::None,
            Dip::Hold { left } => Dip::In { done: n - left },
            Dip::In { done } if done + n >= fade => Dip::None,
            Dip::In { done } => Dip::In { done: done + n },
        };
    }

    /// A transport command: at once, or — rendering ahead — when the
    /// anticipator has the new state ready.
    fn transport_command(&mut self, cmd: TransportCommand) {
        match self.ahead.as_deref_mut() {
            Some(link) => link.command(&mut self.transport, cmd),
            None => self.transport.apply(cmd),
        }
    }

    /// Process graphs on `pool`'s worker threads as well (control thread,
    /// before the processor goes live). Several engines may share a pool.
    pub fn set_worker_pool(&mut self, pool: Option<Arc<WorkerPool>>) {
        self.pool = pool;
    }

    pub fn worker_pool(&self) -> Option<&Arc<WorkerPool>> {
        self.pool.as_ref()
    }

    /// Process one device callback. Realtime-safe.
    pub fn process_device(&mut self, io: &mut dyn DeviceBuffers) {
        let started = Instant::now();
        let _ftz = ScopedFlushDenormals::new();
        let frames = io.frames();
        if frames > 0 {
            // The audio thread's priority, for the threads that work for
            // it (a system call now and then).
            if self.callbacks.is_multiple_of(4096) {
                faderframe_realtime::priority::note_audio_thread();
            }
            self.callbacks = self.callbacks.wrapping_add(1);
        }
        // A panic's resets get a quarter of the callback.
        let reset_until = started
            + std::time::Duration::from_secs_f64(
                frames as f64 / f64::from(self.stream_rate.max(1)) * 0.25,
            );
        self.drain_messages(frames > 0, reset_until);
        if frames == 0 {
            // Idle pump (no stream running): apply state, measure nothing.
            self.shared.transport.publish(&self.transport);
            self.shared.epoch.advance();
            return;
        }
        // Varispeed: the engine renders the frames the device's need at
        // the speed (resampled both ways).
        match self.varispeed.take() {
            Some(mut v) => {
                let speed = self.shared.speed();
                v.process(io, speed, |e| self.render(e, started));
                self.varispeed = Some(v);
            }
            None => self.render(io, started),
        }
        self.apply_dip(io);
        self.ping(io);
    }

    /// A round-trip ping (hardware inserts): an impulse onto an output
    /// after the graph's, then the input watched for it, the frames counted
    /// from the output sample to the input one.
    fn ping(&mut self, io: &mut dyn DeviceBuffers) {
        if self.ping.is_none() {
            let r = self.shared.ping_request.swap(0, Ordering::Relaxed);
            if r != 0 {
                self.ping = Some(PingRun {
                    output: ((r >> 16) & 0xffff) as usize,
                    input: (r & 0xffff) as usize,
                    elapsed: None,
                });
            }
        }
        let Some(mut run) = self.ping else {
            return;
        };
        let frames = io.frames();
        match run.elapsed {
            None => {
                if run.output >= io.output_channels() || run.input >= io.input_channels() {
                    self.shared.ping_result.store(PING_LOST, Ordering::Relaxed);
                    self.ping = None;
                    return;
                }
                if let Some(s) = io.output(run.output).first_mut() {
                    *s += 0.5;
                }
                run.elapsed = Some(frames as u64);
            }
            Some(elapsed) => {
                let found = io.input(run.input).iter().position(|s| s.abs() > 0.1);
                if let Some(f) = found {
                    self.shared
                        .ping_result
                        .store((elapsed + f as u64) as i64, Ordering::Relaxed);
                    self.ping = None;
                    return;
                }
                let elapsed = elapsed + frames as u64;
                if elapsed > u64::from(self.stream_rate.max(1)) {
                    self.shared.ping_result.store(PING_LOST, Ordering::Relaxed);
                    self.ping = None;
                    return;
                }
                run.elapsed = Some(elapsed);
            }
        }
        self.ping = Some(run);
    }

    /// One callback's processing (the device's frames, or the engine's
    /// under varispeed).
    fn render(&mut self, io: &mut dyn DeviceBuffers, started: Instant) {
        let frames = io.frames();
        for c in 0..io.output_channels() {
            io.output(c).fill(0.0);
        }
        self.ctx.preview_active = self.preview.is_some() && self.shared.preview.is_active();
        let rate = self.stream_rate as f64;
        // Buffered DSP shares this device deadline across smaller graph chunks.
        // Leave 10% (at least 100 us) for downstream work and driver return.
        let period = std::time::Duration::from_secs_f64(frames as f64 / rate);
        let reserve = period
            .mul_f64(0.1)
            .max(std::time::Duration::from_micros(100));
        let graph_ok = self
            .graph
            .as_ref()
            .is_some_and(|g| g.config().sample_rate == rate);
        self.shared
            .rate_mismatch
            .store(!graph_ok && self.graph.is_some(), Ordering::Relaxed);
        let max_block = self
            .graph
            .as_ref()
            .map_or(1024, |g| g.processing_quantum())
            .max(1);
        // Whole callbacks at or below the reservoir retain its no-wait realtime
        // policy. Only internal chunks of a larger callback share this budget.
        self.ctx.callback_deadline =
            (frames > max_block).then_some(started + period.saturating_sub(reserve));

        if let Some(link) = self.ahead.as_deref_mut() {
            link.poll(&mut self.transport, frames);
        }
        self.ctx.ahead_seq = self.ahead.as_deref().map_or(0, |l| l.seq());
        let inputs = io.input_channels();
        for v in &mut self.voices {
            if v.rate() != self.stream_rate || usize::from(v.input()) >= inputs {
                v.release(&mut self.midi);
            }
        }
        self.midi.take(frames, rate, &self.shared.midi_dropped);
        // Voice to MIDI: what the inputs sing joins this callback's MIDI.
        if let Some(clock) = self.midi.clock() {
            let now = clock.now_ns();
            let rate = self.stream_rate;
            let hears =
                |v: &crate::voice::VoiceRun| v.rate() == rate && usize::from(v.input()) < inputs;
            // The analyses (pitch tracking: FFTs) side by side on the DSP
            // workers when several listen, not one after another here.
            {
                let mut listening = crate::voice::Listening::new();
                for v in self.voices.iter().filter(|v| hears(v)) {
                    listening.add(v, io.input(usize::from(v.input())));
                }
                match self.pool.as_deref() {
                    Some(pool) if listening.len() > 1 => {
                        pool.run(&listening, (listening.len() - 1).min(pool.useful_helpers()));
                    }
                    _ => faderframe_realtime::PoolJob::work(&listening),
                }
            }
            for v in self.voices.iter_mut().filter(|v| hears(v)) {
                v.deliver(&mut self.midi, now, &self.shared.midi_dropped);
            }
        }
        let callback_ns = self.midi_out.as_ref().map_or(0, |q| q.clock.now_ns());

        if let Some(clock) = self.midi.clock() {
            self.shared.callback.store(
                clock.now_ns(),
                self.transport.position(),
                self.transport.jumps(),
            );
        }
        let mut graph_ns = 0u64;
        let mut offset = 0;
        let frame_cb = self.ctx.history.end();
        while offset < frames {
            // A loop change rendered ahead is taken on at its frame, where
            // the anticipators take it on.
            let until_amend = self
                .ahead
                .as_deref_mut()
                .and_then(|l| l.amend_due(&mut self.transport, offset));
            let mut n = self
                .transport
                .frames_until_wrap((frames - offset).min(max_block));
            if let Some(k) = until_amend {
                n = n.min(k).max(1);
            }
            // A stop on the exact frame: the chunk ends there.
            if let Some(at) = self.stop_at
                && self.transport.playing()
                && !self.transport.scrubbing()
            {
                let pos = self.transport.position();
                if pos < at {
                    n = n.min((at - pos) as usize).max(1);
                }
            }
            self.midi.chunk(offset, n, &mut self.ctx.midi_input);
            let info = self.transport.info(&self.ctx.timeline.timeline, rate);
            self.ctx.transport = info;
            let counting_in = self.transport.counting_in();
            self.ctx.history.record(
                info.sample_position,
                info.playing,
                self.transport.jumps(),
                counting_in,
                n,
            );
            self.counted_in = match (counting_in > 0, info.playing, self.counted_in) {
                (true, _, _) => {
                    self.count_in_end = self.ctx.history.end();
                    CountedIn::Playing
                }
                (false, false, CountedIn::Playing) => CountedIn::Stopped,
                (false, true, CountedIn::Stopped) => CountedIn::No,
                (false, _, c) => c,
            };
            self.ctx.discontinuity = self.transport.take_discontinuity();
            self.ctx
                .launch
                .transport(info.playing, self.transport.position());
            if self.ctx.discontinuity {
                self.click.reset();
            }
            let pos = self.transport.position();
            if info.playing
                && info.recording
                && let Some(rec) = self.midi_recorder.as_deref_mut()
            {
                rec.capture(
                    &self.ctx.midi_input,
                    pos,
                    n,
                    &self.shared.midi_record_overruns,
                    &self.shared.midi.consumed,
                );
            }
            if info.playing
                && info.recording
                && let Some(rec) = self.recorder.as_deref_mut()
            {
                let got = rec.capture(&*io, offset, n, pos, &self.shared.record_overruns);
                self.shared
                    .recorded_frames
                    .fetch_add(got as u64, Ordering::Relaxed);
            }
            let clock_ports = self.shared.midi.clock_ports.load(Ordering::Relaxed);
            let scrubbing = self.transport.scrubbing();
            // Scrub snippets don't start and stop external gear.
            if clock_ports != 0
                && !scrubbing
                && let Some(q) = self.midi_out.as_deref_mut()
            {
                let ns_per_frame = 1e9 / rate.max(1.0);
                // Due when this position is heard (the graph's latency too).
                let latency = (self.output_latency + self.graph_latency) as usize + offset;
                let dropped = &self.shared.midi_out_dropped;
                let last = &mut self.clock_due;
                self.clock
                    .chunk(&info, self.ctx.discontinuity, n, |o, bytes| {
                        let due = (callback_ns
                            + ((latency + o as usize) as f64 * ns_per_frame) as u64)
                            .max(*last + 1);
                        *last = due;
                        for port in 0..64u16 {
                            if clock_ports & (1u64 << port) != 0
                                && let Some(m) =
                                    faderframe_midi::MidiOutputEvent::new(port, due, bytes)
                                && q.producer.push(m).is_err()
                            {
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
            }
            let mtc_ports = self.shared.midi.mtc_ports.load(Ordering::Relaxed);
            if mtc_ports != 0
                && !scrubbing
                && let Some(q) = self.midi_out.as_deref_mut()
            {
                let ns_per_frame = 1e9 / rate.max(1.0);
                let latency = (self.output_latency + self.graph_latency) as usize + offset;
                let dropped = &self.shared.midi_out_dropped;
                let last = &mut self.mtc_due;
                crate::midi::MtcGen.chunk(&info, n, &self.shared.midi, |o, bytes| {
                    let due = (callback_ns + ((latency + o as usize) as f64 * ns_per_frame) as u64)
                        .max(*last + 1);
                    *last = due;
                    for port in 0..64u16 {
                        if mtc_ports & (1u64 << port) != 0
                            && let Some(m) = faderframe_midi::MidiOutputEvent::new(port, due, bytes)
                            && q.producer.push(m).is_err()
                        {
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            if graph_ok && let Some(graph) = self.graph.as_deref_mut() {
                let ins = io.input_channels();
                graph.fill_device_inputs(n, |first, buf| {
                    for c in 0..buf.num_channels() {
                        let ch = first as usize + c;
                        if ch < ins {
                            buf.channel_mut(c)
                                .copy_from_slice(&io.input(ch)[offset..offset + n]);
                        } else {
                            buf.channel_mut(c).fill(0.0);
                        }
                    }
                });
                let cx = ProcessContext {
                    frames: n,
                    sample_rate: rate,
                    data: &self.ctx,
                };
                let g0 = Instant::now();
                match self.pool.as_deref() {
                    Some(pool) => graph.process_parallel(&cx, pool),
                    None => graph.process(&cx),
                }
                graph_ns += g0.elapsed().as_nanos() as u64;
                // External MIDI: due when this callback's audio is heard.
                if let Some(q) = self.midi_out.as_deref_mut() {
                    let base = callback_ns;
                    let latency = self.output_latency as usize + offset;
                    let ns_per_frame = 1e9 / rate.max(1.0);
                    let dropped = &self.shared.midi_out_dropped;
                    graph.read_event_outputs(|port, buf| {
                        if port == crate::midi::NO_PORT {
                            return;
                        }
                        for ev in buf.iter() {
                            let due = base
                                + ((latency + ev.sample_offset as usize) as f64 * ns_per_frame)
                                    as u64;
                            let m = if let faderframe_midi::MidiEvent::NoteExpression {
                                channel,
                                key,
                                kind,
                                value,
                            } = ev.event
                            {
                                // For MIDI 2.0 ports (per-note controllers).
                                faderframe_midi::MidiOutputEvent::expression(
                                    port, due, channel, key, kind, value,
                                )
                            } else {
                                let (bytes, len) = ev.event.to_bytes();
                                let Some(m) =
                                    faderframe_midi::MidiOutputEvent::new(port, due, &bytes[..len])
                                else {
                                    continue;
                                };
                                m
                            };
                            if q.producer.push(m).is_err() {
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }
                let outs = io.output_channels();
                graph.read_device_outputs(|first, buf| {
                    for c in 0..buf.num_channels() {
                        let ch = first as usize + c;
                        if ch < outs {
                            let dst = &mut io.output(ch)[offset..offset + n];
                            for (o, s) in dst.iter_mut().zip(buf.channel(c)) {
                                *o += *s;
                            }
                        }
                    }
                });
            }
            // After a count-in the take may record without the click.
            let click = match self.shared.metronome.mode() {
                MetronomeMode::Off => false,
                MetronomeMode::Recording => info.recording,
                MetronomeMode::Always => true,
            } && !(self.counted_in != CountedIn::No
                && self.shared.metronome.count_in_only());
            // A count-in before playback (the transport standing while it
            // counts) clicks whatever the metronome's mode: heard with
            // these frames while it ended less than the latency ago.
            let count_in_heard =
                self.count_in_end > self.ctx.history.chunk_start() - i64::from(self.graph_latency);
            if scrubbing {
                // Snippets fade in and out (no clicks between them).
                for c in 0..io.output_channels() {
                    for (i, o) in io.output(c)[offset..offset + n].iter_mut().enumerate() {
                        *o *= self.transport.scrub_gain(i);
                    }
                }
            }
            if (click || count_in_heard || self.click.sounding()) && !scrubbing {
                // As late as the graph's outputs: the beats of what is heard
                // with this chunk (before a loop wrap or a locate, maybe).
                let (clicks, history, timeline, metronome) = (
                    &mut self.click,
                    &self.ctx.history,
                    &self.ctx.timeline,
                    &self.shared.metronome,
                );
                history.pieces(self.graph_latency, 0, n, |o, len, played| {
                    if played.start != clicks.run {
                        // A new run cuts the clicks sounding — not where a
                        // count-in turns into the playback it counted in.
                        let counted_in = clicks.counting_in && played.playing;
                        clicks.run = played.start;
                        if !counted_in {
                            clicks.reset();
                        }
                    }
                    clicks.counting_in = played.count_in > 0;
                    if played.count_in > 0 {
                        // The beats before the start: the timeline's, the
                        // count-in's frames before its position (before
                        // the project's start, the tempo and metre there
                        // carried back).
                        clicks.render(
                            io,
                            offset + o,
                            len,
                            played.pos - played.count_in as i64,
                            &timeline.timeline,
                            rate,
                            metronome,
                            true,
                        );
                    } else if played.playing {
                        // Off: the clicks sounding ring out.
                        clicks.render(
                            io,
                            offset + o,
                            len,
                            played.pos,
                            &timeline.timeline,
                            rate,
                            metronome,
                            click,
                        );
                    }
                });
            }
            self.ctx.launch.played(pos, n);
            self.ctx.launch.follow(&self.ctx.timeline);
            self.transport.advance(n);
            if let Some(at) = self.stop_at
                && self.transport.playing()
                && pos < at
                && self.transport.position() >= at
            {
                self.stop_at = None;
                self.transport_command(TransportCommand::Stop);
            }
            offset += n;
        }
        self.ctx.launch.publish(&self.shared.launch);
        self.shared.heard.publish(
            &self.ctx.history,
            crate::history::ns_since_epoch(started),
            frame_cb,
            i64::from(self.graph_latency),
            (self.transport.jumps(), self.transport.position()),
        );
        if self.ctx.preview_active
            && let Some(p) = self.preview.as_deref_mut()
        {
            // The album instead of the project, on the meters too.
            p.render(
                &self.shared.preview,
                io,
                &self.ctx.scope,
                self.ctx.scope.source(),
            );
        }
        self.shared.transport.publish(&self.transport);
        // Single writer: a plain load and store.
        let g = self.shared.graph_wall_ns.load(Ordering::Relaxed);
        self.shared
            .graph_wall_ns
            .store(g + graph_ns, Ordering::Relaxed);
        let budget_ns = (frames as f64 * 1e9 / rate.max(1.0)) as u64;
        if graph_ok && let Some(graph) = self.graph.as_deref_mut() {
            graph.finish_cycle(budget_ns);
        }
        self.shared
            .metrics
            .record(started.elapsed().as_nanos() as u64, budget_ns);
        self.shared
            .metrics
            .record_xruns(self.ctx.worker_underruns.swap(0, Ordering::Relaxed));
        // No streamed page reference survives past this point.
        self.shared.epoch.advance();
    }
}

impl AudioCallback for EngineProcessor {
    fn prepare(&mut self, info: &StreamInfo) {
        self.stream_rate = info.sample_rate;
        self.output_latency = info.buffer_size + info.output_latency;
        self.shared
            .output_latency
            .store(self.output_latency, Ordering::Relaxed);
        self.shared
            .stream_sample_rate
            .store(info.sample_rate, Ordering::Relaxed);
        self.shared
            .stream_buffer_size
            .store(info.buffer_size, Ordering::Relaxed);
        self.shared
            .stream_inputs
            .store(u32::from(info.input_channels), Ordering::Relaxed);
        self.shared
            .stream_outputs
            .store(u32::from(info.output_channels), Ordering::Relaxed);
    }

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        self.process_device(io);
    }
}

/// Most meter channels a track reports.
pub const METER_MAX: usize = 16;

/// Meter values of one track for the UI (mono tracks report both sides).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackMeter {
    pub left: MeterReading,
    pub right: MeterReading,
    /// Every channel of the strip's output (a surround bed's too).
    pub channels: [MeterReading; METER_MAX],
    pub count: usize,
}

/// The control-thread half: rebuilds graphs, publishes snapshots and
/// parameter values, sends transport commands and reads meters/metrics.
/// Per-node timings of a compiled graph and what each node does: node `i`
/// of [`NodeTimings`] works for `owners[i]`; timing group `g` is the track
/// `groups[g]`.
#[derive(Clone)]
pub struct GraphProfile {
    pub timings: Arc<NodeTimings>,
    pub owners: Arc<[Option<crate::build::NodeOwner>]>,
    pub groups: Arc<[faderframe_core::TrackId]>,
}

/// What is being heard (see [`EngineController::heard_at`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Heard {
    /// The timeline position leaving the speakers.
    pub position: i64,
    /// Whether that audio is playback (not the transport's state: it stops
    /// that much later).
    pub playing: bool,
    /// A locate or stop the ears have not reached yet: where it went.
    pub jumped: Option<i64>,
    /// The run of playback it belongs to (changes at every loop wrap,
    /// locate, start and stop).
    pub run: i64,
    /// Timeline frames it moves per second while playing (the stream's
    /// rate times varispeed's speed).
    pub per_second: f64,
    /// Frames of a count-in heard before playback is (0: none): the
    /// transport stands at `position` while it counts in.
    pub counting_in: u64,
}

/// Where the engine is, from any thread: [`EngineController::clock`].
#[derive(Clone)]
pub struct EngineClock {
    shared: Arc<EngineShared>,
    varispeed_delay: u32,
}

impl EngineClock {
    /// The playhead as of `t_ns` on the MIDI clock (extrapolated from the
    /// last callback while playing); `None` before the first callback.
    pub fn position_at(&self, t_ns: u64) -> Option<i64> {
        let (cb, pos, _) = self.shared.callback.load();
        if cb == 0 {
            return None;
        }
        if !self.shared.transport.snapshot().playing {
            return Some(pos);
        }
        let rate =
            self.shared.stream_sample_rate.load(Ordering::Relaxed) as f64 * self.shared.speed();
        Some(pos + ((t_ns as f64 - cb as f64) * rate / 1e9) as i64)
    }

    /// Frames from processing to hearing: the device's (buffer and
    /// output), varispeed's resampler and the graph's output latency.
    pub fn output_latency(&self) -> u32 {
        self.shared.output_latency.load(Ordering::Relaxed)
            + self.varispeed_delay
            + self.shared.graph_latency.load(Ordering::Relaxed)
    }

    pub fn speed(&self) -> f64 {
        self.shared.speed()
    }

    pub fn playing(&self) -> bool {
        self.shared.transport.snapshot().playing
    }

    /// [`EngineController::heard_ahead`], from any thread.
    pub fn heard_ahead(&self, at: std::time::Instant) -> Option<Heard> {
        let device = self.shared.output_latency.load(Ordering::Relaxed) + self.varispeed_delay;
        heard(&self.shared, device, at, false)
    }
}

/// What is heard at `at` from the play history the audio thread published
/// (`device`: the device's latency, frames); `hold`: never past the frames
/// processed.
fn heard(shared: &EngineShared, device: u32, at: std::time::Instant, hold: bool) -> Option<Heard> {
    let snap = shared.heard.read()?;
    let rate = f64::from(shared.stream_sample_rate.load(Ordering::Relaxed).max(1));
    let speed = shared.speed();
    let since_ns = crate::history::ns_since_epoch(at) as f64 - snap.callback_ns as f64;
    // No callback for a second: no stream runs any more.
    if since_ns > 1e9 {
        return None;
    }
    let device = f64::from(device) * speed;
    let frames = since_ns.max(0.0) * rate * speed / 1e9 - device;
    let played = if hold {
        snap.heard(frames)
    } else {
        snap.heard_ahead(frames)
    }?;
    Some(Heard {
        position: played.pos,
        playing: played.playing,
        jumped: (played.jumps != snap.jumps).then_some(snap.jumped_to),
        run: played.start,
        per_second: rate * speed,
        counting_in: played.count_in,
    })
}

pub struct EngineController {
    /// Live SysEx to the tracks playing from an input port.
    live_sysex: crate::midi::LiveSysexSender,
    config: EngineConfig,
    tx: Producer<Message>,
    graph_tx: MailboxSender<CompiledGraph<EngineContext>>,
    timeline_tx: MailboxSender<Arc<TimelineSnapshot>>,
    click_tx: MailboxSender<Arc<ClickSounds>>,
    modulation_tx: MailboxSender<Arc<ModulationSet>>,
    garbage: Consumer<Garbage>,
    shared: Arc<EngineShared>,
    params: Arc<ParamTable>,
    meters: Arc<MeterBank>,
    scope: Arc<faderframe_realtime::ScopeRing>,
    slots: SlotRegistry,
    readback: Arc<ParamTable>,
    plugins: PluginHost,
    profile: Option<GraphProfile>,
    /// Per-node timing wanted (the performance meter is looking).
    node_timing: bool,
    stats: GraphStats,
    warnings: Vec<String>,
    plan: Arc<StreamPlan>,
    /// Automation lanes being written (they do not drive their parameter).
    suspended_lanes: std::collections::HashSet<faderframe_core::AutomationLaneId>,
    /// A take folder playing one take instead of its comp.
    take_audition: Option<(faderframe_core::ClipId, usize)>,
    /// MIDI port maps and shared MIDI state for graph builds.
    midi_routing: crate::build::MidiRouting,
    /// Tracks taking live MIDI input.
    midi_live: std::collections::HashSet<faderframe_core::TrackId>,
    /// Tracks whose plugins are being edited (kept on the audio thread).
    edited: std::collections::HashSet<faderframe_core::TrackId>,
    /// The album song whose inserts run after the master strip.
    album_monitor: Option<faderframe_core::SongId>,
    /// How the master is listened to.
    listen: crate::build::Listen,
    /// Stretcher voices per track in the installed graph (a timeline edit
    /// that changes them rebuilds the graph).
    voices: Vec<(faderframe_core::TrackId, crate::nodes::StretchVoices)>,
    /// The tracks with launcher clips in the installed graph.
    launch_tracks: Vec<faderframe_core::TrackId>,
    /// What parameter edits can change of the installed graph (diffuse
    /// spreads, folder buses' tracks, sends placing their source): a
    /// change rebuilds.
    param_shape: crate::build::ParamShape,
    /// Frames from where each track is metered to the graph's outputs
    /// (the latency after its strip: buses, the master's devices,
    /// compensation).
    meter_delays: std::collections::HashMap<faderframe_core::TrackId, u32>,
    /// The latency each track's audio has where it is metered (its own
    /// path: sources, plugins, the buses before it).
    track_latencies: std::collections::HashMap<faderframe_core::TrackId, u32>,
    /// Low-latency monitoring: tracks heard live skip plugin delay
    /// compensation (their effects with latency bypassed, their outputs
    /// summed as they come). Off: everything aligned.
    low_latency: bool,
    /// The latency an effect on a monitored path keeps (frames).
    low_latency_limit: u32,
    /// The tracks it applies to in the installed graph, and the effects
    /// it bypassed there.
    low_latency_tracks: std::collections::HashSet<faderframe_core::TrackId>,
    bypassed_for_monitoring: Vec<faderframe_core::PluginInstanceId>,
    /// The latest timeline snapshot (handed to an anticipator that starts).
    timeline: Arc<TimelineSnapshot>,
    /// The modulation published last, the tracks' buses, and the
    /// modulation the installed graph was built for.
    modulation: Arc<ModulationSet>,
    mod_buses: std::collections::HashMap<faderframe_core::TrackId, Arc<crate::modulation::ModBus>>,
    mod_shape: Vec<(faderframe_core::TrackId, Vec<faderframe_core::TrackId>)>,
    /// Render-ahead: the anticipator, its lookahead, the rings by track,
    /// the tracks rendered ahead in the installed graph, reader misses.
    ahead: Option<crate::ahead::Anticipator>,
    /// The shallow tier, with buses rendered ahead.
    bus_ahead: Option<crate::ahead::Anticipator>,
    ahead_threads: usize,
    /// The shallow tier's rings by track (strips' echoes, buses' outputs).
    bus_rings: std::collections::HashMap<faderframe_core::TrackId, Arc<crate::ahead::AheadRing>>,
    /// The cue paths' rings from the shallow tier, by (channel, port).
    cue_rings:
        std::collections::HashMap<(faderframe_core::TrackId, u8), Arc<crate::ahead::AheadRing>>,
    /// The talkback microphone (a session setting).
    talkback: crate::slots::Talkback,
    ahead_setting: Option<std::time::Duration>,
    ahead_rings: std::collections::HashMap<faderframe_core::TrackId, Arc<crate::ahead::AheadRing>>,
    ahead_tracks: std::collections::HashSet<faderframe_core::TrackId>,
    /// Those whose strips are rendered ahead too.
    ahead_strips: std::collections::HashSet<faderframe_core::TrackId>,
    /// Render buses ahead (and the strips that reach them).
    ahead_buses: bool,
    /// The device callback size the graph was built for.
    graph_device_block: usize,
    /// The device output count it was built for.
    graph_device_outputs: usize,
    ahead_misses: Arc<AtomicU64>,
    /// Varispeed is on.
    varispeed: bool,
    /// The loop as last sent to the transport (samples, switch).
    sent_loop: Option<(Option<LoopRange>, bool)>,
}

/// The tracks with launcher clips (sorted).
/// Where the click plays in each cue mix: its outputs, its click level
/// under its master level (nothing while it is muted), its switch.
fn cue_clicks(project: &Project) -> Vec<crate::click::CueClick> {
    project
        .cue_mixes
        .iter()
        .map(|c| crate::click::CueClick {
            first_channel: c.first_channel,
            channels: c.layout.channel_count().clamp(1, 2) as u8,
            gain: if c.mute {
                0.0
            } else {
                faderframe_core::db_to_gain(c.click_db) * faderframe_core::db_to_gain(c.level_db)
            },
            on: c.click,
        })
        .collect()
}

fn launch_tracks(project: &Project) -> Vec<faderframe_core::TrackId> {
    let mut v: Vec<_> = project.launcher.slots.keys().map(|k| k.track).collect();
    v.dedup();
    v
}

/// Voices every audio track needs.
fn voice_needs(project: &Project) -> Vec<(faderframe_core::TrackId, crate::nodes::StretchVoices)> {
    project
        .tracks
        .iter()
        .filter(|t| t.kind == faderframe_project::TrackKind::Audio)
        .map(|t| (t.id, crate::build::stretch_voices(project, t)))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

impl EngineController {
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Rate the audio stream actually runs at (as reported via `prepare`).
    pub fn stream_sample_rate(&self) -> u32 {
        self.shared.stream_sample_rate.load(Ordering::Relaxed)
    }

    pub fn stream_buffer_size(&self) -> u32 {
        self.shared.stream_buffer_size.load(Ordering::Relaxed)
    }

    /// The device's callbacks changed size since the graph was built: a
    /// rebuild lets buffered devices (the preamps) follow.
    pub fn device_block_changed(&self) -> bool {
        self.stream_buffer_size() as usize != self.graph_device_block
    }

    /// The device's output count changed since the graph was built: a
    /// rebuild folds surround beds down to it (or stops doing so).
    pub fn device_outputs_changed(&self) -> bool {
        self.shared.stream_outputs.load(Ordering::Relaxed) as usize != self.graph_device_outputs
    }

    /// True while the running graph does not match the stream rate.
    pub fn needs_rate_rebuild(&self) -> bool {
        self.stream_sample_rate() != self.config.sample_rate
    }

    /// Switch the engine to a new sample rate. The caller must re-render
    /// sources and call [`Self::sync`] with [`Impact::Graph`].
    pub fn set_sample_rate(&mut self, rate: u32) {
        self.config.sample_rate = rate;
    }

    pub fn plugins(&mut self) -> &mut PluginHost {
        &mut self.plugins
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn graph_stats(&self) -> &GraphStats {
        &self.stats
    }

    pub fn node_timings(&self) -> Option<Arc<NodeTimings>> {
        self.profile.as_ref().map(|p| Arc::clone(&p.timings))
    }

    /// Timings of the current graph with what each node does for whom.
    pub fn graph_profile(&self) -> Option<GraphProfile> {
        self.profile.clone()
    }

    /// A built-in plugin's tap: its live parameters, the audio going in and
    /// out for an analyser, its meters.
    pub fn plugin_tap(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<Arc<faderframe_plugin_host::tap::AnalysisTap>> {
        self.plugins.tap(plugin)
    }

    /// The gain reduction a hosted plugin reports now (dB), if it does.
    pub fn plugin_reduction(&self, plugin: faderframe_core::PluginInstanceId) -> Option<f32> {
        self.plugins.reduction(plugin)
    }

    /// Host `slot`'s plugin (instantiate it) outside the graph; `false`
    /// when it cannot be loaded.
    pub fn host_plugin(&mut self, slot: &faderframe_project::PluginSlot) -> bool {
        self.plugins.instance(slot).is_ok()
    }

    /// Latency of a hosted plugin instance (samples).
    pub fn plugin_latency(&self, plugin: faderframe_core::PluginInstanceId) -> Option<u32> {
        self.plugins.latency(plugin)
    }

    /// Whether the loaded plugin is an instrument.
    pub fn plugin_is_instrument(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.is_instrument(plugin)
    }

    /// The note expressions a hosted plugin accepts (`None`: unknown).
    pub fn plugin_note_expressions(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<Vec<faderframe_midi::NoteExpressionKind>> {
        self.plugins.note_expressions(plugin)
    }

    /// Plugin instances that reported a processing failure.
    pub fn failed_plugins(&self) -> Vec<faderframe_core::PluginInstanceId> {
        self.plugins.failed()
    }

    /// The plugin stopped working (crashed, hung or failed processing).
    pub fn plugin_failed(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.is_failed(plugin)
    }

    /// The plugin runs in a helper process.
    pub fn plugin_sandboxed(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.is_sandboxed(plugin)
    }

    /// Plugins that reported unsaved state since the last call.
    pub fn take_dirty_plugins(&mut self) -> Vec<faderframe_core::PluginInstanceId> {
        self.plugins.take_dirty()
    }

    /// Forget the plugin's instance: the next graph sync creates it again
    /// from its slot. `false` if it was not loaded.
    pub fn reload_plugin(&mut self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.reload(plugin)
    }

    /// Measure every graph node (per-track and per-plugin load). Costs a
    /// clock read per node and callback, so it is only on while wanted;
    /// the total DSP load is always measured.
    pub fn set_node_timing(&mut self, on: bool) {
        self.node_timing = on;
        if let Some(p) = &self.profile {
            p.timings.set_enabled(on);
        }
    }

    pub fn node_timing(&self) -> bool {
        self.node_timing
    }

    /// Highest single-callback DSP load since the last call (resets it).
    pub fn take_peak_load(&self) -> f64 {
        self.shared.metrics.take_peak_load()
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.shared.metrics.snapshot()
    }

    /// Wall-clock time the graph took, all callbacks so far.
    pub fn graph_wall_ns(&self) -> u64 {
        self.shared.graph_wall_ns.load(Ordering::Relaxed)
    }

    /// Locate so that playback is at `position` at time `at_ns` on the MIDI
    /// clock (the engine adds the time until it applies the command) and
    /// play, or stop there.
    pub fn chase(&mut self, position: i64, at_ns: u64, play: bool) -> Result<(), EngineError> {
        self.send(Message::Chase {
            position,
            at_ns,
            play,
        })
    }

    /// Where the processing position is at `t_ns` on the MIDI clock
    /// (extrapolated from the last callback while playing); `None` before
    /// the first callback with a MIDI clock.
    pub fn position_at(&self, t_ns: u64) -> Option<i64> {
        self.position_and_jumps_at(t_ns).map(|(p, _)| p)
    }

    /// [`Self::position_at`], and how often the transport had jumped
    /// (stop, locate, scrub; not loop wraps) by the callback it is
    /// extrapolated from -- read together, so a jump is never seen with the
    /// position from before it, or the other way round.
    pub fn position_and_jumps_at(&self, t_ns: u64) -> Option<(i64, u32)> {
        let (cb, pos, jumps) = self.shared.callback.load();
        if cb == 0 {
            return None;
        }
        if !self.shared.transport.snapshot().playing {
            return Some((pos, jumps));
        }
        // At the varispeed's speed.
        let rate = self.stream_sample_rate() as f64 * self.speed();
        Some((pos + ((t_ns as f64 - cb as f64) * rate / 1e9) as i64, jumps))
    }

    /// Measure the round trip from device output `output` to input
    /// `input` (both from 0): an impulse goes out, its arrival is counted
    /// in frames; see [`Self::ping_result`]. Ping while stopped.
    pub fn ping(&self, output: u16, input: u16) {
        self.shared
            .ping_result
            .store(PING_PENDING, Ordering::Relaxed);
        self.shared.ping_request.store(
            (1 << 63) | (u64::from(output) << 16) | u64::from(input),
            Ordering::Relaxed,
        );
    }

    /// The last ping's answer: `None` while waiting, `Some(Ok(frames))`,
    /// or `Some(Err(()))` when nothing came back within a second.
    pub fn ping_result(&self) -> Option<Result<u32, ()>> {
        match self.shared.ping_result.load(Ordering::Relaxed) {
            PING_PENDING => None,
            PING_LOST => Some(Err(())),
            v if v >= 0 => Some(Ok(v as u32)),
            _ => None,
        }
    }

    /// A handle that tells where the engine is from any thread (picture
    /// outputs).
    pub fn clock(&self) -> EngineClock {
        EngineClock {
            shared: Arc::clone(&self.shared),
            varispeed_delay: self.varispeed_delay(),
        }
    }

    /// Frames from processing to hearing: the device's (buffer and
    /// output), varispeed's resampler when it runs, and the running graph's
    /// output latency (plugin delay compensation lines everything up to
    /// it).
    pub fn output_latency(&self) -> u32 {
        self.device_latency() + self.graph_latency()
    }

    /// The device's part of [`Self::output_latency`] (buffer and output,
    /// varispeed's resampler).
    pub fn device_latency(&self) -> u32 {
        self.shared.output_latency.load(Ordering::Relaxed) + self.varispeed_delay()
    }

    /// The running graph's output latency (frames): every output heard is
    /// this late behind the timeline position processed.
    pub fn graph_latency(&self) -> u32 {
        self.shared.graph_latency.load(Ordering::Relaxed)
    }

    /// The play history around what is heard, as last published (what
    /// [`Self::heard_at`] reads; diagnostics and tests).
    pub fn heard_snapshot(&self) -> Option<crate::history::HeardSnapshot> {
        self.shared.heard.read()
    }

    /// What is heard at `now` (the timeline position of the audio leaving
    /// the speakers, from the play history; see [`crate::history`]) —
    /// `None` before the first callback. While a locate or stop has not
    /// reached the ears yet, `jumped` says where the transport went.
    pub fn heard_at(&self, now: std::time::Instant) -> Option<Heard> {
        heard(&self.shared, self.device_latency(), now, true)
    }

    /// What will be heard at `at`, a moment from now (a picture for when it
    /// reaches the screen): [`Self::heard_at`], but past the frames
    /// processed so far the newest run carries on as it goes. After Play
    /// it stays on the stopped position until the latency has filled, as
    /// the sound does.
    pub fn heard_ahead(&self, at: std::time::Instant) -> Option<Heard> {
        heard(&self.shared, self.device_latency(), at, false)
    }

    /// Varispeed's resampler delay each way (0 when it does not run).
    pub fn varispeed_delay(&self) -> u32 {
        if self.varispeed {
            crate::varispeed::DELAY as u32
        } else {
            0
        }
    }

    pub fn reset_metrics(&self) {
        self.shared.metrics.reset();
    }

    /// Objects leaked because the garbage queue was full (should stay 0).
    pub fn leaked_objects(&self) -> u64 {
        self.shared.leaked.load(Ordering::Relaxed)
    }

    /// Shared atomics (transport position, metrics, streaming epoch) for
    /// helper threads such as the disk loader.
    pub fn shared(&self) -> Arc<EngineShared> {
        Arc::clone(&self.shared)
    }

    /// Every plugin the registry knows (built-ins and scanned formats).
    pub fn available_plugins(&self) -> Vec<faderframe_plugin_host::PluginDescriptor> {
        self.plugins.registry().scan()
    }

    /// A plugin's parameters, from an instance made for the asking (not
    /// hosted; control thread).
    pub fn describe_parameters(
        &self,
        plugin: &faderframe_project::PluginRef,
    ) -> Option<Vec<faderframe_plugin_host::ParameterInfo>> {
        self.plugins
            .registry()
            .instantiate(crate::plugins::host_format(plugin.format), &plugin.id)
            .ok()
            .map(|i| i.parameters().to_vec())
    }

    /// Let plugins handle their requests (call from the UI timer); returns
    /// what they asked for (a restart needs a graph rebuild).
    pub fn poll_plugins(&mut self) -> faderframe_plugin_host::PluginPoll {
        self.plugins.poll()
    }

    /// Preset files of a plugin format's own folders (e.g. VST3).
    pub fn plugin_preset_files(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Vec<std::path::PathBuf> {
        self.plugins.preset_files(plugin)
    }

    /// The encoded state of one of those preset files.
    pub fn plugin_state_from_preset(
        &self,
        plugin: faderframe_core::PluginInstanceId,
        data: &[u8],
    ) -> Result<String, faderframe_plugin_host::PluginError> {
        self.plugins.state_from_preset_file(plugin, data)
    }

    /// The plugin's own programs (a VST3 program list), by name.
    pub fn plugin_programs(&self, plugin: faderframe_core::PluginInstanceId) -> Vec<String> {
        self.plugins.programs(plugin)
    }

    /// The submenu of each of [`Self::plugin_programs`] ("" at the top), or
    /// empty: none are grouped.
    pub fn plugin_program_groups(&self, plugin: faderframe_core::PluginInstanceId) -> Vec<String> {
        self.plugins.program_groups(plugin)
    }

    pub fn plugin_current_program(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<usize> {
        self.plugins.current_program(plugin)
    }

    /// Switch the plugin to one of its programs (it takes effect with the
    /// processor's next block). Apply any contextual parameter overrides
    /// before the session captures the new state as one undo step.
    pub fn select_plugin_program(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        index: usize,
        overrides: &[(faderframe_core::ParameterId, f64)],
    ) -> Result<(), faderframe_plugin_host::PluginError> {
        self.plugins.select_program(plugin, index, overrides)
    }

    /// Changes the plugin's processor has not taken yet.
    pub fn plugin_changes_pending(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.changes_pending(plugin)
    }

    /// The slot state was captured from the running plugin.
    pub fn note_plugin_state(&mut self, plugin: faderframe_core::PluginInstanceId, state: &str) {
        self.plugins.note_state(plugin, state);
    }

    /// Parameter moves made in plugins' own editors since the last call
    /// (for automation writing).
    pub fn take_plugin_edits(
        &mut self,
    ) -> Vec<(
        faderframe_core::PluginInstanceId,
        faderframe_plugin_host::EditorEdit,
    )> {
        self.plugins.take_editor_edits()
    }

    /// Current value of a hosted plugin's parameter (plain units).
    pub fn plugin_parameter_value(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
    ) -> Option<f64> {
        self.plugins.parameter_value(plugin, parameter)
    }

    /// The plugin's own text for a parameter value.
    pub fn format_plugin_parameter(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
        value: f64,
    ) -> Option<String> {
        self.plugins.format_parameter(plugin, parameter, value)
    }

    /// Record a value the plugin changed itself as already applied.
    pub fn note_plugin_parameter(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
        value: f64,
    ) {
        self.plugins.note_parameter(plugin, parameter, value);
    }

    /// The plugin's own editor GUI, if it has one.
    pub fn plugin_editor(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<&mut dyn faderframe_plugin_host::PluginEditor> {
        self.plugins.editor(plugin)
    }

    /// File descriptors and timers hosted plugins registered (serviced by
    /// the UI main loop).
    pub fn plugin_event_sources(
        &self,
    ) -> Vec<(
        faderframe_core::PluginInstanceId,
        faderframe_plugin_host::PluginEventSources,
    )> {
        self.plugins.event_sources()
    }

    pub fn plugin_on_fd(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        fd: faderframe_plugin_host::PluginFd,
    ) {
        self.plugins.on_fd(plugin, fd);
    }

    pub fn plugin_on_timer(&mut self, plugin: faderframe_core::PluginInstanceId, timer: u32) {
        self.plugins.on_timer(plugin, timer);
    }

    /// Encoded state of a plugin instance (for saving into the project).
    /// Process plugins in 64-bit floating point where they can; `true` when
    /// it changed (the caller rebuilds the graph to reactivate them).
    pub fn set_plugin_double_precision(&mut self, on: bool) -> bool {
        self.plugins.set_double_precision(on)
    }

    pub fn plugin_double_precision(&self) -> bool {
        self.plugins.double_precision()
    }

    pub fn plugin_state(&mut self, plugin: faderframe_core::PluginInstanceId) -> Option<String> {
        self.plugins.capture_state(plugin)
    }

    /// Ask a plugin for its state without waiting (see
    /// [`PluginHost::request_state`]).
    pub fn request_plugin_state(&mut self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.request_state(plugin)
    }

    /// A state asked for with [`Self::request_plugin_state`], once here.
    pub fn take_plugin_state(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<Option<String>> {
        self.plugins.take_state(plugin)
    }

    /// Audio of the analysed track (see [`Self::set_analysis_source`]).
    pub fn scope(&self) -> &Arc<faderframe_realtime::ScopeRing> {
        &self.scope
    }

    /// Play `source` instead of the project (album playback; paused at
    /// its start), or the project again.
    pub fn set_preview(
        &mut self,
        source: Option<Arc<faderframe_audio_files::StreamSource>>,
    ) -> Result<(), EngineError> {
        self.shared
            .preview
            .set(source.as_ref().map(|s| s.frames() as i64));
        let msg = Message::Preview(source.map(|s| Box::new(crate::preview::Preview::new(s))));
        self.tx.push(msg).map_err(|_| EngineError::QueueFull)
    }

    /// Album playback's state (play, pause, locate, where it is).
    pub fn preview(&self) -> &crate::preview::PreviewShared {
        &self.shared.preview
    }

    /// The track whose post-fader output the scope receives.
    pub fn set_analysis_source(&self, track: Option<TrackId>) {
        self.scope.set_source(track.map(|t| t.raw()));
    }

    /// Does the plugin have a sidechain input?
    pub fn plugin_has_sidechain(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.has_sidechain(plugin)
    }

    /// An instantiated plugin's output buses, main first.
    pub fn plugin_outputs(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<&[crate::plugins::OutputBus]> {
        self.plugins.outputs(plugin)
    }

    /// Parameters of an instantiated plugin (for automation lists).
    pub fn plugin_parameters(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<&[faderframe_plugin_host::ParameterInfo]> {
        self.plugins.parameters(plugin)
    }

    /// The plugin's own group of a parameter ("Scene A/Osc 1").
    pub fn plugin_parameter_group(
        &self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
    ) -> Option<String> {
        self.plugins.parameter_group(plugin, parameter)
    }

    /// Does the plugin take notes (per-note modulators move it)?
    pub fn plugin_takes_notes(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.plugins.takes_notes(plugin)
    }

    /// Does the plugin's parameter take modulation per note (each voice
    /// its own)?
    pub fn plugin_modulatable_per_note(
        &self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
    ) -> bool {
        self.plugins.modulatable_per_note(plugin, parameter)
    }

    /// Does the plugin's parameter take modulation (that leaves its value)?
    pub fn plugin_modulatable(
        &self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
    ) -> bool {
        self.plugins.modulatable(plugin, parameter)
    }

    /// Automated fader gain (linear), pan and mute of a track, as the audio
    /// thread last applied them (only meaningful while a lane drives them).
    pub fn automated_strip(&self, track: TrackId) -> Option<(f32, f32, bool)> {
        let s = self.slots.strip_of(track)?;
        Some((
            self.readback.get(s.volume),
            self.readback.get(s.pan),
            self.readback.get(s.mute) >= 0.5,
        ))
    }

    /// Automated send gain (linear), as last applied.
    pub fn automated_send(&self, send: faderframe_core::SendId) -> Option<f32> {
        self.slots.send_of(send).map(|s| self.readback.get(s))
    }

    /// Lanes being written by the user; takes effect with the next
    /// timeline sync.
    /// A take folder plays take `.1` whole instead of its comp (`None`:
    /// every folder its comp); applies with the next timeline update.
    pub fn set_take_audition(&mut self, take: Option<(faderframe_core::ClipId, usize)>) {
        self.take_audition = take;
    }

    pub fn set_suspended_lanes(
        &mut self,
        lanes: std::collections::HashSet<faderframe_core::AutomationLaneId>,
    ) {
        self.suspended_lanes = lanes;
    }

    /// Disk-loading plan for the current timeline.
    pub fn stream_plan(&self) -> Arc<StreamPlan> {
        Arc::clone(&self.plan)
    }

    pub fn graphs_installed(&self) -> u64 {
        self.shared.graphs_installed.load(Ordering::Relaxed)
    }

    fn send(&mut self, msg: Message) -> Result<(), EngineError> {
        self.tx.push(msg).map_err(|_| EngineError::QueueFull)
    }

    /// Drop objects the audio thread has retired. Call regularly (e.g. from
    /// the UI frame timer).
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while self.garbage.pop().is_ok() {
            n += 1;
        }
        for a in self.ahead.iter_mut().chain(self.bus_ahead.iter_mut()) {
            while a.garbage.pop().is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Re-synchronise the engine with the project after an edit.
    pub fn sync(
        &mut self,
        project: &Project,
        sources: &SourceMap,
        impact: Impact,
    ) -> Result<(), EngineError> {
        self.collect_garbage();
        self.collect_listening();
        match impact {
            Impact::None => Ok(()),
            // The loop: to the transport as it is (no snapshot, nothing
            // rebuilt; rendering ahead, no new sequence either).
            Impact::Transport => self.send_loop(project),
            // A spread made diffuse (or no longer): its filters are made
            // with the graph. A track into (or out of) a folder bus, a send
            // given (or no longer) a pan of its own: routing.
            Impact::Params if crate::build::param_shape(project) != self.param_shape => {
                self.sync(project, sources, Impact::Graph)
            }
            Impact::Params => {
                // A batch may carry a loop change too.
                self.send_loop(project)?;
                self.update_params(project)
            }
            Impact::Timeline
                if voice_needs(project) != self.voices
                    || launch_tracks(project) != self.launch_tracks =>
            {
                // A track gained (or no longer needs) stretcher voices, or
                // launcher clips (those tracks play live).
                self.sync(project, sources, Impact::Graph)
            }
            Impact::Timeline => {
                self.update_timeline(project, sources)?;
                self.update_params(project)
            }
            Impact::Graph => {
                self.rebuild_graph(project)?;
                self.update_timeline(project, sources)?;
                self.update_params(project)
            }
        }
    }

    pub fn update_params(&mut self, project: &Project) -> Result<(), EngineError> {
        // A track became live or armed: out of the render-ahead graph. A
        // track gained (or lost) modulators or a follower's source. Another
        // track is heard live with low-latency monitoring.
        if (self.ahead.is_some() && !self.ahead_plan_installed(project))
            || crate::modulation::shape(project) != self.mod_shape
            || self.monitoring_wanted(project) != self.low_latency_tracks
        {
            self.rebuild_graph(project)?;
        }
        self.publish_modulation(project);
        self.slots.write_params(
            project,
            &self.params,
            &self.midi_live,
            &self.suspended_lanes,
        )?;
        self.slots
            .write_talkback(project, &self.params, &self.talkback)?;
        self.shared.metronome.set_cues(&cue_clicks(project));
        self.plugins.sync_parameters(project);
        Ok(())
    }

    /// Send the tracks' modulation to the audio thread when it changed.
    fn publish_modulation(&mut self, project: &Project) {
        let set = crate::modulation::build_set(project, &self.plugins, &mut self.mod_buses);
        if *self.modulation == set {
            return;
        }
        self.modulation = Arc::new(set);
        drop(
            self.modulation_tx
                .send(Box::new(Arc::clone(&self.modulation))),
        );
        // The anticipators' modulators too.
        for a in self.ahead.iter().chain(self.bus_ahead.iter()) {
            drop(a.modulation_tx.send(Box::new(Arc::clone(&self.modulation))));
        }
    }

    /// The outputs of a track's modulators now (bipolar ones −1..1,
    /// unipolar ones 0..1).
    pub fn modulator_values(
        &self,
        track: faderframe_core::TrackId,
    ) -> Vec<(faderframe_core::ModulatorId, f32)> {
        self.modulation.track(track).map_or_else(Vec::new, |t| {
            t.modulators
                .iter()
                .enumerate()
                .map(|(i, m)| (m.id, t.bus.get(i)))
                .collect()
        })
    }

    /// What modulation moves a track's fader (in travel) and pan by now.
    pub fn strip_modulation(&self, track: faderframe_core::TrackId) -> (f32, f32) {
        self.modulation
            .track(track)
            .map_or((0.0, 0.0), crate::modulation::TrackModulation::strip)
    }

    pub fn rebuild_graph(&mut self, project: &Project) -> Result<(), EngineError> {
        self.slots.retain_project(project);
        self.plugins.retain_project(project);
        let mut prepare =
            PrepareConfig::new(self.config.sample_rate as f64, self.config.max_block_size);
        prepare.measure_nodes = self.config.measure_nodes;
        prepare.parallel_min_ns = self.config.parallel_min_ns;
        prepare.device_block = self.shared.stream_buffer_size.load(Ordering::Relaxed) as usize;
        prepare.device_outputs = self.shared.stream_outputs.load(Ordering::Relaxed) as usize;
        self.graph_device_block = prepare.device_block;
        self.graph_device_outputs = prepare.device_outputs;
        let sets = self.ahead_plan(project);
        let monitoring = self.monitoring_wanted(project);
        let lookahead = self.ahead.as_ref().map(|a| a.lookahead);
        let bus_ring_frames = self
            .bus_ahead
            .as_ref()
            .map_or(0, |a| crate::ahead::ring_frames(a.lookahead, a.block));
        if let Some(a) = &self.bus_ahead {
            a.set_lookahead(crate::ahead::shallow_lookahead(
                prepare.device_block,
                a.block,
            ));
        }
        let plan = lookahead.map(|lookahead| crate::build::AheadPlan {
            tracks: &sets.tracks,
            strips: &sets.strips,
            rings: &mut self.ahead_rings,
            ring_frames: crate::ahead::ring_frames(lookahead, self.config.max_block_size),
            bus_rings: &mut self.bus_rings,
            bus_ring_frames,
            cue_rings: &mut self.cue_rings,
            misses: Arc::clone(&self.ahead_misses),
        });
        // Album songs' inserts are hosted (editors, parameters) even when
        // they are not monitored.
        for slot in project.album.inserts() {
            if let Err(e) = self.plugins.instance(slot) {
                tracing::warn!("{}: {e}", slot.plugin.name);
            }
        }
        let monitor = self
            .album_monitor
            .and_then(|id| project.album.song(id))
            .map_or(&[][..], |s| &s.inserts[..]);
        let built = build_graph(
            project,
            &mut self.slots,
            &mut self.plugins,
            &prepare,
            &self.midi_routing,
            plan,
            monitor,
            &self.listen,
            &crate::build::Monitoring {
                tracks: monitoring.clone(),
                limit: self.low_latency_limit,
            },
            &self.talkback,
        )?;
        self.low_latency_tracks = monitoring;
        self.bypassed_for_monitoring = built.bypassed_for_monitoring.clone();
        let compiled = built.builder.compile(&prepare)?;
        // Plugins loaded for this graph: their code and constants mapped
        // before the audio thread first runs them (only new mappings).
        faderframe_realtime::memory::prefault_code();
        let mut ahead_prepare = prepare;
        // Not on the audio thread: never worth staying serial.
        ahead_prepare.parallel_min_ns = 0;
        ahead_prepare.measure_nodes = false;
        for (a, built) in [
            (&self.ahead, built.ahead),
            (&self.bus_ahead, built.bus_ahead),
        ] {
            if let (Some(a), Some((builder, rings))) = (a, built) {
                let graph = builder.compile(&ahead_prepare)?;
                drop(a.graph_tx.send(Box::new(crate::ahead::AheadGraph {
                    graph: Box::new(graph),
                    rings,
                })));
            }
        }
        self.ahead_rings.retain(|t, _| sets.tracks.contains(t));
        self.bus_rings.retain(|t, _| sets.tracks.contains(t));
        if sets.tracks.is_empty() {
            self.cue_rings.clear();
        }
        self.ahead_tracks = sets.tracks;
        self.ahead_strips = sets.strips;
        self.voices = voice_needs(project);
        self.param_shape = crate::build::param_shape(project);
        self.launch_tracks = launch_tracks(project);
        self.mod_shape = crate::modulation::shape(project);
        // Parameters must be valid before the new graph's processors read them.
        self.slots.write_params(
            project,
            &self.params,
            &self.midi_live,
            &self.suspended_lanes,
        )?;
        self.slots
            .write_talkback(project, &self.params, &self.talkback)?;
        self.stats = compiled.stats().clone();
        // Where each track is metered (its strip, or the echo of a strip
        // rendered ahead), and how much later that is heard.
        let mut metered: std::collections::HashMap<faderframe_core::TrackId, u32> =
            std::collections::HashMap::new();
        for (id, owner) in &built.owners {
            if owner.work == crate::build::NodeWork::Strip
                && let Some(at) = compiled.output_latency(*id)
            {
                let e = metered.entry(owner.track).or_insert(0);
                *e = (*e).max(at);
            }
        }
        self.meter_delays = metered
            .iter()
            .map(|(t, at)| (*t, self.stats.output_latency.saturating_sub(*at)))
            .collect();
        self.track_latencies = metered;
        let mut owners = vec![None; compiled.timings().len()];
        for (id, owner) in &built.owners {
            if let Some(i) = compiled.index_of(*id) {
                owners[i] = Some(*owner);
            }
        }
        compiled.timings().set_enabled(self.node_timing);
        self.profile = Some(GraphProfile {
            timings: compiled.timings(),
            owners: owners.into(),
            groups: project.tracks.iter().map(|t| t.id).collect(),
        });
        self.warnings = built.warnings;
        for w in &self.warnings {
            tracing::warn!("{w}");
        }
        // A graph the audio thread never picked up is dropped right here.
        drop(self.graph_tx.send(Box::new(compiled)));
        Ok(())
    }

    pub fn update_timeline(
        &mut self,
        project: &Project,
        sources: &SourceMap,
    ) -> Result<(), EngineError> {
        let snapshot = TimelineSnapshot::build_with(
            project,
            sources,
            self.config.sample_rate,
            &self.suspended_lanes,
            self.take_audition,
        );
        self.plan = Arc::new(snapshot.stream_plan());
        let snapshot = Arc::new(snapshot);
        // A snapshot a thread never picked up is dropped right here.
        drop(self.timeline_tx.send(Box::new(Arc::clone(&snapshot))));
        for a in self.ahead.iter().chain(self.bus_ahead.iter()) {
            drop(a.timeline_tx.send(Box::new(Arc::clone(&snapshot))));
        }
        self.timeline = snapshot;
        // The loop in samples follows the tempo.
        self.send_loop(project)
    }

    /// The project's loop in samples, as the transport takes it: at least
    /// `MIN_LOOP_SECONDS` long (every wrap splits the callback into another
    /// pass through the whole graph, and a loop of a few frames would make
    /// dozens of them in one callback), switched on only with a range.
    pub fn loop_in_samples(&self, project: &Project) -> (Option<LoopRange>, bool) {
        let shortest = (f64::from(self.config.sample_rate) * MIN_LOOP_SECONDS) as i64;
        let range = project.loop_range.and_then(|r| {
            let start = self.musical_to_samples(project, r.start);
            let end = self.musical_to_samples(project, r.end);
            LoopRange::new(start, end.max(start + shortest))
        });
        (range, project.loop_enabled && range.is_some())
    }

    /// Send the project's loop to the transport when it differs from what
    /// was sent last (a loop dragged while playing: one command per
    /// change, nothing else).
    pub fn send_loop(&mut self, project: &Project) -> Result<(), EngineError> {
        let wanted = self.loop_in_samples(project);
        if self.sent_loop == Some(wanted) {
            return Ok(());
        }
        let (range, enabled) = wanted;
        self.send(Message::Transport(TransportCommand::SetLoopRange(range)))?;
        self.send(Message::Transport(TransportCommand::SetLoopEnabled(
            enabled,
        )))?;
        self.sent_loop = Some(wanted);
        Ok(())
    }

    pub fn transport(&mut self, cmd: TransportCommand) -> Result<(), EngineError> {
        if let TransportCommand::Locate(to) = cmd {
            self.shared.locate_target.store(to, Ordering::Relaxed);
            self.shared.locate_at.store(
                crate::history::ns_since_epoch(Instant::now()).max(1),
                Ordering::Release,
            );
        }
        self.send(Message::Transport(cmd))
    }

    /// Stop playback on the exact frame `at` when it gets there (once), or
    /// not (`None`).
    pub fn stop_at(&mut self, at: Option<i64>) -> Result<(), EngineError> {
        self.send(Message::StopAt(at))
    }

    /// Varispeed on (for the stream as it runs) or off; see
    /// [`crate::varispeed`].
    pub fn set_varispeed(&mut self, on: bool) -> Result<(), EngineError> {
        self.set_speed(1.0);
        let v = on.then(|| {
            let ins = self.shared.stream_inputs.load(Ordering::Relaxed) as usize;
            let outs = self.shared.stream_outputs.load(Ordering::Relaxed) as usize;
            let buffer = self.shared.stream_buffer_size.load(Ordering::Relaxed) as usize;
            Box::new(crate::varispeed::Varispeed::new(
                ins,
                outs.max(1),
                (buffer * 4).max(8192),
            ))
        });
        self.varispeed = on;
        self.send(Message::Varispeed(v))
    }

    pub fn varispeed(&self) -> bool {
        self.varispeed
    }

    /// The varispeed's speed (clamped to ±12 %).
    pub fn set_speed(&self, speed: f64) {
        let d = crate::varispeed::MAX_DEVIATION;
        let s = speed.clamp(1.0 - d, 1.0 + d);
        self.shared.speed.store(s.to_bits(), Ordering::Relaxed);
    }

    pub fn speed(&self) -> f64 {
        self.shared.speed()
    }

    /// Launch or stop launcher clips (see [`crate::launch`]).
    pub fn launch(&mut self, cmd: crate::launch::LaunchCommand) -> Result<(), EngineError> {
        self.send(Message::Launch(cmd))
    }

    /// The launcher's state as last published.
    pub fn launch_status(&self) -> Vec<crate::launch::TrackStatus> {
        self.shared.launch.read()
    }

    /// Start capturing `targets` (only while the transport records, and only
    /// timeline samples `from..to`). Ring capacity covers `seconds` of audio
    /// in case the writer stalls. Returns the ends for the writer thread.
    pub fn begin_recording(
        &mut self,
        targets: Vec<RecordTarget>,
        from: i64,
        to: i64,
        seconds: f64,
    ) -> Result<RecordStreams, EngineError> {
        let (rt, streams) =
            crate::record::rings(targets, from, to, self.config.sample_rate, seconds);
        // Each take counts its own dropped frames (`record_counters`).
        self.shared.record_overruns.store(0, Ordering::Relaxed);
        self.send(Message::BeginRecord(Box::new(rt)))?;
        Ok(streams)
    }

    /// Stop capturing; the writer sees its streams finish once the audio
    /// thread has let go of them (after the next callback and garbage
    /// collection).
    pub fn end_recording(&mut self) -> Result<(), EngineError> {
        self.send(Message::EndRecord)
    }

    /// Feed live MIDI from this queue (replaces the previous one).
    /// SysEx that arrived on MIDI input `port`: the tracks playing live
    /// from that port get it with the next callback (their plugins).
    /// `false` when it does not fit.
    pub fn send_live_sysex(&mut self, port: u16, bytes: &[u8]) -> bool {
        self.live_sysex.send(port, bytes)
    }

    pub fn set_midi_input(
        &mut self,
        queue: faderframe_midi::MidiInputQueue,
    ) -> Result<(), EngineError> {
        self.send(Message::MidiInput(Box::new(queue)))
    }

    /// Port keys → port indices of the MIDI input (a change needs a graph
    /// rebuild to reach tracks routed to a named port).
    pub fn set_midi_ports(&mut self, ports: std::collections::HashMap<String, u16>) {
        self.midi_routing.inputs = ports;
    }

    pub fn midi_ports(&self) -> &std::collections::HashMap<String, u16> {
        &self.midi_routing.inputs
    }

    /// Output port keys → indices (a change needs a graph rebuild).
    pub fn set_midi_output_ports(&mut self, ports: std::collections::HashMap<String, u16>) {
        self.midi_routing.outputs = ports;
    }

    pub fn midi_output_ports(&self) -> &std::collections::HashMap<String, u16> {
        &self.midi_routing.outputs
    }

    /// Send MIDI to external devices through this queue.
    pub fn set_midi_output(
        &mut self,
        queue: faderframe_midi::MidiOutputQueue,
    ) -> Result<(), EngineError> {
        self.send(Message::MidiOutput(Box::new(queue)))
    }

    /// Auditioning, mapped controls and clock outputs.
    pub fn midi_shared(&self) -> &Arc<crate::midi::MidiShared> {
        &self.shared.midi
    }

    /// The running stream's input channels (0 without one).
    pub fn stream_inputs(&self) -> usize {
        self.shared.stream_inputs.load(Ordering::Relaxed) as usize
    }

    /// The running stream's sample rate (0 without one).
    pub fn stream_rate(&self) -> u32 {
        self.shared.stream_sample_rate.load(Ordering::Relaxed)
    }

    /// Listen to this voice only on the audio thread (its notes join the
    /// live MIDI), or to none (see [`crate::voice`]).
    pub fn set_voice(&mut self, run: Option<crate::voice::VoiceRun>) -> Result<(), EngineError> {
        self.send(Message::Voice(run.map(Box::new)))
    }

    /// Listen to one more voice, beside those listened to (one on the same
    /// input and port is replaced; past [`crate::voice::MAX_VOICES`] it is
    /// not listened to).
    pub fn add_voice(&mut self, run: crate::voice::VoiceRun) -> Result<(), EngineError> {
        self.send(Message::VoiceAdd(Box::new(run)))
    }

    /// Stop listening to `input` for `port` (its notes end).
    pub fn remove_voice(&mut self, input: u16, port: u16) -> Result<(), EngineError> {
        self.send(Message::VoiceRemove { input, port })
    }

    /// How every voice is heard.
    pub fn set_voice_config(
        &mut self,
        config: faderframe_analysis::voice::VoiceConfig,
        glide: crate::voice::Glide,
    ) -> Result<(), EngineError> {
        self.send(Message::VoiceConfig(config, glide))
    }

    /// MIDI output messages lost because the sender fell behind.
    pub fn midi_output_dropped(&self) -> u64 {
        self.shared.midi_out_dropped.load(Ordering::Relaxed)
    }

    /// Low-latency monitoring (applies with the next graph build): tracks
    /// heard live ([`crate::build::live_monitored`]) bypass their effects
    /// with latency and are summed as they come — heard at once rather than
    /// after the plugin latency elsewhere, and not aligned with it. Playback
    /// of every other track, and renders (their own engines), stay
    /// compensated.
    pub fn set_low_latency_monitoring(&mut self, on: bool) {
        self.low_latency = on;
    }

    pub fn low_latency_monitoring(&self) -> bool {
        self.low_latency
    }

    /// Effects on a monitored path with at most this latency (frames) stay
    /// on (applies with the next graph build).
    pub fn set_low_latency_limit(&mut self, frames: u32) {
        self.low_latency_limit = frames;
    }

    pub fn low_latency_limit(&self) -> u32 {
        self.low_latency_limit
    }

    /// The tracks low-latency monitoring applies to in the running graph.
    pub fn low_latency_tracks(&self) -> &std::collections::HashSet<faderframe_core::TrackId> {
        &self.low_latency_tracks
    }

    /// Effects low-latency monitoring bypasses in the running graph.
    pub fn bypassed_for_monitoring(&self) -> &[faderframe_core::PluginInstanceId] {
        &self.bypassed_for_monitoring
    }

    fn monitoring_wanted(
        &self,
        project: &Project,
    ) -> std::collections::HashSet<faderframe_core::TrackId> {
        if self.low_latency {
            crate::build::live_monitored(project, &self.midi_live)
        } else {
            std::collections::HashSet::new()
        }
    }

    /// Which tracks take live MIDI input (applied with the next parameter
    /// update).
    pub fn set_midi_live(&mut self, tracks: std::collections::HashSet<faderframe_core::TrackId>) {
        self.midi_live = tracks;
    }

    /// Tracks whose plugins are being edited: they play on the audio thread
    /// (changes are heard at once). Applied by [`Self::update_params`].
    pub fn set_edited_tracks(
        &mut self,
        tracks: std::collections::HashSet<faderframe_core::TrackId>,
    ) {
        self.edited = tracks;
    }

    /// Listen on headphones (binaural, with a room) or speakers; applies
    /// with [`Self::relisten`] (crossfaded, no rebuild) or the next graph
    /// build.
    pub fn set_binaural(&mut self, room: Option<faderframe_binaural::Room>) {
        self.listen.binaural = room;
    }

    /// Hear the master as the listening settings now say: a renderer for
    /// the master's layout is made here and handed to the running
    /// listening node, which crossfades to it (no rebuild; renders, which
    /// set their listening before their first graph, are built with it).
    pub fn relisten(&mut self, project: &Project) {
        self.collect_listening();
        let Some(master) = project.master() else {
            return;
        };
        let listening =
            crate::nodes::Listening::new(master.layout, self.config.sample_rate, &self.listen);
        drop(self.listen.link.send(Box::new(listening)));
    }

    /// Drop what the listening node has retired.
    fn collect_listening(&mut self) {
        while let Some(old) = self.listen.link.collect() {
            drop(old);
        }
    }

    pub fn binaural(&self) -> Option<faderframe_binaural::Room> {
        self.listen.binaural
    }

    /// The head and the headphone correction listening on headphones uses
    /// (a graph rebuild, as `set_binaural`).
    pub fn set_head(&mut self, head: faderframe_binaural::Head) {
        self.listen.head = head;
    }

    pub fn head(&self) -> &faderframe_binaural::Head {
        &self.listen.head
    }

    pub fn set_headphone_correction(
        &mut self,
        correction: Option<Arc<faderframe_binaural::Correction>>,
    ) {
        self.listen.correction = correction;
    }

    /// The listening settings (headphones, head, correction).
    pub fn listening(&self) -> &crate::build::Listen {
        &self.listen
    }

    pub fn headphone_correction(&self) -> Option<&Arc<faderframe_binaural::Correction>> {
        self.listen.correction.as_ref()
    }

    /// Bass management of the speaker monitoring (immediate, no rebuild;
    /// listening only — renders never have it).
    pub fn set_bass_management(
        &mut self,
        bass: faderframe_core::surround::BassManagement,
    ) -> Result<(), EngineError> {
        let slots = self.slots.bass_management()?;
        for (slot, v) in slots.iter().zip(bass.clamped().values()) {
            self.params.set(*slot, v);
        }
        Ok(())
    }

    /// The talkback microphone: switched on or off, its level and dim
    /// reach the cue mixes at once (parameters); another input rebuilds
    /// the graph (the caller syncs with [`Impact::Graph`] when `true`).
    pub fn set_talkback(
        &mut self,
        project: &Project,
        talkback: crate::slots::Talkback,
    ) -> Result<bool, EngineError> {
        let rebuild = talkback.input != self.talkback.input;
        self.talkback = talkback;
        if !rebuild {
            self.slots
                .write_talkback(project, &self.params, &self.talkback)?;
        }
        Ok(rebuild)
    }

    pub fn talkback(&self) -> crate::slots::Talkback {
        self.talkback
    }

    /// The mono check (immediate, no rebuild).
    pub fn set_mono_check(&mut self, on: bool) -> Result<(), EngineError> {
        let slot = self.slots.monitor_mono()?;
        self.params.set(slot, if on { 1.0 } else { 0.0 });
        Ok(())
    }

    pub fn set_album_monitor(&mut self, song: Option<faderframe_core::SongId>) {
        self.album_monitor = song;
    }

    pub fn album_monitor(&self) -> Option<faderframe_core::SongId> {
        self.album_monitor
    }

    /// Render tracks nobody plays live `lookahead` ahead of the playhead on
    /// threads of their own (`None`: everything on the audio thread). The
    /// caller rebuilds the graph ([`Self::sync`] with [`Impact::Graph`]).
    pub fn set_render_ahead(&mut self, lookahead: Option<std::time::Duration>, threads: usize) {
        if lookahead == self.ahead_setting && threads == self.ahead_threads {
            return;
        }
        self.ahead_setting = lookahead;
        self.ahead_threads = threads;
        self.restart_ahead();
    }

    /// (Re)start the anticipators for the current settings: the deep one,
    /// and with buses rendered ahead the shallow one reading its rings.
    fn restart_ahead(&mut self) {
        let had = self.ahead.take().is_some();
        self.bus_ahead = None;
        if had {
            let _ = self.send(Message::AheadOff);
        }
        self.ahead_rings.clear();
        self.bus_rings.clear();
        self.cue_rings.clear();
        self.ahead_tracks.clear();
        self.ahead_strips.clear();
        let Some(lookahead) = self.ahead_setting else {
            return;
        };
        let ctx = || EngineContext {
            worker_underruns: AtomicU64::new(0),
            callback_deadline: None,
            transport: Default::default(),
            discontinuity: false,
            timeline: Arc::clone(&self.timeline),
            modulation: Arc::clone(&self.modulation),
            params: Arc::clone(&self.params),
            readback: Arc::clone(&self.readback),
            meters: Arc::clone(&self.meters),
            scope: Arc::clone(&self.scope),
            midi_input: crate::midi::MidiInputBlock::with_capacity(0),
            ahead_seq: 0,
            preview_active: false,
            launch: crate::launch::LaunchState::new(),
            history: Default::default(),
            recording: Default::default(),
        };
        let rate = self.config.sample_rate as f64;
        let (deep, deep_tier) = crate::ahead::start(
            ctx(),
            rate,
            crate::ahead::Options {
                block: self.config.max_block_size,
                lookahead,
                threads: self.ahead_threads,
                upstream: None,
                follow: None,
                epoch: Arc::clone(&self.shared.epoch),
            },
        );
        let scheduling = crate::ahead::Scheduling::new();
        let mut tiers = Vec::new();
        if self.ahead_buses {
            let block = crate::ahead::SHALLOW_BLOCK.min(self.config.max_block_size);
            let (shallow, tier) = crate::ahead::start(
                ctx(),
                rate,
                crate::ahead::Options {
                    block,
                    lookahead: std::time::Duration::from_secs_f64(
                        crate::ahead::SHALLOW_MAX as f64 / rate,
                    ),
                    threads: self.ahead_threads.div_ceil(2),
                    upstream: Some(Arc::clone(&deep.progress)),
                    follow: Some(Arc::clone(&scheduling)),
                    epoch: Arc::clone(&self.shared.epoch),
                },
            );
            shallow.set_lookahead(crate::ahead::shallow_lookahead(
                self.stream_buffer_size() as usize,
                block,
            ));
            tiers.push(tier);
            self.bus_ahead = Some(shallow);
        }
        tiers.push(deep_tier);
        let link = crate::ahead::AheadLink::new(tiers, rate, scheduling);
        if self.send(Message::Ahead(Box::new(link))).is_ok() {
            self.ahead = Some(deep);
        } else {
            self.bus_ahead = None;
        }
    }

    pub fn render_ahead(&self) -> Option<std::time::Duration> {
        self.ahead_setting
    }

    /// Tracks rendered ahead in the installed graph.
    pub fn ahead_tracks(&self) -> &std::collections::HashSet<faderframe_core::TrackId> {
        &self.ahead_tracks
    }

    /// Blocks in which a rendered-ahead track's audio was not there yet.
    pub fn ahead_misses(&self) -> u64 {
        self.ahead_misses.load(Ordering::Relaxed)
    }

    /// Tracks whose channel strips (and sends) are rendered ahead too:
    /// they reach buses rendered ahead ([`Self::set_render_ahead_buses`]).
    pub fn ahead_strips(&self) -> &std::collections::HashSet<faderframe_core::TrackId> {
        &self.ahead_strips
    }

    /// Render buses ahead too, when everything reaching them can be: their
    /// devices, and the strips and sends reaching them, run in a shallow
    /// second anticipator (moves of those faders are heard after a device
    /// callback and two small blocks). The caller rebuilds the graph
    /// ([`Self::sync`] with [`Impact::Graph`]).
    pub fn set_render_ahead_buses(&mut self, on: bool) {
        if on != self.ahead_buses {
            self.ahead_buses = on;
            self.restart_ahead();
        }
    }

    pub fn render_ahead_buses(&self) -> bool {
        self.ahead_buses
    }

    /// What to render ahead: what can be, but while playing nothing that
    /// plays on the audio thread now (moving it there would leave a gap); a
    /// stop brings it back ([`Self::ahead_wants_rebuild`]).
    fn ahead_plan(&self, project: &Project) -> crate::build::AheadSets {
        if self.ahead.is_none() {
            return Default::default();
        }
        // Counting in is started: what it renders ahead plays after it.
        let playing = self.shared.transport.snapshot().started();
        let now = crate::build::AheadSets {
            tracks: self.ahead_tracks.clone(),
            strips: self.ahead_strips.clone(),
        };
        crate::build::ahead_sets(
            project,
            &self.midi_live,
            &self.edited,
            self.ahead_buses,
            playing.then_some(&now),
        )
    }

    /// Is what is rendered ahead what should be?
    fn ahead_plan_installed(&self, project: &Project) -> bool {
        let plan = self.ahead_plan(project);
        plan.tracks == self.ahead_tracks && plan.strips == self.ahead_strips
    }

    /// Stopped with tracks that could be rendered ahead but are not: a
    /// graph rebuild moves them.
    pub fn ahead_wants_rebuild(&self, project: &Project) -> bool {
        self.ahead.is_some()
            && !self.shared.transport.snapshot().started()
            && !self.ahead_plan_installed(project)
    }

    pub fn midi_live(&self) -> &std::collections::HashSet<faderframe_core::TrackId> {
        &self.midi_live
    }

    /// Start capturing the MIDI input of `targets` inside `from..to`.
    pub fn begin_midi_recording(
        &mut self,
        targets: Vec<crate::midi::MidiRecordTarget>,
        from: i64,
        to: i64,
    ) -> Result<rtrb::Consumer<crate::midi::RecordedMidi>, EngineError> {
        let (rec, rx) = crate::midi::midi_recording(targets, from, to, 16 * 1024);
        self.send(Message::BeginMidiRecord(Box::new(rec)))?;
        Ok(rx)
    }

    pub fn end_midi_recording(&mut self) -> Result<(), EngineError> {
        self.send(Message::EndMidiRecord)
    }

    /// MIDI input events dropped (more than a block holds) and recorded
    /// events lost.
    pub fn midi_counters(&self) -> (u64, u64) {
        (
            self.shared.midi_dropped.load(Ordering::Relaxed),
            self.shared.midi_record_overruns.load(Ordering::Relaxed),
        )
    }

    /// Frames dropped by the recorder (writer too slow) and captured.
    pub fn record_counters(&self) -> (u64, u64) {
        (
            self.shared.record_overruns.load(Ordering::Relaxed),
            self.shared.recorded_frames.load(Ordering::Relaxed),
        )
    }

    pub fn metronome(&self) -> &MetronomeShared {
        &self.shared.metronome
    }

    /// The metronome's sounds from now on (made at the stream's rate on
    /// the control side; [`ClickSounds::synth`] or the user's files).
    pub fn set_click_sounds(&self, sounds: ClickSounds) {
        drop(self.click_tx.send(Box::new(Arc::new(sounds))));
    }

    /// Release all voices/tails (panic button).
    pub fn reset_processors(&mut self) -> Result<(), EngineError> {
        self.send(Message::ResetProcessors)
    }

    pub fn transport_snapshot(&self) -> TransportSnapshot {
        self.shared.transport.snapshot()
    }

    pub fn musical_to_samples(&self, project: &Project, pos: MusicalTime) -> i64 {
        project
            .timeline
            .to_samples(pos, self.config.sample_rate as f64)
    }

    pub fn samples_to_musical(&self, project: &Project, samples: i64) -> MusicalTime {
        project
            .timeline
            .to_musical(samples, self.config.sample_rate as f64)
    }

    /// Quasi-peak (PPM) metering for a track's meter channels, on or off;
    /// returns the range it applied to (it moves when the strip's channels
    /// change, so the caller applies it again then).
    pub fn set_meter_ppm(&self, track: TrackId, on: bool) -> Option<MeterRange> {
        let range = self.slots.meter_of(track)?;
        for c in 0..u32::from(range.channels) {
            self.meters.set_ppm(range.first + c, on);
        }
        Some(range)
    }

    /// The meter channels of a track.
    pub fn meter_range(&self, track: TrackId) -> Option<MeterRange> {
        self.slots.meter_of(track)
    }

    /// Frames from a track's meter to the graph's outputs: its meter reads
    /// the strip as it is processed, heard this much (and the device's
    /// latency) later.
    pub fn meter_delay(&self, track: TrackId) -> u32 {
        self.meter_delays.get(&track).copied().unwrap_or(0)
    }

    /// The latency of a track's audio where it is metered (after its
    /// strip): everything on its path so far, compensation included.
    pub fn track_latency(&self, track: TrackId) -> u32 {
        self.track_latencies.get(&track).copied().unwrap_or(0)
    }

    /// Consume the meter values accumulated since the last call.
    pub fn take_meter(&self, track: TrackId) -> Option<TrackMeter> {
        let range = self.slots.meter_of(track)?;
        // As many as the strip meters now (the range may be larger).
        let used = self.slots.metered(track).unwrap_or(range.channels);
        let mut m = TrackMeter {
            count: usize::from(used.min(range.channels)).min(METER_MAX),
            ..TrackMeter::default()
        };
        for (c, reading) in m.channels.iter_mut().enumerate().take(m.count) {
            *reading = self.meters.take(range.first + c as u32);
        }
        m.left = m.channels[0];
        m.right = if m.count > 1 {
            m.channels[1]
        } else {
            m.channels[0]
        };
        Some(m)
    }
}
