//! The helper process: hosts one plugin instance with the application's
//! own (in-process) factories, answers FaderFrame's requests, services the
//! plugin GUI's descriptors and timers on its main thread (Unix: a `poll`
//! loop, which on macOS also hands AppKit its events; Windows: a message
//! loop woken by a thread reading the control pipe), and runs the audio
//! blocks on an audio thread of its own (which copies the scheduling of
//! FaderFrame's audio thread). It ends when FaderFrame says so or goes
//! away.

use crate::shm::{Block, HelperIo};
use crate::sys::{self, Connected, Control, Ready, Signal, Waiter};
use crate::wire::{self, EditorCall, Instantiated, Param, Polled, Request, Response};
use faderframe_audio_graph::NodeIo;
use faderframe_plugin_host::{
    ParentWindow, PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor,
    PluginRegistry, ProcessConfig,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// This process was started as a plugin helper.
pub fn is_helper() -> bool {
    std::env::var_os(sys::ENV_HELPER).is_some()
}

struct Audio {
    block: Arc<Block>,
    thread: JoinHandle<()>,
}

struct Helper {
    registry: PluginRegistry,
    instance: Option<Box<dyn PluginInstance>>,
    audio: Option<Audio>,
    go: Arc<Waiter>,
    done: Arc<Signal>,
    /// Parameter values as FaderFrame last heard them.
    sent: HashMap<u32, f64>,
    /// The window of our own the editor sits in (macOS: views do not
    /// embed across processes).
    #[cfg(target_os = "macos")]
    window: Option<crate::mac::EditorWindow>,
}

/// Serve FaderFrame until it says quit or goes away; returns the exit code.
pub fn run(registry: PluginRegistry) -> i32 {
    let Some(Connected { control, go, done }) = sys::connect() else {
        eprintln!("faderframe plugin helper: started without its connections");
        return 2;
    };
    let mut h = Helper {
        registry,
        instance: None,
        audio: None,
        go: Arc::new(go),
        done: Arc::new(done),
        sent: HashMap::new(),
        #[cfg(target_os = "macos")]
        window: None,
    };
    serve(&mut h, control);
    h.stop_audio();
    #[cfg(target_os = "macos")]
    {
        h.window = None;
    }
    h.instance = None;
    0
}

/// The plugin's timers: id → (period in ms, next due).
#[derive(Default)]
struct Timers(HashMap<u32, (u32, Instant)>);

impl Timers {
    fn update(&mut self, timers: &[(u32, u32)]) {
        self.0
            .retain(|id, (period, _)| timers.contains(&(*id, *period)));
        for &(id, period) in timers {
            self.0
                .entry(id)
                .or_insert((period, Instant::now() + period_of(period)));
        }
    }

    /// Until the next one is due (`None`: no timers).
    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        self.0
            .values()
            .map(|(_, due)| due.saturating_duration_since(now))
            .min()
    }

    fn fire(&mut self, inst: &mut dyn PluginInstance) {
        let now = Instant::now();
        for (id, (period, due)) in self.0.iter_mut() {
            if *due <= now {
                inst.on_timer(*id);
                *due = now + period_of(*period);
            }
        }
    }
}

/// Unix: `poll` the control socket and the plugin's descriptors.
#[cfg(unix)]
fn serve(h: &mut Helper, mut control: Control) {
    use faderframe_plugin_host::PluginFd;
    use std::os::fd::AsRawFd;
    let mut timers = Timers::default();
    loop {
        let sources = h
            .instance
            .as_ref()
            .map(|i| i.event_sources())
            .unwrap_or_default();
        timers.update(&sources.timers);
        let timeout = timers.timeout();
        // AppKit and the run loop get their turn at least this often.
        #[cfg(target_os = "macos")]
        let timeout = {
            let cap = Duration::from_millis(if h.window.is_some() { 10 } else { 50 });
            Some(timeout.map_or(cap, |t| t.min(cap)))
        };
        let timeout = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
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
            return;
        }
        if fds[0].revents != 0 {
            match wire::recv::<Request>(&mut control) {
                Ok((req, payload)) => {
                    if !h.handle(req, payload, &mut control) {
                        return;
                    }
                }
                // FaderFrame is gone (or broke the protocol).
                Err(_) => return,
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
            timers.fire(inst.as_mut());
        }
        #[cfg(target_os = "macos")]
        crate::mac::pump(h.window.is_some());
    }
}

/// Windows: a thread reads the requests and wakes the main thread, which
/// waits in `MsgWaitForMultipleObjectsEx` so the editor's window messages
/// keep flowing.
#[cfg(windows)]
fn serve(h: &mut Helper, mut control: Control) {
    sys::ui::prepare();
    let Ok(wake) = sys::event(None) else {
        return;
    };
    let wake = Arc::new(wake);
    let (tx, rx) = std::sync::mpsc::channel::<Option<(Request, Vec<u8>)>>();
    let Ok(mut reader) = control.try_clone() else {
        return;
    };
    let woken = Arc::clone(&wake);
    let spawned = std::thread::Builder::new()
        .name("faderframe-sandbox-control".into())
        .spawn(move || {
            loop {
                let msg = wire::recv::<Request>(&mut reader).ok();
                let end = msg.is_none();
                if tx.send(msg).is_err() {
                    break;
                }
                sys::set_event(&woken);
                if end {
                    break;
                }
            }
        });
    if spawned.is_err() {
        return;
    }
    let mut timers = Timers::default();
    loop {
        let sources = h
            .instance
            .as_ref()
            .map(|i| i.event_sources())
            .unwrap_or_default();
        timers.update(&sources.timers);
        sys::ui::wait(&wake, timers.timeout());
        sys::ui::pump();
        while let Ok(msg) = rx.try_recv() {
            // `None`: FaderFrame is gone (or broke the protocol).
            let Some((req, payload)) = msg else {
                return;
            };
            if !h.handle(req, payload, &mut control) {
                return;
            }
        }
        if let Some(inst) = h.instance.as_mut() {
            timers.fire(inst.as_mut());
        }
    }
}

