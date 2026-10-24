//! Plugin UIs against a plugin and UIs written here (linked into the test):
//! the bundle's UIs are found (`lv2:binary` and `ui:binary`, X11 and
//! external), a UI reaches its instance (instance-access, data-access) and
//! is made again with the plugin, and an external UI shows its own window
//! until the user closes it.
#![allow(clippy::unwrap_used, unsafe_code)]

use faderframe_plugin_host::{ParentWindow, PluginInstance, ProcessConfig, WindowApi};
use faderframe_plugin_lv2::sys::{self, LV2_Descriptor, LV2_Feature, LV2_Handle};
use faderframe_plugin_lv2::{Lv2Instance, scan};
use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// What happened, in order.
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The tests share the log: one at a time.
static TURN: Mutex<()> = Mutex::new(());
static SERIAL: AtomicU32 = AtomicU32::new(0);
/// The user closes the external UI's window at its next `run`.
static CLOSE: AtomicBool = AtomicBool::new(false);

fn log(line: String) {
    LOG.lock().unwrap().push(line);
}

fn take_log() -> Vec<String> {
    std::mem::take(&mut *LOG.lock().unwrap())
}

// --- the plugin ---------------------------------------------------------------

const URI: &CStr = c"urn:faderframe:test#ui-plugin";
const EXTENSION: &CStr = c"urn:faderframe:test#extension";
/// What the plugin's `extension_data` gives for [`EXTENSION`].
static MARKER: u32 = 0x5eed;

struct Plugin {
    serial: u32,
}

unsafe extern "C" fn instantiate(
    _: *const LV2_Descriptor,
    _rate: f64,
    _bundle: *const c_char,
    _features: *const *const LV2_Feature,
) -> LV2_Handle {
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed) + 1;
    log(format!("plugin {serial} made"));
    Box::into_raw(Box::new(Plugin { serial })).cast()
}

unsafe extern "C" fn connect(_: LV2_Handle, _port: u32, _data: *mut c_void) {}

unsafe extern "C" fn run(_: LV2_Handle, _n: u32) {}

unsafe extern "C" fn cleanup(h: LV2_Handle) {
    // SAFETY: our handle, from `instantiate`.
    let p = unsafe { Box::from_raw(h.cast::<Plugin>()) };
    log(format!("plugin {} gone", p.serial));
}

unsafe extern "C" fn extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: a C string from the host (or the UI).
    if unsafe { CStr::from_ptr(uri) } == EXTENSION {
        (&MARKER as *const u32).cast()
    } else {
        std::ptr::null()
    }
}

struct Descriptor(LV2_Descriptor);
// SAFETY: immutable.
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

// --- the UIs ------------------------------------------------------------------

const X11_UI: &CStr = c"urn:faderframe:test#x11-ui";
const EXTERNAL_UI: &CStr = c"urn:faderframe:test#external-ui";

/// The features as `(uri, data)`.
fn features(list: *const *const LV2_Feature) -> Vec<(String, *mut c_void)> {
    let mut out = Vec::new();
    let mut i = 0;
    // SAFETY: a null-terminated feature list.
    unsafe {
        while !(*list.add(i)).is_null() {
            let f = &**list.add(i);
            out.push((CStr::from_ptr(f.uri).to_str().unwrap().to_string(), f.data));
            i += 1;
        }
    }
    out
}

fn feature(list: &[(String, *mut c_void)], uri: &str) -> Option<*mut c_void> {
    list.iter().find(|(u, _)| u == uri).map(|(_, d)| *d)
}

/// The instance's serial, read through instance-access, after checking
/// data-access reaches the same plugin's `extension_data`.
fn reached(list: &[(String, *mut c_void)]) -> u32 {
    let handle = feature(list, sys::uri::INSTANCE_ACCESS).unwrap();
    let access = feature(list, sys::uri::DATA_ACCESS).unwrap();
    // SAFETY: what the features say they are.
    unsafe {
        let access = &*access.cast::<sys::LV2_Extension_Data_Feature>();
        let ext = (access.data_access.unwrap())(EXTENSION.as_ptr());
        assert_eq!(ext, (&MARKER as *const u32).cast(), "data-access");
        (*handle.cast::<Plugin>()).serial
    }
}

