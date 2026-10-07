//! The C ABI of LV2 and the extensions the host speaks (from the LV2
//! headers, ISC: lv2/core/lv2.h, urid, options, worker, state, atom, ui,
//! log, buf-size).

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void};

pub type LV2_Handle = *mut c_void;
pub type LV2_URID = u32;

#[repr(C)]
pub struct LV2_Feature {
    pub uri: *const c_char,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct LV2_Descriptor {
    pub uri: *const c_char,
    pub instantiate: Option<
        unsafe extern "C" fn(
            descriptor: *const LV2_Descriptor,
            sample_rate: f64,
            bundle_path: *const c_char,
            features: *const *const LV2_Feature,
        ) -> LV2_Handle,
    >,
    pub connect_port: Option<unsafe extern "C" fn(LV2_Handle, port: u32, data: *mut c_void)>,
    pub activate: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub run: Option<unsafe extern "C" fn(LV2_Handle, sample_count: u32)>,
    pub deactivate: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub cleanup: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub extension_data: Option<unsafe extern "C" fn(uri: *const c_char) -> *const c_void>,
}

pub type LV2_Descriptor_Function = unsafe extern "C" fn(index: u32) -> *const LV2_Descriptor;

#[repr(C)]
pub struct LV2_URID_Map {
    pub handle: *mut c_void,
    pub map: Option<unsafe extern "C" fn(*mut c_void, uri: *const c_char) -> LV2_URID>,
}

#[repr(C)]
pub struct LV2_URID_Unmap {
    pub handle: *mut c_void,
    pub unmap: Option<unsafe extern "C" fn(*mut c_void, urid: LV2_URID) -> *const c_char>,
}

pub const LV2_OPTIONS_INSTANCE: u32 = 0;

#[repr(C)]
pub struct LV2_Options_Option {
    pub context: u32,
    pub subject: u32,
    pub key: LV2_URID,
    pub size: u32,
    pub type_: LV2_URID,
    pub value: *const c_void,
}

pub const LV2_WORKER_SUCCESS: u32 = 0;
pub const LV2_WORKER_ERR_NO_SPACE: u32 = 2;

pub type LV2_Worker_Respond_Function =
    unsafe extern "C" fn(handle: *mut c_void, size: u32, data: *const c_void) -> u32;

#[repr(C)]
pub struct LV2_Worker_Interface {
    pub work: Option<
        unsafe extern "C" fn(
            LV2_Handle,
            respond: LV2_Worker_Respond_Function,
            handle: *mut c_void,
            size: u32,
            data: *const c_void,
        ) -> u32,
    >,
    pub work_response:
        Option<unsafe extern "C" fn(LV2_Handle, size: u32, body: *const c_void) -> u32>,
    pub end_run: Option<unsafe extern "C" fn(LV2_Handle) -> u32>,
}

#[repr(C)]
pub struct LV2_Worker_Schedule {
    pub handle: *mut c_void,
    pub schedule_work:
        Option<unsafe extern "C" fn(*mut c_void, size: u32, data: *const c_void) -> u32>,
}

pub const LV2_STATE_IS_POD: u32 = 1;
pub const LV2_STATE_IS_PORTABLE: u32 = 2;
pub const LV2_STATE_SUCCESS: u32 = 0;

pub type LV2_State_Store_Function = unsafe extern "C" fn(
    handle: *mut c_void,
    key: u32,
    value: *const c_void,
    size: usize,
    type_: u32,
    flags: u32,
) -> u32;

pub type LV2_State_Retrieve_Function = unsafe extern "C" fn(
    handle: *mut c_void,
    key: u32,
    size: *mut usize,
    type_: *mut u32,
    flags: *mut u32,
) -> *const c_void;

#[repr(C)]
pub struct LV2_State_Interface {
    pub save: Option<
        unsafe extern "C" fn(
            LV2_Handle,
            store: LV2_State_Store_Function,
            handle: *mut c_void,
            flags: u32,
            features: *const *const LV2_Feature,
        ) -> u32,
    >,
    pub restore: Option<
        unsafe extern "C" fn(
            LV2_Handle,
            retrieve: LV2_State_Retrieve_Function,
            handle: *mut c_void,
            flags: u32,
            features: *const *const LV2_Feature,
        ) -> u32,
    >,
}

#[repr(C)]
pub struct LV2_State_Map_Path {
    pub handle: *mut c_void,
    pub abstract_path: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char>,
    pub absolute_path: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char>,
}

#[repr(C)]
pub struct LV2_State_Free_Path {
    pub handle: *mut c_void,
    pub free_path: Option<unsafe extern "C" fn(*mut c_void, *mut c_char)>,
}

#[repr(C)]
pub struct LV2_Log_Log {
    pub handle: *mut c_void,
    /// Variadic in C (a shim of our own, `log.c`).
    pub printf: *const c_void,
    pub vprintf: *const c_void,
}

/// An atom's header.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LV2_Atom {
    pub size: u32,
    pub type_: u32,
}

