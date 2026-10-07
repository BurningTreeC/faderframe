//! The host against an LV2 plugin written here (linked into the test, no
//! bundle needed): ports, sample-accurate automation, latency, MIDI and
//! the transport as atoms, the worker, state, and no heap use on the
//! audio thread.
#![allow(clippy::unwrap_used, unsafe_code)]

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::{PluginInstance, PluginProcessContext, ProcessConfig};
use faderframe_plugin_lv2::sys::{self, LV2_Descriptor, LV2_Feature, LV2_Handle};
use faderframe_plugin_lv2::{Lv2Instance, scan};
use faderframe_transport::TransportInfo;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;

// --- counting allocator (this thread, inside `no_heap`) ------------------------------

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

struct Counting;

fn count() {
    let _ = COUNT.try_with(|c| {
        if let Some(n) = c.get() {
            c.set(Some(n + 1));
        }
    });
}

// SAFETY: delegates to System with the same contract.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        // SAFETY: as the caller's.
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        // SAFETY: as the caller's.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        // SAFETY: as the caller's.
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        count();
        // SAFETY: as the caller's.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn heap_events(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(Some(0)));
    f();
    COUNT.with(|c| c.replace(None)).unwrap_or(0)
}

// --- the plugin ----------------------------------------------------------------------

const URI: &CStr = c"urn:faderframe:test#host";

const P_IN: usize = 0;
const P_OUT: usize = 1;
const P_GAIN: usize = 2;
const P_LATENCY: usize = 3;
const P_CONTROL: usize = 4;

struct Test {
    ports: [*mut c_void; 5],
    map: *const sys::LV2_URID_Map,
    schedule: *const sys::LV2_Worker_Schedule,
    notes: i32,
    worked: i32,
    speed: f32,
}

fn urid(t: &Test, uri: &CStr) -> u32 {
    // SAFETY: the host's map feature.
    unsafe { ((*t.map).map.unwrap())((*t.map).handle, uri.as_ptr()) }
}

unsafe extern "C" fn instantiate(
    _: *const LV2_Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    features: *const *const LV2_Feature,
) -> LV2_Handle {
    let mut t = Box::new(Test {
        ports: [std::ptr::null_mut(); 5],
        map: std::ptr::null(),
        schedule: std::ptr::null(),
        notes: 0,
        worked: 0,
        speed: -1.0,
    });
    let mut i = 0;
    // SAFETY: a null-terminated feature list.
    unsafe {
        while !(*features.add(i)).is_null() {
            let f = &**features.add(i);
            match CStr::from_ptr(f.uri).to_str().unwrap() {
                sys::uri::URID_MAP => t.map = f.data.cast(),
                sys::uri::WORKER_SCHEDULE => t.schedule = f.data.cast(),
                _ => {}
            }
            i += 1;
        }
    }
    assert!(!t.map.is_null() && !t.schedule.is_null());
    Box::into_raw(t).cast()
}

unsafe extern "C" fn connect(h: LV2_Handle, port: u32, data: *mut c_void) {
    // SAFETY: our handle.
    let t = unsafe { &mut *h.cast::<Test>() };
    t.ports[port as usize] = data;
}

unsafe extern "C" fn run(h: LV2_Handle, n: u32) {
    // SAFETY: our handle; ports connected by the host.
    unsafe {
        let t = &mut *h.cast::<Test>();
        let gain = *t.ports[P_GAIN].cast::<f32>();
        let input = std::slice::from_raw_parts(t.ports[P_IN].cast::<f32>(), n as usize);
        let out = std::slice::from_raw_parts_mut(t.ports[P_OUT].cast::<f32>(), n as usize);
        for (o, i) in out.iter_mut().zip(input) {
            *o = i * gain;
        }
        *t.ports[P_LATENCY].cast::<f32>() = 32.0;
        let midi = urid(t, c"http://lv2plug.in/ns/ext/midi#MidiEvent");
        let object = urid(t, c"http://lv2plug.in/ns/ext/atom#Object");
        let speed_key = urid(t, c"http://lv2plug.in/ns/ext/time#speed");
        // The sequence: header (size, type), body (unit, pad), events.
        let seq = t.ports[P_CONTROL].cast::<u8>();
        let size = *seq.cast::<u32>() as usize;
        let mut at = 16;
        while at + 16 <= 8 + size {
            let ev = seq.add(at);
            let len = *ev.add(8).cast::<u32>() as usize;
            let ty = *ev.add(12).cast::<u32>();
            let body = std::slice::from_raw_parts(ev.add(16), len);
            if ty == midi && body[0] & 0xF0 == 0x90 {
                t.notes += 1;
                let s = &*t.schedule;
                let msg = t.notes;
                (s.schedule_work.unwrap())(s.handle, 4, (&msg as *const i32).cast());
            } else if ty == object {
                // id, otype, then key, context, atom header, value.
                let mut p = 8;
                while p + 16 <= len {
                    let key = u32::from_ne_bytes(body[p..p + 4].try_into().unwrap());
                    let vlen = u32::from_ne_bytes(body[p + 8..p + 12].try_into().unwrap()) as usize;
                    if key == speed_key {
                        t.speed = f32::from_ne_bytes(body[p + 16..p + 20].try_into().unwrap());
                    }
                    p += 16 + vlen.div_ceil(8) * 8;
                }
            }
            at += 16 + len.div_ceil(8) * 8;
        }
    }
}

