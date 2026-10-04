//! Editor parent windows on macOS: an `NSWindow` per editor whose content
//! view the plugin adds its own view to. GTK's macOS backend runs the
//! application's event loop, so these windows get their events; closes and
//! resizes are noticed by polling on the UI tick.
//!
//! Positions are top-left corners in GDK's convention (y downwards from
//! the top of the primary screen); Cocoa's y axis points up from its
//! bottom.

use super::{ParentEvent, Rect};
use faderframe_plugin_host::{ParentWindow, WindowApi};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

struct Editor {
    window: Retained<NSWindow>,
    /// Content size last seen (points).
    size: (u32, u32),
    /// Shown by us (not hidden on the plugin's request).
    shown: bool,
}

pub struct Parents {
    mtm: MainThreadMarker,
    windows: RefCell<HashMap<u64, Editor>>,
    next: Cell<u64>,
}

impl Parents {
    pub fn connect() -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("plugin editors need the main thread")?;
        Ok(Self {
            mtm,
            windows: RefCell::new(HashMap::new()),
            next: Cell::new(1),
        })
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
        self.with(win, |e| {
            let f = e.window.frame();
            (f.origin.x as i32, (h - (f.origin.y + f.size.height)) as i32)
        })
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
