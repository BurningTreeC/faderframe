//! FaderFrame's side of a sandboxed plugin: [`RemoteInstance`] starts the
//! helper and forwards control calls (answering frequent reads from what
//! the last poll brought); [`RemoteProcessor`] runs a block through shared
//! memory and waits for it with a deadline.

use crate::shm::{Block, BlockIn, MAX_OUT_BUFFERS, block_size};
use crate::sys::{self, Control, Ready, Signal, Waiter};
use crate::wire::{self, EditorCall, Request, Response};
use crate::{BLOCK_TIMEOUT, Launcher};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_midi::NoteExpressionKind;
use faderframe_plugin_host::{
    EditorEdit, EditorRequests, ParameterInfo, ParentWindow, PluginDescriptor, PluginEditor,
    PluginError, PluginFormat, PluginInstance, PluginPoll, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus, TailLength, WindowApi,
};
use faderframe_realtime::TryCell;
use std::collections::HashMap;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Loading a plugin, its state or a preset can take a while.
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Our ends of a helper's per-block wake-ups.
struct Pipes {
    go: Signal,
    done: Waiter,
}

/// One activation as the audio thread uses it.
struct Channel {
    block: Block,
    pipes: Arc<Pipes>,
    seq: u32,
    /// Blocks processed: the calling thread's scheduling goes to the helper
    /// on the first and every `SCHED_EVERY` after (the caller may change —
    /// rendered ahead, then live).
    blocks: u32,
    /// The instance's gain-reduction cell, set from the helper's answer.
    reduction: Arc<faderframe_plugin_host::Reduction>,
    /// The engine's deadline for the whole device callback (none: one
    /// block's duration from the call).
    deadline: Option<Instant>,
    /// A block sent and not answered in time: when it was sent. The next
    /// request waits for it (the helper may still read the shared memory).
    in_flight: Option<Instant>,
    /// Seconds per frame (the activation's rate).
    frame_seconds: f64,
    /// Blocks given up on (silence played): the engine counts them.
    late: u64,
}

/// How often the calling thread's scheduling is sent to the helper again.
const SCHED_EVERY: u32 = 1024;

fn silence(io: &mut NodeIo<'_>) {
    for out in io.audio_out.iter_mut() {
        out.clear();
    }
}