unsafe extern "C" fn cleanup(h: LV2_Handle) {
    // SAFETY: made by `instantiate`.
    drop(unsafe { Box::from_raw(h.cast::<Test>()) });
}

unsafe extern "C" fn work(
    _h: LV2_Handle,
    respond: sys::LV2_Worker_Respond_Function,
    handle: *mut c_void,
    size: u32,
    data: *const c_void,
) -> u32 {
    // SAFETY: the host's respond function with the message it gave.
    unsafe { respond(handle, size, data) }
}

unsafe extern "C" fn work_response(h: LV2_Handle, _size: u32, _body: *const c_void) -> u32 {
    // SAFETY: our handle.
    unsafe { (*h.cast::<Test>()).worked += 1 };
    0
}

unsafe extern "C" fn save(
    h: LV2_Handle,
    store: sys::LV2_State_Store_Function,
    handle: *mut c_void,
    _flags: u32,
    _features: *const *const LV2_Feature,
) -> u32 {
    // SAFETY: our handle and the host's store.
    unsafe {
        let t = &*h.cast::<Test>();
        let int = urid(t, c"http://lv2plug.in/ns/ext/atom#Int");
        let float = urid(t, c"http://lv2plug.in/ns/ext/atom#Float");
        let flags = sys::LV2_STATE_IS_POD | sys::LV2_STATE_IS_PORTABLE;
        for (key, v) in [
            (c"urn:faderframe:test#notes", t.notes),
            (c"urn:faderframe:test#worked", t.worked),
        ] {
            store(
                handle,
                urid(t, key),
                (&v as *const i32).cast(),
                4,
                int,
                flags,
            );
        }
        store(
            handle,
            urid(t, c"urn:faderframe:test#speed"),
            (&t.speed as *const f32).cast(),
            4,
            float,
            flags,
        );
    }
    0
}

unsafe extern "C" fn restore(
    h: LV2_Handle,
    retrieve: sys::LV2_State_Retrieve_Function,
    handle: *mut c_void,
    _flags: u32,
    _features: *const *const LV2_Feature,
) -> u32 {
    // SAFETY: our handle and the host's retrieve.
    unsafe {
        let t = &mut *h.cast::<Test>();
        let (mut size, mut ty, mut fl) = (0usize, 0u32, 0u32);
        let v = retrieve(
            handle,
            urid(t, c"urn:faderframe:test#notes"),
            &mut size,
            &mut ty,
            &mut fl,
        );
        if !v.is_null() && size == 4 {
            t.notes = *v.cast::<i32>();
        }
    }
    0
}

static WORKER: sys::LV2_Worker_Interface = sys::LV2_Worker_Interface {
    work: Some(work),
    work_response: Some(work_response),
    end_run: None,
};

static STATE: sys::LV2_State_Interface = sys::LV2_State_Interface {
    save: Some(save),
    restore: Some(restore),
};

unsafe extern "C" fn extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: a C string from the host.
    match unsafe { CStr::from_ptr(uri) }.to_str().unwrap() {
        sys::uri::WORKER_INTERFACE => (&WORKER as *const sys::LV2_Worker_Interface).cast(),
        sys::uri::STATE_INTERFACE => (&STATE as *const sys::LV2_State_Interface).cast(),
        _ => std::ptr::null(),
    }
}

