//! VST3 hosting against an in-process test plugin written with the `vst3`
//! crate: a separate component and controller (connection points and
//! messages), stepped parameters, sample-accurate automation, notes and
//! MIDI-mapped controllers, editor edits, output parameters and state.
#![allow(non_snake_case, clippy::unwrap_used)]

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::{
    PluginFactory, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
};
use faderframe_plugin_vst3::{Vst3Factory, module, scan, util};
use faderframe_transport::TransportInfo;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Mutex, Once};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

// --- counting allocator (allocations on the thread that enables it) -------

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
thread_local! { static COUNT: Cell<bool> = const { Cell::new(false) }; }

// SAFETY: forwards to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if COUNT.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: as documented by GlobalAlloc.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if COUNT.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: as documented by GlobalAlloc.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

// --- the test plugin -------------------------------------------------------

const GAIN: ParamID = 1;
const MODE: ParamID = 2;
const METER: ParamID = 3;
const LATENCY: u32 = 64;

/// What the processor got through its connection point.
static MESSAGE_VALUE: AtomicI64 = AtomicI64::new(0);
/// The controller's component handler (the test plays the editor).
static HANDLER: Mutex<Option<ComPtr<IComponentHandler>>> = Mutex::new(None);
static PROCESSING: AtomicBool = AtomicBool::new(false);

fn read_f64(stream: *mut IBStream) -> Option<f64> {
    let s = unsafe { ComRef::from_raw(stream) }?;
    let mut b = [0u8; 8];
    let mut n = 0;
    unsafe { s.read(b.as_mut_ptr() as *mut c_void, 8, &mut n) };
    (n == 8).then(|| f64::from_le_bytes(b))
}

fn write_bytes(stream: *mut IBStream, b: &[u8]) {
    if let Some(s) = unsafe { ComRef::from_raw(stream) } {
        let mut n = 0;
        unsafe { s.write(b.as_ptr() as *mut c_void, b.len() as i32, &mut n) };
    }
}

struct TestProcessor {
    gain: Cell<f64>,
    held: Cell<i32>,
}

impl Class for TestProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl TestProcessor {
    const CID: TUID = uid(0x11111111, 0x22222222, 0x33333333, 0x44444444);
}

impl IPluginBaseTrait for TestProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for TestProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = TestController::CID };
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> i32 {
        let audio = media == MediaTypes_::kAudio as i32;
        let event_in = media == MediaTypes_::kEvent as i32 && dir == BusDirections_::kInput as i32;
        (audio || event_in) as i32
    }
    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = if media == MediaTypes_::kAudio as i32 {
            2
        } else {
            16
        };
        util::write_wstr(&mut bus.name, "Main");
        bus.busType = BusTypes_::kMain as i32;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive;
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match read_f64(state) {
            Some(g) => {
                self.gain.set(g);
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_bytes(state, &self.gain.get().to_le_bytes());
        kResultOk
    }
}

