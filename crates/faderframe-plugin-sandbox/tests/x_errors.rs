//! A plugin GUI's X error does not end its process (issue #12: Xlib's
//! default error handler calls `exit(1)`; a sandbox helper died of a
//! BadWindow in `X_GetProperty`). The plugin here makes the same error on
//! a connection of its own — a property read of a window that is gone —
//! in a helper process, and the helper must keep answering; and in
//! FaderFrame's own process for plugins hosted there.
//!
//! Needs an X server (`DISPLAY`, XWayland is one) and libX11; without
//! them the tests say so and pass.
#![cfg(all(unix, not(target_os = "macos")))]
#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor, PluginError,
    PluginFactory, PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor,
    PluginRegistry, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_plugin_sandbox::xlib::{Process, Tolerance, tolerate_x_errors, x_errors_survived};
use faderframe_plugin_sandbox::{Launcher, child, instantiate_sandboxed};
use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};

const NO_X: &str = "no X server";

/// Make one BadWindow error the way a plugin GUI would: on its own Xlib
/// connection, a property read of a window destroyed just before.
fn bad_window_error() -> Result<(), String> {
    type Open = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type Root = unsafe extern "C" fn(*mut c_void) -> c_ulong;
    type Create = unsafe extern "C" fn(
        *mut c_void,
        c_ulong,
        c_int,
        c_int,
        c_uint,
        c_uint,
        c_uint,
        c_ulong,
        c_ulong,
    ) -> c_ulong;
    type OnWindow = unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int;
    type GetProperty = unsafe extern "C" fn(
        *mut c_void,
        c_ulong,
        c_ulong,
        c_long,
        c_long,
        c_int,
        c_ulong,
        *mut c_ulong,
        *mut c_int,
        *mut c_ulong,
        *mut c_ulong,
        *mut *mut c_uchar,
    ) -> c_int;
    type Sync = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
    // SAFETY: dlopen/dlsym with NUL-terminated names; each symbol is cast
    // to its Xlib signature (Xlib.h); the display is used only while open.
    unsafe {
        let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            return Err(NO_X.into());
        }
        let sym = |name: &std::ffi::CStr| {
            let p = libc::dlsym(lib, name.as_ptr());
            assert!(!p.is_null(), "libX11 without {name:?}");
            p
        };
        let open: Open = std::mem::transmute(sym(c"XOpenDisplay"));
        let root: Root = std::mem::transmute(sym(c"XDefaultRootWindow"));
        let create: Create = std::mem::transmute(sym(c"XCreateSimpleWindow"));
        let destroy: OnWindow = std::mem::transmute(sym(c"XDestroyWindow"));
        let get_property: GetProperty = std::mem::transmute(sym(c"XGetWindowProperty"));
        let sync: Sync = std::mem::transmute(sym(c"XSync"));
        let close: Close = std::mem::transmute(sym(c"XCloseDisplay"));
        let display = open(std::ptr::null());
        if display.is_null() {
            return Err(NO_X.into());
        }
        let window = create(display, root(display), 0, 0, 10, 10, 0, 0, 0);
        destroy(display, window);
        sync(display, 0);
        let (mut kind, mut format, mut items, mut after) = (0, 0, 0, 0);
        let mut data: *mut c_uchar = std::ptr::null_mut();
        // XA_WM_NAME (39), any type: the read the issue's log shows.
        get_property(
            display,
            window,
            39,
            0,
            64,
            0,
            0,
            &mut kind,
            &mut format,
            &mut items,
            &mut after,
            &mut data,
        );
        // The error arrives (and the handler runs) here at the latest.
        sync(display, 0);
        close(display);
    }
    Ok(())
}

struct Factory;

fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.x-error".into(),
        name: "X Error".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: stereo.clone(),
        audio_outputs: stereo,
        note_inputs: 0,
        note_outputs: 0,
    }
}

fn param(id: u32, name: &str) -> ParameterInfo {
    ParameterInfo {
        id: ParameterId(id),
        name: name.into(),
        min: 0.0,
        max: 1e9,
        default: 0.0,
        unit: ParameterUnit::None,
        automatable: false,
        stepped: true,
    }
}

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        vec![descriptor()]
    }
    fn instantiate(&self, _id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(Plugin {
            descriptor: descriptor(),
            params: vec![param(0, "Make an X error"), param(1, "X errors survived")],
        }))
    }
}

/// Setting parameter 0 makes an X error; parameter 1 counts the errors
/// the process survived.
struct Plugin {
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
}

struct Silent;

impl PluginProcessor for Silent {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, _io: &mut NodeIo<'_>) -> ProcessStatus {
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

impl PluginInstance for Plugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }
    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        match id.0 {
            0 => Some(0.0),
            1 => Some(x_errors_survived() as f64),
            _ => None,
        }
    }
    fn set_parameter(&mut self, id: ParameterId, _v: f64) -> Result<(), PluginError> {
        match id.0 {
            0 => bad_window_error().map_err(PluginError::Failed),
            _ => Err(PluginError::UnknownParameter(id)),
        }
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Silent))
    }
}

/// When started as a helper this "test" serves the host, then exits.
#[test]
fn helper_entry() {
    if !child::is_helper() {
        return;
    }
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    std::process::exit(child::run(registry));
}

#[test]
fn a_sandboxed_plugin_survives_an_x_error_of_its_gui() {
    let launcher = Launcher {
        exe: std::env::current_exe().unwrap(),
        args: ["helper_entry", "--exact", "--nocapture", "--test-threads=1"]
            .map(String::from)
            .to_vec(),
        env: Vec::new(),
    };
    let mut inst = instantiate_sandboxed(&launcher, PluginFormat::Clap, "test.x-error").unwrap();
    match inst.set_parameter(ParameterId(0), 1.0) {
        Err(e) if e.to_string().contains(NO_X) => {
            eprintln!("skipped: {NO_X} (DISPLAY unset or libX11 missing)");
            return;
        }
        r => r.expect("the helper answers after the X error"),
    }
    // Still there: it answers polls, and says it lived through the error.
    for _ in 0..3 {
        inst.poll();
        inst.settle();
    }
    assert!(
        inst.parameter(ParameterId(1)).unwrap_or(0.0) >= 1.0,
        "the helper counted the error it survived"
    );
    inst.set_parameter(ParameterId(0), 1.0).unwrap();
    inst.poll();
    inst.settle();
    inst.poll();
    assert!(inst.parameter(ParameterId(1)).unwrap_or(0.0) >= 2.0);
}

#[test]
fn faderframe_survives_an_x_error_of_a_plugin_it_hosts() {
    // Nothing installed a handler in this test process: ours goes in.
    let t = tolerate_x_errors(Process::Host);
    if t == Tolerance::NoXlib {
        eprintln!("skipped: no libX11");
        return;
    }
    assert_eq!(t, Tolerance::Installed);
    let before = x_errors_survived();
    match bad_window_error() {
        Err(e) if e == NO_X => eprintln!("skipped: {NO_X}"),
        r => {
            r.unwrap();
            assert!(
                x_errors_survived() > before,
                "the error reached our handler"
            );
        }
    }
}
