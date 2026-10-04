//! Editor parent windows on X11 (Linux/BSD). FaderFrame is a Wayland
//! client, so these come from a separate X11 connection (XWayland): plain
//! top-level windows the plugins put their GUIs into.

use super::{ParentEvent, Rect};
use faderframe_plugin_host::{ParentWindow, WindowApi};
use gtk::glib;
use x11rb::connection::Connection;
use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

struct Atoms {
    wm_protocols: xproto::Atom,
    wm_delete_window: xproto::Atom,
    net_wm_name: xproto::Atom,
    utf8_string: xproto::Atom,
    net_wm_window_type: xproto::Atom,
    net_wm_window_type_dialog: xproto::Atom,
    net_wm_pid: xproto::Atom,
}

pub struct Parents {
    conn: RustConnection,
    root: xproto::Window,
    black: u32,
    atoms: Atoms,
}

fn x_err(e: impl std::fmt::Display) -> String {
    format!("X11: {e}")
}

impl Parents {
    pub fn connect() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(x_err)?;
        let (root, black) = {
            let s = &conn.setup().roots[screen];
            (s.root, s.black_pixel)
        };
        let atom = |name: &[u8]| -> Result<xproto::Atom, String> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(x_err)?
                .reply()
                .map_err(x_err)?
                .atom)
        };
        let atoms = Atoms {
            wm_protocols: atom(b"WM_PROTOCOLS")?,
            wm_delete_window: atom(b"WM_DELETE_WINDOW")?,
            net_wm_name: atom(b"_NET_WM_NAME")?,
            utf8_string: atom(b"UTF8_STRING")?,
            net_wm_window_type: atom(b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_dialog: atom(b"_NET_WM_WINDOW_TYPE_DIALOG")?,
            net_wm_pid: atom(b"_NET_WM_PID")?,
        };
        Ok(Self {
            conn,
            root,
            black,
            atoms,
        })
    }

    fn size_hints(
        &self,
        win: xproto::Window,
        (w, h): (u32, u32),
        resizable: bool,
        pos: Option<(i32, i32)>,
    ) {
        let mut hints = WmSizeHints::new();
        // A user-specified position: window managers keep it.
        hints.position = pos.map(|(x, y)| (WmSizeHintsSpecification::UserSpecified, x, y));
        if !resizable {
            // Fixed size: window managers float it instead of tiling.
            hints.min_size = Some((w as i32, h as i32));
            hints.max_size = Some((w as i32, h as i32));
        }
        let _ = hints.set_normal_hints(&self.conn, win);
    }

    pub fn create_window(
        &self,
        title: &str,
        size: (u32, u32),
        resizable: bool,
        pos: (i32, i32),
    ) -> Result<u64, String> {
        let win = self.conn.generate_id().map_err(x_err)?;
        let a = &self.atoms;
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                win,
                self.root,
                pos.0.clamp(-32_000, 32_000) as i16,
                pos.1.clamp(-32_000, 32_000) as i16,
                size.0.clamp(1, 16_000) as u16,
                size.1.clamp(1, 16_000) as u16,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &xproto::CreateWindowAux::new()
                    .background_pixel(self.black)
                    .event_mask(xproto::EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(x_err)?;
        let replace = xproto::PropMode::REPLACE;
        let _ = self.conn.change_property8(
            replace,
            win,
            xproto::AtomEnum::WM_NAME,
            xproto::AtomEnum::STRING,
            title.as_bytes(),
        );
        let _ = self.conn.change_property8(
            replace,
            win,
            a.net_wm_name,
            a.utf8_string,
            title.as_bytes(),
        );
        let _ = self.conn.change_property8(
            replace,
            win,
            xproto::AtomEnum::WM_CLASS,
            xproto::AtomEnum::STRING,
            b"faderframe-plugin\0FaderFrame\0",
        );
        let _ = self.conn.change_property32(
            replace,
            win,
            a.wm_protocols,
            xproto::AtomEnum::ATOM,
            &[a.wm_delete_window],
        );
        let _ = self.conn.change_property32(
            replace,
            win,
            a.net_wm_window_type,
            xproto::AtomEnum::ATOM,
            &[a.net_wm_window_type_dialog],
        );
        let _ = self.conn.change_property32(
            replace,
            win,
            a.net_wm_pid,
            xproto::AtomEnum::CARDINAL,
            &[std::process::id()],
        );
        self.size_hints(win, size, resizable, Some(pos));
        self.conn.flush().map_err(x_err)?;
        Ok(u64::from(win))
    }

    /// The handle editors embed into.
    pub fn parent(&self, win: u64) -> ParentWindow {
        ParentWindow {
            api: WindowApi::X11,
            handle: win,
        }
    }

    /// The display's scale (X11 editors take it from the system).
    pub fn scale(&self) -> f64 {
        1.0
    }

    /// Map at `pos` (asked again after mapping: some window managers only
    /// honour a configure request).
    pub fn map_at(&self, win: u64, pos: (i32, i32)) {
        let win = id(win);
        let _ = self.conn.map_window(win);
        let _ = self
            .conn
            .configure_window(win, &xproto::ConfigureWindowAux::new().x(pos.0).y(pos.1));
        let _ = self.conn.flush();
    }

    /// Monitors as the X server sees them, and the one with connector
    /// `main` (the one showing FaderFrame). With XWayland these can differ
    /// from the Wayland (logical) layout, e.g. physical pixels when the
    /// compositor disables XWayland scaling.
    pub fn monitors(&self, main: Option<&str>) -> (Vec<Rect>, Option<Rect>) {
        let all = self.named_monitors();
        let main_rect = all
            .iter()
            .find(|(name, _)| Some(name.as_str()) == main)
            .map(|(_, r)| *r);
        (all.into_iter().map(|(_, r)| r).collect(), main_rect)
    }

    fn named_monitors(&self) -> Vec<(String, Rect)> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let Ok(Ok(reply)) = self
            .conn
            .randr_get_monitors(self.root, true)
            .map(|c| c.reply())
        else {
            return Vec::new();
        };
        reply
            .monitors
            .iter()
            .map(|m| {
                let name = self
                    .conn
                    .get_atom_name(m.name)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                    .unwrap_or_default();
                (
                    name,
                    (
                        i32::from(m.x),
                        i32::from(m.y),
                        i32::from(m.width),
                        i32::from(m.height),
                    ),
                )
            })
            .collect()
    }

    /// Top-left corner on the screen.
    pub fn position(&self, win: u64) -> Option<(i32, i32)> {
        let r = self
            .conn
            .translate_coordinates(id(win), self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some((i32::from(r.dst_x), i32::from(r.dst_y)))
    }

    pub fn unmap(&self, win: u64) {
        let _ = self.conn.unmap_window(id(win));
        let _ = self.conn.flush();
    }

    pub fn raise(&self, win: u64) {
        let win = id(win);
        let _ = self.conn.map_window(win);
        let _ = self.conn.configure_window(
            win,
            &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE),
        );
        let _ = self.conn.flush();
    }

    pub fn resize(&self, win: u64, size: (u32, u32), resizable: bool) {
        let win = id(win);
        self.size_hints(win, size, resizable, None);
        let _ = self.conn.configure_window(
            win,
            &xproto::ConfigureWindowAux::new()
                .width(size.0.max(1))
                .height(size.1.max(1)),
        );
        let _ = self.conn.flush();
    }

    pub fn destroy(&self, win: u64) {
        let _ = self.conn.destroy_window(id(win));
        let _ = self.conn.flush();
    }
}

