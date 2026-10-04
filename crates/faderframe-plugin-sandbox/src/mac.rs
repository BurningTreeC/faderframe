//! macOS: the helper's AppKit side. A view cannot be embedded into
//! another process's window, so a sandboxed plugin's editor gets a window
//! of the helper's own ([`EditorWindow`]); the helper's main loop hands
//! AppKit its events and run-loop work between requests ([`pump`]).

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};
use std::ffi::c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
}

/// The application object, set up on first use: an accessory app (no Dock
/// icon, no menu bar of its own) whose windows can still take the focus.
fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    thread_local!(static READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    let app = NSApplication::sharedApplication(mtm);
    if !READY.with(|r| r.replace(true)) {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
    }
    app
}

/// Run what is due on the main thread without blocking: AppKit events
/// (once an editor window exists) and the run loop's timers and sources
/// (plugins use them with or without an editor).
pub fn pump(windows: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if windows {
        let app = app(mtm);
        // SAFETY: reads an immutable framework constant.
        let mode = unsafe { NSDefaultRunLoopMode };
        let past = NSDate::distantPast();
        while let Some(ev) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&past),
            mode,
            true,
        ) {
            app.sendEvent(&ev);
        }
        app.updateWindows();
    } else {
        // SAFETY: runs the calling (main) thread's run loop once, without
        // waiting.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0) };
    }
}

/// A window of the helper's own holding a plugin's editor view.
pub struct EditorWindow {
    window: Retained<NSWindow>,
    /// Content size last set or seen (points).
    size: (u32, u32),
    resizable: bool,
    shown: bool,
}

impl EditorWindow {
    pub fn new(title: &str, size: (u32, u32), resizable: bool) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let _ = app(mtm);
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
        // here, not released on close) until dropped.
        let window = unsafe {
            let w = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            w.setReleasedWhenClosed(false);
            w
        };
        window.setTitle(&NSString::from_str(title));
        let mut w = Self {
            window,
            size,
            resizable,
            shown: false,
        };
        w.resize(size);
        w.window.center();
        Some(w)
    }

    /// The content view the editor embeds into (an `NSView*`).
    pub fn view(&self) -> u64 {
        self.window
            .contentView()
            .map_or(0, |v| Retained::as_ptr(&v) as usize as u64)
    }

    /// Bring the window (and with it this helper) to the front.
    pub fn show(&mut self) {
        self.shown = true;
        if self.window.isMiniaturized() {
            self.window.deminiaturize(None);
        }
        self.window.makeKeyAndOrderFront(None);
        if let Some(mtm) = MainThreadMarker::new() {
            // `activate` needs macOS 14.
            #[allow(deprecated)]
            app(mtm).activateIgnoringOtherApps(true);
        }
    }

    pub fn hide(&mut self) {
        self.shown = false;
        self.window.orderOut(None);
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        let s = NSSize::new(f64::from(size.0.max(1)), f64::from(size.1.max(1)));
        self.size = size;
        if !self.resizable {
            self.window.setContentMinSize(s);
            self.window.setContentMaxSize(s);
        }
        self.window.setContentSize(s);
    }

    /// The user closed the window since the last call.
    pub fn closed(&mut self) -> bool {
        if self.shown && !self.window.isVisible() && !self.window.isMiniaturized() {
            self.shown = false;
            return true;
        }
        false
    }

    /// A size the user dragged the window to since the last call.
    pub fn dragged(&mut self) -> Option<(u32, u32)> {
        let view = self.window.contentView()?;
        let f = view.frame().size;
        let size = (f.width.round() as u32, f.height.round() as u32);
        (self.resizable && size != self.size && size.0 > 0 && size.1 > 0).then(|| {
            self.size = size;
            size
        })
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        self.window.close();
    }
}
