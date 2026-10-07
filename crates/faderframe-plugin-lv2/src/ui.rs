//! Plugin UIs: X11 UIs (`ui:X11UI`) embedded into the host's parent
//! window. They talk to the plugin only through ports (control values and
//! atom messages), so they keep working when the plugin is instantiated
//! again; their idle callback runs on the host's timer.

use crate::core::Shared;
use crate::instance::Lv2Instance;
use crate::scan::{Lv2Plugin, PortKind, Ui};
use crate::sys::{self, LV2UI_Descriptor, LV2UI_Handle};
use crate::urid::Urids;
use faderframe_plugin_host::{EditorRequests, ParentWindow, PluginEditor, PluginError, WindowApi};
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_void};

/// Features a UI may require.
const SUPPORTED: &[&str] = &[
    sys::uri::URID_MAP,
    sys::uri::URID_UNMAP,
    sys::uri::UI_PARENT,
    sys::uri::UI_RESIZE,
    sys::uri::UI_TOUCH,
    sys::uri::UI_PORT_MAP,
    sys::uri::UI_IDLE,
    sys::uri::UI_FIXED_SIZE,
    sys::uri::UI_NO_USER_RESIZE,
    sys::uri::OPTIONS,
    sys::uri::LOG,
];

/// The UI to embed: an X11 UI whose required features the host has.
pub fn find(model: &Lv2Plugin) -> Option<&Ui> {
    model.uis.iter().find(|u| {
        u.is_x11()
            && u.required_features
                .iter()
                .all(|f| SUPPORTED.contains(&f.as_str()))
    })
}

/// What a UI did, for the instance to apply.
#[derive(Debug, PartialEq)]
pub enum UiEvent {
    Control(u32, f32),
    Touch(u32, bool),
    /// An atom for an atom input port: port, type, body.
    Atom(u32, u32, Vec<u8>),
}

/// What the UI's callbacks reach (boxed: its address is the controller).
pub struct Controller {
    events: RefCell<Vec<UiEvent>>,
    requests: Cell<EditorRequests>,
    symbols: Vec<CString>,
}

unsafe extern "C" fn write(
    controller: *mut c_void,
    port: u32,
    size: u32,
    protocol: u32,
    buffer: *const c_void,
) {
    // SAFETY: the controller we gave the UI, alive while it is.
    let c = unsafe { &*controller.cast::<Controller>() };
    if buffer.is_null() {
        return;
    }
    if protocol == 0 && size as usize == std::mem::size_of::<f32>() {
        // SAFETY: a float, as the protocol says.
        let v = unsafe { buffer.cast::<f32>().read_unaligned() };
        c.events.borrow_mut().push(UiEvent::Control(port, v));
    } else if protocol == Urids::get().atom_event_transfer && size >= 8 {
        // SAFETY: an atom: its header, then `size − 8` bytes of body.
        let (header, body) = unsafe {
            let h = buffer.cast::<sys::LV2_Atom>().read_unaligned();
            let n = (h.size as usize).min(size as usize - 8);
            (
                h,
                std::slice::from_raw_parts(buffer.cast::<u8>().add(8), n).to_vec(),
            )
        };
        c.events
            .borrow_mut()
            .push(UiEvent::Atom(port, header.type_, body));
    }
}

unsafe extern "C" fn resize(handle: *mut c_void, width: i32, height: i32) -> i32 {
    // SAFETY: our controller.
    let c = unsafe { &*handle.cast::<Controller>() };
    if width > 0 && height > 0 {
        let mut r = c.requests.get();
        r.resize = Some((width as u32, height as u32));
        c.requests.set(r);
    }
    0
}

unsafe extern "C" fn touch(handle: *mut c_void, port: u32, grabbed: bool) {
    // SAFETY: our controller.
    let c = unsafe { &*handle.cast::<Controller>() };
    c.events.borrow_mut().push(UiEvent::Touch(port, grabbed));
}

unsafe extern "C" fn port_index(handle: *mut c_void, symbol: *const c_char) -> u32 {
    if symbol.is_null() {
        return sys::LV2UI_INVALID_PORT_INDEX;
    }
    // SAFETY: our controller and a C string from the UI.
    let (c, s) = unsafe { (&*handle.cast::<Controller>(), CStr::from_ptr(symbol)) };
    c.symbols
        .iter()
        .position(|x| x.as_c_str() == s)
        .map_or(sys::LV2UI_INVALID_PORT_INDEX, |i| i as u32)
}

