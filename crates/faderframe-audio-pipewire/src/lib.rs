//! Native PipeWire backend.
//!
//! FaderFrame appears in the PipeWire graph as one DSP node (a `pw_filter`)
//! with a mono float port per channel, like a JACK client but without the
//! pipewire-jack layer. Inputs and outputs are processed in the same cycle
//! on PipeWire's realtime data thread (`PW_FILTER_FLAG_RT_PROCESS`), which
//! keeps recording sample-aligned with playback.
//!
//! A control thread runs a PipeWire main loop and owns every PipeWire
//! object; the returned stream only holds that thread and a channel to it.
//! The graph owns the sample rate and quantum: the node asks for the
//! requested ones (`node.rate`, `node.latency`; or holds the graph at them
//! with `node.force-rate`/`node.force-quantum`), the realtime callback
//! re-prepares the engine whenever the cycle size or rate changes, and a
//! running stream can ask for another buffer size or rate without
//! reopening.
//!
//! Devices are the graph's sink and source nodes, by `node.name`. With
//! auto-connect the node's ports are linked to the devices asked for —
//! several per direction, their channels one after another — or to the
//! server's default sink and source (following the default when it
//! changes). Devices that go away keep their channels' places; when none
//! of a direction's devices is there, the default stands in until one
//! returns, and the links come back by themselves: a device that is
//! unplugged and plugged in again needs no restart. Devices with clocks of
//! their own are kept in step by PipeWire (the devices that follow the
//! graph's driver resample adaptively), so one stream can play on and
//! record from several interfaces.
//!
//! No PipeWire type escapes this crate: the engine only sees
//! [`faderframe_audio::DeviceBuffers`].

#![cfg(target_os = "linux")]

mod graph;

use faderframe_audio::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    MAX_BUFFER_SIZE, RateControl, StreamConfig, StreamInfo, StreamMonitor, StreamRouting,
    StreamStatus,
};
use graph::{Defaults, Graph, NodeInfo, PortInfo, Settings, Wanted, json_name, plan};
use pipewire as pw;
use pw::properties::properties;
use pw::spa::sys as spa_sys;
use pw::sys as pw_sys;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DSP_FORMAT: &str = "32 bit float mono audio";
/// How long opening a stream may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the first cycle (to report the graph's real rate).
const FIRST_CYCLE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the control thread brings the links in line with the graph
/// (after nodes and ports came or went, settled for this long).
const RELINK_EVERY: Duration = Duration::from_millis(100);

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

fn record(graph: &mut Graph, global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>) {
    let Some(props) = global.props else { return };
    let number = |key: &str| props.get(key).and_then(|n| n.parse::<u32>().ok());
    match global.type_ {
        pw::types::ObjectType::Node => {
            graph.nodes.insert(
                global.id,
                NodeInfo {
                    class: props.get("media.class").unwrap_or_default().to_string(),
                    name: props.get("node.name").unwrap_or_default().to_string(),
                    description: props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .unwrap_or_default()
                        .to_string(),
                    priority: props
                        .get("priority.session")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(0),
                    clock: props
                        .get("clock.name")
                        .map(str::to_string)
                        .or_else(|| props.get("device.id").map(|d| format!("device {d}"))),
                },
            );
        }
        pw::types::ObjectType::Port => {
            let Some(node) = number("node.id") else {
                return;
            };
            graph.ports.insert(
                global.id,
                PortInfo {
                    node,
                    input: props.get("port.direction") == Some("in"),
                    index: number("port.id").unwrap_or(global.id),
                    channel: props
                        .get("audio.channel")
                        .or_else(|| props.get("port.name"))
                        .unwrap_or_default()
                        .to_string(),
                    monitor: props.get("port.monitor") == Some("true"),
                },
            );
        }
        pw::types::ObjectType::Link => {
            if let (Some(o), Some(i)) = (number("link.output.port"), number("link.input.port")) {
                graph.links.insert(global.id, (o, i));
            }
        }
        _ => {}
    }
}

/// The metadata object `global` is, if it is `default` or `settings`.
fn metadata_name(
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) -> Option<&'static str> {
    if global.type_ != pw::types::ObjectType::Metadata {
        return None;
    }
    match global.props.and_then(|p| p.get("metadata.name")) {
        Some("default") => Some("default"),
        Some("settings") => Some("settings"),
        _ => None,
    }
}

