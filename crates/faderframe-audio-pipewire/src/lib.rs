//! Native PipeWire backend.
//!
//! FaderFrame appears in the PipeWire graph as one DSP node (a `pw_filter`)
//! with a mono float port per channel, like a JACK client but without the
//! pipewire-jack layer. Inputs and outputs are processed in the same cycle
//! on PipeWire's realtime data thread (`PW_FILTER_FLAG_RT_PROCESS`), which
//! keeps recording sample-aligned with playback.
//!
//! A control thread runs a PipeWire main loop and owns every PipeWire
//! object; the returned stream only holds that thread and a channel to stop
//! it. The graph owns the sample rate and quantum: the node asks for the
//! requested ones (`node.rate`, `node.latency`), and the realtime callback
//! re-prepares the engine whenever the cycle size or rate changes. With
//! auto-connect, the ports are linked to the highest-priority sink and
//! source once the node and its ports appear in the registry.
//!
//! No PipeWire type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![cfg(target_os = "linux")]

use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, DeviceBuffers, DeviceInfo,
    MAX_BUFFER_SIZE, StreamConfig, StreamInfo, StreamMonitor, StreamStatus,
};
use pipewire as pw;
use pw::properties::properties;
use pw::spa::sys as spa_sys;
use pw::sys as pw_sys;
use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DSP_FORMAT: &str = "32 bit float mono audio";
/// How long opening a stream may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the first cycle (to report the graph's real rate).
const FIRST_CYCLE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct PipeWireBackend;

/// Is a PipeWire server socket there?
fn server_running() -> bool {
    let name = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".into());
    if name.starts_with('/') {
        return Path::new(&name).exists();
    }
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| Path::new(&d).join(&name).exists())
}

fn unavailable(e: impl std::fmt::Display) -> AudioError {
    AudioError::BackendUnavailable(format!("cannot connect to PipeWire ({e})"))
}

// --- the graph as seen through the registry ----------------------------------------

#[derive(Clone, Debug, Default)]
struct NodeInfo {
    class: String,
    name: String,
    priority: i64,
}

#[derive(Clone, Debug)]
struct PortInfo {
    node: u32,
    /// `port.direction == "in"`.
    input: bool,
    /// Position on its node.
    index: u32,
}

#[derive(Debug, Default)]
struct Graph {
    nodes: HashMap<u32, NodeInfo>,
    ports: HashMap<u32, PortInfo>,
}

impl Graph {
    fn record(&mut self, global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>) {
        let Some(props) = global.props else { return };
        match global.type_ {
            pw::types::ObjectType::Node => {
                self.nodes.insert(
                    global.id,
                    NodeInfo {
                        class: props.get("media.class").unwrap_or_default().to_string(),
                        name: props.get("node.name").unwrap_or_default().to_string(),
                        priority: props
                            .get("priority.session")
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(0),
                    },
                );
            }
            pw::types::ObjectType::Port => {
                let Some(node) = props.get("node.id").and_then(|n| n.parse().ok()) else {
                    return;
                };
                self.ports.insert(
                    global.id,
                    PortInfo {
                        node,
                        input: props.get("port.direction") == Some("in"),
                        index: props
                            .get("port.id")
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(global.id),
                    },
                );
            }
            _ => {}
        }
    }

    fn remove(&mut self, id: u32) {
        self.nodes.remove(&id);
        self.ports.remove(&id);
    }

    /// The highest-priority node of a media class.
    fn best(&self, class: &str) -> Option<u32> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.class == class)
            .max_by_key(|(id, n)| (n.priority, std::cmp::Reverse(**id)))
            .map(|(id, _)| *id)
    }

    /// A node's ports of one direction, in port order.
    fn ports_of(&self, node: u32, input: bool) -> Vec<u32> {
        let mut ports: Vec<(u32, u32)> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == node && p.input == input)
            .map(|(id, p)| (p.index, *id))
            .collect();
        ports.sort_unstable();
        ports.into_iter().map(|(_, id)| id).collect()
    }
}