struct X11Ui {
    serial: u32,
}

/// The external UI: the widget first, as the extension wants.
#[repr(C)]
struct ExternalUi {
    widget: sys::LV2_External_UI_Widget,
    host: *const sys::LV2_External_UI_Host,
    controller: sys::LV2UI_Controller,
    serial: u32,
    closed: bool,
}

unsafe extern "C" fn ui_instantiate(
    descriptor: *const sys::LV2UI_Descriptor,
    plugin_uri: *const c_char,
    _bundle: *const c_char,
    _write: sys::LV2UI_Write_Function,
    controller: sys::LV2UI_Controller,
    widget: *mut sys::LV2UI_Widget,
    list: *const *const LV2_Feature,
) -> sys::LV2UI_Handle {
    // SAFETY: C strings and pointers from the host.
    unsafe {
        assert_eq!(CStr::from_ptr(plugin_uri), URI);
        let list = features(list);
        let serial = reached(&list);
        if CStr::from_ptr((*descriptor).uri) == X11_UI {
            let parent = feature(&list, sys::uri::UI_PARENT).unwrap() as usize;
            log(format!("ui {serial} in {parent}"));
            // No window of its own: the host does not ask X11 for a size.
            *widget = std::ptr::null_mut();
            return Box::into_raw(Box::new(X11Ui { serial })).cast();
        }
        assert!(feature(&list, sys::uri::UI_PARENT).is_none());
        let host = feature(&list, sys::uri::EXTERNAL_UI_HOST)
            .unwrap()
            .cast::<sys::LV2_External_UI_Host>();
        let title = CStr::from_ptr((*host).plugin_human_id).to_str().unwrap();
        log(format!("external {serial} as {title}"));
        let ui = Box::into_raw(Box::new(ExternalUi {
            widget: sys::LV2_External_UI_Widget {
                run: Some(external_run),
                show: Some(external_show),
                hide: Some(external_hide),
            },
            host,
            controller,
            serial,
            closed: false,
        }));
        *widget = ui.cast();
        ui.cast()
    }
}

