//! A loaded plugin: its library, handle, port buffers and the audio
//! thread's processing. The control side reaches it through a `TryCell`
//! (taking it only to restore state or to re-instantiate); `run` happens
//! here, on the audio thread, in pieces split at automation events (LV2
//! control ports are read once per `run`).

use crate::atom::AtomBuffer;
use crate::features::HostFeatures;
use crate::scan::{Lv2Plugin, PortKind};
use crate::sys::{self, LV2_Descriptor, LV2_Handle};
use crate::urid::Urids;
use crate::worker::{self, Responses, Worker};
use faderframe_audio_graph::NodeIo;
use faderframe_midi::MidiEvent;
use faderframe_plugin_host::{PluginProcessContext, ProcessStatus};
use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Bytes of an atom port's buffer (at least what the plugin asks for).
pub const SEQUENCE_SIZE: usize = 32 * 1024;
/// Bytes of the rings between the UI and the plugin.
const UI_RING: usize = 64 * 1024;

/// What the control side and the audio thread share.
pub struct Shared {
    /// Control ports' values (f32 bits) by port index: inputs as set,
    /// outputs as the plugin last wrote them.
    pub values: Box<[AtomicU32]>,
    /// The latency port moved (the host must restart the plugin).
    pub latency_changed: AtomicBool,
    /// Atom output events go to the UI.
    pub ui_open: AtomicBool,
    /// Offline rendering: the `lv2:freeWheeling` port reads 1.
    pub freewheel: AtomicBool,
}

impl Shared {
    pub fn new(plugin: &Lv2Plugin) -> Shared {
        Shared {
            values: plugin
                .ports
                .iter()
                .map(|p| AtomicU32::new(initial_value(p).to_bits()))
                .collect(),
            latency_changed: AtomicBool::new(false),
            ui_open: AtomicBool::new(false),
            freewheel: AtomicBool::new(false),
        }
    }