/// Connect, list the graph once and disconnect.
fn scan_graph() -> Result<Graph, AudioError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unavailable)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(unavailable)?;
    let core = context.connect_rc(None).map_err(unavailable)?;
    let registry = core.get_registry_rc().map_err(unavailable)?;
    let graph = Rc::new(RefCell::new(Graph::default()));
    let g = Rc::clone(&graph);
    let _listener = registry
        .add_listener_local()
        .global(move |global| g.borrow_mut().record(global))
        .register();
    roundtrip(&mainloop, &core)?;
    let graph = graph.take();
    Ok(graph)
}

/// Process every pending event (a core sync and its `done`).
fn roundtrip(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
) -> Result<(), AudioError> {
    let done = Rc::new(std::cell::Cell::new(false));
    let pending = core.sync(0).map_err(unavailable)?;
    let (d, ml) = (Rc::clone(&done), mainloop.clone());
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                d.set(true);
                ml.quit();
            }
        })
        .register();
    // A server that never answers must not hang the caller.
    let ml = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| ml.quit());
    timer
        .update_timer(Some(OPEN_TIMEOUT), None)
        .into_result()
        .map_err(unavailable)?;
    while !done.get() {
        mainloop.run();
        if !done.get() {
            return Err(unavailable("no answer from the server"));
        }
    }
    Ok(())
}

impl AudioBackend for PipeWireBackend {
    fn id(&self) -> &'static str {
        "pipewire"
    }

    fn display_name(&self) -> &'static str {
        "PipeWire (native)"
    }

    fn is_available(&self) -> bool {
        server_running()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        if !server_running() {
            return Err(unavailable("no server socket"));
        }
        let g = scan_graph()?;
        let count = |class: &str, input: bool| {
            g.best(class)
                .map_or(0, |n| g.ports_of(n, input).len() as u16)
        };
        let name = |class: &str| g.best(class).map(|n| g.nodes[&n].name.clone());
        Ok(vec![DeviceInfo {
            id: "pipewire".into(),
            name: match (name("Audio/Sink"), name("Audio/Source")) {
                (Some(o), Some(i)) => format!("PipeWire graph ({o} / {i})"),
                (Some(o), None) => format!("PipeWire graph ({o})"),
                _ => "PipeWire graph".into(),
            },
            input_channels: count("Audio/Source", false),
            output_channels: count("Audio/Sink", true),
            sample_rates: Vec::new(),
            current_sample_rate: None,
            current_buffer_size: None,
            is_default: true,
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        if !server_running() {
            return Err(unavailable("no server socket"));
        }
        let rate = config.sample_rate.unwrap_or(48_000);
        let frames = config.buffer_size.unwrap_or(256);
        faderframe_audio::validate_format(rate, frames)?;
        let info = StreamInfo {
            backend: "pipewire",
            device: config.client_name.clone(),
            sample_rate: rate,
            buffer_size: frames,
            input_channels: config.input_channels,
            output_channels: config.output_channels,
            input_latency: 0,
            output_latency: 0,
        };
        let monitor = StreamMonitor::new(rate, frames);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        let thread = {
            let (info, monitor) = (info.clone(), Arc::clone(&monitor));
            std::thread::Builder::new()
                .name("pipewire-control".into())
                .spawn(move || {
                    if let Err(e) = run(config, info, callback, monitor, quit_rx, &ready_tx) {
                        let _ = ready_tx.send(Err(e));
                    }
                })
                .map_err(|e| AudioError::Stream(format!("control thread: {e}")))?
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = quit_tx.send(());
                return Err(unavailable("timed out connecting"));
            }
        }
        // The first cycle tells the graph's real rate and quantum.
        let start = Instant::now();
        while monitor.status().callbacks == 0 && start.elapsed() < FIRST_CYCLE_TIMEOUT {
            std::thread::sleep(Duration::from_millis(5));
        }
        let stream = PipeWireStream {
            thread: Some(thread),
            quit: quit_tx,
            info,
            monitor,
        };
        tracing::info!("PipeWire stream open: {}", stream.info());
        Ok(Box::new(stream))
    }
}

// --- realtime side -------------------------------------------------------------------

/// State of the realtime callback. Only the data thread touches it (the
/// process event); the control thread owns the box and frees it after the
/// filter is destroyed.
struct RtState {
    callback: Box<dyn AudioCallback>,
    /// Port data of the filter's input and output ports.
    inputs: Vec<*mut c_void>,
    outputs: Vec<*mut c_void>,
    /// Buffers of the current cycle.
    in_bufs: Vec<*const f32>,
    out_bufs: Vec<*mut f32>,
    /// Stands in for unconnected ports.
    silence: Vec<f32>,
    sink: Vec<Vec<f32>>,
    info: StreamInfo,
    xruns: u64,
}

/// What the filter's events get as their data pointer.
struct Shared {
    rt: UnsafeCell<RtState>,
    monitor: Arc<StreamMonitor>,
}

struct PwIo<'a> {
    frames: usize,
    ins: &'a [*const f32],
    outs: &'a [*mut f32],
}