fn data<T>(r: &T) -> *mut c_void {
    (r as *const T).cast_mut().cast()
}

/// The features given to the UI (alive as long as it is).
struct Features {
    _map: Box<sys::LV2_URID_Map>,
    _unmap: Box<sys::LV2_URID_Unmap>,
    _resize: Box<sys::LV2UI_Resize>,
    _touch: Box<sys::LV2UI_Touch>,
    _port_map: Box<sys::LV2UI_Port_Map>,
    _values: Box<[f32; 2]>,
    _options: Box<[sys::LV2_Options_Option]>,
    _uris: Vec<CString>,
    _list: Vec<sys::LV2_Feature>,
    pointers: Vec<*const sys::LV2_Feature>,
}

impl Features {
    fn new(controller: *mut c_void, parent: u64, rate: f64, scale: f64) -> Features {
        let u = Urids::get();
        let map = Box::new(crate::urid::map_feature());
        let unmap = Box::new(crate::urid::unmap_feature());
        let resize_f = Box::new(sys::LV2UI_Resize {
            handle: controller,
            ui_resize: Some(resize),
        });
        let touch_f = Box::new(sys::LV2UI_Touch {
            handle: controller,
            touch: Some(touch),
        });
        let port_map = Box::new(sys::LV2UI_Port_Map {
            handle: controller,
            port_index: Some(port_index),
        });
        let values = Box::new([rate as f32, scale as f32]);
        let opt = |key: u32, value: *const f32| sys::LV2_Options_Option {
            context: sys::LV2_OPTIONS_INSTANCE,
            subject: 0,
            key,
            size: 4,
            type_: u.atom_float,
            value: value.cast(),
        };
        let options: Box<[sys::LV2_Options_Option]> = vec![
            opt(u.sample_rate, &values[0]),
            opt(u.ui_scale, &values[1]),
            sys::LV2_Options_Option {
                context: 0,
                subject: 0,
                key: 0,
                size: 0,
                type_: 0,
                value: std::ptr::null(),
            },
        ]
        .into_boxed_slice();
        let list_data: Vec<(&str, *mut c_void)> = vec![
            (sys::uri::URID_MAP, data(&*map)),
            (sys::uri::URID_UNMAP, data(&*unmap)),
            (sys::uri::UI_PARENT, parent as usize as *mut c_void),
            (sys::uri::UI_RESIZE, data(&*resize_f)),
            (sys::uri::UI_TOUCH, data(&*touch_f)),
            (sys::uri::UI_PORT_MAP, data(&*port_map)),
            (sys::uri::UI_IDLE, std::ptr::null_mut()),
            (sys::uri::OPTIONS, options.as_ptr().cast_mut().cast()),
        ];
        let uris: Vec<CString> = list_data
            .iter()
            .map(|(u, _)| CString::new(*u).unwrap_or_default())
            .collect();
        let list: Vec<sys::LV2_Feature> = uris
            .iter()
            .zip(&list_data)
            .map(|(u, (_, d))| sys::LV2_Feature {
                uri: u.as_ptr(),
                data: *d,
            })
            .collect();
        let mut pointers: Vec<*const sys::LV2_Feature> =
            list.iter().map(|f| f as *const sys::LV2_Feature).collect();
        pointers.push(std::ptr::null());
        Features {
            _map: map,
            _unmap: unmap,
            _resize: resize_f,
            _touch: touch_f,
            _port_map: port_map,
            _values: values,
            _options: options,
            _uris: uris,
            _list: list,
            pointers,
        }
    }
}

pub struct Editor {
    desc: *const LV2UI_Descriptor,
    handle: LV2UI_Handle,
    widget: sys::LV2UI_Widget,
    idle: *const sys::LV2UI_Idle_Interface,
    ui_resize: *const sys::LV2UI_Resize,
    /// Must outlive the UI.
    _features: Box<Features>,
    controller: Box<Controller>,
    /// The value last sent per port (`None`: not yet).
    sent: Vec<Option<f32>>,
    /// The UI asked for no size: ask its window (a few times).
    sized: bool,
    size_tries: u32,
    closed: bool,
    message: Vec<u8>,
    atom: Vec<u64>,
    _library: libloading::Library,
}

