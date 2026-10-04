//! The helper process: hosts one plugin instance with the application's
//! own (in-process) factories, answers FaderFrame's requests, services the
//! plugin GUI's descriptors and timers in a `poll` loop on its main thread,
//! and runs the audio blocks on an audio thread of its own (which copies
//! the scheduling of FaderFrame's audio thread). It ends when FaderFrame
//! says so or goes away.

use crate::host::{CONTROL_FD, DONE_FD, ENV_HELPER, GO_FD};
use crate::shm::{Block, HelperIo};
use crate::sys::{self, Ready};
use crate::wire::{self, EditorCall, Instantiated, Param, Polled, Request, Response};
use faderframe_audio_graph::NodeIo;
use faderframe_plugin_host::{
    ParentWindow, PluginFd, PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor,
    PluginRegistry, ProcessConfig,
};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// This process was started as a plugin helper.
pub fn is_helper() -> bool {
    std::env::var_os(ENV_HELPER).is_some()
}

struct Audio {
    block: Arc<Block>,
    thread: JoinHandle<()>,
}

struct Helper {
    registry: PluginRegistry,
    instance: Option<Box<dyn PluginInstance>>,
    audio: Option<Audio>,
    go: Arc<OwnedFd>,
    done: Arc<OwnedFd>,
    /// Parameter values as FaderFrame last heard them.
    sent: HashMap<u32, f64>,
}

/// Serve FaderFrame until it says quit or goes away; returns the exit code.
pub fn run(registry: PluginRegistry) -> i32 {
    let (Some(control), Some(go), Some(done)) = (
        sys::take_fd(CONTROL_FD),
        sys::take_fd(GO_FD),
        sys::take_fd(DONE_FD),
    ) else {
        eprintln!("faderframe plugin helper: started without its descriptors");
        return 2;
    };
    // The audio thread must never block on these.
    let _ = sys::set_nonblocking(go.as_raw_fd());
    let _ = sys::set_nonblocking(done.as_raw_fd());
    let mut control = UnixStream::from(control);
    let mut h = Helper {
        registry,
        instance: None,
        audio: None,
        go: Arc::new(go),
        done: Arc::new(done),
        sent: HashMap::new(),
    };
    // The plugin's timers: id → (period in ms, next due).
    let mut timers: HashMap<u32, (u32, Instant)> = HashMap::new();
    loop {
        let sources = h
            .instance
            .as_ref()
            .map(|i| i.event_sources())
            .unwrap_or_default();
        timers.retain(|id, (period, _)| sources.timers.contains(&(*id, *period)));
        for &(id, period) in &sources.timers {
            timers
                .entry(id)
                .or_insert((period, Instant::now() + period_of(period)));
        }
        let now = Instant::now();
        let timeout = timers
            .values()
            .map(|(_, due)| due.saturating_duration_since(now))
            .min()
            .map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        let mut fds = vec![libc::pollfd {
            fd: control.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        for f in &sources.fds {
            let mut events = 0;
            if f.read {
                events |= libc::POLLIN;
            }
            if f.write {
                events |= libc::POLLOUT;
            }
            fds.push(libc::pollfd {
                fd: f.fd,
                events,
                revents: 0,
            });
        }
        if sys::poll(&mut fds, timeout).is_err() {
            break;
        }
        if fds[0].revents != 0 {
            match wire::recv::<Request>(&mut control) {
                Ok((req, payload)) => {
                    if !h.handle(req, payload, &mut control) {
                        break;
                    }
                }
                // FaderFrame is gone (or broke the protocol).
                Err(_) => break,
            }
        }
        if let Some(inst) = h.instance.as_mut() {
            for (p, f) in fds[1..].iter().zip(&sources.fds) {
                if p.revents == 0 || p.revents & libc::POLLNVAL != 0 {
                    continue;
                }
                inst.on_fd(PluginFd {
                    fd: f.fd,
                    read: p.revents & (libc::POLLIN | libc::POLLPRI) != 0,
                    write: p.revents & libc::POLLOUT != 0,
                    error: p.revents & (libc::POLLERR | libc::POLLHUP) != 0,
                });
            }
            let now = Instant::now();
            for (id, (period, due)) in timers.iter_mut() {
                if *due <= now {
                    inst.on_timer(*id);
                    *due = now + period_of(*period);
                }
            }
        }
    }
    h.stop_audio();
    h.instance = None;
    0
}

fn period_of(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms.max(1)))
}

fn failed(e: impl std::fmt::Display) -> (Response, Vec<u8>) {
    (Response::Failed(e.to_string()), Vec::new())
}