unsafe extern "C" fn ui_cleanup(h: sys::LV2UI_Handle) {
    // SAFETY: a handle from `ui_instantiate`, of the kind the test opened
    // (cleanup is not told which descriptor made it).
    unsafe {
        if KIND.with(|k| k.get()) == Kind::External {
            let ui = Box::from_raw(h.cast::<ExternalUi>());
            log(format!("external {} gone", ui.serial));
        } else {
            let ui = Box::from_raw(h.cast::<X11Ui>());
            log(format!("ui {} gone", ui.serial));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    X11,
    External,
}

thread_local! {
    /// Which UI the current test opens (the cleanup cannot tell).
    static KIND: std::cell::Cell<Kind> = const { std::cell::Cell::new(Kind::X11) };
}

unsafe extern "C" fn external_run(w: *mut sys::LV2_External_UI_Widget) {
    // SAFETY: our widget, the first field of an `ExternalUi`.
    let ui = unsafe { &mut *w.cast::<ExternalUi>() };
    assert!(!ui.closed, "run after the window closed");
    if CLOSE.swap(false, Ordering::Relaxed) {
        ui.closed = true;
        log(format!("external {} closed by the user", ui.serial));
        // SAFETY: the host's feature, alive while the UI is.
        unsafe { ((*ui.host).ui_closed.unwrap())(ui.controller) };
    }
}

unsafe extern "C" fn external_show(w: *mut sys::LV2_External_UI_Widget) {
    // SAFETY: our widget.
    let ui = unsafe { &*w.cast::<ExternalUi>() };
    log(format!("external {} shown", ui.serial));
}

unsafe extern "C" fn external_hide(w: *mut sys::LV2_External_UI_Widget) {
    // SAFETY: our widget.
    let ui = unsafe { &*w.cast::<ExternalUi>() };
    log(format!("external {} hidden", ui.serial));
}

unsafe extern "C" fn ui_idle(_: sys::LV2UI_Handle) -> i32 {
    0
}

static IDLE: sys::LV2UI_Idle_Interface = sys::LV2UI_Idle_Interface {
    idle: Some(ui_idle),
};

unsafe extern "C" fn ui_extension_data(uri: *const c_char) -> *const c_void {
    // SAFETY: a C string from the host.
    if unsafe { CStr::from_ptr(uri) }.to_str() == Ok(sys::uri::UI_IDLE) {
        (&IDLE as *const sys::LV2UI_Idle_Interface).cast()
    } else {
        std::ptr::null()
    }
}

struct UiDescriptor(sys::LV2UI_Descriptor);
// SAFETY: immutable.
unsafe impl Sync for UiDescriptor {}

static UI_DESCRIPTORS: [UiDescriptor; 2] = [
    UiDescriptor(sys::LV2UI_Descriptor {
        uri: X11_UI.as_ptr(),
        instantiate: Some(ui_instantiate),
        cleanup: Some(ui_cleanup),
        port_event: None,
        extension_data: Some(ui_extension_data),
    }),
    UiDescriptor(sys::LV2UI_Descriptor {
        uri: EXTERNAL_UI.as_ptr(),
        instantiate: Some(ui_instantiate),
        cleanup: Some(ui_cleanup),
        port_event: None,
        extension_data: None,
    }),
];

unsafe extern "C" fn ui_entry(index: u32) -> *const sys::LV2UI_Descriptor {
    UI_DESCRIPTORS
        .get(index as usize)
        .map_or(std::ptr::null(), |d| &d.0)
}

/// What its bundle would say: the X11 UI's binary as `lv2:binary` (as
/// JUCE writes it, in a file of its own), the external UI's as the older
/// `ui:binary`; both want the instance.
fn model(uis: &str) -> Arc<scan::Lv2Plugin> {
    static CALLS: AtomicU32 = AtomicU32::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ff-lv2-ui-{}-{call}.lv2", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.ttl"),
        format!(
            r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:faderframe:test#ui-plugin> a lv2:Plugin ;
    lv2:binary <none.so> ;
    <http://usefulinc.com/ns/doap#name> "UI Test" ;
    {uis} ;
    lv2:port [ a lv2:InputPort , lv2:ControlPort ; lv2:index 0 ; lv2:symbol "gain" ; lv2:name "Gain" ;
          lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 2.0 ] .
<urn:faderframe:test#x11-ui> a ui:X11UI ;
    lv2:binary <none_ui.so> ;
    rdfs:seeAlso <ui.ttl> .
<urn:faderframe:test#external-ui> a <http://kxstudio.sf.net/ns/lv2ext/external-ui#Widget> ;
    ui:binary <none_ui.so> ;
    lv2:requiredFeature <http://lv2plug.in/ns/ext/instance-access> .
"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("ui.ttl"),
        r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix ui: <http://lv2plug.in/ns/extensions/ui#> .
<urn:faderframe:test#x11-ui>
    lv2:requiredFeature ui:idleInterface , ui:parent , <http://lv2plug.in/ns/ext/urid#map> ,
        <http://lv2plug.in/ns/ext/instance-access> ;
    lv2:extensionData ui:idleInterface .
"#,
    )
    .unwrap();
    let mut b = scan::describe_bundle(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(b.plugins.len(), 1);
    Arc::new(b.plugins.remove(0))
}

fn instance(uis: &str) -> Lv2Instance {
    Lv2Instance::with_entry(model(uis), entry)
        .unwrap()
        .with_ui_entry(ui_entry)
}

fn serial_of(line: &str) -> u32 {
    line.split(' ').nth(1).unwrap().parse().unwrap()
}

#[test]
fn the_bundles_uis_are_found_by_either_binary() {
    let _turn = TURN.lock().unwrap();
    let m = model("ui:ui <urn:faderframe:test#x11-ui> , <urn:faderframe:test#external-ui>");
    assert_eq!(m.uis.len(), 2, "{:?}", m.uis);
    let x11 = m.uis.iter().find(|u| u.is_x11()).unwrap();
    assert!(x11.binary.ends_with("none_ui.so"));
    assert!(
        x11.required_features
            .iter()
            .any(|f| f == sys::uri::INSTANCE_ACCESS),
        "from the UI's own file: {:?}",
        x11.required_features
    );
    let external = m.uis.iter().find(|u| u.is_external()).unwrap();
    assert!(external.binary.ends_with("none_ui.so"));
}

#[test]
fn an_embedded_ui_reaches_its_instance_and_is_made_again_with_it() {
    let _turn = TURN.lock().unwrap();
    KIND.with(|k| k.set(Kind::X11));
    take_log();
    let mut inst =
        instance("ui:ui <urn:faderframe:test#x11-ui> , <urn:faderframe:test#external-ui>");
    let first = serial_of(&take_log()[0]);
    let ed = inst.editor().unwrap();
    assert!(ed.can_embed(WindowApi::X11));
    assert!(ed.can_float(WindowApi::X11), "the external UI");
    ed.open_embedded(WindowApi::X11, 1.0).unwrap();
    ed.attach(ParentWindow {
        api: WindowApi::X11,
        handle: 77,
    })
    .unwrap();
    assert!(ed.is_open());
    assert_eq!(take_log(), [format!("ui {first} in 77")]);

    // Another rate makes the plugin again: the UI goes before its instance
    // and comes back on the new one, in the same window.
    let config = ProcessConfig {
        sample_rate: 44_100.0,
        max_block_size: 256,
        sidechain: false,
        double_precision: false,
    };
    let processor = inst.create_processor(&config).unwrap();
    let second = first + 1;
    assert_eq!(
        take_log(),
        [
            format!("ui {first} gone"),
            format!("plugin {second} made"),
            format!("plugin {first} gone"),
            format!("ui {second} in 77"),
        ]
    );
    assert!(inst.editor().unwrap().is_open());

    inst.editor().unwrap().close();
    assert!(!inst.editor().unwrap().is_open());
    assert_eq!(take_log(), [format!("ui {second} gone")]);
    drop(processor);
    drop(inst);
    assert_eq!(take_log(), [format!("plugin {second} gone")]);
}

#[test]
fn an_external_ui_shows_its_own_window_until_the_user_closes_it() {
    let _turn = TURN.lock().unwrap();
    KIND.with(|k| k.set(Kind::External));
    take_log();
    let mut inst = instance("ui:ui <urn:faderframe:test#external-ui>");
    let n = serial_of(&take_log()[0]);
    let ed = inst.editor().unwrap();
    assert!(!ed.can_embed(WindowApi::X11));
    assert!(ed.can_float(WindowApi::X11));
    ed.open_floating(WindowApi::X11, "UI Test · Keys").unwrap();
    assert!(ed.is_open());
    ed.raise();
    assert_eq!(
        take_log(),
        [
            format!("external {n} as UI Test · Keys"),
            format!("external {n} shown"),
            format!("external {n} shown"),
        ]
    );

    // Its events run on the host's timer; the user closes the window.
    let timer = inst.event_sources().timers[0].0;
    inst.on_timer(timer);
    assert!(inst.editor().unwrap().is_open());
    CLOSE.store(true, Ordering::Relaxed);
    inst.on_timer(timer);
    assert!(!inst.editor().unwrap().is_open());
    assert!(inst.event_sources().timers.is_empty(), "no UI left to run");
    assert_eq!(
        take_log(),
        [
            format!("external {n} closed by the user"),
            format!("external {n} gone"),
        ]
    );

    // Opened again and closed by the host: hidden, then cleaned up.
    let ed = inst.editor().unwrap();
    ed.open_floating(WindowApi::X11, "UI Test · Keys").unwrap();
    ed.close();
    assert_eq!(
        take_log(),
        [
            format!("external {n} as UI Test · Keys"),
            format!("external {n} shown"),
            format!("external {n} hidden"),
            format!("external {n} gone"),
        ]
    );
}