impl Editor {
    pub fn open(
        model: &Lv2Plugin,
        ui: &Ui,
        parent: u64,
        rate: f64,
        scale: f64,
    ) -> Result<Editor, String> {
        // SAFETY: loading the UI's library runs its initialisers (the
        // user opened the editor).
        let library = unsafe { libloading::Library::new(&ui.binary) }
            .map_err(|e| format!("{}: {e}", ui.binary.display()))?;
        // SAFETY: the LV2 UI entry point.
        let entry: libloading::Symbol<'_, sys::LV2UI_Descriptor_Function> =
            unsafe { library.get(b"lv2ui_descriptor\0") }.map_err(|e| e.to_string())?;
        let mut desc = std::ptr::null();
        for i in 0..1024 {
            // SAFETY: indices from 0 until it gives null.
            let d = unsafe { entry(i) };
            if d.is_null() {
                break;
            }
            // SAFETY: a descriptor's URI is a C string.
            if unsafe { CStr::from_ptr((*d).uri) }.to_bytes() == ui.uri.as_bytes() {
                desc = d;
                break;
            }
        }
        if desc.is_null() {
            return Err(format!("{} is not in its library", ui.uri));
        }
        let controller = Box::new(Controller {
            events: RefCell::new(Vec::new()),
            requests: Cell::new(EditorRequests::default()),
            symbols: model
                .ports
                .iter()
                .map(|p| CString::new(p.symbol.clone()).unwrap_or_default())
                .collect(),
        });
        let cptr = (&*controller as *const Controller)
            .cast_mut()
            .cast::<c_void>();
        let features = Box::new(Features::new(cptr, parent, rate, scale));
        let plugin_uri = CString::new(model.uri.clone()).map_err(|e| e.to_string())?;
        let mut bundle = ui.bundle.to_string_lossy().into_owned();
        if !bundle.ends_with('/') {
            bundle.push('/');
        }
        let bundle = CString::new(bundle).map_err(|e| e.to_string())?;
        let mut widget: sys::LV2UI_Widget = std::ptr::null_mut();
        // SAFETY: the UI's instantiate with our callbacks and features,
        // which live as long as the editor.
        let handle = unsafe {
            match (*desc).instantiate {
                Some(f) => f(
                    desc,
                    plugin_uri.as_ptr(),
                    bundle.as_ptr(),
                    write,
                    cptr,
                    &mut widget,
                    features.pointers.as_ptr(),
                ),
                None => std::ptr::null_mut(),
            }
        };
        if handle.is_null() {
            return Err(format!("{}: the UI did not start", model.name));
        }
        let ext = |uri: &str| -> *const c_void {
            let Ok(c) = CString::new(uri) else {
                return std::ptr::null();
            };
            // SAFETY: the UI's extension data query.
            unsafe {
                (*desc)
                    .extension_data
                    .map_or(std::ptr::null(), |f| f(c.as_ptr()))
            }
        };
        let idle = ext(sys::uri::UI_IDLE).cast::<sys::LV2UI_Idle_Interface>();
        let ui_resize = ext(sys::uri::UI_RESIZE).cast::<sys::LV2UI_Resize>();
        let mut e = Editor {
            desc,
            handle,
            widget,
            idle,
            ui_resize,
            _features: features,
            sent: vec![None; model.ports.len()],
            sized: controller.requests.get().resize.is_some(),
            size_tries: 0,
            controller,
            closed: false,
            message: Vec::new(),
            atom: vec![0; 1024],
            _library: library,
        };
        if !e.sized
            && let Some(size) = window_size(e.widget as usize as u32)
        {
            e.request_size(size);
        }
        Ok(e)
    }

    fn request_size(&mut self, size: (u32, u32)) {
        let mut r = self.controller.requests.get();
        r.resize = Some(size);
        self.controller.requests.set(r);
        self.sized = true;
    }

    pub fn take_events(&mut self) -> Vec<UiEvent> {
        std::mem::take(&mut *self.controller.events.borrow_mut())
    }

    pub fn forget_sent(&mut self) {
        self.sent.iter_mut().for_each(|s| *s = None);
    }

    fn port_event(&self, port: u32, size: u32, format: u32, buffer: *const c_void) {
        // SAFETY: the UI's port_event with a buffer valid for the call.
        unsafe {
            if let Some(f) = (*self.desc).port_event {
                f(self.handle, port, size, format, buffer);
            }
        }
    }