impl IAudioProcessorTrait for TestProcessor {
    unsafe fn setBusArrangements(
        &self,
        _i: *mut SpeakerArrangement,
        _ni: i32,
        _o: *mut SpeakerArrangement,
        _no: i32,
    ) -> tresult {
        kResultTrue
    }
    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        _index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        LATENCY
    }
    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }
    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        PROCESSING.store(state != 0, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let d = unsafe { &mut *data };
        let n = d.numSamples as usize;
        // Gain points (sample accurate: each point applies from its offset).
        let mut points = [(0i32, 0.0f64); 64];
        let mut count = 0;
        if let Some(changes) = unsafe { ComRef::from_raw(d.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(q) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                if unsafe { q.getParameterId() } != GAIN {
                    continue;
                }
                for p in 0..unsafe { q.getPointCount() } {
                    let (mut o, mut v) = (0, 0.0);
                    if unsafe { q.getPoint(p, &mut o, &mut v) } == kResultTrue && count < 64 {
                        points[count] = (o, v);
                        count += 1;
                    }
                }
            }
        }
        // Notes: each held note adds 0.25 (from its offset).
        let mut notes = [(0i32, 0i32); 64];
        let mut ncount = 0;
        if let Some(events) = unsafe { ComRef::from_raw(d.inputEvents) } {
            for i in 0..unsafe { events.getEventCount() } {
                let mut e: Event = unsafe { std::mem::zeroed() };
                unsafe { events.getEvent(i, &mut e) };
                let delta = match e.r#type as u32 {
                    t if t == Event_::EventTypes_::kNoteOnEvent => 1,
                    t if t == Event_::EventTypes_::kNoteOffEvent => -1,
                    _ => 0,
                };
                if delta != 0 && ncount < 64 {
                    notes[ncount] = (e.sampleOffset, delta);
                    ncount += 1;
                }
            }
        }
        if n == 0 || d.numInputs < 1 || d.numOutputs < 1 {
            return kResultOk;
        }
        let ins = unsafe { std::slice::from_raw_parts(d.inputs, 1) };
        let outs = unsafe { std::slice::from_raw_parts(d.outputs, 1) };
        let mut peak = 0.0f32;
        for ch in 0..2 {
            let i = unsafe { *ins[0].__field0.channelBuffers32.add(ch) };
            let o = unsafe { *outs[0].__field0.channelBuffers32.add(ch) };
            let (mut gain, mut held) = (self.gain.get(), self.held.get());
            let (mut pi, mut ni) = (0, 0);
            for s in 0..n {
                while pi < count && points[pi].0 as usize <= s {
                    gain = points[pi].1;
                    pi += 1;
                }
                while ni < ncount && notes[ni].0 as usize <= s {
                    held += notes[ni].1;
                    ni += 1;
                }
                let v = unsafe { *i.add(s) } * (2.0 * gain) as f32 + 0.25 * held as f32;
                unsafe { *o.add(s) = v };
                peak = peak.max(v.abs());
            }
            if ch == 1 {
                self.gain.set(gain);
                self.held.set(held);
            }
        }
        // Report a meter value to the controller.
        if let Some(out) = unsafe { ComRef::from_raw(d.outputParameterChanges) } {
            let mut index = 0;
            let q = unsafe { out.addParameterData(&METER, &mut index) };
            if let Some(q) = unsafe { ComRef::from_raw(q) } {
                unsafe { q.addPoint(0, (peak as f64 / 4.0).min(1.0), &mut index) };
            }
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IConnectionPointTrait for TestProcessor {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }
    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }
    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let Some(m) = (unsafe { ComRef::from_raw(message) }) else {
            return kInvalidArgument;
        };
        let id = unsafe { CStr::from_ptr(m.getMessageID()) };
        if id.to_bytes() == b"hello"
            && let Some(attrs) = unsafe { ComRef::from_raw(m.getAttributes()) }
        {
            let mut v = 0;
            unsafe { attrs.getInt(c"value".as_ptr(), &mut v) };
            let (mut p, mut size) = (std::ptr::null(), 0u32);
            unsafe { attrs.getBinary(c"blob".as_ptr(), &mut p, &mut size) };
            let blob = unsafe { std::slice::from_raw_parts(p as *const u8, size as usize) };
            MESSAGE_VALUE.store(
                v + blob.iter().map(|&b| b as i64).sum::<i64>(),
                Ordering::Relaxed,
            );
        }
        kResultOk
    }
}

struct TestController {
    values: Mutex<[f64; 4]>,
    host: Mutex<Option<ComPtr<IHostApplication>>>,
    ui_size: Mutex<u32>,
}

impl Class for TestController {
    type Interfaces = (IEditController, IConnectionPoint, IMidiMapping);
}

impl TestController {
    const CID: TUID = uid(0x55555555, 0x66666666, 0x77777777, 0x88888888);
}

impl IPluginBaseTrait for TestController {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        let host = unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast::<IHostApplication>());
        *self.host.lock().unwrap() = host;
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        *self.host.lock().unwrap() = None;
        kResultOk
    }
}

impl IConnectionPointTrait for TestController {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        // Say hello through a host-created message.
        let host = self.host.lock().unwrap().clone();
        let Some(host) = host else {
            return kResultFalse;
        };
        let mut obj: *mut c_void = std::ptr::null_mut();
        let mut cid = util::tuid(&IMessage::IID);
        let mut iid = util::tuid(&IMessage::IID);
        if unsafe { host.createInstance(&mut cid, &mut iid, &mut obj) } != kResultOk {
            return kResultFalse;
        }
        let Some(msg) = (unsafe { ComPtr::from_raw(obj as *mut IMessage) }) else {
            return kResultFalse;
        };
        unsafe {
            msg.setMessageID(c"hello".as_ptr());
            if let Some(a) = ComRef::from_raw(msg.getAttributes()) {
                a.setInt(c"value".as_ptr(), 40);
                let blob = [1u8, 1];
                a.setBinary(c"blob".as_ptr(), blob.as_ptr() as *const c_void, 2);
            }
            if let Some(o) = ComRef::from_raw(other) {
                o.notify(msg.as_ptr());
            }
        }
        kResultOk
    }
    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }
    unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
        kResultOk
    }
}

