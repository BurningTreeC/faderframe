//! FaderFrame's side of a sandboxed plugin: [`RemoteInstance`] starts the
//! helper and forwards control calls (answering frequent reads from what
//! the last poll brought); [`RemoteProcessor`] runs a block through shared
//! memory and waits for it with a deadline.

use crate::shm::{Block, BlockIn, MAX_BUFFERS, block_size};
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
    sched_sent: bool,
}

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
        let frames = io.frames.min(self.block.max_frames());
        let h = self.block.header();
        if !self.sched_sent {
            let (packed, extra) = faderframe_realtime::thread_scheduling();
            h.sched_extra.store(extra, Ordering::Relaxed);
            h.sched.store(packed, Ordering::Release);
            self.sched_sent = true;
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
        let mut out_channels = [0usize; MAX_BUFFERS];
        let n_out = io.audio_out.len().min(MAX_BUFFERS);
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
        let fail = |io: &mut NodeIo<'_>| {
            dead.store(true, Ordering::Relaxed);
            silence(io);
            ProcessStatus::Error
        };
        if !self.pipes.go.signal() {
            return fail(io);
        }
        let deadline = Instant::now() + BLOCK_TIMEOUT;
        while h.done.load(Ordering::Acquire) != self.seq {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return fail(io);
            }
            if self.pipes.done.wait(Some(left)) == Ready::HungUp
                && h.done.load(Ordering::Acquire) != self.seq
            {
                return fail(io);
            }
        }
        self.block
            .read_response(frames, io.audio_out, io.events_out.first_mut())
    }
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
            return ProcessStatus::Continue;
        };
        ch.process(ctx, io, &self.dead)
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
    values: HashMap<u32, f64>,
    latency: u32,
    tail: TailLength,
    note_expressions: Option<Vec<NoteExpressionKind>>,
    has_editor: bool,
    editor_open: bool,
    programs: Vec<String>,
    program: Option<usize>,
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
}

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
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
            values: HashMap::new(),
            latency: 0,
            tail: TailLength::None,
            note_expressions: None,
            has_editor: false,
            editor_open: false,
            programs: Vec::new(),
            program: None,
            pending: false,
            requests: EditorRequests::default(),
            edits: Vec::new(),
            config: None,
            active: None,
            activations: 0,
            needs_restart: false,
            formatted: HashMap::new(),
            reaped: false,
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
                inst.values = i.values.into_iter().collect();
                inst.latency = i.latency;
                inst.tail = i.tail.into();
                inst.note_expressions = i
                    .note_expressions
                    .map(|v| v.into_iter().filter_map(wire::expression_kind).collect());
                inst.has_editor = i.has_editor;
                inst.programs = i.programs;
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

    fn request(
        &mut self,
        req: &Request,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(Response, Vec<u8>), PluginError> {
        if self.dead.load(Ordering::Relaxed) {
            return Err(failed("the plugin's process is gone"));
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

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        self.values.get(&id.0).copied()
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        self.values.insert(id.0, value);
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
        if self.dead.load(Ordering::Relaxed) {
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

    fn current_program(&self) -> Option<usize> {
        self.program
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
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
        let Ok((Response::Polled(p), _)) = self.request(&Request::Poll, &[], REQUEST_TIMEOUT)
        else {
            return PluginPoll::default();
        };
        for (id, v) in p.values {
            self.values.insert(id, v);
        }
        if let Some(params) = &p.params {
            self.params = params.iter().map(ParameterInfo::from).collect();
            self.modulation = params
                .iter()
                .filter(|p| p.modulatable)
                .map(|p| (p.id, p.per_note))
                .collect();
        }
        if let Some(t) = p.tail {
            self.tail = t.into();
        }
        if let Some(programs) = p.programs {
            self.programs = programs;
        }
        self.program = p.program;
        self.pending = p.pending;
        let latency_changed = p.latency != self.latency;
        self.latency = p.latency;
        self.edits.extend(p.edits.into_iter().map(EditorEdit::from));
        let r: EditorRequests = p.editor.into();
        self.requests.resize = r.resize.or(self.requests.resize);
        self.requests.show |= r.show;
        self.requests.hide |= r.hide;
        self.requests.closed |= r.closed;
        self.editor_open = p.editor_open;
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
        if self.config.as_ref() != Some(config) || self.active.is_none() || self.needs_restart {
            self.needs_restart = false;
            self.deactivate();
            let mut block = Block::create(config.max_block_size.max(1) as usize).map_err(failed)?;
            let req = Request::Activate {
                sample_rate: config.sample_rate,
                max_block: config.max_block_size,
                sidechain: config.sidechain,
                shm: block.name().to_string(),
                shm_size: block.size() as u64,
                double_precision: config.double_precision,
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
            self.activations += 1;
            self.active = Some((
                Arc::new(TryCell::new(Channel {
                    block,
                    pipes: Arc::clone(&self.pipes),
                    seq: 0,
                    sched_sent: false,
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