struct Descriptor(LV2_Descriptor);
// SAFETY: immutable after start.
unsafe impl Sync for Descriptor {}

static DESCRIPTOR: Descriptor = Descriptor(LV2_Descriptor {
    uri: URI.as_ptr(),
    instantiate: Some(instantiate),
    connect_port: Some(connect),
    activate: None,
    run: Some(run),
    deactivate: None,
    cleanup: Some(cleanup),
    extension_data: Some(extension_data),
});

unsafe extern "C" fn entry(index: u32) -> *const LV2_Descriptor {
    if index == 0 {
        &DESCRIPTOR.0
    } else {
        std::ptr::null()
    }
}

/// What its bundle would say about it.
fn model() -> Arc<scan::Lv2Plugin> {
    // A bundle of each call's own: the tests run side by side, and one
    // removing a shared bundle left the other scanning nothing.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ff-lv2-host-{}-{call}.lv2", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.ttl"),
        r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
@prefix work: <http://lv2plug.in/ns/ext/worker#> .
<urn:faderframe:test#host> a lv2:Plugin , lv2:AmplifierPlugin ;
    lv2:binary <none.so> ;
    <http://usefulinc.com/ns/doap#name> "Test Host" ;
    lv2:requiredFeature <http://lv2plug.in/ns/ext/urid#map> , work:schedule ;
    lv2:extensionData work:interface , <http://lv2plug.in/ns/ext/state#interface> ;
    lv2:port [ a lv2:InputPort , lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in" ; lv2:name "In" ] ,
        [ a lv2:OutputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "out" ; lv2:name "Out" ] ,
        [ a lv2:InputPort , lv2:ControlPort ; lv2:index 2 ; lv2:symbol "gain" ; lv2:name "Gain" ;
          lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 2.0 ] ,
        [ a lv2:OutputPort , lv2:ControlPort ; lv2:index 3 ; lv2:symbol "latency" ; lv2:name "Latency" ;
          lv2:portProperty lv2:reportsLatency ] ,
        [ a lv2:InputPort , atom:AtomPort ; atom:bufferType atom:Sequence ; lv2:index 4 ;
          lv2:symbol "control" ; lv2:name "Control" ;
          atom:supports <http://lv2plug.in/ns/ext/midi#MidiEvent> , <http://lv2plug.in/ns/ext/time#Position> ] .
"#,
    )
    .unwrap();
    let mut b = scan::describe_bundle(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(b.plugins.len(), 1);
    Arc::new(b.plugins.remove(0))
}

fn state_value(i: &mut Lv2Instance, key: &str) -> Vec<u8> {
    let bytes = i.save_state().unwrap();
    let s = faderframe_plugin_lv2::state::State::from_bytes(&bytes).unwrap();
    s.properties
        .iter()
        .find(|p| p.key == key)
        .map(|p| p.value.clone())
        .unwrap_or_default()
}

fn int(v: &[u8]) -> i32 {
    i32::from_le_bytes(v[..4].try_into().unwrap())
}

#[test]
fn a_plugin_runs_with_the_host_features() {
    let model = model();
    let mut inst = Lv2Instance::with_entry(Arc::clone(&model), entry).unwrap();
    assert_eq!(
        inst.parameters().len(),
        1,
        "gain only (latency is an output)"
    );
    let config = ProcessConfig {
        sample_rate: 44_100.0,
        max_block_size: 256,
        sidechain: false,
        double_precision: false,
    };
    let mut proc = inst.create_processor(&config).unwrap();
    assert_eq!(inst.latency_samples(), 32, "measured after activation");

    let mut inputs = vec![AudioBuffer::new(ChannelLayout::Mono, 256)];
    inputs[0].set_len(256);
    inputs[0].channel_mut(0).fill(0.5);
    let mut outputs = vec![AudioBuffer::new(ChannelLayout::Mono, 256)];
    outputs[0].set_len(256);
    let mut midi = vec![MidiBuffer::with_capacity(64)];
    let mut transport = TransportInfo {
        sample_rate: 44_100.0,
        ..TransportInfo::default()
    };
    let block = |proc: &mut Box<dyn faderframe_plugin_host::PluginProcessor>,
                 midi: &[MidiBuffer],
                 outputs: &mut [AudioBuffer],
                 transport: &TransportInfo,
                 events: &[ParameterEvent]| {
        let mut io = NodeIo {
            frames: 256,
            audio_in: &inputs,
            audio_out: outputs,
            events_in: midi,
            events_out: &mut [],
        };
        let ctx = PluginProcessContext {
            transport,
            param_events: events,
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        proc.process(&ctx, &mut io);
    };
    block(&mut proc, &midi, &mut outputs, &transport, &[]);
    assert!(
        outputs[0]
            .channel(0)
            .iter()
            .all(|s| (*s - 0.5).abs() < 1e-6)
    );
    // A host change reaches the next block; automation splits one.
    inst.set_parameter(ParameterId(2), 2.0).unwrap();
    block(
        &mut proc,
        &midi,
        &mut outputs,
        &transport,
        &[ParameterEvent {
            parameter: ParameterId(2),
            value: 0.5,
            sample_offset: 100,
        }],
    );
    let out = outputs[0].channel(0);
    assert!(
        out[..100].iter().all(|s| (*s - 1.0).abs() < 1e-6),
        "{}",
        out[0]
    );
    assert!(
        out[100..].iter().all(|s| (*s - 0.25).abs() < 1e-6),
        "{}",
        out[100]
    );
    assert_eq!(inst.parameter(ParameterId(2)), Some(0.5));

    // Notes as MIDI atoms; the worker answers each one.
    for at in [3, 200] {
        midi[0]
            .push(TimedMidiEvent::new(
                at,
                MidiEvent::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 90,
                },
            ))
            .unwrap();
    }
    transport.playing = true;
    block(&mut proc, &midi, &mut outputs, &transport, &[]);
    midi[0].clear();
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        block(&mut proc, &midi, &mut outputs, &transport, &[]);
    }
    assert_eq!(int(&state_value(&mut inst, "urn:faderframe:test#notes")), 2);
    assert_eq!(
        int(&state_value(&mut inst, "urn:faderframe:test#worked")),
        2
    );
    let speed = state_value(&mut inst, "urn:faderframe:test#speed");
    assert_eq!(
        f32::from_le_bytes(speed[..4].try_into().unwrap()),
        1.0,
        "the transport plays"
    );

    // State goes to a second instance (ports by symbol, properties).
    let saved = inst.save_state().unwrap();
    let mut other = Lv2Instance::with_entry(Arc::clone(&model), entry).unwrap();
    other.load_state(&saved).unwrap();
    assert_eq!(other.parameter(ParameterId(2)), Some(0.5));
    assert_eq!(
        int(&state_value(&mut other, "urn:faderframe:test#notes")),
        2
    );

    // The audio thread's work allocates nothing: notes, automation,
    // transport changes, worker replies.
    for round in 0..20u32 {
        midi[0]
            .push(TimedMidiEvent::new(
                round % 200,
                MidiEvent::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 90,
                },
            ))
            .unwrap();
        transport.playing = round % 3 != 0;
        transport.sample_position += 256;
        let events = [ParameterEvent {
            parameter: ParameterId(2),
            value: (round % 4) as f32 * 0.25,
            sample_offset: 17 * (round % 10),
        }];
        let n = heap_events(|| block(&mut proc, &midi, &mut outputs, &transport, &events));
        assert_eq!(n, 0, "round {round}: {n} heap operations");
        midi[0].clear();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn a_rate_change_makes_the_plugin_again_with_its_state() {
    let mut inst = Lv2Instance::with_entry(model(), entry).unwrap();
    inst.set_parameter(ParameterId(2), 1.5).unwrap();
    let cfg = |rate: f64| ProcessConfig {
        sample_rate: rate,
        max_block_size: 512,
        sidechain: false,
        double_precision: false,
    };
    let _a = inst.create_processor(&cfg(48_000.0)).unwrap();
    let first = inst.activation();
    let _b = inst.create_processor(&cfg(48_000.0)).unwrap();
    assert_eq!(inst.activation(), first, "the same configuration keeps it");
    let _c = inst.create_processor(&cfg(96_000.0)).unwrap();
    assert!(inst.activation() > first, "another rate restarts it");
    assert_eq!(inst.parameter(ParameterId(2)), Some(1.5));
}