/// A sequence's body header (time in frames).
#[repr(C)]
pub struct LV2_Atom_Sequence_Body {
    pub unit: u32,
    pub pad: u32,
}

/// An event of a sequence: its time, then the atom.
#[repr(C)]
pub struct LV2_Atom_Event {
    pub frames: i64,
    pub body: LV2_Atom,
}

// ---- UIs ----

pub type LV2UI_Handle = *mut c_void;
pub type LV2UI_Controller = *mut c_void;
pub type LV2UI_Widget = *mut c_void;

pub type LV2UI_Write_Function = unsafe extern "C" fn(
    controller: LV2UI_Controller,
    port_index: u32,
    buffer_size: u32,
    port_protocol: u32,
    buffer: *const c_void,
);

#[repr(C)]
pub struct LV2UI_Descriptor {
    pub uri: *const c_char,
    pub instantiate: Option<
        unsafe extern "C" fn(
            descriptor: *const LV2UI_Descriptor,
            plugin_uri: *const c_char,
            bundle_path: *const c_char,
            write_function: LV2UI_Write_Function,
            controller: LV2UI_Controller,
            widget: *mut LV2UI_Widget,
            features: *const *const LV2_Feature,
        ) -> LV2UI_Handle,
    >,
    pub cleanup: Option<unsafe extern "C" fn(LV2UI_Handle)>,
    pub port_event: Option<
        unsafe extern "C" fn(
            LV2UI_Handle,
            port_index: u32,
            buffer_size: u32,
            format: u32,
            buffer: *const c_void,
        ),
    >,
    pub extension_data: Option<unsafe extern "C" fn(uri: *const c_char) -> *const c_void>,
}

pub type LV2UI_Descriptor_Function = unsafe extern "C" fn(index: u32) -> *const LV2UI_Descriptor;

#[repr(C)]
pub struct LV2UI_Resize {
    pub handle: *mut c_void,
    pub ui_resize: Option<unsafe extern "C" fn(*mut c_void, width: i32, height: i32) -> i32>,
}

#[repr(C)]
pub struct LV2UI_Idle_Interface {
    pub idle: Option<unsafe extern "C" fn(LV2UI_Handle) -> i32>,
}

#[repr(C)]
pub struct LV2UI_Show_Interface {
    pub show: Option<unsafe extern "C" fn(LV2UI_Handle) -> i32>,
    pub hide: Option<unsafe extern "C" fn(LV2UI_Handle) -> i32>,
}

#[repr(C)]
pub struct LV2UI_Port_Map {
    pub handle: *mut c_void,
    pub port_index: Option<unsafe extern "C" fn(*mut c_void, symbol: *const c_char) -> u32>,
}

#[repr(C)]
pub struct LV2UI_Touch {
    pub handle: *mut c_void,
    pub touch: Option<unsafe extern "C" fn(*mut c_void, port_index: u32, grabbed: bool)>,
}

/// `LV2UI_INVALID_PORT_INDEX`.
pub const LV2UI_INVALID_PORT_INDEX: u32 = u32::MAX;

