//! Native editors under the editor header (Bypass, Presets, Outputs,
//! Parameters) on X11. GTK on Wayland cannot hold another program's
//! window, but GTK can open GDK's X11 display (XWayland) beside the
//! Wayland one: the editor's window is a GTK window there, with the header
//! as its title bar, and the plugin's GUI goes into an X11 window of ours
//! laid over the area under the header (moved with it every UI tick).
//!
//! The GDK X11 functions are looked up in the running GTK, so a GTK built
//! without X11 still runs (editors then get plain windows).

use super::Parents;
use crate::state::AppState;
use gtk::glib::translate::{FromGlibPtrNone, ToGlibPtr};
use gtk::{gdk, prelude::*};
use std::cell::{Cell, OnceCell};
use std::ffi::{c_char, c_ulong};
use std::rc::Rc;
use std::sync::OnceLock;

type DisplayOpen = unsafe extern "C" fn(*const c_char) -> *mut gdk::ffi::GdkDisplay;
type SurfaceXid = unsafe extern "C" fn(*mut gdk::ffi::GdkSurface) -> c_ulong;

struct X11Api {
    open: DisplayOpen,
    xid: SurfaceXid,
}

fn api() -> Option<&'static X11Api> {
    static API: OnceLock<Option<X11Api>> = OnceLock::new();
    API.get_or_init(|| {
        let lib = libloading::os::unix::Library::this();
        // SAFETY: GTK's own X11 functions with their signatures in
        // gdk/x11/gdkx11display.h and gdkx11surface.h; the library is the
        // GTK already loaded into the process, which stays loaded.
        let api = unsafe {
            lib.get::<DisplayOpen>(b"gdk_x11_display_open\0")
                .ok()
                .map(|s| *s)
                .zip(
                    lib.get::<SurfaceXid>(b"gdk_x11_surface_get_xid\0")
                        .ok()
                        .map(|s| *s),
                )
                .map(|(open, xid)| X11Api { open, xid })
        };
        std::mem::forget(lib);
        api
    })
    .as_ref()
}

thread_local! {
    /// GDK's X11 display, opened for the first framed editor (`None`: not
    /// available).
    static DISPLAY: OnceCell<Option<gdk::Display>> = const { OnceCell::new() };
}

fn display(app: &AppState) -> Option<gdk::Display> {
    if std::env::var_os("FADERFRAME_EDITOR_HEADER").is_some_and(|v| v == "0") {
        return None;
    }
    DISPLAY.with(|d| {
        d.get_or_init(|| {
            let api = api()?;
            // SAFETY: NULL names $DISPLAY; the call returns a display or
            // NULL.
            let ptr = unsafe { (api.open)(std::ptr::null()) };
            if ptr.is_null() {
                tracing::warn!("GTK cannot open the X11 display: plugin editors without header");
                return None;
            }
            // SAFETY: a live display; the reference taken here is kept for
            // the program's life (whoever else owns it).
            let display = unsafe { gdk::Display::from_glib_none(ptr) };
            crate::style::add_display(&display, &app.theme.borrow());
            Some(display)
        })
        .clone()
    })
}

/// The X11 window of a surface on GDK's X11 display.
fn xid(surface: &gdk::Surface) -> Option<u64> {
    let api = api()?;
    let x11 = gtk::glib::Type::from_name("GdkX11Surface")?;
    if !surface.type_().is_a(x11) {
        return None;
    }
    // SAFETY: an X11 surface (checked above), borrowed for the call.
    let id = unsafe { (api.xid)(surface.to_glib_none().0) };
    // `c_ulong` is 32 bits wide on some targets.
    #[allow(clippy::unnecessary_cast)]
    (id != 0).then_some(id as u64)
}

pub struct Frame {
    window: gtk::Window,
    /// The room under the header the plugin's GUI covers.
    area: gtk::DrawingArea,
    /// The GTK window's X11 window.
    top: u64,
    /// Ours inside `top`, the plugin's GUI inside it.
    child: u64,
    resizable: bool,
    /// The header's height (X11 pixels).
    header: u32,
    /// Where `child` is (pixels of `top`), once placed.
    placed: Cell<Option<(i32, i32, u32, u32)>>,
    /// Where the window goes once shown.
    pending_move: Cell<Option<(i32, i32)>>,
}