impl IMidiMappingTrait for TestController {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        cc: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        if bus == 0 && cc == ControllerNumbers_::kCtrlVolume as i16 {
            unsafe { *id = GAIN };
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

impl IEditControllerTrait for TestController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        if let Some(g) = read_f64(state) {
            self.values.lock().unwrap()[GAIN as usize] = g;
        }
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let s = unsafe { ComRef::from_raw(state) }.unwrap();
        let mut b = [0u8; 4];
        let mut n = 0;
        unsafe { s.read(b.as_mut_ptr() as *mut c_void, 4, &mut n) };
        *self.ui_size.lock().unwrap() = u32::from_le_bytes(b);
        kResultOk
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_bytes(state, &self.ui_size.lock().unwrap().to_le_bytes());
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        3
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        use ParameterInfo_::ParameterFlags_::*;
        let info = unsafe { &mut *info };
        let (id, name, steps, flags) = match index {
            0 => (GAIN, "Gain", 0, kCanAutomate),
            1 => (MODE, "Mode", 2, kCanAutomate | kIsList),
            2 => (METER, "Meter", 0, kIsReadOnly),
            _ => return kInvalidArgument,
        };
        info.id = id;
        util::write_wstr(&mut info.title, name);
        util::write_wstr(&mut info.shortTitle, name);
        info.stepCount = steps;
        info.defaultNormalizedValue = if id == GAIN { 0.5 } else { 0.0 };
        info.unitId = 0;
        info.flags = flags;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, n: f64, s: *mut String128) -> tresult {
        if id != GAIN {
            return kResultFalse;
        }
        util::write_wstr(unsafe { &mut *s }, &format!("x{:.2}", 2.0 * n));
        kResultOk
    }
    unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _id: u32, n: f64) -> f64 {
        n
    }
    unsafe fn plainParamToNormalized(&self, _id: u32, p: f64) -> f64 {
        p
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.values
            .lock()
            .unwrap()
            .get(id as usize)
            .copied()
            .unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        if let Some(x) = self.values.lock().unwrap().get_mut(id as usize) {
            *x = v;
        }
        kResultOk
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *HANDLER.lock().unwrap() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, _name: *const std::ffi::c_char) -> *mut IPlugView {
        std::ptr::null_mut()
    }
}

struct TestFactory;

impl Class for TestFactory {
    type Interfaces = (IPluginFactory2,);
}

fn copy(dst: &mut [std::ffi::c_char], s: &str) {
    for (d, b) in dst.iter_mut().zip(s.bytes().chain(std::iter::once(0))) {
        *d = b as std::ffi::c_char;
    }
}

impl IPluginFactoryTrait for TestFactory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy(&mut info.vendor, "FaderFrame Tests");
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        2
    }
    unsafe fn getClassInfo(&self, _index: i32, _info: *mut PClassInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let cid = unsafe { *(cid as *const TUID) };
        let unknown = if cid == TestProcessor::CID {
            ComWrapper::new(TestProcessor {
                gain: Cell::new(0.5),
                held: Cell::new(0),
            })
            .to_com_ptr::<FUnknown>()
        } else if cid == TestController::CID {
            ComWrapper::new(TestController {
                values: Mutex::new([0.0, 0.5, 0.0, 0.0]),
                host: Mutex::new(None),
                ui_size: Mutex::new(0),
            })
            .to_com_ptr::<FUnknown>()
        } else {
            None
        };
        match unknown {
            Some(u) => unsafe {
                ((*(*u.as_ptr()).vtbl).queryInterface)(u.as_ptr(), iid as *const TUID, obj)
            },
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for TestFactory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let info = unsafe { &mut *info };
        let (cid, cat, name) = match index {
            0 => (TestProcessor::CID, "Audio Module Class", "Test Gain"),
            1 => (
                TestController::CID,
                "Component Controller Class",
                "Test Gain",
            ),
            _ => return kInvalidArgument,
        };
        info.cid = cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy(&mut info.category, cat);
        copy(&mut info.name, name);
        copy(&mut info.subCategories, "Fx|Dynamics");
        copy(&mut info.vendor, "FaderFrame Tests");
        copy(&mut info.version, "1.2.3");
        kResultOk
    }
}

// --- harness --------------------------------------------------------------