    pub fn get(&self, port: u32) -> Option<f32> {
        self.values
            .get(port as usize)
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    pub fn set(&self, port: u32, value: f32) {
        if let Some(v) = self.values.get(port as usize) {
            v.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// A control port's value before anything set it.
pub fn initial_value(p: &crate::scan::Port) -> f32 {
    if p.enabled {
        return 1.0;
    }
    if p.free_wheeling {
        return 0.0;
    }
    let mut v = p.default.or(p.minimum).unwrap_or(0.0);
    if let (Some(lo), Some(hi)) = (p.minimum, p.maximum)
        && lo <= hi
    {
        v = v.clamp(lo, hi);
    }
    v as f32
}

/// The control side's ends of a core's queues.
pub struct Links {
    /// Control values for the plugin (port, value).
    pub edits: rtrb::Producer<(u32, f32)>,
    /// Atoms from the UI for the plugin: port, type, body.
    pub to_plugin: rtrb::Producer<u8>,
    /// Atoms from the plugin for the UI: port, type, body.
    pub from_plugin: rtrb::Consumer<u8>,
}

/// What the transport looked like after the last block (to tell the
/// plugin only about changes).
#[derive(Clone, Copy, PartialEq)]
struct Expect {
    playing: bool,
    tempo: f64,
    numerator: u8,
    denominator: u8,
    next: i64,
}

pub struct Core {
    /// `false` once the control side replaced or dropped it: process
    /// gives silence.
    pub live: bool,
    desc: *const LV2_Descriptor,
    pub handle: LV2_Handle,
    pub max_frames: usize,
    active: bool,
    /// Must outlive the handle (the plugin keeps pointers into it).
    features: Option<Box<HostFeatures>>,
    /// Stopped before the plugin is cleaned up.
    worker: Option<Worker>,
    responses: Option<Responses>,
    shared: Arc<Shared>,
    controls: Box<[f32]>,
    /// Audio and CV buffers by port index (empty for other ports).
    audio: Vec<Vec<f32>>,
    atoms: Vec<Option<AtomBuffer>>,
    inputs: [Vec<u32>; 2],
    outputs: Vec<Vec<u32>>,
    /// Ports that are input/output controls, atoms in/out.
    control_in: Vec<u32>,
    control_out: Vec<u32>,
    atom_in: Vec<u32>,
    atom_out: Vec<u32>,
    audio_ports: Vec<u32>,
    midi_in: Option<u32>,
    midi_out: Option<u32>,
    time_ports: Vec<u32>,
    latency_port: Option<u32>,
    freewheel_port: Option<u32>,
    last_latency: Option<f32>,
    edits: rtrb::Consumer<(u32, f32)>,
    ui_in: rtrb::Consumer<u8>,
    to_ui: rtrb::Producer<u8>,
    expect: Option<Expect>,
    pending_reset: bool,
    scratch: Vec<u8>,
    /// Keeps the shared library loaded while the handle lives.
    _binary: Binary,
}

// SAFETY: the plugin handle is used by one thread at a time (the cell's
// holder); LV2's audio functions may run on any thread.
unsafe impl Send for Core {}

/// Where a plugin's code is.
#[derive(Clone)]
pub enum Binary {
    /// Its bundle's shared library.
    Library(Arc<libloading::Library>),
    /// An entry point linked into this program (tests).
    Static(sys::LV2_Descriptor_Function),
}

/// The plugin's descriptor in `binary`.
pub fn descriptor(binary: &Binary, uri: &str) -> Result<*const LV2_Descriptor, String> {
    let entry: sys::LV2_Descriptor_Function = match binary {
        Binary::Static(f) => *f,
        // SAFETY: `lv2_descriptor` is the LV2 entry point of this type.
        Binary::Library(library) => {
            *unsafe { library.get::<sys::LV2_Descriptor_Function>(b"lv2_descriptor\0") }
                .map_err(|e| e.to_string())?
        }
    };
    for index in 0..4096 {
        // SAFETY: the entry point, with indices from 0 until it gives null.
        let d = unsafe { entry(index) };
        if d.is_null() {
            break;
        }
        // SAFETY: a valid descriptor's URI is a C string.
        let du = unsafe { std::ffi::CStr::from_ptr((*d).uri) };
        if du.to_bytes() == uri.as_bytes() {
            return Ok(d);
        }
    }
    Err(format!("{uri} is not in its library"))
}

/// The bundle path as LV2 wants it (ending in a separator).
pub fn bundle_path(plugin: &Lv2Plugin) -> CString {
    let mut s = plugin.bundle.to_string_lossy().into_owned();
    if !s.ends_with('/') {
        s.push('/');
    }
    CString::new(s).unwrap_or_default()
}

impl Core {
    /// Instantiate `plugin` at `rate` for blocks of up to `max_frames`.
    pub fn new(
        plugin: &Lv2Plugin,
        binary: Binary,
        rate: f64,
        max_frames: usize,
        shared: Arc<Shared>,
    ) -> Result<(Core, Links), String> {
        let desc = descriptor(&binary, &plugin.uri)?;
        let wants_worker = plugin
            .required_features
            .iter()
            .chain(&plugin.optional_features)
            .any(|f| f == sys::uri::WORKER_SCHEDULE)
            || plugin
                .extension_data
                .iter()
                .any(|f| f == sys::uri::WORKER_INTERFACE);
        let mut worker = wants_worker.then(Worker::new);
        let sequence = plugin
            .ports
            .iter()
            .filter_map(|p| p.minimum_size)
            .max()
            .map_or(SEQUENCE_SIZE, |m| (m as usize).max(SEQUENCE_SIZE));
        let features = HostFeatures::new(
            rate,
            max_frames as u32,
            sequence as u32,
            worker.as_ref().map(Worker::feature),
        );
        let bundle = bundle_path(plugin);
        // SAFETY: the descriptor's instantiate with the features (alive as
        // long as the handle: kept in the core).
        let handle = unsafe {
            match (*desc).instantiate {
                Some(f) => f(desc, rate, bundle.as_ptr(), features.as_ptr()),
                None => std::ptr::null_mut(),
            }
        };
        if handle.is_null() {
            return Err(format!("{} did not instantiate", plugin.name));
        }
        let mut responses = None;
        if let Some(w) = worker.as_mut() {
            let name = CString::new(sys::uri::WORKER_INTERFACE).unwrap_or_default();
            // SAFETY: the descriptor's extension data query.
            let iface = unsafe {
                (*desc)
                    .extension_data
                    .map_or(std::ptr::null(), |f| f(name.as_ptr()))
            };
            if iface.is_null() {
                worker = None;
            } else {
                let iface = iface.cast::<sys::LV2_Worker_Interface>();
                // SAFETY: the interface and handle live until the core is
                // dropped, which stops the worker first.
                unsafe { w.start(handle, iface) };
                responses = Some(Responses::new(w, iface));
            }
        }
        let n = plugin.ports.len();
        let mut controls = vec![0f32; n].into_boxed_slice();
        let mut audio = vec![Vec::new(); n];
        let mut atoms: Vec<Option<AtomBuffer>> = (0..n).map(|_| None).collect();
        let mut s = Core {
            live: true,
            desc,
            handle,
            max_frames,
            active: false,
            features: Some(features),
            worker,
            responses,
            shared: Arc::clone(&shared),
            controls: Box::new([]),
            audio: Vec::new(),
            atoms: Vec::new(),
            inputs: [Vec::new(), Vec::new()],
            outputs: plugin.output_buses(),
            control_in: Vec::new(),
            control_out: Vec::new(),
            atom_in: Vec::new(),
            atom_out: Vec::new(),
            audio_ports: Vec::new(),
            midi_in: plugin.midi_port(true),
            midi_out: plugin.midi_port(false),
            time_ports: Vec::new(),
            latency_port: None,
            freewheel_port: None,
            last_latency: None,
            edits: rtrb::RingBuffer::new(1).1,
            ui_in: rtrb::RingBuffer::new(1).1,
            to_ui: rtrb::RingBuffer::new(1).0,
            expect: None,
            pending_reset: false,
            scratch: Vec::with_capacity(worker::MESSAGE),
            _binary: binary,
        };
        let (main, side) = plugin.audio_inputs();
        s.inputs = [main, side];
        for p in &plugin.ports {
            let i = p.index;
            match p.kind {
                PortKind::Control => {
                    controls[i as usize] = shared.get(i).unwrap_or(0.0);
                    if p.input {
                        s.control_in.push(i);
                        if p.free_wheeling {
                            s.freewheel_port = Some(i);
                        }
                    } else {
                        s.control_out.push(i);
                        if p.latency {
                            s.latency_port = Some(i);
                        }
                    }
                }
                PortKind::Audio | PortKind::Cv => {
                    audio[i as usize] = vec![0.0; max_frames.max(1)];
                    s.audio_ports.push(i);
                }
                PortKind::Atom { .. } => {
                    let size = p
                        .minimum_size
                        .map_or(sequence, |m| (m as usize).max(sequence));
                    let mut b = AtomBuffer::new(size);
                    if p.input {
                        b.clear_input();
                        s.atom_in.push(i);
                        if p.time {
                            s.time_ports.push(i);
                        }
                    } else {
                        b.prepare_output();
                        s.atom_out.push(i);
                    }
                    atoms[i as usize] = Some(b);
                }
                PortKind::Other => {}
            }
        }
        // A plugin that takes the transport on its MIDI port only.
        if s.time_ports.is_empty()
            && let Some(m) = s.midi_in
            && plugin.ports.iter().any(|p| p.index == m && p.time)
        {
            s.time_ports.push(m);
        }
        s.controls = controls;
        s.audio = audio;
        s.atoms = atoms;
        let (edits_tx, edits_rx) = rtrb::RingBuffer::new(1024);
        let (to_plugin, ui_in) = rtrb::RingBuffer::new(UI_RING);
        let (to_ui, from_plugin) = rtrb::RingBuffer::new(UI_RING);
        s.edits = edits_rx;
        s.ui_in = ui_in;
        s.to_ui = to_ui;
        s.connect_all(0);
        Ok((
            s,
            Links {
                edits: edits_tx,
                to_plugin,
                from_plugin,
            },
        ))
    }

    fn connect(&mut self, port: u32, data: *mut std::ffi::c_void) {
        // SAFETY: the descriptor's connect_port with a buffer the core owns
        // (it outlives the handle's use of it).
        unsafe {
            if let Some(f) = (*self.desc).connect_port {
                f(self.handle, port, data);
            }
        }
    }

    /// Connect every port; audio at `offset` frames into its buffer.
    fn connect_all(&mut self, offset: usize) {
        for i in 0..self.controls.len() {
            let control = self.control_in.binary_search(&(i as u32)).is_ok()
                || self.control_out.binary_search(&(i as u32)).is_ok();
            let ptr: *mut f32 = if control {
                &mut self.controls[i]
            } else {
                std::ptr::null_mut()
            };
            if self.audio[i].is_empty() && self.atoms[i].is_none() {
                self.connect(i as u32, ptr.cast());
            }
        }
        self.connect_audio(offset);
        for i in 0..self.atoms.len() {
            if let Some(b) = self.atoms[i].as_mut() {
                let ptr = b.as_ptr();
                self.connect(i as u32, ptr);
            }
        }
    }

    fn connect_audio(&mut self, offset: usize) {
        for k in 0..self.audio_ports.len() {
            let p = self.audio_ports[k] as usize;
            let buf = &mut self.audio[p];
            let at = offset.min(buf.len().saturating_sub(1));
            let ptr = buf[at..].as_mut_ptr();
            self.connect(p as u32, ptr.cast());
        }
    }

    pub fn activate(&mut self) {
        if !self.active {
            // SAFETY: the descriptor's activate (instantiation class: the
            // caller holds the cell).
            unsafe {
                if let Some(f) = (*self.desc).activate {
                    f(self.handle);
                }
            }
            self.active = true;
            self.expect = None;
        }
    }

    pub fn deactivate(&mut self) {
        if self.active {
            // SAFETY: as in `activate`.
            unsafe {
                if let Some(f) = (*self.desc).deactivate {
                    f(self.handle);
                }
            }
            self.active = false;
        }
    }

    /// Keep the worker from running while the guard lives.
    pub fn pause_worker(&self) -> Option<std::sync::MutexGuard<'_, ()>> {
        self.worker.as_ref().and_then(Worker::pause)
    }

    /// The worker's schedule feature (for restores that load in it).
    pub fn schedule(&self) -> Option<sys::LV2_Worker_Schedule> {
        self.worker.as_ref().map(Worker::feature)
    }

    /// The plugin's extension data for `uri`.
    pub fn extension(&self, uri: &str) -> *const std::ffi::c_void {
        let Ok(name) = CString::new(uri) else {
            return std::ptr::null();
        };
        // SAFETY: the descriptor's extension data query.
        unsafe {
            (*self.desc)
                .extension_data
                .map_or(std::ptr::null(), |f| f(name.as_ptr()))
        }
    }

    /// Run a block of silence and read the latency port (control side,
    /// right after activation); the plugin is reset afterwards.
    pub fn measure_latency(&mut self) -> u32 {
        let Some(port) = self.latency_port else {
            return 0;
        };
        let frames = self.max_frames.clamp(1, 256);
        for &p in &self.audio_ports {
            self.audio[p as usize].fill(0.0);
        }
        for &p in &self.atom_in {
            if let Some(b) = self.atoms[p as usize].as_mut() {
                b.clear_input();
            }
        }
        for &p in &self.atom_out {
            if let Some(b) = self.atoms[p as usize].as_mut() {
                b.prepare_output();
            }
        }
        // SAFETY: an activated plugin, its ports connected.
        unsafe {
            if let Some(run) = (*self.desc).run {
                run(self.handle, frames as u32);
            }
        }
        let latency = self.controls[port as usize];
        self.last_latency = Some(latency);
        self.shared.set(port, latency);
        // Start afresh (LV2: activate resets the plugin).
        self.deactivate();
        self.activate();
        latency.max(0.0).round() as u32
    }

    fn set_control(&mut self, port: u32, value: f32) {
        if self.control_in.binary_search(&port).is_ok()
            && let Some(c) = self.controls.get_mut(port as usize)
        {
            *c = value;
            self.shared.set(port, value);
        }
    }

    /// One block (audio thread).
    pub fn process(
        &mut self,
        ctx: &PluginProcessContext<'_>,
        io: &mut NodeIo<'_>,
    ) -> ProcessStatus {
        let n = io.frames.min(self.max_frames);
        // SAFETY: reading the descriptor's function table.
        let run = unsafe { (*self.desc).run };
        let (true, true, Some(run)) = (self.live, self.active, run) else {
            silence(io);
            return ProcessStatus::Continue;
        };
        if n == 0 {
            silence(io);
            return ProcessStatus::Continue;
        }
        let u = Urids::get();
        while let Ok((port, value)) = self.edits.pop() {
            if let Some(c) = self.controls.get_mut(port as usize) {
                *c = value;
            }
        }
        if let Some(p) = self.freewheel_port {
            self.controls[p as usize] = if self.shared.freewheel.load(Ordering::Relaxed) {
                1.0
            } else {
                0.0
            };
        }
        // Audio in: graph input `b` into bus `b` (main, then sidechain).
        for (bus, ports) in self.inputs.iter().enumerate() {
            let src = io.audio_in.get(bus).filter(|b| b.num_channels() > 0);
            for (c, &p) in ports.iter().enumerate() {
                let dst = &mut self.audio[p as usize][..n];
                match src {
                    Some(b) => dst.copy_from_slice(&b.channel(c.min(b.num_channels() - 1))[..n]),
                    None => dst.fill(0.0),
                }
            }
        }
        // The input slice outlives `io`'s borrow below.
        let events_in = io.events_in;
        let midi = events_in.first();
        let mut moved = false;
        let mut next_midi = 0;
        let mut next_param = 0;
        let mut start = 0;
        let ui_open = self.shared.ui_open.load(Ordering::Relaxed);
        while start < n {
            while let Some(e) = ctx.param_events.get(next_param)
                && e.sample_offset as usize <= start
            {
                self.set_control(e.parameter.0, e.value);
                next_param += 1;
            }
            let end = ctx
                .param_events
                .get(next_param)
                .map_or(n, |e| (e.sample_offset as usize).clamp(start + 1, n));
            for k in 0..self.atom_in.len() {
                if let Some(b) = self.atoms[self.atom_in[k] as usize].as_mut() {
                    b.clear_input();
                }
            }
            if start == 0 {
                self.send_transport(ctx.transport, n);
                self.send_ui_messages();
                if std::mem::take(&mut self.pending_reset)
                    && let Some(port) = self.midi_in
                    && let Some(buf) = self.atoms[port as usize].as_mut()
                {
                    for ch in 0..16u8 {
                        buf.push(
                            0,
                            u.midi_event,
                            &[0xB0 | ch, MidiEvent::CC_ALL_SOUND_OFF, 0],
                        );
                        buf.push(
                            0,
                            u.midi_event,
                            &[0xB0 | ch, MidiEvent::CC_ALL_NOTES_OFF, 0],
                        );
                    }
                }
            }
            if let (Some(port), Some(midi)) = (self.midi_in, midi)
                && let Some(buf) = self.atoms[port as usize].as_mut()
            {
                let events = midi.as_slice();
                while let Some(e) = events.get(next_midi)
                    && (e.sample_offset as usize) < end
                {
                    let t = (e.sample_offset as usize).saturating_sub(start) as i64;
                    match e.event {
                        MidiEvent::SysEx(r) => {
                            if let Some(bytes) = midi.sysex(&r) {
                                buf.push(t, u.midi_event, bytes);
                            }
                        }
                        MidiEvent::NoteExpression { .. } => {}
                        other => {
                            let (bytes, len) = other.to_bytes();
                            if len > 0 {
                                buf.push(t, u.midi_event, &bytes[..len]);
                            }
                        }
                    }
                    next_midi += 1;
                }
            }
            for k in 0..self.atom_out.len() {
                if let Some(b) = self.atoms[self.atom_out[k] as usize].as_mut() {
                    b.prepare_output();
                }
            }
            if start > 0 {
                self.connect_audio(start);
                moved = true;
            }
            // SAFETY: an activated plugin with every port connected to
            // buffers of at least `end − start` frames from the offset.
            unsafe { run(self.handle, (end - start) as u32) };
            if let Some(r) = self.responses.as_mut() {
                // SAFETY: the worker interface of this handle, on the
                // thread that runs it.
                unsafe { r.deliver(self.handle) };
            }
            self.collect_outputs(io, start, ui_open);
            start = end;
        }
        if moved {
            self.connect_audio(0);
        }
        // Audio out: bus `b` into graph output `b`.
        for (b, out) in io.audio_out.iter_mut().enumerate() {
            let Some(ports) = self.outputs.get(b).filter(|p| !p.is_empty()) else {
                out.clear();
                continue;
            };
            for c in 0..out.num_channels() {
                let p = ports[c.min(ports.len() - 1)] as usize;
                out.channel_mut(c)[..n].copy_from_slice(&self.audio[p][..n]);
                out.channel_mut(c)[n..].fill(0.0);
            }
        }
        for &p in &self.control_out {
            self.shared.set(p, self.controls[p as usize]);
        }
        if let Some(p) = self.latency_port {
            let v = self.controls[p as usize];
            if self.last_latency.is_some_and(|l| (l - v).abs() >= 0.5) {
                self.shared.latency_changed.store(true, Ordering::Relaxed);
            }
            self.last_latency = Some(v);
        }
        ProcessStatus::Continue
    }

    /// Plugin's atom output: MIDI to the graph, everything to an open UI.
    fn collect_outputs(&mut self, io: &mut NodeIo<'_>, start: usize, ui_open: bool) {
        let u = Urids::get();
        for k in 0..self.atom_out.len() {
            let port = self.atom_out[k];
            let Some(buf) = self.atoms[port as usize].as_ref() else {
                continue;
            };
            for (t, type_, body) in buf.events() {
                if Some(port) == self.midi_out
                    && type_ == u.midi_event
                    && let Some(out) = io.events_out.first_mut()
                {
                    let at = (start as i64 + t.max(0)) as u32;
                    if body.first() == Some(&0xF0) {
                        let _ = out.push_sysex(at, body);
                    } else if let Some(ev) = MidiEvent::from_bytes(body) {
                        let _ = out.push(faderframe_midi::TimedMidiEvent::new(at, ev));
                    }
                }
                if ui_open && type_ != u.midi_event {
                    push_message(&mut self.to_ui, port, type_, body);
                }
            }
        }
    }

    fn send_ui_messages(&mut self) {
        while worker::pop(&mut self.ui_in, &mut self.scratch) {
            let m = &self.scratch;
            if m.len() < 8 {
                continue;
            }
            let port = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
            let type_ = u32::from_le_bytes([m[4], m[5], m[6], m[7]]);
            if let Some(b) = self.atoms.get_mut(port as usize).and_then(Option::as_mut) {
                b.push(0, type_, &m[8..]);
            }
        }
    }

    fn send_transport(&mut self, t: &faderframe_transport::TransportInfo, frames: usize) {
        let now = Expect {
            playing: t.playing,
            tempo: t.tempo,
            numerator: t.time_signature.numerator,
            denominator: t.time_signature.denominator,
            next: t.sample_position,
        };
        let changed = self.expect != Some(now);
        self.expect = Some(Expect {
            next: if t.playing {
                t.sample_position + frames as i64
            } else {
                t.sample_position
            },
            ..now
        });
        if !changed || self.time_ports.is_empty() {
            return;
        }
        let u = Urids::get();
        let unit = f64::from(t.time_signature.denominator.max(1));
        let beat = t.quarter_position * unit / 4.0;
        let bar_beat = ((t.quarter_position - t.bar_start_quarters) * unit / 4.0) as f32;
        let frame = t.sample_position.to_ne_bytes();
        let speed = if t.playing { 1f32 } else { 0.0 }.to_ne_bytes();
        let bpm = (t.tempo as f32).to_ne_bytes();
        let bar = i64::from(t.bar_index).to_ne_bytes();
        let bar_beat = bar_beat.to_ne_bytes();
        let beat = beat.to_ne_bytes();
        let per_bar = f32::from(t.time_signature.numerator).to_ne_bytes();
        let beat_unit = i32::from(t.time_signature.denominator).to_ne_bytes();
        let props: [(u32, u32, &[u8]); 8] = [
            (u.time_frame, u.atom_long, &frame),
            (u.time_speed, u.atom_float, &speed),
            (u.time_bpm, u.atom_float, &bpm),
            (u.time_bar, u.atom_long, &bar),
            (u.time_bar_beat, u.atom_float, &bar_beat),
            (u.time_beat, u.atom_double, &beat),
            (u.time_beats_per_bar, u.atom_float, &per_bar),
            (u.time_beat_unit, u.atom_int, &beat_unit),
        ];
        for k in 0..self.time_ports.len() {
            let p = self.time_ports[k];
            if let Some(b) = self.atoms[p as usize].as_mut() {
                b.push_object(0, u.time_position, &props);
            }
        }
    }

    /// LV2 has no reset for the audio thread (activation is not realtime
    /// safe): voices are stopped with All Sound Off / All Notes Off on
    /// every channel in the next block.
    pub fn reset(&mut self) {
        self.pending_reset = true;
        self.expect = None;
    }
}

/// Queue a message for the other side: port, type, body.
pub fn push_message(tx: &mut rtrb::Producer<u8>, port: u32, type_: u32, body: &[u8]) -> bool {
    let len = 8 + body.len();
    if body.len() > worker::MESSAGE || tx.slots() < 4 + len {
        return false;
    }
    match tx.write_chunk_uninit(4 + len) {
        Ok(chunk) => {
            chunk.fill_from_iter(
                (len as u32)
                    .to_le_bytes()
                    .into_iter()
                    .chain(port.to_le_bytes())
                    .chain(type_.to_le_bytes())
                    .chain(body.iter().copied()),
            );
            true
        }
        Err(_) => false,
    }
}

fn silence(io: &mut NodeIo<'_>) {
    for out in io.audio_out.iter_mut() {
        out.clear();
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // The worker thread stops before the plugin goes.
        self.worker = None;
        self.responses = None;
        self.deactivate();
        // SAFETY: the handle is not used after this.
        unsafe {
            if let Some(f) = (*self.desc).cleanup {
                f(self.handle);
            }
        }
        self.features = None;
    }
}
