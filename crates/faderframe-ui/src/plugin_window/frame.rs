//! Native editors under the editor header (Bypass, Presets, Outputs,
//! Parameters). The editor window is a GTK window with the header as its
//! title bar and a room under it; the plugin's GUI goes into a native
//! window of ours laid over the room, kept there every UI tick. Where that
//! window lives is the backend's business (`Parents`):
//!
//! * X11: GTK on Wayland cannot hold another program's window, but GTK can
//!   open GDK's X11 display (XWayland) beside the Wayland one — the GTK
//!   window opens there, the plugin's window is an X11 child of it.
//! * Windows: a child window inside GTK's (GTK's DirectComposition
//!   content stays behind child windows).
//! * macOS: a borderless child window above GTK's: GDK takes every mouse
//!   event in its own windows, so a view inside one would never get any;
//!   macOS moves child windows with their parent.
//!
//! Backend pixels: physical pixels on X11 and Windows, points on macOS —
//! what plugins size their editors in.

use super::Parents;
use crate::state::AppState;
use gtk::{gdk, prelude::*};
use std::cell::Cell;
use std::rc::Rc;

/// A rectangle in backend pixels: x, y, width, height.
pub type Room = (i32, i32, u32, u32);

pub struct Frame {
    window: gtk::Window,
    /// The room under the header the plugin's GUI covers.
    area: gtk::DrawingArea,
    /// The GTK window's native window.
    top: u64,
    /// Ours for the plugin's GUI, over the room.
    child: u64,
    resizable: bool,
    /// The header's height (backend pixels).
    header: u32,
    /// The room as last placed (backend pixels in `top`), once placed.
    placed: Cell<Option<Room>>,
    /// Where the window goes once shown.
    pending_move: Cell<Option<(i32, i32)>>,
}

impl Frame {
    /// A GTK window with `header` as its title bar and room for an editor
    /// of `size` (backend pixels) under it, realized but not shown. `None`
    /// where the backend has no frames (or `FADERFRAME_EDITOR_HEADER=0`).
    pub fn build(
        app: &Rc<AppState>,
        x: &Parents,
        title: &str,
        header: &gtk::HeaderBar,
        size: (u32, u32),
        resizable: bool,
    ) -> Option<Frame> {
        if std::env::var_os("FADERFRAME_EDITOR_HEADER").is_some_and(|v| v == "0") {
            return None;
        }
        let display = x.frame_display()?;
        let window = gtk::Window::builder()
            .display(&display)
            .title(title)
            .resizable(resizable)
            .build();
        window.add_css_class("plugin-editor");
        if gdk::Display::default().is_some_and(|d| d == display) {
            window.set_application(Some(&app.app));
            if let Some(main) = app.window.borrow().as_ref() {
                window.set_transient_for(Some(main));
            }
        } else {
            // Not one of the application's windows (GtkApplication's
            // Wayland side would handle it as its own), but its actions
            // work here and it looks like the others.
            window.insert_action_group("app", Some(&app.app));
            crate::style::add_display(&display, &app.theme.borrow());
        }
        window.set_titlebar(Some(header));
        let area = gtk::DrawingArea::new();
        area.add_css_class("plugin-gui");
        window.set_child(Some(&area));
        WidgetExt::realize(&window);
        let Some((surface, top)) = window
            .surface()
            .and_then(|s| x.frame_handle(&s).map(|top| (s, top)))
        else {
            window.destroy();
            return None;
        };
        let scale = x.frame_scale(&surface);
        let (w, h) = logical(size, scale);
        let header_h = header.measure(gtk::Orientation::Vertical, -1).1.max(0);
        if resizable {
            area.set_hexpand(true);
            area.set_vexpand(true);
            window.set_default_size(w, h + header_h);
        } else {
            area.set_content_width(w);
            area.set_content_height(h);
        }
        let child = match x.create_child(top, size) {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!("no window for the plugin editor in its frame: {e}");
                x.release_top(top);
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
            header: (f64::from(header_h) * scale).round() as u32,
            placed: Cell::new(None),
            pending_move: Cell::new(None),
        })
    }

    pub fn window(&self) -> &gtk::Window {
        &self.window
    }

    /// The GTK window's native window.
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

    /// Move the window to `pos` (screen) once it is shown.
    pub fn move_when_shown(&self, pos: (i32, i32)) {
        self.pending_move.set(Some(pos));
    }

    /// Shown where it belongs: its position is worth keeping.
    pub fn settled(&self) -> bool {
        self.window.is_mapped() && self.placed.get().is_some()
    }

    /// Backend pixels per GTK pixel, rounded up.
    pub fn scale(&self, x: &Parents) -> u32 {
        self.window
            .surface()
            .map_or(1.0, |s| x.frame_scale(&s))
            .ceil()
            .max(1.0) as u32
    }

    /// The room under the header in `top`'s backend pixels, while shown.
    fn room(&self, x: &Parents) -> Option<Room> {
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
        let scale = x.frame_scale(&self.window.surface()?);
        Some((
            ((f64::from(at.x()) + tx) * scale).round() as i32,
            ((f64::from(at.y()) + ty) * scale).round() as i32,
            (f64::from(w) * scale).round() as u32,
            (f64::from(h) * scale).round() as u32,
        ))
    }

    /// Per UI tick: keep the plugin's window over the room under the
    /// header. Returns the room's size (backend pixels) when it changed.
    pub fn follow(&self, x: &Parents) -> Option<(u32, u32)> {
        let room = self.room(x)?;
        if let Some(pos) = self.pending_move.take() {
            x.move_to(self.top, pos);
        }
        let before = self.placed.replace(Some(room));
        x.place_child(self.top, self.child, room, before != Some(room));
        let size = (room.2, room.3);
        (before.map(|(.., w, h)| (w, h)) != Some(size)).then_some(size)
    }

    /// The plugin asked for another size (backend pixels).
    pub fn request_size(&self, x: &Parents, size: (u32, u32)) {
        let scale = self.window.surface().map_or(1.0, |s| x.frame_scale(&s));
        let (w, h) = logical(size, scale);
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

    pub fn hide(&self, x: &Parents) {
        self.window.set_visible(false);
        x.hide_child(self.top, self.child);
        self.placed.set(None);
    }

    /// Close after the plugin closed its GUI: our window for it goes
    /// first, then the GTK window.
    pub fn close(&self, x: Option<&Parents>) {
        if let Some(x) = x {
            x.release_child(self.top, self.child);
            x.release_top(self.top);
        }
        self.window.destroy();
    }
}

/// GTK pixels for `size` backend pixels (rounded up).
fn logical(size: (u32, u32), scale: f64) -> (i32, i32) {
    let scale = scale.max(f64::MIN_POSITIVE);
    (
        (f64::from(size.0) / scale).ceil() as i32,
        (f64::from(size.1) / scale).ceil() as i32,
    )
}