/// The server's defaults and clock settings, as their metadata say.
#[derive(Debug, Default)]
struct Meta {
    defaults: Defaults,
    settings: Settings,
}

impl Meta {
    /// One metadata property of `default` or `settings`; whether a default
    /// device changed.
    fn set(&mut self, which: &str, key: &str, value: Option<&str>) -> bool {
        match which {
            "default" => {
                let name = value.and_then(json_name);
                let slot = match key {
                    "default.audio.sink" => &mut self.defaults.sink,
                    "default.audio.source" => &mut self.defaults.source,
                    _ => return false,
                };
                let changed = *slot != name;
                *slot = name;
                changed
            }
            _ => {
                self.settings.set(key, value);
                false
            }
        }
    }
}

type Bound = Vec<(pw::metadata::Metadata, pw::metadata::MetadataListener)>;

/// Bind `default`/`settings` metadata as it appears, feeding `meta`;
/// `changed` runs when a default device changes.
fn bind_metadata(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    bound: &Rc<RefCell<Bound>>,
    meta: &Rc<RefCell<Meta>>,
    changed: Rc<dyn Fn()>,
) {
    let Some(which) = metadata_name(global) else {
        return;
    };
    let Ok(md) = registry.bind::<pw::metadata::Metadata, _>(global) else {
        return;
    };
    let meta = Rc::clone(meta);
    let listener = md
        .add_listener_local()
        .property(move |subject, key, _, value| {
            if subject == 0
                && let Some(key) = key
                && meta.borrow_mut().set(which, key, value)
            {
                changed();
            }
            0
        })
        .register();
    bound.borrow_mut().push((md, listener));
}

/// Connect, list the graph and its metadata once and disconnect.
fn scan_graph() -> Result<(Graph, Meta), AudioError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unavailable)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(unavailable)?;
    let core = context.connect_rc(None).map_err(unavailable)?;
    let registry = core.get_registry_rc().map_err(unavailable)?;
    let graph = Rc::new(RefCell::new(Graph::default()));
    let meta = Rc::new(RefCell::new(Meta::default()));
    let bound: Rc<RefCell<Bound>> = Rc::default();
    let _listener = {
        let (g, registry2, meta, bound) = (
            Rc::clone(&graph),
            registry.clone(),
            Rc::clone(&meta),
            Rc::clone(&bound),
        );
        registry
            .add_listener_local()
            .global(move |global| {
                record(&mut g.borrow_mut(), global);
                bind_metadata(&registry2, global, &bound, &meta, Rc::new(|| {}));
            })
            .register()
    };
    // The globals, then the bound metadata's properties.
    roundtrip(&mainloop, &core)?;
    roundtrip(&mainloop, &core)?;
    bound.borrow_mut().clear();
    let graph = graph.take();
    let meta = meta.take();
    Ok((graph, meta))
}

