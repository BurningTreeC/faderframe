//! URIDs: one map for the process (plugins, their UIs and the host agree
//! on every number), strings kept forever (`unmap` hands out pointers).

use crate::sys::{LV2_URID, LV2_URID_Map, LV2_URID_Unmap};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct Table {
    ids: HashMap<CString, LV2_URID>,
    /// By URID − 1; boxed so the pointers stay put.
    uris: Vec<Box<CStr>>,
}

fn table() -> &'static Mutex<Table> {
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

/// The URID of `uri` (made on first use; 0 for an empty or invalid one).
pub fn map(uri: &str) -> LV2_URID {
    match CString::new(uri) {
        Ok(c) => map_c(&c),
        Err(_) => 0,
    }
}

fn map_c(uri: &CStr) -> LV2_URID {
    let Ok(mut t) = table().lock() else {
        return 0;
    };
    if let Some(&id) = t.ids.get(uri) {
        return id;
    }
    let owned: Box<CStr> = uri.into();
    t.uris.push(owned);
    let id = t.uris.len() as LV2_URID;
    t.ids.insert(uri.to_owned(), id);
    id
}

/// The URI of `id`.
pub fn unmap(id: LV2_URID) -> Option<String> {
    let t = table().lock().ok()?;
    let s = t.uris.get((id as usize).checked_sub(1)?)?;
    Some(s.to_string_lossy().into_owned())
}

unsafe extern "C" fn map_fn(_: *mut c_void, uri: *const c_char) -> LV2_URID {
    if uri.is_null() {
        return 0;
    }
    // SAFETY: LV2 passes a null-terminated URI.
    map_c(unsafe { CStr::from_ptr(uri) })
}

unsafe extern "C" fn unmap_fn(_: *mut c_void, id: LV2_URID) -> *const c_char {
    let Ok(t) = table().lock() else {
        return std::ptr::null();
    };
    match (id as usize).checked_sub(1).and_then(|i| t.uris.get(i)) {
        // The box lives as long as the process.
        Some(s) => s.as_ptr(),
        None => std::ptr::null(),
    }
}

/// The `urid:map` feature's data (for every instance and UI).
pub fn map_feature() -> LV2_URID_Map {
    LV2_URID_Map {
        handle: std::ptr::null_mut(),
        map: Some(map_fn),
    }
}

pub fn unmap_feature() -> LV2_URID_Unmap {
    LV2_URID_Unmap {
        handle: std::ptr::null_mut(),
        unmap: Some(unmap_fn),
    }
}

/// URIDs the host uses all the time, mapped once.
#[derive(Clone, Copy, Debug)]
pub struct Urids {
    pub atom_sequence: u32,
    pub atom_chunk: u32,
    pub atom_int: u32,
    pub atom_long: u32,
    pub atom_float: u32,
    pub atom_double: u32,
    pub atom_bool: u32,
    pub atom_string: u32,
    pub atom_path: u32,
    pub atom_urid: u32,
    pub atom_uri: u32,
    pub atom_object: u32,
    pub atom_blank: u32,
    pub atom_event_transfer: u32,
    pub midi_event: u32,
    pub time_position: u32,
    pub time_frame: u32,
    pub time_speed: u32,
    pub time_bar: u32,
    pub time_bar_beat: u32,
    pub time_beat: u32,
    pub time_bpm: u32,
    pub time_beats_per_bar: u32,
    pub time_beat_unit: u32,
    pub min_block: u32,
    pub max_block: u32,
    pub nominal_block: u32,
    pub sequence_size: u32,
    pub sample_rate: u32,
    pub ui_scale: u32,
    pub log_error: u32,
    pub log_warning: u32,
    pub log_note: u32,
    pub log_trace: u32,
}

impl Urids {
    pub fn get() -> &'static Urids {
        static U: OnceLock<Urids> = OnceLock::new();
        U.get_or_init(|| {
            use crate::sys::uri as u;
            let atom = |n: &str| map(&format!("http://lv2plug.in/ns/ext/atom#{n}"));
            Urids {
                atom_sequence: map(u::ATOM_SEQUENCE),
                atom_chunk: map(u::ATOM_CHUNK),
                atom_int: map(u::ATOM_INT),
                atom_long: map(u::ATOM_LONG),
                atom_float: map(u::ATOM_FLOAT),
                atom_double: map(u::ATOM_DOUBLE),
                atom_bool: atom("Bool"),
                atom_string: atom("String"),
                atom_path: atom("Path"),
                atom_urid: atom("URID"),
                atom_uri: atom("URI"),
                atom_object: map(u::ATOM_OBJECT),
                atom_blank: map(u::ATOM_BLANK),
                atom_event_transfer: map(u::ATOM_EVENT_TRANSFER),
                midi_event: map(u::MIDI_EVENT),
                time_position: map(u::TIME_POSITION),
                time_frame: map(u::TIME_FRAME),
                time_speed: map(u::TIME_SPEED),
                time_bar: map(u::TIME_BAR),
                time_bar_beat: map(u::TIME_BAR_BEAT),
                time_beat: map(u::TIME_BEAT),
                time_bpm: map(u::TIME_BPM),
                time_beats_per_bar: map(u::TIME_BEATS_PER_BAR),
                time_beat_unit: map(u::TIME_BEAT_UNIT),
                min_block: map(u::MIN_BLOCK),
                max_block: map(u::MAX_BLOCK),
                nominal_block: map(u::NOMINAL_BLOCK),
                sequence_size: map(u::SEQUENCE_SIZE),
                sample_rate: map(u::SAMPLE_RATE),
                ui_scale: map(u::UI_SCALE),
                log_error: map(u::LOG_ERROR),
                log_warning: map(u::LOG_WARNING),
                log_note: map(u::LOG_NOTE),
                log_trace: map(u::LOG_TRACE),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_map_to_stable_numbers_and_back() {
        let a = map("urn:test:a");
        let b = map("urn:test:b");
        assert_ne!(a, b);
        assert_eq!(map("urn:test:a"), a);
        assert_eq!(unmap(b).as_deref(), Some("urn:test:b"));
        assert_eq!(unmap(0), None);
        let m = map_feature();
        let c = CString::new("urn:test:a").unwrap();
        // SAFETY: the feature's function with a valid string.
        assert_eq!(unsafe { (m.map.unwrap())(m.handle, c.as_ptr()) }, a);
    }
}
