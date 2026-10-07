//! The features the host gives an instance: URID maps, options (block
//! sizes, sample rate), bounded block length, the worker's schedule, a log.
//! Built once per instance; every pointer handed out stays valid while the
//! [`HostFeatures`] lives (it is boxed and never moved).

use crate::sys::{
    self, LV2_Feature, LV2_Log_Log, LV2_Options_Option, LV2_URID_Map, LV2_URID_Unmap,
};
use crate::urid::Urids;
use std::ffi::{CStr, CString, c_char, c_void};

unsafe extern "C" {
    fn ff_lv2_log_printf(handle: *mut c_void, type_: u32, fmt: *const c_char, ...) -> i32;
    fn ff_lv2_log_vprintf(
        handle: *mut c_void,
        type_: u32,
        fmt: *const c_char,
        ap: *mut c_void,
    ) -> i32;
}

/// A plugin's log message (formatted by `log.c`).
#[unsafe(no_mangle)]
extern "C" fn ff_lv2_log_message(_handle: *mut c_void, type_: u32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: `log.c` passes a null-terminated buffer.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let text = text.trim_end();
    let u = Urids::get();
    if type_ == u.log_error {
        tracing::warn!("LV2 plugin error: {text}");
    } else if type_ == u.log_warning {
        tracing::warn!("LV2 plugin: {text}");
    } else if type_ == u.log_note {
        tracing::info!("LV2 plugin: {text}");
    } else {
        tracing::debug!("LV2 plugin: {text}");
    }
}

/// Host features every plugin may use (to check its required ones).
pub const SUPPORTED: &[&str] = &[
    sys::uri::URID_MAP,
    sys::uri::URID_UNMAP,
    sys::uri::OPTIONS,
    sys::uri::BOUNDED_BLOCK,
    sys::uri::WORKER_SCHEDULE,
    sys::uri::LOG,
    sys::uri::STATE_LOAD_DEFAULT,
    sys::uri::STATE_MAP_PATH,
    sys::uri::STATE_FREE_PATH,
    sys::uri::HARD_RT,
    sys::uri::IS_LIVE,
    sys::uri::IN_PLACE_BROKEN,
    "http://lv2plug.in/ns/ext/state#threadSafeRestore",
];

struct OptionValues {
    min_block: i32,
    max_block: i32,
    nominal_block: i32,
    sequence_size: i32,
    sample_rate: f32,
}

pub struct HostFeatures {
    _map: Box<LV2_URID_Map>,
    _unmap: Box<LV2_URID_Unmap>,
    _values: Box<OptionValues>,
    _options: Box<[LV2_Options_Option]>,
    _schedule: Option<Box<sys::LV2_Worker_Schedule>>,
    _log: Box<LV2_Log_Log>,
    _uris: Vec<CString>,
    _features: Vec<LV2_Feature>,
    pointers: Vec<*const LV2_Feature>,
}