impl Frame {
    /// A GTK window on GDK's X11 display with `header` as its title bar
    /// and room for an editor of `size` (X11 pixels) under it, realized
    /// but not shown. `None` where there is no such display.
    pub fn build(
        app: &Rc<AppState>,
        x: &Parents,
        title: &str,
        header: &gtk::HeaderBar,
        size: (u32, u32),
        resizable: bool,
    ) -> Option<Frame> {
        let display = display(app)?;
        let window = gtk::Window::builder()
            .display(&display)
            .title(title)
            .resizable(resizable)
            .build();
        window.add_css_class("plugin-editor");
        // Not one of the application's windows (GtkApplication's Wayland
        // side would handle it as its own), but its actions work here.
        window.insert_action_group("app", Some(&app.app));
        window.set_titlebar(Some(header));
        let area = gtk::DrawingArea::new();
        area.add_css_class("plugin-gui");
        window.set_child(Some(&area));
        WidgetExt::realize(&window);
        let Some(top) = window.surface().as_ref().and_then(xid) else {
            window.destroy();
            return None;
        };
        let scale = scale_of(&window);
        let (w, h) = (size.0.div_ceil(scale), size.1.div_ceil(scale));
        let header_h = header.measure(gtk::Orientation::Vertical, -1).1.max(0) as u32;
        if resizable {
            area.set_hexpand(true);
            area.set_vexpand(true);
            window.set_default_size(w as i32, (h + header_h) as i32);
        } else {
            area.set_content_width(w as i32);
            area.set_content_height(h as i32);
        }
        let child = match x.create_child(top, size) {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!("no window for the plugin editor in its frame: {e}");
                window.destroy();
                return None;
            }
        };
        // As the plain windows: window managers float dialogs.
        x.set_dialog(top);
        Some(Frame {
            window,
            area,
            top,
            child,
            resizable,
            header: header_h * scale,
            placed: Cell::new(None),
            pending_move: Cell::new(None),
        })
    }

    pub fn window(&self) -> &gtk::Window {
        &self.window
    }

    pub fn top(&self) -> u64 {
        self.top
    }

    /// The window the plugin's GUI embeds into.
    pub fn child(&self) -> u64 {
        self.child
    }

    /// The whole window's size for an editor of `size`.
    pub fn outer_size(&self, size: (u32, u32)) -> (u32, u32) {
        (size.0, size.1 + self.header)
    }

    /// The whole window's size (X11 pixels).
    pub fn size(&self, x: &Parents) -> Option<(u32, u32)> {
        x.size(self.top)
    }

    /// Move the window to `pos` (screen) once it is shown.
    pub fn move_when_shown(&self, pos: (i32, i32)) {
        self.pending_move.set(Some(pos));
    }

    /// Shown where it belongs: its position is worth keeping.
    pub fn settled(&self) -> bool {
        self.window.is_mapped() && self.placed.get().is_some()
    }

    /// X11 pixels per GTK pixel.
    pub fn scale(&self) -> u32 {
        scale_of(&self.window)
    }

    /// The room under the header in `top`'s pixels, while shown.
    fn area_rect(&self) -> Option<(i32, i32, u32, u32)> {
        if !self.window.is_mapped() {
            return None;
        }
        let (w, h) = (self.area.width(), self.area.height());
        if w <= 0 || h <= 0 {
            return None;
        }
        let at = self
            .area
            .compute_point(&self.window, &gtk::graphene::Point::zero())?;
        let (tx, ty) = self.window.surface_transform();
        let scale = scale_of(&self.window) as f64;
        Some((
            ((f64::from(at.x()) + tx) * scale).round() as i32,
            ((f64::from(at.y()) + ty) * scale).round() as i32,
            (f64::from(w) * scale).round() as u32,
            (f64::from(h) * scale).round() as u32,
        ))
    }

    /// Per UI tick: keep the plugin's window over the room under the
    /// header. Returns the room's size (X11 pixels) when it changed.
    pub fn follow(&self, x: &Parents) -> Option<(u32, u32)> {
        let rect = self.area_rect()?;
        if let Some(pos) = self.pending_move.take() {
            x.move_to(self.top, pos);
        }
        let before = self.placed.replace(Some(rect));
        if before == Some(rect) {
            return None;
        }
        x.place_child(self.child, rect);
        let size = (rect.2, rect.3);
        (before.map(|(.., w, h)| (w, h)) != Some(size)).then_some(size)
    }

    /// The plugin asked for another size (X11 pixels).
    pub fn request_size(&self, size: (u32, u32)) {
        let scale = scale_of(&self.window);
        let (w, h) = (size.0.div_ceil(scale) as i32, size.1.div_ceil(scale) as i32);
        if self.resizable {
            let header = (self.window.height() - self.area.height()).max(0);
            self.window.set_default_size(w, h + header);
        } else {
            self.area.set_content_width(w);
            self.area.set_content_height(h);
        }
    }

    pub fn show(&self) {
        self.window.present();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Close after the plugin closed its GUI: our window inside goes out
    /// first, then the GTK window.
    pub fn close(&self, x: Option<&Parents>) {
        if let Some(x) = x {
            x.release_child(self.child);
        }
        self.window.destroy();
    }
}

/// X11 pixels per GTK pixel of `window`.
fn scale_of(window: &gtk::Window) -> u32 {
    window
        .surface()
        .map_or(1, |s| s.scale_factor().max(1) as u32)
}