impl Channel {
    fn process(
        &mut self,
        ctx: &PluginProcessContext<'_>,
        io: &mut NodeIo<'_>,
        dead: &AtomicBool,
    ) -> ProcessStatus {
        let started = Instant::now();
        let frames = io.frames.min(self.block.max_frames());
        let h = self.block.header();
        if self.blocks.is_multiple_of(SCHED_EVERY) {
            // A system call now and then: the helper's audio thread takes
            // the caller's priority.
            let (packed, extra) = faderframe_realtime::thread_scheduling();
            h.sched_extra.store(extra, Ordering::Relaxed);
            h.sched.store(packed, Ordering::Release);
        }
        self.blocks = self.blocks.wrapping_add(1);
        let fail = |io: &mut NodeIo<'_>| {
            dead.store(true, Ordering::Relaxed);
            silence(io);
            ProcessStatus::Error
        };
        // How long this call may wait for the helper: the callback's
        // budget, else a block's duration (past it the callback is late in
        // any case) — a stuck helper costs its own track a block of
        // silence, never the engine a quarter of a second.
        let limit = self.deadline.unwrap_or_else(|| {
            started + Duration::from_secs_f64(frames as f64 * self.frame_seconds)
        });
        if let Some(sent) = self.in_flight {
            // The block given up on last time: its answer first. Still
            // not here: this block's events never reach the helper (the
            // engine keeps their note-offs for the next block sent).
            match self.wait_done(limit.max(started), sent) {
                Wait::Done => self.in_flight = None,
                Wait::Late => {
                    self.late += 1;
                    silence(io);
                    return ProcessStatus::Missed;
                }
                Wait::Dead => return fail(io),
            }
        }
        // macOS: the helper's audio thread follows the device's workgroup.
        h.wg_gen.store(
            faderframe_realtime::process_workgroup_generation(),
            Ordering::Release,
        );
        let joined = h.wg_joined.load(Ordering::Acquire);
        if joined != 0 {
            crate::workgroup::note_joined(joined);
        }
        let mut out_channels = [0usize; MAX_OUT_BUFFERS];
        let n_out = io.audio_out.len().min(MAX_OUT_BUFFERS);
        for (c, b) in out_channels.iter_mut().zip(io.audio_out.iter()) {
            *c = b.num_channels();
        }
        self.block.write_request(&BlockIn {
            frames,
            transport: ctx.transport,
            params: ctx.param_events,
            mods: ctx.param_mods,
            note_mods: ctx.note_mods,
            audio_in: io.audio_in,
            events_in: io.events_in.first(),
            out_channels: &out_channels[..n_out],
            events_out: !io.events_out.is_empty(),
        });
        self.seq = self.seq.wrapping_add(1);
        h.seq.store(self.seq, Ordering::Release);
        if !self.pipes.go.signal() {
            return fail(io);
        }
        match self.wait_done(limit, Instant::now()) {
            Wait::Done => {}
            Wait::Late => {
                self.in_flight = Some(Instant::now());
                self.late += 1;
                silence(io);
                return ProcessStatus::Continue;
            }
            Wait::Dead => return fail(io),
        }
        let h = self.block.header();
        let status = self
            .block
            .read_response(frames, io.audio_out, io.events_out.first_mut());
        match f32::from_bits(h.reduction.load(Ordering::Relaxed)) {
            v if v.is_nan() => self.reduction.set_unreported(),
            v => self.reduction.set(v),
        }
        status
    }

    /// Wait for the helper's answer to the last block until `limit`; a
    /// block unanswered `BLOCK_TIMEOUT` after `sent` (or a helper gone)
    /// means it is dead.
    fn wait_done(&self, limit: Instant, sent: Instant) -> Wait {
        let h = self.block.header();
        let dead_at = sent + BLOCK_TIMEOUT;
        loop {
            if h.done.load(Ordering::Acquire) == self.seq {
                return Wait::Done;
            }
            let now = Instant::now();
            if now >= dead_at {
                return Wait::Dead;
            }
            if now >= limit {
                return Wait::Late;
            }
            let until = limit.min(dead_at);
            if self.pipes.done.wait(Some(until - now)) == Ready::HungUp
                && h.done.load(Ordering::Acquire) != self.seq
            {
                return Wait::Dead;
            }
        }
    }
}

/// How waiting for a helper's answer ended.
enum Wait {
    Done,
    /// Not in time for this callback (it may still come).
    Late,
    Dead,
}

/// The audio-thread half of a sandboxed plugin.
pub struct RemoteProcessor {
    channel: Arc<TryCell<Channel>>,
    /// A newer activation replaced this one: fall silent.
    retired: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
}

impl PluginProcessor for RemoteProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if self.dead.load(Ordering::Relaxed) {
            silence(io);
            return ProcessStatus::Error;
        }
        if self.retired.load(Ordering::Relaxed) {
            silence(io);
            return ProcessStatus::Continue;
        }
        let Some(mut ch) = self.channel.try_lock() else {
            silence(io);
            return ProcessStatus::Missed;
        };
        ch.process(ctx, io, &self.dead)
    }

    fn set_callback_deadline(&mut self, deadline: Option<std::time::Instant>) {
        if let Some(mut ch) = self.channel.try_lock() {
            ch.deadline = deadline;
        }
    }

    fn take_underruns(&mut self) -> u64 {
        self.channel
            .try_lock()
            .map_or(0, |mut ch| std::mem::take(&mut ch.late))
    }

    fn reset(&mut self) {
        if let Some(ch) = self.channel.try_lock() {
            ch.block.header().reset.store(1, Ordering::Release);
        }
    }
}