/// Process every pending event (a core sync and its `done`).
fn roundtrip(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
) -> Result<(), AudioError> {
    let done = Rc::new(Cell::new(false));
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

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Requested,
            live_buffer_size: true,
            live_sample_rate: true,
            output_devices: 4,
            input_devices: 4,
            clocked_by_server: true,
            follows_devices: true,
            free_channel_counts: true,
            force_format: true,
        }
    }

    fn is_available(&self) -> bool {
        server_running()
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        if !server_running() {
            return Err(unavailable("no server socket"));
        }
        let (g, meta) = scan_graph()?;
        Ok(g.devices(&meta.defaults, &meta.settings))
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
        let latency = Arc::new(PortLatency::default());
        let routing = Arc::new(Mutex::new(StreamRouting::default()));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (control_tx, control_rx) = pw::channel::channel::<Control>();
        let thread = {
            let shared = Parts {
                info: info.clone(),
                monitor: Arc::clone(&monitor),
                latency: Arc::clone(&latency),
                routing: Arc::clone(&routing),
            };
            std::thread::Builder::new()
                .name("pipewire-control".into())
                .spawn(move || {
                    if let Err(e) = run(config, shared, callback, control_rx, &ready_tx) {
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
                let _ = control_tx.send(Control::Quit);
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
            control: control_tx,
            info,
            monitor,
            latency,
            routing,
        };
        tracing::info!("PipeWire stream open: {}", stream.info());
        Ok(Box::new(stream))
    }
}

// --- latency as the graph reports it ----------------------------------------------------

/// The latency the graph reports on the node's ports (SPA_PARAM_Latency):
/// for the outputs, from them to the devices; for the inputs, from the
/// devices to them. In quanta (×1000), frames and nanoseconds; written by
/// the control thread, read by the data thread and the control side.
#[derive(Debug)]
struct PortLatency {
    /// [inputs, outputs]; `u32::MAX` quanta: not reported yet.
    quanta_milli: [AtomicU32; 2],
    frames: [AtomicU32; 2],
    ns: [AtomicU64; 2],
}

impl Default for PortLatency {
    fn default() -> Self {
        Self {
            quanta_milli: [AtomicU32::new(u32::MAX), AtomicU32::new(u32::MAX)],
            frames: [AtomicU32::new(0), AtomicU32::new(0)],
            ns: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }
}

impl PortLatency {
    fn set(&self, output: bool, quanta: f32, frames: i32, ns: i64) {
        let i = usize::from(output);
        self.frames[i].store(frames.max(0) as u32, Ordering::Relaxed);
        self.ns[i].store(ns.max(0) as u64, Ordering::Relaxed);
        self.quanta_milli[i].store(
            (quanta.max(0.0) * 1000.0).round().min(1e9) as u32,
            Ordering::Relaxed,
        );
    }

    /// In frames at a quantum and rate (realtime safe).
    #[inline]
    fn get(&self, output: bool, quantum: u32, rate: u32) -> Option<u32> {
        let i = usize::from(output);
        let q = self.quanta_milli[i].load(Ordering::Relaxed);
        if q == u32::MAX {
            return None;
        }
        let ns = self.ns[i].load(Ordering::Relaxed);
        let frames = u64::from(q) * u64::from(quantum) / 1000
            + u64::from(self.frames[i].load(Ordering::Relaxed))
            + ns * u64::from(rate) / 1_000_000_000;
        Some(frames.min(u64::from(u32::MAX - 1)) as u32)
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
    /// The graph's accumulated xrun duration (samples) last seen: `None`
    /// before the first cycle.
    xrun_samples: Option<u64>,
    /// The data thread's stack mapped (on the first cycle).
    stack_ready: bool,
}

/// `SPA_IO_CLOCK_FLAG_XRUN_RECOVER`: this node did not finish the last
/// cycle in time.
const XRUN_RECOVER: u32 = 1 << 1;

/// What the filter's events get as their data pointer.
struct Shared {
    rt: UnsafeCell<RtState>,
    monitor: Arc<StreamMonitor>,
    latency: Arc<PortLatency>,
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
    if !rt.stack_ready {
        // Once: the stack deep calls will reach, mapped now rather than
        // page by page in later cycles.
        rt.stack_ready = true;
        faderframe_realtime::memory::prefault_stack(256 * 1024);
    }
    let rate = pos.clock.rate.denom;
    let mut changed = false;
    if (rate > 0 && rate != rt.info.sample_rate) || n as u32 != rt.info.buffer_size {
        if rate > 0 {
            rt.info.sample_rate = rate;
            shared.monitor.set_sample_rate(rate);
        }
        rt.info.buffer_size = n as u32;
        shared.monitor.set_buffer_size(n as u32);
        changed = true;
    }
    // The devices' latency as the graph reports it (atomics only).
    let sr = rt.info.sample_rate;
    let input = shared.latency.get(false, n as u32, sr).unwrap_or(0);
    let output = shared.latency.get(true, n as u32, sr).unwrap_or(0);
    if input != rt.info.input_latency || output != rt.info.output_latency {
        rt.info.input_latency = input;
        rt.info.output_latency = output;
        shared.monitor.set_input_latency(input);
        shared.monitor.set_output_latency(output);
        changed = true;
    }
    if changed {
        rt.callback.prepare(&rt.info);
    }
    // One xrun per cycle after which the graph's xrun duration grew (it
    // is a duration in samples, not a count), or for which PipeWire says
    // this node itself was late.
    let grew = rt.xrun_samples.is_some_and(|seen| pos.clock.xrun > seen);
    if grew || pos.clock.flags & XRUN_RECOVER != 0 {
        shared.monitor.record_xrun();
    }
    rt.xrun_samples = Some(pos.clock.xrun);
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

/// A port's latency param for one direction: (quanta, frames, ns).
fn parse_latency(pod: &pw::spa::pod::Pod, direction: u32) -> Option<(f32, i32, i64)> {
    let object = pod.as_object().ok()?;
    let prop = |key: u32| object.find_prop(pw::spa::utils::Id(key)).map(|p| p.value());
    let d = prop(spa_sys::SPA_PARAM_LATENCY_direction)?.get_id().ok()?.0;
    if d != direction {
        return None;
    }
    let quanta = prop(spa_sys::SPA_PARAM_LATENCY_minQuantum)
        .and_then(|v| v.get_float().ok())
        .unwrap_or(0.0);
    let frames = prop(spa_sys::SPA_PARAM_LATENCY_minRate)
        .and_then(|v| v.get_int().ok())
        .unwrap_or(0);
    let ns = prop(spa_sys::SPA_PARAM_LATENCY_minNs)
        .and_then(|v| v.get_long().ok())
        .unwrap_or(0);
    Some((quanta, frames, ns))
}

/// The node's own ports, bound to hear their Latency params (the graph
/// keeps them up to date as links come and go).
#[derive(Default)]
struct OwnPorts {
    bound: Vec<(u32, pw::port::Port, pw::port::PortListener)>,
    /// Per port: (output, quanta, frames, ns).
    latency: HashMap<u32, (bool, f32, i32, i64)>,
}

impl OwnPorts {
    /// The largest latency of each direction into `cells`.
    fn publish(&self, cells: &PortLatency) {
        for output in [false, true] {
            let most = self
                .latency
                .values()
                .filter(|v| v.0 == output)
                .max_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
            if let Some(&(_, q, f, ns)) = most {
                cells.set(output, q, f, ns);
            }
        }
    }
}

/// Bind `global` if it is one of node `ours`'s ports, to follow its
/// latency.
fn bind_own_port(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    ours: u32,
    own: &Rc<RefCell<OwnPorts>>,
    cells: &Arc<PortLatency>,
) {
    if global.type_ != pw::types::ObjectType::Port {
        return;
    }
    let Some(props) = global.props else { return };
    if props.get("node.id").and_then(|n| n.parse::<u32>().ok()) != Some(ours) {
        return;
    }
    // An output port's latency towards the devices is its playback side
    // (the param for the input direction); an input port's, its capture
    // side (as pipewire-jack maps JACK's latency ranges).
    let output = props.get("port.direction") == Some("out");
    let direction = if output {
        spa_sys::SPA_DIRECTION_INPUT
    } else {
        spa_sys::SPA_DIRECTION_OUTPUT
    };
    let Ok(port) = registry.bind::<pw::port::Port, _>(global) else {
        return;
    };
    let id = global.id;
    let (own2, cells) = (Rc::clone(own), Arc::clone(cells));
    let listener = port
        .add_listener_local()
        .param(move |_, kind, _, _, pod| {
            if kind != pw::spa::param::ParamType::Latency {
                return;
            }
            if let Some((q, f, ns)) = pod.and_then(|p| parse_latency(p, direction)) {
                let mut own = own2.borrow_mut();
                own.latency.insert(id, (output, q, f, ns));
                own.publish(&cells);
            }
        })
        .register();
    port.subscribe_params(&[pw::spa::param::ParamType::Latency]);
    own.borrow_mut().bound.push((id, port, listener));
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

/// Messages to the control thread.
enum Control {
    Quit,
    /// Ask the graph for this buffer size.
    Latency(u32),
    /// Ask the graph for this rate.
    Rate(u32),
}

/// What the control thread shares with the stream handle.
struct Parts {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    latency: Arc<PortLatency>,
    routing: Arc<Mutex<StreamRouting>>,
}

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

/// Set node properties of a live filter (on the loop's thread).
fn update_node(raw: *mut pw_sys::pw_filter, props: pw::properties::PropertiesBox) {
    let props = props.into_raw();
    // SAFETY: the filter outlives the main loop run, this runs on the
    // loop's thread, and the properties are freed after the call that
    // copies them.
    unsafe {
        pw_sys::pw_filter_update_properties(raw, std::ptr::null_mut(), &(*props).dict);
        pw_sys::pw_properties_free(props);
    }
}

/// The rate and buffer-size properties for a request: asked for, or held
/// with `force`.
fn format_props(
    frames: Option<u32>,
    rate: Option<u32>,
    at: u32,
    force: bool,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    add_format(&mut props, frames, rate, at, force);
    props
}

fn add_format(
    props: &mut pw::properties::PropertiesBox,
    frames: Option<u32>,
    rate: Option<u32>,
    at: u32,
    force: bool,
) {
    if let Some(frames) = frames {
        props.insert("node.latency", format!("{frames}/{}", rate.unwrap_or(at)));
        if force {
            props.insert("node.force-quantum", frames.to_string());
        }
    }
    if let Some(rate) = rate {
        props.insert("node.rate", format!("1/{rate}"));
        if force {
            props.insert("node.force-rate", rate.to_string());
        }
    }
}

fn run(
    config: StreamConfig,
    parts: Parts,
    callback: Box<dyn AudioCallback>,
    control: pw::channel::Receiver<Control>,
    ready: &mpsc::SyncSender<Result<(), AudioError>>,
) -> Result<(), AudioError> {
    let Parts {
        info,
        monitor,
        latency,
        routing,
    } = parts;
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
    add_format(
        &mut props,
        config.buffer_size,
        config.sample_rate,
        info.sample_rate,
        config.force_format,
    );
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
            xrun_samples: None,
            stack_ready: false,
        }),
        monitor: Arc::clone(&monitor),
        latency: Arc::clone(&latency),
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

    // The graph, the server's defaults and settings, kept up to date.
    let graph = Rc::new(RefCell::new(Graph::default()));
    let meta = Rc::new(RefCell::new(Meta::default()));
    let bound: Rc<RefCell<Bound>> = Rc::default();
    // Something changed: the links are brought in line on the next tick.
    let dirty = Rc::new(Cell::new(true));
    let own: Rc<RefCell<OwnPorts>> = Rc::default();
    let _registry_listener = {
        let (graph, g2, registry2, meta, bound) = (
            Rc::clone(&graph),
            Rc::clone(&graph),
            registry.clone(),
            Rc::clone(&meta),
            Rc::clone(&bound),
        );
        let (own, own2, cells, raw) = (
            Rc::clone(&own),
            Rc::clone(&own),
            Arc::clone(&latency),
            filter.raw,
        );
        let (d1, d2, d3) = (Rc::clone(&dirty), Rc::clone(&dirty), Rc::clone(&dirty));
        registry
            .add_listener_local()
            .global(move |global| {
                record(&mut graph.borrow_mut(), global);
                // SAFETY: the filter outlives the main loop run.
                let ours = unsafe { pw_sys::pw_filter_get_node_id(raw) };
                bind_own_port(&registry2, global, ours, &own, &cells);
                if global.type_ != pw::types::ObjectType::Link {
                    d1.set(true);
                }
                let d = Rc::clone(&d3);
                bind_metadata(
                    &registry2,
                    global,
                    &bound,
                    &meta,
                    Rc::new(move || d.set(true)),
                );
            })
            .global_remove(move |id| {
                if g2.borrow_mut().remove(id) {
                    d2.set(true);
                }
                let mut own = own2.borrow_mut();
                own.bound.retain(|(port, ..)| *port != id);
                own.latency.remove(&id);
            })
            .register()
    };

    // Auto-connect: links to the devices asked for (or the defaults), made
    // and unmade as the graph changes.
    let wanted = Wanted {
        outputs: config.output_devices.clone(),
        inputs: config.input_devices.clone(),
    };
    // Links this node made, by port pair.
    let links: Rc<RefCell<HashMap<(u32, u32), pw::link::Link>>> = Rc::default();
    // Channel counts of devices seen, so a missing one keeps its place.
    let known: Rc<RefCell<HashMap<String, usize>>> = Rc::default();
    let n_ports = config.input_channels as usize + config.output_channels as usize;
    let relink = {
        let (graph, meta, links, known, core, registry, routing, dirty) = (
            Rc::clone(&graph),
            Rc::clone(&meta),
            Rc::clone(&links),
            Rc::clone(&known),
            core.clone(),
            registry.clone(),
            Arc::clone(&routing),
            Rc::clone(&dirty),
        );
        let raw = filter.raw;
        let auto = config.auto_connect;
        move || {
            if !dirty.replace(false) {
                return;
            }
            // SAFETY: the filter outlives the main loop run (it is dropped
            // after `run` returns).
            let ours = unsafe { pw_sys::pw_filter_get_node_id(raw) };
            let g = graph.borrow();
            let mine = g.ports_of(ours, true).len() + g.ports_of(ours, false).len();
            if mine < n_ports {
                // Our ports are not all there yet.
                dirty.set(true);
                return;
            }
            {
                let mut known = known.borrow_mut();
                for n in g.nodes.values() {
                    if let Some(id) = g.by_name(&n.name) {
                        let width = g.ports_of(id, n.plays()).len();
                        if width > 0 {
                            known.insert(n.name.clone(), width);
                        }
                    }
                }
            }
            let p = plan(&g, ours, &wanted, &meta.borrow().defaults, &known.borrow());
            if auto {
                let mut made = links.borrow_mut();
                // Links of ours not wanted any more go.
                made.retain(|pair, _| {
                    let keep = p.links.contains(pair);
                    if !keep && let Some(id) = g.link_between(pair.0, pair.1) {
                        let _ = registry.destroy_global(id);
                    }
                    keep
                });
                for &(out_port, in_port) in &p.links {
                    if made.contains_key(&(out_port, in_port))
                        || g.link_between(out_port, in_port).is_some()
                    {
                        continue;
                    }
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
                        Ok(l) => {
                            made.insert((out_port, in_port), l);
                        }
                        Err(e) => {
                            tracing::warn!("PipeWire: cannot link {out_port} → {in_port}: {e}");
                        }
                    }
                }
            }
            // What the channels are connected to (with auto-connect off,
            // whatever links exist, made by others).
            let mut r = if auto {
                p.routing
            } else {
                connected(&g, ours, n_ports)
            };
            r.missing.dedup();
            if let Ok(mut shared) = routing.lock() {
                if *shared != r && !r.missing.is_empty() {
                    tracing::info!("PipeWire: not connected now: {}", r.missing.join(", "));
                }
                *shared = r;
            }
        }
    };
    let relink_timer = mainloop.loop_().add_timer(move |_| relink());
    relink_timer.update_timer(Some(RELINK_EVERY), Some(RELINK_EVERY));

    // Requests from the stream handle.
    let wanted_format = Rc::new(Cell::new((config.buffer_size, config.sample_rate)));
    let pinned = Rc::new(Cell::new(config.buffer_size.is_some()));
    let _control = {
        let (ml, monitor, raw) = (mainloop.clone(), Arc::clone(&monitor), filter.raw);
        let (wanted_format, pinned) = (Rc::clone(&wanted_format), Rc::clone(&pinned));
        let force = config.force_format;
        control.attach(mainloop.loop_(), move |msg| {
            let (mut frames, mut rate) = wanted_format.get();
            match msg {
                Control::Quit => {
                    ml.quit();
                    return;
                }
                Control::Latency(f) => frames = Some(f),
                Control::Rate(r) => rate = Some(r),
            }
            wanted_format.set((frames, rate));
            pinned.set(true);
            let at = monitor.status().sample_rate;
            tracing::info!(
                "PipeWire: asking the graph for {} frames at {} Hz",
                frames.map_or_else(|| "any".into(), |f| f.to_string()),
                rate.unwrap_or(at)
            );
            update_node(raw, format_props(frames, rate, at, force));
        })
    };
    // With no buffer size asked for, the node takes the graph's cycle -- and
    // then asks for exactly that one. A node that asks for nothing lets any
    // client with a longer latency (a browser, a notification through
    // pipewire-pulse) drag the whole graph up, and every such change
    // re-prepares the engine and rebuilds whatever buffers by the device's
    // callbacks (the preamps, the Guitar Station) mid-song.
    let pin = {
        let (monitor, raw, meta, pinned) = (
            Arc::clone(&monitor),
            filter.raw,
            Rc::clone(&meta),
            Rc::clone(&pinned),
        );
        let timer = mainloop.loop_().add_timer(move |_| {
            if pinned.get() {
                return;
            }
            let status = monitor.status();
            if status.callbacks == 0 || status.buffer_size == 0 || status.sample_rate == 0 {
                return;
            }
            // The configured default, not whatever the graph runs at now (a
            // browser playing when the stream opened); the cycle it found
            // where the server says nothing.
            let frames = match meta.borrow().settings.quantum {
                0 => status.buffer_size,
                q => q,
            };
            update_node(
                raw,
                properties! {
                    "node.latency" => format!("{frames}/{}", status.sample_rate),
                },
            );
            tracing::info!("PipeWire: asking the graph for {frames} frames");
            pinned.set(true);
        });
        timer.update_timer(
            Some(Duration::from_millis(100)),
            Some(Duration::from_millis(250)),
        );
        timer
    };
    let _ = ready.send(Ok(()));
    mainloop.run();
    drop(pin);
    drop(relink_timer);
    bound.borrow_mut().clear();
    own.borrow_mut().bound.clear();

    // Links first, then the node, then the connection.
    links.borrow_mut().clear();
    drop(filter);
    monitor.set_running(false);
    Ok(())
}

/// What the node's ports are linked to (links anyone made).
fn connected(g: &Graph, ours: u32, _ports: usize) -> StreamRouting {
    let mut r = StreamRouting::default();
    let label = |port: u32| {
        let p = g.ports.get(&port)?;
        let n = g.nodes.get(&p.node)?;
        let name = if n.description.is_empty() {
            &n.name
        } else {
            &n.description
        };
        Some((name.clone(), format!("{name} · {}", p.channel)))
    };
    for (input, list) in [(true, &mut r.inputs), (false, &mut r.outputs)] {
        for port in g.ports_of(ours, input) {
            let peer = g.links.values().find_map(|&(o, i)| {
                if input && i == port {
                    Some(o)
                } else if !input && o == port {
                    Some(i)
                } else {
                    None
                }
            });
            list.push(peer.and_then(label).map(|(device, name)| {
                if !r.devices.contains(&device) {
                    r.devices.push(device);
                }
                name
            }));
        }
    }
    r
}

struct PipeWireStream {
    thread: Option<JoinHandle<()>>,
    control: pw::channel::Sender<Control>,
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    latency: Arc<PortLatency>,
    routing: Arc<Mutex<StreamRouting>>,
}

impl AudioStream for PipeWireStream {
    fn info(&self) -> StreamInfo {
        let status = self.monitor.status();
        let mut info = self.info.clone();
        info.sample_rate = status.sample_rate;
        info.buffer_size = status.buffer_size;
        let at = |output| {
            self.latency
                .get(output, status.buffer_size, status.sample_rate)
                .unwrap_or(0)
        };
        info.input_latency = at(false);
        info.output_latency = at(true);
        if let Ok(r) = self.routing.lock()
            && !r.devices.is_empty()
        {
            info.device = r.devices.join(" + ");
        }
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }

    fn request_buffer_size(&mut self, frames: u32) -> Result<(), AudioError> {
        faderframe_audio::validate_format(self.monitor.status().sample_rate.max(8_000), frames)?;
        self.control
            .send(Control::Latency(frames))
            .map_err(|_| AudioError::Stream("the PipeWire control thread has stopped".into()))
    }

    fn request_sample_rate(&mut self, rate: u32) -> Result<(), AudioError> {
        faderframe_audio::validate_format(rate, self.monitor.status().buffer_size.max(1))?;
        self.control
            .send(Control::Rate(rate))
            .map_err(|_| AudioError::Stream("the PipeWire control thread has stopped".into()))
    }

    fn routing(&self) -> StreamRouting {
        self.routing.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