fn period_of(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms.max(1)))
}

fn failed(e: impl std::fmt::Display) -> (Response, Vec<u8>) {
    (Response::Failed(e.to_string()), Vec::new())
}

impl Helper {
    /// Answer one request; `false` to quit.
    fn handle(&mut self, req: Request, payload: Vec<u8>, control: &mut Control) -> bool {
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
                double_precision,
            } => self.activate(
                ProcessConfig {
                    sample_rate,
                    max_block_size: max_block,
                    sidechain,
                    double_precision,
                },
                &shm,
                shm_size as usize,
            ),
            Request::Editor(call) => self.editor(call),
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
                    Request::SelectProgram { index } => match inst.select_program(index) {
                        Ok(()) => (Response::Done, Vec::new()),
                        Err(e) => failed(e),
                    },
                    Request::StateFromPresetFile => match inst.state_from_preset_file(&payload) {
                        Ok(bytes) => (Response::Bytes, bytes),
                        Err(e) => failed(e),
                    },
                    Request::Poll => self.poll(),
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
            programs: inst.programs(),
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
            Some(e) if e.is_open() => {
                #[allow(unused_mut)]
                let mut r = e.take_requests();
                #[cfg(target_os = "macos")]
                if let Some(w) = self.window.as_mut() {
                    own_window_requests(e, w, &mut r);
                    if r.closed {
                        self.window = None;
                    }
                }
                (true, r.into())
            }
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
            program: inst.current_program(),
            programs: p.params_changed.then(|| inst.programs()),
            pending: inst.changes_pending(),
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

impl Helper {
    fn editor(&mut self, call: EditorCall) -> (Response, Vec<u8>) {
        let Some(inst) = self.instance.as_mut() else {
            return failed("no plugin");
        };
        let Some(ed) = inst.editor() else {
            return failed("the plugin has no editor");
        };
        // A view cannot go into FaderFrame's windows: the editor floats,
        // in a window of ours unless the plugin brings its own.
        #[cfg(target_os = "macos")]
        match call {
            EditorCall::CanEmbed(_) => return (Response::Flag(false), Vec::new()),
            EditorCall::CanFloat(api) => {
                let api = api.into();
                return (
                    Response::Flag(ed.can_embed(api) || ed.can_float(api)),
                    Vec::new(),
                );
            }
            EditorCall::OpenFloating { api, ref title } if ed.can_embed(api.into()) => {
                let (resp, window) = own_window(ed, api.into(), title);
                self.window = window;
                return (resp, Vec::new());
            }
            EditorCall::Close => {
                // Already closed when the user closed our window.
                if ed.is_open() {
                    ed.close();
                }
                self.window = None;
                return (Response::Done, Vec::new());
            }
            EditorCall::Raise => {
                if let Some(w) = self.window.as_mut() {
                    w.show();
                }
                return (Response::Done, Vec::new());
            }
            _ => {}
        }
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
            EditorCall::Raise => Response::Done,
            EditorCall::CanResize => Response::Flag(ed.can_resize()),
            EditorCall::SetSize { width, height } => Response::Size(ed.set_size(width, height)),
        };
        (resp, Vec::new())
    }
}

/// Open the editor embedded into a window of the helper's own.
#[cfg(target_os = "macos")]
fn own_window(
    ed: &mut dyn faderframe_plugin_host::PluginEditor,
    api: faderframe_plugin_host::WindowApi,
    title: &str,
) -> (Response, Option<crate::mac::EditorWindow>) {
    let size = match ed.open_embedded(api, 1.0) {
        Ok(size) => size,
        Err(e) => return (Response::Failed(e.to_string()), None),
    };
    let resizable = ed.can_resize();
    let Some(mut w) = crate::mac::EditorWindow::new(title, size, resizable) else {
        ed.close();
        return (Response::Failed("no window for the editor".into()), None);
    };
    if let Err(e) = ed.attach(ParentWindow {
        api,
        handle: w.view(),
    }) {
        ed.close();
        return (Response::Failed(e.to_string()), None);
    }
    w.show();
    (Response::Done, Some(w))
}

/// What the editor asked of its window, and what the user did to it:
/// handled here (FaderFrame only hears that it was closed).
#[cfg(target_os = "macos")]
fn own_window_requests(
    ed: &mut dyn faderframe_plugin_host::PluginEditor,
    w: &mut crate::mac::EditorWindow,
    r: &mut faderframe_plugin_host::EditorRequests,
) {
    if let Some(size) = r.resize.take() {
        w.resize(size);
    }
    if std::mem::take(&mut r.show) {
        w.show();
    }
    if std::mem::take(&mut r.hide) {
        w.hide();
    }
    if let Some(size) = w.dragged()
        && let Some(applied) = ed.set_size(size.0, size.1)
        && applied != size
    {
        w.resize(applied);
    }
    if w.closed() {
        ed.close();
        r.closed = true;
    }
}

/// The helper's audio thread: one block per wake-up byte.
fn audio_loop(
    mut processor: Box<dyn PluginProcessor>,
    block: Arc<Block>,
    go: Arc<Waiter>,
    done: Arc<Signal>,
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
        match go.wait(Some(Duration::from_millis(100))) {
            // FaderFrame is gone.
            Ready::HungUp => break,
            Ready::Woken => {}
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
                harmony: &faderframe_plugin_host::NO_HARMONY,
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
        if !done.signal() {
            break;
        }
    }
}