impl DeviceBuffers for PwIo<'_> {
    fn frames(&self) -> usize {
        self.frames
    }

    fn input_channels(&self) -> usize {
        self.ins.len()
    }

    fn output_channels(&self) -> usize {
        self.outs.len()
    }

    fn input(&self, channel: usize) -> &[f32] {
        // SAFETY: every pointer is a PipeWire DSP buffer or our silence
        // buffer, valid for `frames` samples for the rest of the cycle.
        unsafe { std::slice::from_raw_parts(self.ins[channel], self.frames) }
    }

    fn output(&mut self, channel: usize) -> &mut [f32] {
        // SAFETY: as above, and each output port has its own buffer, so the
        // slices never alias (the `&mut self` borrow prevents two at once).
        unsafe { std::slice::from_raw_parts_mut(self.outs[channel], self.frames) }
    }
}

unsafe extern "C" fn on_process(data: *mut c_void, position: *mut spa_sys::spa_io_position) {
    // SAFETY: `data` is the `Shared` the control thread keeps alive until
    // the filter is destroyed, and with RT_PROCESS this event runs only on
    // the data thread, so the realtime state is never aliased.
    let shared = unsafe { &*(data as *const Shared) };
    let rt = unsafe { &mut *shared.rt.get() };
    // SAFETY: PipeWire passes the driver's position for this cycle.
    let Some(pos) = (unsafe { position.as_ref() }) else {
        return;
    };
    let n = pos.clock.duration as usize;
    if n == 0 || n > MAX_BUFFER_SIZE as usize {
        return;
    }
    let rate = pos.clock.rate.denom;
    if (rate > 0 && rate != rt.info.sample_rate) || n as u32 != rt.info.buffer_size {
        if rate > 0 {
            rt.info.sample_rate = rate;
            shared.monitor.set_sample_rate(rate);
        }
        rt.info.buffer_size = n as u32;
        shared.monitor.set_buffer_size(n as u32);
        rt.callback.prepare(&rt.info);
    }
    if pos.clock.xrun > rt.xruns {
        if rt.xruns > 0 {
            for _ in rt.xruns..pos.clock.xrun {
                shared.monitor.record_xrun();
            }
        }
        rt.xruns = pos.clock.xrun;
    }
    let RtState {
        callback,
        inputs,
        outputs,
        in_bufs,
        out_bufs,
        silence,
        sink,
        ..
    } = rt;
    for (port, buf) in inputs.iter().zip(in_bufs.iter_mut()) {
        // SAFETY: RT-safe call on our own port data.
        let p = unsafe { pw_sys::pw_filter_get_dsp_buffer(*port, n as u32) } as *const f32;
        *buf = if p.is_null() { silence.as_ptr() } else { p };
    }
    for ((port, buf), spare) in outputs.iter().zip(out_bufs.iter_mut()).zip(sink.iter_mut()) {
        // SAFETY: as above.
        let p = unsafe { pw_sys::pw_filter_get_dsp_buffer(*port, n as u32) } as *mut f32;
        *buf = if p.is_null() { spare.as_mut_ptr() } else { p };
    }
    let mut io = PwIo {
        frames: n,
        ins: in_bufs,
        outs: out_bufs,
    };
    callback.process(&mut io);
    shared.monitor.record_callback();
}