impl HostFeatures {
    /// For an instance at `sample_rate` taking blocks of up to `max_block`
    /// frames, atom buffers of `sequence_size` bytes; `schedule`: the
    /// worker's handle (`None`: no worker).
    pub fn new(
        sample_rate: f64,
        max_block: u32,
        sequence_size: u32,
        schedule: Option<sys::LV2_Worker_Schedule>,
    ) -> Box<HostFeatures> {
        let u = Urids::get();
        let map = Box::new(crate::urid::map_feature());
        let unmap = Box::new(crate::urid::unmap_feature());
        let values = Box::new(OptionValues {
            min_block: 1,
            max_block: max_block.max(1) as i32,
            nominal_block: max_block.max(1) as i32,
            sequence_size: sequence_size as i32,
            sample_rate: sample_rate as f32,
        });
        let opt = |key: u32, type_: u32, value: *const c_void, size: u32| LV2_Options_Option {
            context: sys::LV2_OPTIONS_INSTANCE,
            subject: 0,
            key,
            size,
            type_,
            value,
        };
        let options: Box<[LV2_Options_Option]> = vec![
            opt(
                u.min_block,
                u.atom_int,
                (&values.min_block as *const i32).cast(),
                4,
            ),
            opt(
                u.max_block,
                u.atom_int,
                (&values.max_block as *const i32).cast(),
                4,
            ),
            opt(
                u.nominal_block,
                u.atom_int,
                (&values.nominal_block as *const i32).cast(),
                4,
            ),
            opt(
                u.sequence_size,
                u.atom_int,
                (&values.sequence_size as *const i32).cast(),
                4,
            ),
            opt(
                u.sample_rate,
                u.atom_float,
                (&values.sample_rate as *const f32).cast(),
                4,
            ),
            opt(0, 0, std::ptr::null(), 0),
        ]
        .into_boxed_slice();
        let schedule = schedule.map(Box::new);
        let log = Box::new(LV2_Log_Log {
            handle: std::ptr::null_mut(),
            printf: ff_lv2_log_printf as *const c_void,
            vprintf: ff_lv2_log_vprintf as *const c_void,
        });
        let mut list: Vec<(&str, *mut c_void)> = vec![
            (
                sys::uri::URID_MAP,
                (&*map as *const LV2_URID_Map).cast_mut().cast(),
            ),
            (
                sys::uri::URID_UNMAP,
                (&*unmap as *const LV2_URID_Unmap).cast_mut().cast(),
            ),
            (sys::uri::OPTIONS, options.as_ptr().cast_mut().cast()),
            (sys::uri::BOUNDED_BLOCK, std::ptr::null_mut()),
            (
                sys::uri::LOG,
                (&*log as *const LV2_Log_Log).cast_mut().cast(),
            ),
            (sys::uri::STATE_LOAD_DEFAULT, std::ptr::null_mut()),
        ];
        if let Some(s) = &schedule {
            list.push((
                sys::uri::WORKER_SCHEDULE,
                (&**s as *const sys::LV2_Worker_Schedule).cast_mut().cast(),
            ));
        }
        let uris: Vec<CString> = list
            .iter()
            .map(|(u, _)| CString::new(*u).unwrap_or_default())
            .collect();
        let features: Vec<LV2_Feature> = uris
            .iter()
            .zip(&list)
            .map(|(u, (_, data))| LV2_Feature {
                uri: u.as_ptr(),
                data: *data,
            })
            .collect();
        let mut pointers: Vec<*const LV2_Feature> =
            features.iter().map(|f| f as *const LV2_Feature).collect();
        pointers.push(std::ptr::null());
        Box::new(HostFeatures {
            _map: map,
            _unmap: unmap,
            _values: values,
            _options: options,
            _schedule: schedule,
            _log: log,
            _uris: uris,
            _features: features,
            pointers,
        })
    }

    /// The null-terminated array for `instantiate`.
    pub fn as_ptr(&self) -> *const *const LV2_Feature {
        self.pointers.as_ptr()
    }
}

/// The features a plugin needs that the host does not have.
pub fn missing(required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|f| !SUPPORTED.contains(&f.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_list_ends_with_null_and_options_too() {
        let f = HostFeatures::new(48_000.0, 512, 8192, None);
        let mut n = 0;
        // SAFETY: the array is null-terminated.
        unsafe {
            while !(*f.as_ptr().add(n)).is_null() {
                n += 1;
            }
        }
        assert_eq!(n, 6);
        // SAFETY: the first entry is valid.
        let first = unsafe { &**f.as_ptr() };
        // SAFETY: a null-terminated URI.
        assert_eq!(
            unsafe { CStr::from_ptr(first.uri) }.to_str().unwrap(),
            sys::uri::URID_MAP
        );
        assert_eq!(
            missing(&[sys::uri::URID_MAP.to_string(), "urn:x".to_string()]),
            ["urn:x"]
        );
    }

    #[test]
    fn the_log_reaches_the_host() {
        let fmt = CString::new("value %d").unwrap();
        // SAFETY: a format and its argument.
        let n = unsafe { ff_lv2_log_printf(std::ptr::null_mut(), 0, fmt.as_ptr(), 42i32) };
        assert_eq!(n, 8);
    }
}
