//! Editor parent windows on macOS: an `NSWindow` per editor whose content
//! view the plugin adds its own view to — a borderless child window over
//! the room under the editor header of a GTK window (`frame.rs`; GDK
//! turns every mouse event in its own windows into a GDK event, so a view
//! inside one would never get any), or a plain titled window. GTK's macOS
//! backend runs the application's event loop and passes events of windows
//! not its own to Cocoa, so these windows get theirs; closes and resizes
//! are noticed by polling on the UI tick.
//!
//! Positions are top-left corners in GDK's convention (y downwards from
//! the top of the primary screen); Cocoa's y axis points up from its
//! bottom.

use super::frame::Room;
use super::{ParentEvent, Rect};
use faderframe_plugin_host::{ParentWindow, WindowApi};
use gtk::glib::translate::ToGlibPtr;
use gtk::{gdk, glib, prelude::*};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSResponder, NSScreen, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;

define_class!(
    // SAFETY: NSWindow may be subclassed; the class adds no instance
    // variables and does not implement Drop.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FaderFrameEditorRoom"]
    struct RoomWindow;

    impl RoomWindow {
        /// Borderless windows do not take the keyboard by default; the
        /// plugin's text fields need it.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

/// `gdk_macos_surface_get_native_window` (GTK 4.8), looked up in the
/// running GTK.
type NativeWindow = unsafe extern "C" fn(*mut gdk::ffi::GdkSurface) -> *mut c_void;

fn native_window() -> Option<NativeWindow> {
    static F: OnceLock<Option<NativeWindow>> = OnceLock::new();
    *F.get_or_init(|| {
        let lib = libloading::os::unix::Library::this();
        // SAFETY: GTK's own function with its signature in
        // gdk/macos/gdkmacossurface.h; the library is the GTK already
        // loaded into the process, which stays loaded.
        let f = unsafe { lib.get::<NativeWindow>(b"gdk_macos_surface_get_native_window\0") }
            .ok()
            .map(|s| *s);
        std::mem::forget(lib);
        f
    })
}

struct Editor {
    window: Retained<NSWindow>,
    /// Content size last seen (points).
    size: (u32, u32),
    /// Shown by us (not hidden on the plugin's request).
    shown: bool,
    /// The GTK window (`Parents::tops`) a child window belongs to.
    child_of: Option<u64>,
    /// A child window attached to its parent (and so on screen).
    attached: bool,
}

pub struct Parents {
    mtm: MainThreadMarker,
    windows: RefCell<HashMap<u64, Editor>>,
    /// GTK's windows of editor frames, kept alive while we refer to them.
    tops: RefCell<HashMap<u64, Retained<NSWindow>>>,
    next: Cell<u64>,
}

impl Parents {
    pub fn connect() -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("plugin editors need the main thread")?;
        Ok(Self {
            mtm,
            windows: RefCell::new(HashMap::new()),
            tops: RefCell::new(HashMap::new()),
            next: Cell::new(1),
        })
    }

    /// Editor frames are ordinary GTK windows here.
    pub fn frame_display(&self) -> Option<gdk::Display> {
        gdk::Display::default()
    }

    /// The `NSWindow` of a GTK surface (kept until [`release_top`](Self::release_top)).
    pub fn frame_handle(&self, surface: &gdk::Surface) -> Option<u64> {
        let macos = glib::Type::from_name("GdkMacosSurface")?;
        if !surface.type_().is_a(macos) {
            return None;
        }
        let f = native_window()?;
        // SAFETY: a macOS surface (checked above), borrowed for the call.
        let ptr = unsafe { f(surface.to_glib_none().0) };
        // SAFETY: GDK's live `NSWindow` (or NULL), retained here.
        let window = unsafe { Retained::retain(ptr.cast::<NSWindow>()) }?;
        let id = Retained::as_ptr(&window) as usize as u64;
        self.tops.borrow_mut().insert(id, window);
        Some(id)
    }

    /// Editors and frames both work in points.
    pub fn frame_scale(&self, _surface: &gdk::Surface) -> f64 {
        1.0
    }

    pub fn release_top(&self, top: u64) {
        self.tops.borrow_mut().remove(&top);
    }

    /// A borderless window for the editor of the frame `top`, a child
    /// window of it once placed.
    pub fn create_child(&self, top: u64, size: (u32, u32)) -> Result<u64, String> {
        if !self.tops.borrow().contains_key(&top) {
            return Err("the editor frame has no window".into());
        }
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(size.0.max(1)), f64::from(size.1.max(1))),
        );
        let this = RoomWindow::alloc(self.mtm).set_ivars(());
        // SAFETY: NSWindow's designated initializer on a new window of
        // ours, on the main thread; it stays alive (retained here, not
        // released on close) until `destroy`.
        let window: Retained<RoomWindow> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        let window = Retained::into_super(window);
        // SAFETY: the window is ours and kept retained (see above).
        unsafe { window.setReleasedWhenClosed(false) };
        window.setHasShadow(false);
        let id = self.next.get();
        self.next.set(id + 1);
        self.windows.borrow_mut().insert(
            id,
            Editor {
                window,
                size,
                shown: false,
                child_of: Some(top),
                attached: false,
            },
        );
        Ok(id)
    }

    /// Keep a child window over `room` (points from the top-left of the
    /// frame's content) and attached; macOS moves it with the frame, but a
    /// resize from the top or left moves the room on screen too, so its
    /// place is checked every tick.
    pub fn place_child(&self, top: u64, child: u64, (x, y, w, h): Room, _changed: bool) {
        let tops = self.tops.borrow();
        let Some(parent) = tops.get(&top) else {
            return;
        };
        let content = parent.contentRectForFrameRect(parent.frame());
        let target = NSRect::new(
            NSPoint::new(
                content.origin.x + f64::from(x),
                content.origin.y + content.size.height - f64::from(y) - f64::from(h),
            ),
            NSSize::new(f64::from(w.max(1)), f64::from(h.max(1))),
        );
        self.with(child, |e| {
            let f = e.window.frame();
            let off = |a: f64, b: f64| (a - b).abs() > 0.5;
            if off(f.origin.x, target.origin.x)
                || off(f.origin.y, target.origin.y)
                || off(f.size.width, target.size.width)
                || off(f.size.height, target.size.height)
            {
                e.window.setFrame_display(target, true);
            }
            if !e.attached {
                // SAFETY: both windows are live; the child stays ours.
                unsafe { parent.addChildWindow_ordered(&e.window, NSWindowOrderingMode::Above) };
                e.window.orderFront(None);
                e.attached = true;
            }
        });
    }

    /// Take a child window off screen (its frame is hidden).
    pub fn hide_child(&self, top: u64, child: u64) {
        let tops = self.tops.borrow();
        self.with(child, |e| {
            if e.attached {
                if let Some(parent) = tops.get(&top) {
                    parent.removeChildWindow(&e.window);
                }
                e.window.orderOut(None);
                e.attached = false;
            }
        });
    }

    /// Close a child window before its frame goes (the editor closed
    /// first).
    pub fn release_child(&self, top: u64, child: u64) {
        self.hide_child(top, child);
        self.destroy(child);
    }

    /// Move a window (top-left corner, GDK's convention).
    pub fn move_to(&self, win: u64, (x, y): (i32, i32)) {
        let point = NSPoint::new(f64::from(x), self.primary_height() - f64::from(y));
        if let Some(top) = self.tops.borrow().get(&win) {
            top.setFrameTopLeftPoint(point);
            return;
        }
        self.with(win, |e| e.window.setFrameTopLeftPoint(point));
    }

    /// Window types are an X11 matter.
    pub fn set_dialog(&self, _win: u64) {}

    /// A window's frame (ours or a frame's GTK window).
    fn frame_of(&self, win: u64) -> Option<NSRect> {
        if let Some(top) = self.tops.borrow().get(&win) {
            return Some(top.frame());
        }
        self.with(win, |e| e.window.frame())
    }

    /// A window's size (points).
    pub fn size(&self, win: u64) -> Option<(u32, u32)> {
        self.frame_of(win)
            .map(|f| (f.size.width.round() as u32, f.size.height.round() as u32))
    }

    /// Height of the primary screen (the origin of both conventions).
    fn primary_height(&self) -> f64 {
        NSScreen::screens(self.mtm)
            .iter()
            .next()
            .map_or(0.0, |s| s.frame().size.height)
    }

    fn with<R>(&self, win: u64, f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        self.windows.borrow_mut().get_mut(&win).map(f)
    }

    pub fn create_window(
        &self,
        title: &str,
        size: (u32, u32),
        resizable: bool,
        pos: (i32, i32),
    ) -> Result<u64, String> {
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(size.0.max(1)), f64::from(size.1.max(1))),
        );
        // SAFETY: a new window on the main thread; it stays alive (retained
        // here, not released on close) until `destroy`.
        let window = unsafe {
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            w.setReleasedWhenClosed(false);
            w
        };
        window.setTitle(&NSString::from_str(title));
        let id = self.next.get();
        self.next.set(id + 1);
        self.windows.borrow_mut().insert(
            id,
            Editor {
                window,
                size,
                shown: false,
                child_of: None,
                attached: false,
            },
        );
        self.resize(id, size, resizable);
        self.place(id, pos);
        Ok(id)
    }

    /// The content view the editor embeds into.
    pub fn parent(&self, win: u64) -> ParentWindow {
        let view = self
            .with(win, |e| e.window.contentView())
            .flatten()
            .map_or(0, |v| Retained::as_ptr(&v) as usize as u64);
        ParentWindow {
            api: WindowApi::Cocoa,
            handle: view,
        }
    }

    /// Cocoa editors work in points; the system scales.
    pub fn scale(&self) -> f64 {
        1.0
    }

    fn place(&self, win: u64, (x, y): (i32, i32)) {
        let top = self.primary_height() - f64::from(y);
        self.with(win, |e| {
            e.window
                .setFrameTopLeftPoint(NSPoint::new(f64::from(x), top))
        });
    }

    pub fn map_at(&self, win: u64, pos: (i32, i32)) {
        self.place(win, pos);
        self.raise(win);
    }

    /// Visible screen areas and the one with the key window (FaderFrame's).
    pub fn monitors(&self, _main: Option<&str>) -> (Vec<Rect>, Option<Rect>) {
        let h = self.primary_height();
        let rect = |s: &NSScreen| {
            let f = s.visibleFrame();
            (
                f.origin.x as i32,
                (h - (f.origin.y + f.size.height)) as i32,
                f.size.width as i32,
                f.size.height as i32,
            )
        };
        let all = NSScreen::screens(self.mtm)
            .iter()
            .map(|s| rect(&s))
            .collect();
        let main = NSScreen::mainScreen(self.mtm).map(|s| rect(&s));
        (all, main)
    }

    pub fn position(&self, win: u64) -> Option<(i32, i32)> {
        let h = self.primary_height();
        self.frame_of(win)
            .map(|f| (f.origin.x as i32, (h - (f.origin.y + f.size.height)) as i32))
    }

    pub fn unmap(&self, win: u64) {
        self.with(win, |e| {
            e.shown = false;
            e.window.orderOut(None);
        });
    }

    pub fn raise(&self, win: u64) {
        self.with(win, |e| {
            e.shown = true;
            if e.window.isMiniaturized() {
                e.window.deminiaturize(None);
            }
            e.window.makeKeyAndOrderFront(None);
        });
    }

    pub fn resize(&self, win: u64, size: (u32, u32), resizable: bool) {
        let s = NSSize::new(f64::from(size.0.max(1)), f64::from(size.1.max(1)));
        self.with(win, |e| {
            e.size = size;
            if !resizable {
                e.window.setContentMinSize(s);
                e.window.setContentMaxSize(s);
            }
            e.window.setContentSize(s);
        });
    }

    pub fn destroy(&self, win: u64) {
        if let Some(e) = self.windows.borrow_mut().remove(&win) {
            e.window.close();
        }
    }

    /// Windows the user closed, content sizes that changed.
    pub fn events(&self) -> Vec<ParentEvent> {
        let mut out = Vec::new();
        for (&id, e) in self.windows.borrow_mut().iter_mut() {
            // Frames' windows answer to their GTK window.
            if e.child_of.is_some() {
                continue;
            }
            if e.shown && !e.window.isVisible() && !e.window.isMiniaturized() {
                e.shown = false;
                out.push(ParentEvent::Close(id));
                continue;
            }
            let Some(view) = e.window.contentView() else {
                continue;
            };
            let f = view.frame().size;
            let size = (f.width.round() as u32, f.height.round() as u32);
            if size != e.size {
                e.size = size;
                out.push(ParentEvent::Resized(id, size));
            }
        }
        out
    }

    /// Not available here (the screenshot action skips plugin editors).
    pub fn capture(&self, _win: u64, _size: (u32, u32)) -> Result<gtk::gdk::MemoryTexture, String> {
        Err("plugin editor screenshots are X11 only".into())
    }
}