impl Helper {
    /// Answer one request; `false` to quit.
    fn handle(&mut self, req: Request, payload: Vec<u8>, control: &mut UnixStream) -> bool {
        let quit = req == Request::Quit;
        let (resp, out) = self.answer(req, payload);
        wire::send(control, &resp, &out).is_ok() && !quit
    }

    fn answer(&mut self, req: Request, payload: Vec<u8>) -> (Response, Vec<u8>) {
        match req {
            Request::Quit => {
                self.stop_audio();
                (Response::Done, Vec::new())
            }
            Request::Instantiate { format, id } => self.instantiate(format.into(), &id),
            Request::Deactivate => {
                self.stop_audio();
                (Response::Done, Vec::new())
            }
            Request::Activate {
                sample_rate,
                max_block,
                sidechain,
                shm,
                shm_size,
            } => self.activate(
                ProcessConfig {
                    sample_rate,
                    max_block_size: max_block,
                    sidechain,
                },
                &shm,
                shm_size as usize,
            ),
            other => {
                let Some(inst) = self.instance.as_mut() else {
                    return failed("no plugin");
                };
                match other {
                    Request::SetParameter { id, value } => {
                        match inst.set_parameter(faderframe_core::ParameterId(id), value) {
                            Ok(()) => {
                                self.sent.insert(id, value);
                                (Response::Done, Vec::new())
                            }
                            Err(e) => failed(e),
                        }
                    }
                    Request::FormatParameter { id, value } => (
                        Response::Text(
                            inst.format_parameter(faderframe_core::ParameterId(id), value),
                        ),
                        Vec::new(),
                    ),
                    Request::SaveState => match inst.save_state() {
                        Ok(bytes) => (Response::Bytes, bytes),
                        Err(e) => failed(e),
                    },
                    Request::LoadState => match inst.load_state(&payload) {
                        Ok(()) => (Response::Done, Vec::new()),
                        Err(e) => failed(e),
                    },
                    Request::PresetFiles => (Response::Paths(inst.preset_files()), Vec::new()),
                    Request::StateFromPresetFile => match inst.state_from_preset_file(&payload) {
                        Ok(bytes) => (Response::Bytes, bytes),
                        Err(e) => failed(e),
                    },
                    Request::Poll => self.poll(),
                    Request::Editor(call) => editor(inst.as_mut(), call),
                    _ => failed("unexpected request"),
                }
            }
        }
    }

    fn instantiate(&mut self, format: PluginFormat, id: &str) -> (Response, Vec<u8>) {
        let mut inst = match self.registry.instantiate(format, id) {
            Ok(i) => i,
            Err(e) => return failed(e),
        };
        let params: Vec<Param> = inst.parameters().iter().map(Param::from).collect();
        let mut values = Vec::with_capacity(params.len());
        for p in &params {
            if let Some(v) = inst.parameter(faderframe_core::ParameterId(p.id)) {
                values.push((p.id, v));
            }
        }
        self.sent = values.iter().copied().collect();
        let info = Instantiated {
            descriptor: inst.descriptor().into(),
            params,
            values,
            latency: inst.latency_samples(),
            tail: inst.tail().into(),
            note_expressions: inst
                .note_expressions()
                .map(|v| v.into_iter().map(wire::expression_index).collect()),
            has_editor: inst.editor().is_some(),
        };
        self.instance = Some(inst);
        (Response::Instantiated(Box::new(info)), Vec::new())
    }

    fn poll(&mut self) -> (Response, Vec<u8>) {
        let Some(inst) = self.instance.as_mut() else {
            return failed("no plugin");
        };
        let p = inst.poll();
        let edits = inst
            .take_editor_edits()
            .into_iter()
            .map(wire::Edit::from)
            .collect();
        let (editor_open, editor) = match inst.editor() {
            Some(e) if e.is_open() => (true, e.take_requests().into()),
            _ => (false, Default::default()),
        };
        let params: Option<Vec<Param>> = p
            .params_changed
            .then(|| inst.parameters().iter().map(Param::from).collect());
        let ids: Vec<u32> = inst.parameters().iter().map(|p| p.id.0).collect();
        let mut values = Vec::new();
        for id in ids {
            if let Some(v) = inst.parameter(faderframe_core::ParameterId(id))
                && self.sent.get(&id) != Some(&v)
            {
                self.sent.insert(id, v);
                values.push((id, v));
            }
        }
        let polled = Polled {
            restart: p.restart,
            params_changed: p.params_changed,
            state_dirty: p.state_dirty,
            edits,
            editor,
            editor_open,
            values,
            params,
            latency: inst.latency_samples(),
            tail: Some(inst.tail().into()),
        };
        (Response::Polled(Box::new(polled)), Vec::new())
    }