    /// Control values that changed, messages from the plugin, then the
    /// UI's idle.
    pub fn idle(
        &mut self,
        model: &Lv2Plugin,
        shared: &Shared,
        from_plugin: &mut rtrb::Consumer<u8>,
    ) {
        for p in &model.ports {
            if p.kind != PortKind::Control {
                continue;
            }
            let Some(v) = shared.get(p.index) else {
                continue;
            };
            if self.sent[p.index as usize] != Some(v) {
                self.sent[p.index as usize] = Some(v);
                self.port_event(p.index, 4, 0, (&v as *const f32).cast());
            }
        }
        let transfer = Urids::get().atom_event_transfer;
        while crate::worker::pop(from_plugin, &mut self.message) {
            let m = &self.message;
            if m.len() < 8 {
                continue;
            }
            let port = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
            let type_ = u32::from_le_bytes([m[4], m[5], m[6], m[7]]);
            let body = &m[8..];
            let words = (8 + body.len()).div_ceil(8);
            if self.atom.len() < words {
                self.atom.resize(words, 0);
            }
            let bytes = as_bytes_mut(&mut self.atom);
            bytes[..4].copy_from_slice(&(body.len() as u32).to_ne_bytes());
            bytes[4..8].copy_from_slice(&type_.to_ne_bytes());
            bytes[8..8 + body.len()].copy_from_slice(body);
            let size = (8 + body.len()) as u32;
            self.port_event(port, size, transfer, self.atom.as_ptr().cast());
        }
        if !self.idle.is_null() {
            // SAFETY: the UI's idle interface.
            let r = unsafe { (*self.idle).idle.map_or(0, |f| f(self.handle)) };
            if r != 0 {
                self.closed = true;
                let mut q = self.controller.requests.get();
                q.closed = true;
                self.controller.requests.set(q);
            }
        }
        if !self.sized && self.size_tries < 10 {
            self.size_tries += 1;
            if let Some(size) = window_size(self.widget as usize as u32) {
                self.request_size(size);
            }
        }
    }

    pub fn take_requests(&mut self) -> EditorRequests {
        self.controller.requests.take()
    }

    pub fn can_resize(&self) -> bool {
        !self.ui_resize.is_null()
    }

    pub fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if self.ui_resize.is_null() {
            return None;
        }
        // SAFETY: the UI's own resize interface.
        let r = unsafe {
            (*self.ui_resize).ui_resize.map_or(1, |f| {
                f((*self.ui_resize).handle, width as i32, height as i32)
            })
        };
        (r == 0).then_some((width, height))
    }
}

fn as_bytes_mut(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: any bytes make valid u64s and u8 has the smallest alignment.
    let (_, bytes, _) = unsafe { words.align_to_mut::<u8>() };
    bytes
}

/// The size of an X11 window (an UI's own), when there is one.
fn window_size(window: u32) -> Option<(u32, u32)> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    if window == 0 {
        return None;
    }
    let (conn, _) = x11rb::connect(None).ok()?;
    let g = conn.get_geometry(window).ok()?.reply().ok()?;
    (g.width > 1 && g.height > 1).then_some((u32::from(g.width), u32::from(g.height)))
}

impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: the UI is not used after this.
        unsafe {
            if let Some(f) = (*self.desc).cleanup {
                f(self.handle);
            }
        }
    }
}

impl PluginEditor for Lv2Instance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        api == WindowApi::X11 && find(&self.model).is_some()
    }

    fn can_float(&mut self, _api: WindowApi) -> bool {
        false
    }

    fn open_embedded(&mut self, api: WindowApi, scale: f64) -> Result<(u32, u32), PluginError> {
        if api != WindowApi::X11 || find(&self.model).is_none() {
            return Err(PluginError::Failed("no X11 editor".into()));
        }
        self.ui_scale = scale;
        // The UI asks for its size once it exists (in `attach`).
        Ok((480, 320))
    }

    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        let ui = find(&self.model).ok_or_else(|| PluginError::Failed("no X11 editor".into()))?;
        let rate = self.rate;
        let ed = Editor::open(&self.model, ui, parent.handle, rate, self.ui_scale)
            .map_err(PluginError::Failed)?;
        self.editor = Some(ed);
        self.shared
            .ui_open
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.idle_ui();
        Ok(())
    }

    fn open_floating(&mut self, _api: WindowApi, _title: &str) -> Result<(), PluginError> {
        Err(PluginError::Failed("LV2 editors embed".into()))
    }

    fn close(&mut self) {
        self.shared
            .ui_open
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.editor = None;
    }

    fn is_open(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| !e.closed)
    }

    fn can_resize(&mut self) -> bool {
        self.editor.as_ref().is_some_and(Editor::can_resize)
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        self.editor.as_mut()?.set_size(width, height)
    }

    fn take_requests(&mut self) -> EditorRequests {
        self.editor
            .as_mut()
            .map(Editor::take_requests)
            .unwrap_or_default()
    }
}
