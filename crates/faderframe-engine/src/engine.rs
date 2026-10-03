use crate::EngineError;
use crate::build::build_graph;
use crate::click::{Click, MetronomeMode, MetronomeShared};
use crate::context::EngineContext;
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
    CallbackMetrics, Epoch, MailboxReceiver, MailboxSender, MeterBank, MeterReading,
    MetricsSnapshot, ParamTable, mailbox,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{
    LoopRange, TransportCommand, TransportShared, TransportSnapshot, TransportState,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            max_block_size: 512,
            param_capacity: 8192,
            meter_capacity: 2048,
            queue_capacity: 256,
            measure_nodes: true,
        }
    }
}

/// Small, ordered control → RT commands. Graphs and timeline snapshots do
/// not use this queue: they go through latest-value mailboxes, so a burst of
/// edits can never overflow it with stale structures.
enum Message {
    Transport(TransportCommand),
    ResetProcessors,
    BeginRecord(Box<Recorder>),
    EndRecord,
}

/// Objects retired by the audio thread, dropped on the control thread.
enum Garbage {
    Graph(#[allow(dead_code)] Box<CompiledGraph<EngineContext>>),
    Timeline(#[allow(dead_code)] Box<TimelineSnapshot>),
    Recorder(#[allow(dead_code)] Box<Recorder>),
}

/// State shared between the processor and the controller (atomics only).
#[derive(Debug, Default)]
pub struct EngineShared {
    pub transport: TransportShared,
    pub metrics: CallbackMetrics,
    stream_sample_rate: AtomicU32,
    stream_buffer_size: AtomicU32,
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
    pub metronome: MetronomeShared,
    /// Frames the recorder had to drop because the writer fell behind.
    record_overruns: AtomicU64,
    /// Frames captured since the engine started.
    recorded_frames: AtomicU64,
}

/// Create a connected controller/processor pair.
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
    let shared = Arc::new(EngineShared {
        epoch,
        ..EngineShared::default()
    });
    shared
        .stream_sample_rate
        .store(config.sample_rate, Ordering::Relaxed);
    let params = Arc::new(ParamTable::new(config.param_capacity));
    let readback = Arc::new(ParamTable::new(config.param_capacity));
    let meters = Arc::new(MeterBank::new(config.meter_capacity));
    let processor = EngineProcessor {
        rx,
        graph_rx,
        timeline_rx,
        garbage: gtx,
        graph: None,
        ctx: EngineContext {
            transport: Default::default(),
            discontinuity: false,
            timeline: Box::new(TimelineSnapshot::empty(config.sample_rate as f64)),
            params: Arc::clone(&params),
            readback: Arc::clone(&readback),
            meters: Arc::clone(&meters),
        },
        transport: TransportState::default(),
        shared: Arc::clone(&shared),
        stream_rate: config.sample_rate,
        recorder: None,
        click: Click::default(),
    };
    let controller = EngineController {
        config,
        readback,
        tx,
        graph_tx,
        timeline_tx,
        garbage: grx,
        shared,
        params,
        meters,
        slots: SlotRegistry::new(config.param_capacity, config.meter_capacity),
        plugins: PluginHost::default(),
        timings: None,
        stats: GraphStats::default(),
        warnings: Vec::new(),
        plan: Arc::new(StreamPlan::default()),
        suspended_lanes: Default::default(),
    };
    (controller, processor)
}

/// The realtime half. Implements [`AudioCallback`]; also driven directly by
/// the offline renderer.
pub struct EngineProcessor {
    rx: Consumer<Message>,
    graph_rx: MailboxReceiver<CompiledGraph<EngineContext>>,
    timeline_rx: MailboxReceiver<TimelineSnapshot>,
    garbage: Producer<Garbage>,
    graph: Option<Box<CompiledGraph<EngineContext>>>,
    ctx: EngineContext,
    transport: TransportState,
    shared: Arc<EngineShared>,
    stream_rate: u32,
    recorder: Option<Box<Recorder>>,
    click: Click,
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

    fn drain_messages(&mut self) {
        if let Some(mut new) = self.graph_rx.take() {
            if let Some(mut old) = self.graph.take() {
                new.adopt_state_from(&mut old);
                self.retire(Garbage::Graph(old));
            }
            self.graph = Some(new);
            self.shared.graphs_installed.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(t) = self.timeline_rx.take() {
            let old = std::mem::replace(&mut self.ctx.timeline, t);
            self.retire(Garbage::Timeline(old));
        }
        for _ in 0..Self::MAX_MESSAGES_PER_CALLBACK {
            let Ok(msg) = self.rx.pop() else { break };
            match msg {
                Message::Transport(cmd) => self.transport.apply(cmd),
                Message::ResetProcessors => {
                    if let Some(g) = &mut self.graph {
                        g.reset_all();
                    }
                    self.click.reset();
                }
                Message::BeginRecord(r) => {
                    if let Some(old) = self.recorder.replace(r) {
                        self.retire(Garbage::Recorder(old));
                    }
                }
                Message::EndRecord => {
                    if let Some(old) = self.recorder.take() {
                        self.retire(Garbage::Recorder(old));
                    }
                }
            }
        }
    }

    /// Process one device callback. Realtime-safe.
    pub fn process_device(&mut self, io: &mut dyn DeviceBuffers) {
        let started = Instant::now();
        self.drain_messages();
        let frames = io.frames();
        if frames == 0 {
            // Idle pump (no stream running): apply state, measure nothing.
            self.shared.transport.publish(&self.transport);
            self.shared.epoch.advance();
            return;
        }
        for c in 0..io.output_channels() {
            io.output(c).fill(0.0);
        }
        let rate = self.stream_rate as f64;
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
            .map_or(1024, |g| g.config().max_block_size)
            .max(1);

        let mut offset = 0;
        while offset < frames {
            let n = self
                .transport
                .frames_until_wrap((frames - offset).min(max_block));
            let info = self.transport.info(&self.ctx.timeline.timeline, rate);
            self.ctx.transport = info;
            self.ctx.discontinuity = self.transport.take_discontinuity();
            if self.ctx.discontinuity {
                self.click.reset();
            }
            let pos = self.transport.position();
            if info.playing
                && info.recording
                && let Some(rec) = self.recorder.as_deref_mut()
            {
                let got = rec.capture(&*io, offset, n, pos, &self.shared.record_overruns);
                self.shared
                    .recorded_frames
                    .fetch_add(got as u64, Ordering::Relaxed);
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
                graph.process(&ProcessContext {
                    frames: n,
                    sample_rate: rate,
                    data: &self.ctx,
                });
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
            let click = match self.shared.metronome.mode() {
                MetronomeMode::Off => false,
                MetronomeMode::Recording => info.recording,
                MetronomeMode::Always => true,
            };
            if click && info.playing {
                self.click.render(
                    io,
                    offset,
                    n,
                    pos,
                    &self.ctx.timeline.timeline,
                    rate,
                    self.shared.metronome.gain(),
                );
            }
            self.transport.advance(n);
            offset += n;
        }
        self.shared.transport.publish(&self.transport);
        let budget_ns = (frames as f64 * 1e9 / rate.max(1.0)) as u64;
        self.shared
            .metrics
            .record(started.elapsed().as_nanos() as u64, budget_ns);
        // No streamed page reference survives past this point.
        self.shared.epoch.advance();
    }
}

impl AudioCallback for EngineProcessor {
    fn prepare(&mut self, info: &StreamInfo) {
        self.stream_rate = info.sample_rate;
        self.shared
            .stream_sample_rate
            .store(info.sample_rate, Ordering::Relaxed);
        self.shared
            .stream_buffer_size
            .store(info.buffer_size, Ordering::Relaxed);
    }

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        self.process_device(io);
    }
}

/// Meter values of one track for the UI (mono tracks report both sides).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackMeter {
    pub left: MeterReading,
    pub right: MeterReading,
}

/// The control-thread half: rebuilds graphs, publishes snapshots and
/// parameter values, sends transport commands and reads meters/metrics.
pub struct EngineController {
    config: EngineConfig,
    tx: Producer<Message>,
    graph_tx: MailboxSender<CompiledGraph<EngineContext>>,
    timeline_tx: MailboxSender<TimelineSnapshot>,
    garbage: Consumer<Garbage>,
    shared: Arc<EngineShared>,
    params: Arc<ParamTable>,
    meters: Arc<MeterBank>,
    slots: SlotRegistry,
    readback: Arc<ParamTable>,
    plugins: PluginHost,
    timings: Option<Arc<NodeTimings>>,
    stats: GraphStats,
    warnings: Vec<String>,
    plan: Arc<StreamPlan>,
    /// Automation lanes being written (they do not drive their parameter).
    suspended_lanes: std::collections::HashSet<faderframe_core::AutomationLaneId>,
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
        self.timings.clone()
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.shared.metrics.snapshot()
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

    /// Parameters of an instantiated plugin (for automation lists).
    pub fn plugin_parameters(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<&[faderframe_plugin_host::ParameterInfo]> {
        self.plugins.parameters(plugin)
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
        match impact {
            Impact::None => Ok(()),
            Impact::Params => self.update_params(project),
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
        self.slots.write_params(project, &self.params)?;
        Ok(())
    }

    pub fn rebuild_graph(&mut self, project: &Project) -> Result<(), EngineError> {
        self.slots.retain_project(project);
        self.plugins.retain_project(project);
        let mut prepare =
            PrepareConfig::new(self.config.sample_rate as f64, self.config.max_block_size);
        prepare.measure_nodes = self.config.measure_nodes;
        let built = build_graph(project, &mut self.slots, &mut self.plugins, &prepare)?;
        let compiled = built.builder.compile(&prepare)?;
        // Parameters must be valid before the new graph's processors read them.
        self.slots.write_params(project, &self.params)?;
        self.stats = compiled.stats().clone();
        self.timings = Some(compiled.timings());
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
        );
        self.plan = Arc::new(snapshot.stream_plan());
        // A snapshot the audio thread never picked up is dropped right here.
        drop(self.timeline_tx.send(Box::new(snapshot)));
        let range = project.loop_range.and_then(|r| {
            LoopRange::new(
                self.musical_to_samples(project, r.start),
                self.musical_to_samples(project, r.end),
            )
        });
        self.send(Message::Transport(TransportCommand::SetLoopRange(range)))?;
        self.send(Message::Transport(TransportCommand::SetLoopEnabled(
            project.loop_enabled && range.is_some(),
        )))
    }

    pub fn transport(&mut self, cmd: TransportCommand) -> Result<(), EngineError> {
        self.send(Message::Transport(cmd))
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
        self.send(Message::BeginRecord(Box::new(rt)))?;
        Ok(streams)
    }

    /// Stop capturing; the writer sees its streams finish once the audio
    /// thread has let go of them (after the next callback and garbage
    /// collection).
    pub fn end_recording(&mut self) -> Result<(), EngineError> {
        self.send(Message::EndRecord)
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

    /// Consume the meter values accumulated since the last call.
    pub fn take_meter(&self, track: TrackId) -> Option<TrackMeter> {
        let range = self.slots.meter_of(track)?;
        Some(TrackMeter {
            left: self.meters.take(range.first),
            right: range
                .channel(1)
                .map_or_else(|| self.meters.take(range.first), |i| self.meters.take(i)),
        })
    }
}