fn install() -> scan::ScannedPlugin {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let factory = ComWrapper::new(TestFactory)
            .to_com_ptr::<IPluginFactory>()
            .unwrap();
        let m = module::register(PathBuf::from("/test/TestGain.vst3"), factory);
        faderframe_plugin_vst3::extend_catalog(scan::describe_module(m));
    });
    faderframe_plugin_vst3::catalog()
        .into_iter()
        .find(|p| p.name == "Test Gain")
        .unwrap()
}

const BLOCK: usize = 128;

struct Rig {
    input: Vec<AudioBuffer>,
    output: Vec<AudioBuffer>,
    events: Vec<MidiBuffer>,
    events_out: Vec<MidiBuffer>,
    transport: TransportInfo,
}

impl Rig {
    fn new() -> Self {
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, BLOCK);
        input.set_len(BLOCK);
        let mut output = AudioBuffer::new(ChannelLayout::Stereo, BLOCK);
        output.set_len(BLOCK);
        for c in 0..2 {
            input.channel_mut(c).fill(1.0);
        }
        Self {
            input: vec![input],
            output: vec![output],
            events: vec![MidiBuffer::with_capacity(64)],
            events_out: vec![MidiBuffer::with_capacity(64)],
            transport: TransportInfo::default(),
        }
    }

    fn run(&mut self, p: &mut dyn PluginProcessor, params: &[ParameterEvent]) -> Vec<f32> {
        let ctx = PluginProcessContext {
            transport: &self.transport,
            param_events: params,
        };
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &self.input,
            audio_out: &mut self.output,
            events_in: &self.events,
            events_out: &mut self.events_out,
        };
        COUNT.with(|c| c.set(true));
        p.process(&ctx, &mut io);
        COUNT.with(|c| c.set(false));
        self.events[0].clear();
        self.output[0].channel(0).to_vec()
    }
}

fn instantiate() -> Box<dyn PluginInstance> {
    let p = install();
    Vst3Factory::new().instantiate(&p.id).unwrap()
}

const CONFIG: ProcessConfig = ProcessConfig {
    sample_rate: 48_000.0,
    max_block_size: BLOCK as u32,
};

#[test]
fn scanning_describes_buses_and_categories() {
    let p = install();
    assert_eq!(p.vendor, "FaderFrame Tests");
    assert_eq!(p.version, "1.2.3");
    assert_eq!(p.features, vec!["audio-effect", "dynamics"]);
    assert_eq!(p.audio_inputs, vec![2]);
    assert_eq!(p.audio_outputs, vec![2]);
    assert_eq!(p.note_inputs, 1);
    assert_eq!(p.id, util::tuid_hex(&TestProcessor::CID));
}