/// A plugin instance in a helper process.
pub struct RemoteInstance {
    child: Child,
    control: Control,
    pipes: Arc<Pipes>,
    dead: Arc<AtomicBool>,
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
    /// Parameters that take modulation: (id, per note too).
    modulation: Vec<(u32, bool)>,
    /// Parameters' groups ("Scene A/Osc 1").
    groups: HashMap<u32, String>,
    values: HashMap<u32, f64>,
    latency: u32,
    tail: TailLength,
    note_expressions: Option<Vec<NoteExpressionKind>>,
    has_editor: bool,
    editor_open: bool,
    programs: Vec<String>,
    program: Option<usize>,
    output_names: Vec<String>,
    /// Output buses the graph takes, and how many the active helper has.
    output_buses: usize,
    active_outputs: usize,
    /// The helper's processor has changes to take (as of the last poll, or
    /// since a program was selected).
    pending: bool,
    requests: EditorRequests,
    edits: Vec<EditorEdit>,
    config: Option<ProcessConfig>,
    active: Option<(Arc<TryCell<Channel>>, Arc<AtomicBool>)>,
    activations: u64,
    needs_restart: bool,
    /// The last formatted value per parameter (editors ask every frame).
    formatted: HashMap<u32, (u64, Option<String>)>,
    /// The dead helper was killed and reaped.
    reaped: bool,
    /// The gain reduction the plugin reports (through the helper).
    reduction: Arc<faderframe_plugin_host::Reduction>,
    /// A Poll or SaveState sent and not answered yet (since when, and what
    /// FaderFrame had changed by then): they are answered at a later tick,
    /// so a busy helper never holds up the UI.
    polling: std::cell::Cell<Option<(Waiting, Instant, Changes)>>,
    /// Answers to polls taken in before another request (applied by the
    /// next `poll`), with the changes they were asked after.
    polled: std::cell::RefCell<Vec<(wire::Polled, Changes)>>,
    /// The state asked for with `request_state`: wanted (not sent yet),
    /// and the answer when it came.
    state_wanted: bool,
    state: std::cell::RefCell<Option<(Vec<u8>, Changes)>>,
    /// What FaderFrame has changed in the helper so far.
    changes: Changes,
    /// Parameters FaderFrame set: the `changes.params` count after each.
    set_at: HashMap<u32, u64>,
}

/// What an answer on its way answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    Poll,
    State,
}

/// Counts of what FaderFrame changed in the helper. A pipelined poll is
/// answered as the helper was when it read the poll: what FaderFrame
/// changed after sending it (opened the editor, loaded a state, …) the
/// answer cannot know, and its picture of those things must not replace
/// FaderFrame's own. (Taking a stale "editor closed" for the truth made
/// FaderFrame destroy the window a just-opened editor sat in; plugins on
/// Xlib then died of a BadWindow error — issue #12.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Changes {
    /// The editor opened, attached, floated or closed.
    editor: u64,
    /// A program selected or a state loaded.
    state: u64,
    /// Activated or deactivated (the latency and tail it reports).
    activation: u64,
    /// Parameter values set (`RemoteInstance::set_at` says which).
    params: u64,
}

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

/// The parameters in a group, by id.
fn groups_of(params: &[wire::Param]) -> HashMap<u32, String> {
    params
        .iter()
        .filter(|p| !p.group.is_empty())
        .map(|p| (p.id, p.group.clone()))
        .collect()
}