impl Parents {
    /// The window's pixels (including the embedded plugin GUI).
    pub fn capture(&self, win: u64, (w, h): (u32, u32)) -> Result<gtk::gdk::MemoryTexture, String> {
        let win = id(win);
        let reply = self
            .conn
            .get_image(
                xproto::ImageFormat::Z_PIXMAP,
                win,
                0,
                0,
                w as u16,
                h as u16,
                !0,
            )
            .map_err(x_err)?
            .reply()
            .map_err(x_err)?;
        if reply.depth != 24 && reply.depth != 32 {
            return Err(format!("X11: unsupported depth {}", reply.depth));
        }
        let stride = reply.data.len() / h.max(1) as usize;
        Ok(gtk::gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            gtk::gdk::MemoryFormat::B8g8r8x8,
            &glib::Bytes::from_owned(reply.data),
            stride,
        ))
    }
}

impl Parents {
    /// Close buttons and resizes since the last call.
    pub fn events(&self) -> Vec<ParentEvent> {
        let mut out = Vec::new();
        let a = &self.atoms;
        while let Ok(Some(ev)) = self.conn.poll_for_event() {
            match ev {
                Event::ClientMessage(m)
                    if m.type_ == a.wm_protocols && m.data.as_data32()[0] == a.wm_delete_window =>
                {
                    out.push(ParentEvent::Close(u64::from(m.window)));
                }
                Event::ConfigureNotify(c) => out.push(ParentEvent::Resized(
                    u64::from(c.window),
                    (u32::from(c.width), u32::from(c.height)),
                )),
                _ => {}
            }
        }
        out
    }
}

fn id(win: u64) -> xproto::Window {
    win as xproto::Window
}