#[test]
fn hosts_a_plugin_with_separate_controller() {
    let mut inst = instantiate();
    // The controller greeted the processor through a host message.
    assert_eq!(MESSAGE_VALUE.load(Ordering::Relaxed), 42);
    let params = inst.parameters().to_vec();
    assert_eq!(params.len(), 3);
    let mode = params.iter().find(|p| p.id.0 == MODE).unwrap();
    assert!(mode.stepped);
    assert_eq!(mode.max, 2.0);
    assert!(!params.iter().find(|p| p.id.0 == METER).unwrap().automatable);
    assert_eq!(
        inst.format_parameter(ParameterId(GAIN), 0.25).as_deref(),
        Some("x0.50")
    );

    let mut proc = inst.create_processor(&CONFIG).unwrap();
    assert!(PROCESSING.load(Ordering::Relaxed));
    assert_eq!(inst.latency_samples(), LATENCY);
    let mut rig = Rig::new();
    ALLOCS.store(0, Ordering::Relaxed);
    // Default gain 0.5 → ×1.
    assert_eq!(rig.run(proc.as_mut(), &[])[0], 1.0);

    // A UI change reaches the processor (and the controller).
    inst.set_parameter(ParameterId(GAIN), 0.25).unwrap();
    assert_eq!(rig.run(proc.as_mut(), &[])[0], 0.5);
    assert_eq!(inst.parameter(ParameterId(GAIN)), Some(0.25));

    // Sample-accurate automation.
    let ev = [ParameterEvent {
        sample_offset: 32,
        parameter: ParameterId(GAIN),
        value: 1.0,
    }];
    let out = rig.run(proc.as_mut(), &ev);
    assert_eq!(out[31], 0.5);
    assert_eq!(out[32], 2.0);

    // Notes are events; CC 7 is mapped to the gain by the plugin.
    rig.events[0]
        .push(TimedMidiEvent {
            sample_offset: 10,
            event: MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 100,
            },
        })
        .unwrap();
    rig.events[0]
        .push(TimedMidiEvent {
            sample_offset: 64,
            event: MidiEvent::ControlChange {
                channel: 0,
                controller: 7,
                value: 127,
            },
        })
        .unwrap();
    let out = rig.run(proc.as_mut(), &[]);
    assert_eq!(out[9], 2.0);
    assert_eq!(out[10], 2.25);
    assert_eq!(out[64], 2.25, "CC 7 → gain 1.0 (unchanged)");
    rig.events[0]
        .push(TimedMidiEvent {
            sample_offset: 0,
            event: MidiEvent::ControlChange {
                channel: 3,
                controller: 7,
                value: 0,
            },
        })
        .unwrap();
    rig.events[0]
        .push(TimedMidiEvent {
            sample_offset: 5,
            event: MidiEvent::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0,
            },
        })
        .unwrap();
    let out = rig.run(proc.as_mut(), &[]);
    assert_eq!(out[0], 0.25, "gain 0 + one held note");
    assert_eq!(out[5], 0.0);
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed),
        0,
        "processing must not allocate"
    );

    // An edit in the plugin's editor reaches the processor via the host.
    let handler = HANDLER.lock().unwrap().clone().unwrap();
    unsafe {
        handler.beginEdit(GAIN);
        handler.performEdit(GAIN, 0.75);
        handler.endEdit(GAIN);
    }
    let poll = inst.poll();
    assert!(poll.state_dirty);
    assert_eq!(rig.run(proc.as_mut(), &[])[0], 1.5);

    // The processor's meter value reaches the controller.
    inst.poll();
    let meter = inst.parameter(ParameterId(METER)).unwrap();
    assert!((meter - 1.5 / 4.0).abs() < 1e-6, "meter {meter}");

    // Stepped parameters are integers in FaderFrame, 0–1 in VST3.
    inst.set_parameter(ParameterId(MODE), 2.0).unwrap();
    assert_eq!(inst.parameter(ParameterId(MODE)), Some(2.0));

    // State: component and controller parts round-trip.
    let saved = inst.save_state().unwrap();
    assert!(saved.starts_with(b"FFV3"));
    inst.set_parameter(ParameterId(GAIN), 0.1).unwrap();
    rig.run(proc.as_mut(), &[]);
    inst.load_state(&saved).unwrap();
    assert_eq!(inst.parameter(ParameterId(GAIN)), Some(0.75));
    assert_eq!(rig.run(proc.as_mut(), &[])[0], 1.5);

    // Reset (stop/locate) cycles processing; dropping deactivates.
    proc.reset();
    assert!(PROCESSING.load(Ordering::Relaxed));
    drop(proc);
    drop(inst);
    assert!(!PROCESSING.load(Ordering::Relaxed));
}

/// `FADERFRAME_TEST_VST3=/path/to/Plugin.vst3 cargo test -p
/// faderframe-plugin-vst3 -- --ignored`: describe, instantiate, process and
/// save/restore a real plugin.
#[test]
#[ignore = "needs an installed VST3 plugin (FADERFRAME_TEST_VST3)"]
fn hosts_an_installed_plugin() {
    let Some(bundle) = std::env::var_os("FADERFRAME_TEST_VST3").map(PathBuf::from) else {
        return;
    };
    let plugins = scan::describe_bundle(&bundle).unwrap();
    assert!(!plugins.is_empty(), "no plugins in {}", bundle.display());
    faderframe_plugin_vst3::extend_catalog(plugins.clone());
    for p in &plugins {
        eprintln!(
            "{} by {} ({:?}): in {:?} out {:?} notes {}",
            p.name, p.vendor, p.features, p.audio_inputs, p.audio_outputs, p.note_inputs
        );
        let mut inst = Vst3Factory::new().instantiate(&p.id).unwrap();
        eprintln!("  {} parameters", inst.parameters().len());
        let mut proc = inst.create_processor(&CONFIG).unwrap();
        eprintln!("  latency {}", inst.latency_samples());
        let mut rig = Rig::new();
        for _ in 0..200 {
            let out = rig.run(proc.as_mut(), &[]);
            assert!(out.iter().all(|v| v.is_finite()));
            inst.poll();
        }
        let state = inst.save_state().unwrap();
        eprintln!("  state {} bytes", state.len());
        inst.load_state(&state).unwrap();
        rig.run(proc.as_mut(), &[]);
        let editor = inst.editor().map(|e| e.can_embed_x11());
        eprintln!("  editor: {editor:?}");
    }
}