impl RemoteInstance {
    /// Start a helper and instantiate `id` of `format` in it.
    pub fn spawn(launcher: &Launcher, format: PluginFormat, id: &str) -> Result<Self, PluginError> {
        let link = sys::spawn(launcher)
            .map_err(|e| failed(format!("cannot start the plugin process: {e}")))?;
        let mut inst = Self {
            child: link.child,
            control: link.control,
            pipes: Arc::new(Pipes {
                go: link.go,
                done: link.done,
            }),
            dead: Arc::new(AtomicBool::new(false)),
            descriptor: PluginDescriptor {
                format,
                id: id.to_string(),
                name: id.to_string(),
                vendor: String::new(),
                version: String::new(),
                category: faderframe_plugin_host::PluginCategory::Effect,
                audio_inputs: Vec::new(),
                audio_outputs: Vec::new(),
                note_inputs: 0,
                note_outputs: 0,
            },
            params: Vec::new(),
            modulation: Vec::new(),
            groups: HashMap::new(),
            values: HashMap::new(),
            latency: 0,
            tail: TailLength::None,
            note_expressions: None,
            has_editor: false,
            editor_open: false,
            programs: Vec::new(),
            program: None,
            output_names: Vec::new(),
            output_buses: 1,
            active_outputs: 1,
            pending: false,
            requests: EditorRequests::default(),
            edits: Vec::new(),
            config: None,
            active: None,
            activations: 0,
            needs_restart: false,
            formatted: HashMap::new(),
            reaped: false,
            polling: std::cell::Cell::new(None),
            polled: std::cell::RefCell::new(Vec::new()),
            state_wanted: false,
            state: std::cell::RefCell::new(None),
            changes: Changes::default(),
            set_at: HashMap::new(),
            reduction: faderframe_plugin_host::Reduction::unreported(),
        };
        let req = Request::Instantiate {
            format: format.into(),
            id: id.to_string(),
        };
        match inst.request(&req, &[], LOAD_TIMEOUT)? {
            (Response::Instantiated(i), _) => {
                inst.descriptor = (&i.descriptor).into();
                inst.params = i.params.iter().map(ParameterInfo::from).collect();
                inst.modulation = i
                    .params
                    .iter()
                    .filter(|p| p.modulatable)
                    .map(|p| (p.id, p.per_note))
                    .collect();
                inst.groups = groups_of(&i.params);
                inst.values = i.values.into_iter().collect();
                inst.latency = i.latency;
                inst.tail = i.tail.into();
                inst.note_expressions = i
                    .note_expressions
                    .map(|v| v.into_iter().filter_map(wire::expression_kind).collect());
                inst.has_editor = i.has_editor;
                inst.programs = i.programs;
                inst.output_names = i.output_names;
                tracing::info!(
                    "{} runs in a sandbox (process {})",
                    inst.descriptor.name,
                    inst.child.id()
                );
                Ok(inst)
            }
            (Response::Failed(e), _) => Err(PluginError::Failed(e)),
            (other, _) => Err(failed(format!("unexpected answer {other:?}"))),
        }
    }