/// URIs the host uses.
pub mod uri {
    pub const LV2: &str = "http://lv2plug.in/ns/lv2core#";
    pub const URID_MAP: &str = "http://lv2plug.in/ns/ext/urid#map";
    pub const URID_UNMAP: &str = "http://lv2plug.in/ns/ext/urid#unmap";
    pub const OPTIONS: &str = "http://lv2plug.in/ns/ext/options#options";
    pub const BOUNDED_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
    pub const POW2_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#powerOf2BlockLength";
    pub const MIN_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#minBlockLength";
    pub const MAX_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
    pub const NOMINAL_BLOCK: &str = "http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
    pub const SEQUENCE_SIZE: &str = "http://lv2plug.in/ns/ext/buf-size#sequenceSize";
    pub const SAMPLE_RATE: &str = "http://lv2plug.in/ns/ext/parameters#sampleRate";
    pub const WORKER_SCHEDULE: &str = "http://lv2plug.in/ns/ext/worker#schedule";
    pub const WORKER_INTERFACE: &str = "http://lv2plug.in/ns/ext/worker#interface";
    pub const STATE_INTERFACE: &str = "http://lv2plug.in/ns/ext/state#interface";
    pub const STATE_MAP_PATH: &str = "http://lv2plug.in/ns/ext/state#mapPath";
    pub const STATE_FREE_PATH: &str = "http://lv2plug.in/ns/ext/state#freePath";
    pub const STATE_LOAD_DEFAULT: &str = "http://lv2plug.in/ns/ext/state#loadDefaultState";
    pub const LOG: &str = "http://lv2plug.in/ns/ext/log#log";
    pub const LOG_ERROR: &str = "http://lv2plug.in/ns/ext/log#Error";
    pub const LOG_WARNING: &str = "http://lv2plug.in/ns/ext/log#Warning";
    pub const LOG_NOTE: &str = "http://lv2plug.in/ns/ext/log#Note";
    pub const LOG_TRACE: &str = "http://lv2plug.in/ns/ext/log#Trace";
    pub const HARD_RT: &str = "http://lv2plug.in/ns/lv2core#hardRTCapable";
    pub const IS_LIVE: &str = "http://lv2plug.in/ns/lv2core#isLive";
    pub const IN_PLACE_BROKEN: &str = "http://lv2plug.in/ns/lv2core#inPlaceBroken";
    pub const ATOM_SEQUENCE: &str = "http://lv2plug.in/ns/ext/atom#Sequence";
    pub const ATOM_CHUNK: &str = "http://lv2plug.in/ns/ext/atom#Chunk";
    pub const ATOM_INT: &str = "http://lv2plug.in/ns/ext/atom#Int";
    pub const ATOM_LONG: &str = "http://lv2plug.in/ns/ext/atom#Long";
    pub const ATOM_FLOAT: &str = "http://lv2plug.in/ns/ext/atom#Float";
    pub const ATOM_DOUBLE: &str = "http://lv2plug.in/ns/ext/atom#Double";
    pub const ATOM_OBJECT: &str = "http://lv2plug.in/ns/ext/atom#Object";
    pub const ATOM_BLANK: &str = "http://lv2plug.in/ns/ext/atom#Blank";
    pub const ATOM_EVENT_TRANSFER: &str = "http://lv2plug.in/ns/ext/atom#eventTransfer";
    pub const ATOM_ATOM_TRANSFER: &str = "http://lv2plug.in/ns/ext/atom#atomTransfer";
    pub const MIDI_EVENT: &str = "http://lv2plug.in/ns/ext/midi#MidiEvent";
    pub const TIME_POSITION: &str = "http://lv2plug.in/ns/ext/time#Position";
    pub const TIME_FRAME: &str = "http://lv2plug.in/ns/ext/time#frame";
    pub const TIME_SPEED: &str = "http://lv2plug.in/ns/ext/time#speed";
    pub const TIME_BAR: &str = "http://lv2plug.in/ns/ext/time#bar";
    pub const TIME_BAR_BEAT: &str = "http://lv2plug.in/ns/ext/time#barBeat";
    pub const TIME_BEAT: &str = "http://lv2plug.in/ns/ext/time#beat";
    pub const TIME_BPM: &str = "http://lv2plug.in/ns/ext/time#beatsPerMinute";
    pub const TIME_BEATS_PER_BAR: &str = "http://lv2plug.in/ns/ext/time#beatsPerBar";
    pub const TIME_BEAT_UNIT: &str = "http://lv2plug.in/ns/ext/time#beatUnit";
    pub const UI_PARENT: &str = "http://lv2plug.in/ns/extensions/ui#parent";
    pub const UI_RESIZE: &str = "http://lv2plug.in/ns/extensions/ui#resize";
    pub const UI_IDLE: &str = "http://lv2plug.in/ns/extensions/ui#idleInterface";
    pub const UI_SHOW: &str = "http://lv2plug.in/ns/extensions/ui#showInterface";
    pub const UI_PORT_MAP: &str = "http://lv2plug.in/ns/extensions/ui#portMap";
    pub const UI_TOUCH: &str = "http://lv2plug.in/ns/extensions/ui#touch";
    pub const UI_REQUEST_VALUE: &str = "http://lv2plug.in/ns/extensions/ui#requestValue";
    pub const UI_NO_USER_RESIZE: &str = "http://lv2plug.in/ns/extensions/ui#noUserResize";
    pub const UI_FIXED_SIZE: &str = "http://lv2plug.in/ns/extensions/ui#fixedSize";
    pub const OPTIONS_INTERFACE: &str = "http://lv2plug.in/ns/ext/options#interface";
    pub const UI_SCALE: &str = "http://lv2plug.in/ns/extensions/ui#scaleFactor";
    pub const UI_X11: &str = "http://lv2plug.in/ns/extensions/ui#X11UI";
    pub const INSTANCE_ACCESS: &str = "http://lv2plug.in/ns/ext/instance-access";
}