    fn activate(&mut self, config: ProcessConfig, shm: &str, size: usize) -> (Response, Vec<u8>) {
        self.stop_audio();
        let Some(inst) = self.instance.as_mut() else {
            return failed("no plugin");
        };
        let block = match Block::open(shm, size) {
            Ok(b) => Arc::new(b),
            Err(e) => return failed(format!("shared memory: {e}")),
        };
        let processor = match inst.create_processor(&config) {
            Ok(p) => p,
            Err(e) => return failed(e),
        };
        let latency = inst.latency_samples();
        let (b, go, done) = (
            Arc::clone(&block),
            Arc::clone(&self.go),
            Arc::clone(&self.done),
        );
        let thread = std::thread::Builder::new()
            .name("faderframe-plugin-audio".into())
            .spawn(move || audio_loop(processor, b, go, done));
        match thread {
            Ok(thread) => {
                self.audio = Some(Audio { block, thread });
                (Response::Activated { latency }, Vec::new())
            }
            Err(e) => failed(e),
        }
    }

    fn stop_audio(&mut self) {
        if let Some(a) = self.audio.take() {
            a.block.header().quit.store(1, Ordering::Release);
            let _ = a.thread.join();
        }
    }
}

fn editor(inst: &mut dyn PluginInstance, call: EditorCall) -> (Response, Vec<u8>) {
    let Some(ed) = inst.editor() else {
        return failed("the plugin has no editor");
    };
    let resp = match call {
        EditorCall::CanEmbed(api) => Response::Flag(ed.can_embed(api.into())),
        EditorCall::CanFloat(api) => Response::Flag(ed.can_float(api.into())),
        EditorCall::OpenEmbedded { api, scale } => match ed.open_embedded(api.into(), scale) {
            Ok(size) => Response::Size(Some(size)),
            Err(e) => Response::Failed(e.to_string()),
        },
        EditorCall::Attach { api, handle } => match ed.attach(ParentWindow {
            api: api.into(),
            handle,
        }) {
            Ok(()) => Response::Done,
            Err(e) => Response::Failed(e.to_string()),
        },
        EditorCall::OpenFloating { api, title } => match ed.open_floating(api.into(), &title) {
            Ok(()) => Response::Done,
            Err(e) => Response::Failed(e.to_string()),
        },
        EditorCall::Close => {
            ed.close();
            Response::Done
        }
        EditorCall::CanResize => Response::Flag(ed.can_resize()),
        EditorCall::SetSize { width, height } => Response::Size(ed.set_size(width, height)),
    };
    (resp, Vec::new())
}

/// The helper's audio thread: one block per wake-up byte.
fn audio_loop(
    mut processor: Box<dyn PluginProcessor>,
    block: Arc<Block>,
    go: Arc<OwnedFd>,
    done: Arc<OwnedFd>,
) {
    faderframe_realtime::flush_denormals_on_this_thread();
    let h = block.header();
    let mut io = HelperIo::default();
    // A fresh block: FaderFrame's first request is 1.
    let mut last = 0u32;
    let mut sched = 0u64;
    loop {
        if h.quit.load(Ordering::Acquire) != 0 {
            break;
        }
        match sys::wait_readable(go.as_raw_fd(), Some(Duration::from_millis(100))) {
            // FaderFrame is gone.
            Ready::HungUp => break,
            Ready::Readable => {
                sys::drain(go.as_raw_fd());
            }
            Ready::TimedOut => continue,
        }
        if h.quit.load(Ordering::Acquire) != 0 {
            break;
        }
        let seq = h.seq.load(Ordering::Acquire);
        if seq == last {
            continue;
        }
        last = seq;
        let s = h.sched.load(Ordering::Acquire);
        if s != 0 && s != sched {
            faderframe_realtime::apply_thread_scheduling(s, h.sched_extra.load(Ordering::Relaxed));
            sched = s;
        }
        if h.reset.swap(0, Ordering::AcqRel) != 0 {
            processor.reset();
        }
        let frames = block.read_request(&mut io);
        let status = {
            let HelperIo {
                ins,
                outs,
                events_in,
                events_out,
                params,
                transport,
                ..
            } = &mut io;
            let ctx = PluginProcessContext {
                transport,
                param_events: params,
            };
            let mut node = NodeIo {
                frames,
                audio_in: ins,
                audio_out: outs,
                events_in,
                events_out,
            };
            processor.process(&ctx, &mut node)
        };
        block.write_response(frames, status, &mut io);
        h.done.store(seq, Ordering::Release);
        if !sys::signal(done.as_raw_fd()) {
            break;
        }
    }
}