    /// The helper crashed, hung or broke the protocol: stop it for good.
    fn mark_dead(&mut self, why: &str) {
        self.dead.store(true, Ordering::Relaxed);
        if !self.reaped {
            self.reaped = true;
            tracing::warn!(
                "{}: the plugin's process is gone ({why}); FaderFrame keeps running without it",
                self.descriptor.name
            );
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Take a poll's answer in: values, parameters, editor requests; what
    /// the host must do about it. `asked` is what FaderFrame had changed
    /// when it sent the poll: what changed since, the answer does not
    /// describe (the next one will).
    fn apply_polled(&mut self, p: wire::Polled, asked: Changes) -> PluginPoll {
        let now = self.changes;
        for (id, v) in p.values {
            // Set by FaderFrame since: the helper's new value is ours.
            if self.set_at.get(&id).is_some_and(|&at| at > asked.params) {
                continue;
            }
            self.values.insert(id, v);
        }
        // Later answers know every value set before this one was asked.
        self.set_at.retain(|_, at| *at > asked.params);
        if let Some(params) = &p.params {
            self.params = params.iter().map(ParameterInfo::from).collect();
            self.modulation = params
                .iter()
                .filter(|p| p.modulatable)
                .map(|p| (p.id, p.per_note))
                .collect();
            self.groups = groups_of(params);
        }
        if let Some(programs) = p.programs {
            self.programs = programs;
        }
        // Edits made in the plugin's editor are events: always taken.
        self.edits.extend(p.edits.into_iter().map(EditorEdit::from));
        // The program, and whether the processor has taken it.
        if asked.state == now.state {
            self.program = p.program;
            self.pending = p.pending;
        }
        // The latency and tail of the activation the helper had then.
        let mut latency_changed = false;
        if asked.activation == now.activation {
            if let Some(t) = p.tail {
                self.tail = t.into();
            }
            latency_changed = p.latency != self.latency;
            self.latency = p.latency;
        }
        // The editor as it was then: opened or closed since, the answer
        // speaks of another (its close request, its "not open").
        if asked.editor == now.editor {
            let r: EditorRequests = p.editor.into();
            self.requests.resize = r.resize.or(self.requests.resize);
            self.requests.show |= r.show;
            self.requests.hide |= r.hide;
            self.requests.closed |= r.closed;
            self.editor_open = p.editor_open;
        }
        let restart = p.restart || (latency_changed && self.active.is_some());
        if restart {
            self.needs_restart = true;
        }
        PluginPoll {
            restart,
            params_changed: p.params_changed,
            state_dirty: p.state_dirty,
        }
    }

    /// Take in the answer of a poll still on its way, so the next answer
    /// read is the next request's (`&self`: through another handle on
    /// the stream).
    fn collect_answer(&self) -> Result<(), PluginError> {
        let Some((waiting, _, asked)) = self.polling.take() else {
            return Ok(());
        };
        let mut control = self.control.try_clone().map_err(failed)?;
        let _ = control.set_read_timeout(Some(match waiting {
            Waiting::Poll => REQUEST_TIMEOUT,
            Waiting::State => LOAD_TIMEOUT,
        }));
        match wire::recv::<Response>(&mut control) {
            Ok((Response::Polled(p), _)) => {
                self.polled.borrow_mut().push((*p, asked));
                Ok(())
            }
            Ok((Response::Bytes, bytes)) if waiting == Waiting::State => {
                *self.state.borrow_mut() = Some((bytes, asked));
                Ok(())
            }
            // The state could not be had: asked again next time.
            Ok((Response::Failed(_), _)) if waiting == Waiting::State => Ok(()),
            Ok((other, _)) => Err(failed(format!("unexpected answer {other:?}"))),
            Err(e) => {
                self.dead.store(true, Ordering::Relaxed);
                Err(failed(format!("the plugin's process does not answer: {e}")))
            }
        }
    }

    fn request(
        &mut self,
        req: &Request,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(Response, Vec<u8>), PluginError> {
        if self.dead.load(Ordering::Relaxed) {
            return Err(failed("the plugin's process is gone"));
        }
        if let Err(e) = self.collect_answer() {
            self.mark_dead(&e.to_string());
            return Err(e);
        }
        let _ = self.control.set_read_timeout(Some(timeout));
        let _ = self.control.set_write_timeout(Some(timeout));
        let r = wire::send(&mut self.control, req, payload)
            .and_then(|()| wire::recv::<Response>(&mut self.control));
        match r {
            Ok(v) => Ok(v),
            Err(e) => {
                self.mark_dead(&e.to_string());
                Err(failed(format!("the plugin's process does not answer: {e}")))
            }
        }
    }

    fn done(
        &mut self,
        req: &Request,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), PluginError> {
        match self.request(req, payload, timeout)? {
            (Response::Done, _) => Ok(()),
            (Response::Failed(e), _) => Err(PluginError::Failed(e)),
            (other, _) => Err(failed(format!("unexpected answer {other:?}"))),
        }
    }

    fn bytes(&mut self, req: &Request, payload: &[u8]) -> Result<Vec<u8>, PluginError> {
        match self.request(req, payload, LOAD_TIMEOUT)? {
            (Response::Bytes, bytes) => Ok(bytes),
            (Response::Failed(e), _) => Err(PluginError::InvalidState(e)),
            (other, _) => Err(failed(format!("unexpected answer {other:?}"))),
        }
    }

    fn editor_call(&mut self, call: EditorCall) -> Option<Response> {
        self.request(&Request::Editor(call), &[], LOAD_TIMEOUT)
            .ok()
            .map(|(r, _)| r)
    }

    fn flag(&mut self, call: EditorCall) -> bool {
        matches!(self.editor_call(call), Some(Response::Flag(true)))
    }

    /// Retire the current activation (its processors fall silent).
    fn deactivate(&mut self) {
        if let Some((ch, retired)) = self.active.take() {
            self.changes.activation += 1;
            retired.store(true, Ordering::Relaxed);
            // Wake the helper's audio thread so it sees the request quickly.
            if let Some(c) = ch.lock_blocking(10_000) {
                c.block.header().quit.store(1, Ordering::Release);
            }
            self.pipes.go.signal();
            let _ = self.done(&Request::Deactivate, &[], REQUEST_TIMEOUT);
        }
    }
}

impl Drop for RemoteInstance {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if !self.dead.load(Ordering::Relaxed) {
            self.deactivate();
            let _ = self.control.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = wire::send(&mut self.control, &Request::Quit, &[]);
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PluginInstance for RemoteInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }

    fn modulatable(&self, id: ParameterId) -> bool {
        self.modulation.iter().any(|(p, _)| *p == id.0)
    }

    fn modulatable_per_note(&self, id: ParameterId) -> bool {
        self.modulation.iter().any(|(p, n)| *p == id.0 && *n)
    }

    fn parameter_group(&self, id: ParameterId) -> Option<String> {
        self.groups.get(&id.0).cloned()
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        self.values.get(&id.0).copied()
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        self.values.insert(id.0, value);
        self.changes.params += 1;
        self.set_at.insert(id.0, self.changes.params);
        self.done(
            &Request::SetParameter { id: id.0, value },
            &[],
            REQUEST_TIMEOUT,
        )
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn preset_files(&self) -> Vec<std::path::PathBuf> {
        // `&self`: ask through a fresh borrow of the stream.
        let Ok(mut control) = self.control.try_clone() else {
            return Vec::new();
        };
        if self.dead.load(Ordering::Relaxed) || self.collect_answer().is_err() {
            return Vec::new();
        }
        let _ = control.set_read_timeout(Some(REQUEST_TIMEOUT));
        match wire::send(&mut control, &Request::PresetFiles, &[])
            .and_then(|()| wire::recv::<Response>(&mut control))
        {
            Ok((Response::Paths(p), _)) => p,
            _ => Vec::new(),
        }
    }

    fn state_from_preset_file(&self, data: &[u8]) -> Result<Vec<u8>, PluginError> {
        if self.dead.load(Ordering::Relaxed) {
            return Err(failed("the plugin's process is gone"));
        }
        self.collect_answer()?;
        let mut control = self.control.try_clone().map_err(failed)?;
        let _ = control.set_read_timeout(Some(LOAD_TIMEOUT));
        match wire::send(&mut control, &Request::StateFromPresetFile, data)
            .and_then(|()| wire::recv::<Response>(&mut control))
        {
            Ok((Response::Bytes, bytes)) => Ok(bytes),
            Ok((Response::Failed(e), _)) => Err(PluginError::InvalidState(e)),
            Ok((other, _)) => Err(failed(format!("unexpected answer {other:?}"))),
            Err(e) => Err(failed(e)),
        }
    }

    fn programs(&self) -> Vec<String> {
        self.programs.clone()
    }

    fn configure_outputs(&mut self, buses: usize) {
        self.output_buses = buses.max(1);
    }

    fn output_bus_names(&mut self) -> Vec<String> {
        self.output_names.clone()
    }

    fn current_program(&self) -> Option<usize> {
        self.program
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        self.changes.state += 1;
        self.done(&Request::SelectProgram { index }, &[], LOAD_TIMEOUT)?;
        self.program = Some(index);
        // Until the next poll says the helper's processor has it.
        self.pending = true;
        Ok(())
    }

    fn changes_pending(&self) -> bool {
        self.pending && !self.dead.load(Ordering::Relaxed)
    }

    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        std::mem::take(&mut self.edits)
    }

    fn note_expressions(&self) -> Option<Vec<NoteExpressionKind>> {
        self.note_expressions.clone()
    }

    fn reduction(&self) -> Option<Arc<faderframe_plugin_host::Reduction>> {
        Some(Arc::clone(&self.reduction))
    }

    fn sandboxed(&self) -> bool {
        true
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn tail(&self) -> TailLength {
        self.tail
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        self.bytes(&Request::SaveState, &[])
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        self.changes.state += 1;
        self.done(&Request::LoadState, data, LOAD_TIMEOUT)
    }

    fn poll(&mut self) -> PluginPoll {
        if self.dead.load(Ordering::Relaxed) {
            // Noticed by the audio thread: crashed, or hung (then stopped).
            let why = match self.child.try_wait() {
                Ok(Some(status)) => format!("it ended: {status}"),
                _ => "it stopped answering".to_string(),
            };
            self.mark_dead(&why);
            return PluginPoll::default();
        }
        // Last tick's answer, if it is here (and any taken in before other
        // requests), then the next poll on its way.
        if let Some((waiting, sent, _)) = self.polling.get() {
            let timeout = match waiting {
                Waiting::Poll => REQUEST_TIMEOUT,
                Waiting::State => LOAD_TIMEOUT,
            };
            if sys::readable(&self.control) {
                if let Err(e) = self.collect_answer() {
                    self.mark_dead(&e.to_string());
                    return PluginPoll::default();
                }
            } else if sent.elapsed() >= timeout {
                self.mark_dead("it stopped answering");
                return PluginPoll::default();
            }
        }
        let answers = std::mem::take(&mut *self.polled.borrow_mut());
        let mut out = PluginPoll::default();
        for (p, asked) in answers {
            out = out.merge(self.apply_polled(p, asked));
        }
        if self.polling.get().is_none() {
            let (req, waiting) = if std::mem::take(&mut self.state_wanted) {
                (Request::SaveState, Waiting::State)
            } else {
                (Request::Poll, Waiting::Poll)
            };
            let _ = self.control.set_write_timeout(Some(REQUEST_TIMEOUT));
            match wire::send(&mut self.control, &req, &[]) {
                Ok(()) => self
                    .polling
                    .set(Some((waiting, Instant::now(), self.changes))),
                Err(e) => self.mark_dead(&e.to_string()),
            }
        }
        out
    }

    fn settle(&mut self) {
        if let Err(e) = self.collect_answer() {
            self.mark_dead(&e.to_string());
        }
    }

    fn request_state(&mut self) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return false;
        }
        self.state_wanted = true;
        true
    }

    fn take_state(&mut self) -> Option<Vec<u8>> {
        let (bytes, asked) = self.state.borrow_mut().take()?;
        if asked.state != self.changes.state {
            // Saved before a state or program FaderFrame loaded since: the
            // state it holds is no longer the plugin's. Asked again.
            self.state_wanted = true;
            return None;
        }
        Some(bytes)
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        (self.has_editor && !self.dead.load(Ordering::Relaxed)).then_some(self as _)
    }

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        if let Some((bits, text)) = self.formatted.get(&id.0)
            && *bits == value.to_bits()
        {
            return text.clone();
        }
        let text = match self
            .request(
                &Request::FormatParameter { id: id.0, value },
                &[],
                REQUEST_TIMEOUT,
            )
            .ok()?
        {
            (Response::Text(t), _) => t,
            _ => None,
        };
        self.formatted.insert(id.0, (value.to_bits(), text.clone()));
        text
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if self.dead.load(Ordering::Relaxed) {
            return Err(failed("the plugin's process is gone"));
        }
        if self.config.as_ref() != Some(config)
            || self.active.is_none()
            || self.needs_restart
            || self.active_outputs != self.output_buses
        {
            self.needs_restart = false;
            self.deactivate();
            self.changes.activation += 1;
            let mut block = Block::create(config.max_block_size.max(1) as usize).map_err(failed)?;
            let req = Request::Activate {
                sample_rate: config.sample_rate,
                max_block: config.max_block_size,
                sidechain: config.sidechain,
                shm: block.name().to_string(),
                shm_size: block.size() as u64,
                double_precision: config.double_precision,
                output_buses: self.output_buses.min(u16::MAX as usize) as u16,
            };
            let latency = match self.request(&req, &[], LOAD_TIMEOUT)? {
                (Response::Activated { latency }, _) => latency,
                (Response::Failed(e), _) => return Err(PluginError::Failed(e)),
                (other, _) => return Err(failed(format!("unexpected answer {other:?}"))),
            };
            // The helper has it mapped: the name is no longer needed.
            block.unlink();
            debug_assert_eq!(block.size(), block_size(block.max_frames()));
            self.latency = latency;
            self.config = Some(*config);
            self.active_outputs = self.output_buses;
            self.activations += 1;
            self.active = Some((
                Arc::new(TryCell::new(Channel {
                    block,
                    pipes: Arc::clone(&self.pipes),
                    seq: 0,
                    blocks: 0,
                    reduction: Arc::clone(&self.reduction),
                    deadline: None,
                    in_flight: None,
                    frame_seconds: 1.0 / config.sample_rate.max(1.0),
                    late: 0,
                })),
                Arc::new(AtomicBool::new(false)),
            ));
        }
        let (channel, retired) = self.active.as_ref().ok_or_else(|| failed("not active"))?;
        Ok(Box::new(RemoteProcessor {
            channel: Arc::clone(channel),
            retired: Arc::clone(retired),
            dead: Arc::clone(&self.dead),
        }))
    }
}

impl PluginEditor for RemoteInstance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        self.flag(EditorCall::CanEmbed(api.into()))
    }

    fn can_float(&mut self, api: WindowApi) -> bool {
        self.flag(EditorCall::CanFloat(api.into()))
    }

    fn open_embedded(&mut self, api: WindowApi, scale: f64) -> Result<(u32, u32), PluginError> {
        self.changes.editor += 1;
        match self.editor_call(EditorCall::OpenEmbedded {
            api: api.into(),
            scale,
        }) {
            Some(Response::Size(Some(size))) => Ok(size),
            Some(Response::Failed(e)) => Err(PluginError::Failed(e)),
            _ => Err(failed("the editor did not open")),
        }
    }

    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        self.changes.editor += 1;
        match self.editor_call(EditorCall::Attach {
            api: parent.api.into(),
            handle: parent.handle,
        }) {
            Some(Response::Done) => {
                self.editor_open = true;
                Ok(())
            }
            Some(Response::Failed(e)) => Err(PluginError::Failed(e)),
            _ => Err(failed("the editor did not attach")),
        }
    }

    fn open_floating(&mut self, api: WindowApi, title: &str) -> Result<(), PluginError> {
        self.changes.editor += 1;
        match self.editor_call(EditorCall::OpenFloating {
            api: api.into(),
            title: title.to_string(),
        }) {
            Some(Response::Done) => {
                self.editor_open = true;
                Ok(())
            }
            Some(Response::Failed(e)) => Err(PluginError::Failed(e)),
            _ => Err(failed("the editor did not open")),
        }
    }

    fn raise(&mut self) {
        if self.editor_open {
            self.editor_call(EditorCall::Raise);
        }
    }

    fn close(&mut self) {
        self.changes.editor += 1;
        if self.editor_open {
            self.editor_call(EditorCall::Close);
        }
        self.editor_open = false;
        self.requests = EditorRequests::default();
    }

    fn is_open(&self) -> bool {
        self.editor_open && !self.dead.load(Ordering::Relaxed)
    }

    fn can_resize(&mut self) -> bool {
        self.flag(EditorCall::CanResize)
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        match self.editor_call(EditorCall::SetSize { width, height }) {
            Some(Response::Size(s)) => s,
            _ => None,
        }
    }

    fn take_requests(&mut self) -> EditorRequests {
        let mut r = std::mem::take(&mut self.requests);
        if self.dead.load(Ordering::Relaxed) {
            r.closed = true;
        }
        r
    }
}