unsafe extern "C" fn on_state_changed(
    data: *mut c_void,
    _old: pw_sys::pw_filter_state,
    state: pw_sys::pw_filter_state,
    error: *const c_char,
) {
    // SAFETY: see `on_process`; this event runs on the control thread and
    // touches only the monitor's atomics.
    let shared = unsafe { &*(data as *const Shared) };
    match state {
        pw_sys::pw_filter_state_PW_FILTER_STATE_STREAMING => shared.monitor.set_running(true),
        pw_sys::pw_filter_state_PW_FILTER_STATE_ERROR => {
            let msg = if error.is_null() {
                String::new()
            } else {
                // SAFETY: PipeWire passes a NUL-terminated message.
                unsafe { std::ffi::CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            };
            tracing::warn!("PipeWire stream error: {msg}");
            shared.monitor.mark_shut_down();
        }
        _ => {}
    }
}

static EVENTS: pw_sys::pw_filter_events = pw_sys::pw_filter_events {
    version: pw_sys::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
};

// --- control thread -------------------------------------------------------------------

/// Owns the filter, its listener hook and the realtime state; tears them
/// down in the right order.
struct Filter {
    raw: *mut pw_sys::pw_filter,
    _hook: Box<spa_sys::spa_hook>,
    _shared: Box<Shared>,
}

impl Drop for Filter {
    fn drop(&mut self) {
        // SAFETY: destroying disconnects the node, so no event runs after
        // this returns and the shared state can be freed with `self`.
        unsafe { pw_sys::pw_filter_destroy(self.raw) };
    }
}

fn add_port(
    filter: *mut pw_sys::pw_filter,
    input: bool,
    name: &str,
) -> Result<*mut c_void, AudioError> {
    let props = properties! {
        "format.dsp" => DSP_FORMAT,
        "port.name" => name,
    };
    let direction = if input {
        spa_sys::SPA_DIRECTION_INPUT
    } else {
        spa_sys::SPA_DIRECTION_OUTPUT
    };
    // SAFETY: a live filter; the properties' ownership passes to PipeWire.
    let port = unsafe {
        pw_sys::pw_filter_add_port(
            filter,
            direction,
            pw_sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
            0,
            props.into_raw(),
            std::ptr::null_mut(),
            0,
        )
    };
    if port.is_null() {
        return Err(AudioError::Stream(format!("cannot add port {name}")));
    }
    Ok(port)
}

fn run(
    config: StreamConfig,
    info: StreamInfo,
    callback: Box<dyn AudioCallback>,
    monitor: Arc<StreamMonitor>,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), AudioError>>,
) -> Result<(), AudioError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unavailable)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(unavailable)?;
    let core = context.connect_rc(None).map_err(unavailable)?;
    let registry = core.get_registry_rc().map_err(unavailable)?;

    let mut props = properties! {
        "media.type" => "Audio",
        "media.category" => "Duplex",
        "media.role" => "DSP",
        "node.name" => config.client_name.as_str(),
        "node.description" => config.client_name.as_str(),
        // Scheduled even while nothing is linked.
        "node.always-process" => "true",
    };
    if let Some(frames) = config.buffer_size {
        props.insert("node.latency", format!("{frames}/{}", info.sample_rate));
    }
    if let Some(rate) = config.sample_rate {
        props.insert("node.rate", format!("1/{rate}"));
    }
    let name = CString::new(config.client_name.as_str())
        .map_err(|_| AudioError::Stream("client name contains NUL".into()))?;
    // SAFETY: a connected core; the properties' ownership passes on.
    let raw = unsafe { pw_sys::pw_filter_new(core.as_raw_ptr(), name.as_ptr(), props.into_raw()) };
    if raw.is_null() {
        return Err(AudioError::Stream("cannot create the PipeWire node".into()));
    }
    // Owns `raw` from here on (destroyed on every exit path).
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let ports = (|| {
        for i in 0..config.input_channels {
            inputs.push(add_port(raw, true, &format!("in_{}", i + 1))?);
        }
        for i in 0..config.output_channels {
            outputs.push(add_port(raw, false, &format!("out_{}", i + 1))?);
        }
        Ok::<(), AudioError>(())
    })();
    let max = MAX_BUFFER_SIZE as usize;
    let mut callback = callback;
    callback.prepare(&info);
    let shared = Box::new(Shared {
        rt: UnsafeCell::new(RtState {
            callback,
            in_bufs: vec![std::ptr::null(); inputs.len()],
            out_bufs: vec![std::ptr::null_mut(); outputs.len()],
            sink: vec![vec![0.0; max]; outputs.len()],
            inputs,
            outputs,
            silence: vec![0.0; max],
            info,
            xruns: 0,
        }),
        monitor: Arc::clone(&monitor),
    });
    // SAFETY: an all-zero hook is the initial state PipeWire expects.
    let mut hook: Box<spa_sys::spa_hook> = Box::new(unsafe { std::mem::zeroed() });
    // SAFETY: the events table is static, and the hook and the shared
    // state live in `Filter`, which destroys the filter before freeing them.
    unsafe {
        pw_sys::pw_filter_add_listener(
            raw,
            hook.as_mut(),
            &EVENTS,
            (shared.as_ref() as *const Shared).cast_mut().cast(),
        );
    }
    let filter = Filter {
        raw,
        _hook: hook,
        _shared: shared,
    };
    ports?;
    // SAFETY: a live filter with its ports.
    let res = unsafe {
        pw_sys::pw_filter_connect(
            filter.raw,
            pw_sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
            std::ptr::null_mut(),
            0,
        )
    };
    if res < 0 {
        return Err(AudioError::Stream(format!(
            "cannot connect the PipeWire node (error {res})"
        )));
    }

    // Auto-connect: link to the best sink and source once our ports exist.
    let graph = Rc::new(RefCell::new(Graph::default()));
    let links: Rc<RefCell<Vec<pw::link::Link>>> = Rc::default();
    let linked = Rc::new(std::cell::Cell::new(!config.auto_connect));
    let try_link = {
        let (graph, links, linked, core) = (
            Rc::clone(&graph),
            Rc::clone(&links),
            Rc::clone(&linked),
            core.clone(),
        );
        let (n_in, n_out) = (
            config.input_channels as usize,
            config.output_channels as usize,
        );
        let raw = filter.raw;
        move || {
            if linked.get() {
                return;
            }
            // SAFETY: the filter outlives the main loop run (it is dropped
            // after `run` returns).
            let ours = unsafe { pw_sys::pw_filter_get_node_id(raw) };
            let g = graph.borrow();
            let (our_in, our_out) = (g.ports_of(ours, true), g.ports_of(ours, false));
            if our_in.len() < n_in || our_out.len() < n_out {
                return;
            }
            let mut pairs = Vec::new();
            if let Some(sink) = g.best("Audio/Sink") {
                pairs.extend(
                    our_out
                        .iter()
                        .zip(g.ports_of(sink, true))
                        .map(|(o, i)| (*o, i)),
                );
            }
            if let Some(source) = g.best("Audio/Source") {
                pairs.extend(
                    g.ports_of(source, false)
                        .into_iter()
                        .zip(our_in.iter().copied()),
                );
            }
            for (out_port, in_port) in pairs {
                let (Some(o), Some(i)) = (g.ports.get(&out_port), g.ports.get(&in_port)) else {
                    continue;
                };
                let props = properties! {
                    "link.output.node" => o.node.to_string(),
                    "link.output.port" => out_port.to_string(),
                    "link.input.node" => i.node.to_string(),
                    "link.input.port" => in_port.to_string(),
                };
                match core.create_object::<pw::link::Link>("link-factory", &props) {
                    Ok(l) => links.borrow_mut().push(l),
                    Err(e) => tracing::warn!("PipeWire: cannot link {out_port} → {in_port}: {e}"),
                }
            }
            linked.set(true);
        }
    };
    let try_link = Rc::new(try_link);
    // The server's default cycle (`clock.quantum` in the settings
    // metadata), for a node with no buffer size of its own to ask for.
    let quantum = Rc::new(std::cell::Cell::new(0u32));
    let settings: Rc<RefCell<Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>>> =
        Rc::default();
    let _registry_listener = {
        let (graph, g2, try_link) = (Rc::clone(&graph), Rc::clone(&graph), Rc::clone(&try_link));
        let (registry2, settings, quantum) =
            (registry.clone(), Rc::clone(&settings), Rc::clone(&quantum));
        registry
            .add_listener_local()
            .global(move |global| {
                graph.borrow_mut().record(global);
                try_link();
                let is_settings = global.type_ == pw::types::ObjectType::Metadata
                    && global
                        .props
                        .is_some_and(|p| p.get("metadata.name") == Some("settings"));
                if is_settings
                    && settings.borrow().is_none()
                    && let Ok(meta) = registry2.bind::<pw::metadata::Metadata, _>(global)
                {
                    let q = Rc::clone(&quantum);
                    let listener = meta
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            if subject == 0 && key == Some("clock.quantum") {
                                q.set(value.and_then(|v| v.parse().ok()).unwrap_or(0));
                            }
                            0
                        })
                        .register();
                    *settings.borrow_mut() = Some((meta, listener));
                }
            })
            .global_remove(move |id| g2.borrow_mut().remove(id))
            .register()
    };
    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| ml.quit());
    // With no buffer size asked for, the node takes the graph's cycle -- and
    // then asks for exactly that one. A node that asks for nothing lets any
    // client with a longer latency (a browser, a notification through
    // pipewire-pulse) drag the whole graph up, and every such change
    // re-prepares the engine and rebuilds whatever buffers by the device's
    // callbacks (the preamps, the Guitar Station) mid-song.
    let pin = (config.buffer_size.is_none()).then(|| {
        let (monitor, raw, quantum) = (Arc::clone(&monitor), filter.raw, Rc::clone(&quantum));
        let pinned = Rc::new(std::cell::Cell::new(false));
        let done = Rc::clone(&pinned);
        let timer = mainloop.loop_().add_timer(move |_| {
            if done.get() {
                return;
            }
            let status = monitor.status();
            if status.callbacks == 0 || status.buffer_size == 0 || status.sample_rate == 0 {
                return;
            }
            // The configured default, not whatever the graph runs at now (a
            // browser playing when the stream opened); the cycle it found
            // where the server says nothing.
            let frames = match quantum.get() {
                0 => status.buffer_size,
                q => q,
            };
            let props = properties! {
                "node.latency" => format!("{frames}/{}", status.sample_rate),
            };
            let props = props.into_raw();
            // SAFETY: the filter outlives the main loop run, this runs on
            // the loop's thread, and the properties are freed after the
            // call that copies them.
            unsafe {
                pw_sys::pw_filter_update_properties(raw, std::ptr::null_mut(), &(*props).dict);
                pw_sys::pw_properties_free(props);
            }
            tracing::info!("PipeWire: asking the graph for {frames} frames");
            done.set(true);
        });
        timer.update_timer(
            Some(Duration::from_millis(100)),
            Some(Duration::from_millis(250)),
        );
        (timer, pinned)
    });
    let _ = ready.send(Ok(()));
    mainloop.run();
    drop(pin);
    settings.borrow_mut().take();

    // Links first, then the node, then the connection.
    links.borrow_mut().clear();
    drop(filter);
    monitor.set_running(false);
    Ok(())
}

struct PipeWireStream {
    thread: Option<JoinHandle<()>>,
    quit: pw::channel::Sender<()>,
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
}

impl AudioStream for PipeWireStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.sample_rate = status.sample_rate;
        info.buffer_size = status.buffer_size;
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
