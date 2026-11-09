//! "Keep on Top": a plugin's editor windows above every other window
//! (`Session::plugin_window_on_top`, saved with the project; the pin in
//! the editor header and the mixer's insert menu set it). The session
//! holds the wish; [`keep`] brings the open windows in line on the UI
//! tick, by the platform's own means:
//!
//! * Windows: topmost windows (`HWND_TOPMOST`).
//! * macOS: the floating window level.
//! * Linux/BSD: X11 windows ask the window manager (EWMH
//!   `_NET_WM_STATE_ABOVE`), which X11 window managers and GNOME's, KDE's
//!   and most Wayland compositors honour for XWayland windows. Wayland
//!   gives applications no such request, so a built-in editor or parameter
//!   window kept on top opens on XWayland (where plugin GUIs live anyway).
//!   Hyprland honours neither and has its own: its pin, over its socket
//!   (`crate::hyprland`).
//!
//! `FADERFRAME_ON_TOP=x11|hyprland` picks the Linux way (tests).

use super::{EDITORS, Host, with_parents};
use crate::state::AppState;
use faderframe_core::PluginInstanceId;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

/// How windows are kept on top here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// The platform's own request on our windows.
    Native,
    /// Hyprland's pin.
    #[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
    Hyprland,
}

pub(super) fn route() -> Route {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        static ROUTE: std::sync::OnceLock<Route> = std::sync::OnceLock::new();
        *ROUTE.get_or_init(|| match std::env::var("FADERFRAME_ON_TOP").as_deref() {
            Ok("x11") => Route::Native,
            Ok("hyprland") => Route::Hyprland,
            _ if crate::hyprland::available() => Route::Hyprland,
            _ => Route::Native,
        })
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    Route::Native
}

/// Is the application's own display a Wayland one (where windows cannot
/// ask to stay on top)?
fn on_wayland() -> bool {
    glib::Type::from_name("GdkWaylandDisplay")
        .is_some_and(|wayland| gdk::Display::default().is_some_and(|d| d.type_().is_a(wayland)))
}

/// The display a built-in editor or parameter window of `plugin` opens
/// on: GDK's X11 display (XWayland) when it is to stay on top and only
/// there it can, else the application's (`None`).
pub(super) fn display_for(app: &AppState, plugin: PluginInstanceId) -> Option<gdk::Display> {
    if route() != Route::Native || !on_wayland() {
        return None;
    }
    let wanted = app
        .session
        .try_borrow()
        .is_ok_and(|s| s.plugin_window_on_top(plugin));
    if !wanted {
        return None;
    }
    super::ensure_parents();
    with_parents(|x| x.frame_display()).flatten()
}

/// Can windows be kept on top here at all (the pin's sensitivity)?
pub(super) fn supported() -> bool {
    match route() {
        Route::Hyprland => true,
        Route::Native if on_wayland() => {
            super::ensure_parents();
            with_parents(|x| x.frame_display().is_some()).unwrap_or(false)
        }
        Route::Native => true,
    }
}

/// A window to keep in line: one of ours (a frame's or a plain editor
/// window's native handle) or a GTK window.
enum Window {
    Native {
        handle: u64,
        title: String,
    },
    Gtk(gtk::Window),
    /// The plugin's own window (its format's, or a sandbox helper's),
    /// titled as FaderFrame asked.
    Floating {
        plugin: PluginInstanceId,
        title: String,
    },
}

/// FaderFrame's process, or one it started (a sandbox helper whose plugin
/// shows its own window).
#[cfg(all(unix, not(target_os = "macos")))]
fn ours_or_helpers(pid: u32) -> bool {
    let me = std::process::id();
    pid == me
        || std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                // "pid (name) state ppid …": the name may hold anything.
                let (_, rest) = stat.rsplit_once(')')?;
                rest.split_whitespace().nth(1)?.parse::<u32>().ok()
            })
            == Some(me)
}

/// What became of a request.
enum Outcome {
    Done,
    /// The window is not on screen yet (try again).
    NotYet,
    /// Only on another display (a Wayland window to reopen on XWayland).
    Reopen,
}

/// How many ticks a window that is not on screen yet is waited for
/// (`keep` runs every few UI ticks: about ten seconds).
const PATIENCE: u32 = 100;

/// One window's wish, as last carried out.
#[derive(Clone, Copy)]
pub(super) struct Applied {
    on: bool,
    tries: u32,
}

fn apply(app: &AppState, window: &Window, on: bool) -> Outcome {
    // (Hyprland and X11 find windows by it.)
    #[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(unused_variables))]
    let title = match window {
        Window::Native { title, .. } | Window::Floating { title, .. } => title.clone(),
        Window::Gtk(w) => w.title().map(|t| t.to_string()).unwrap_or_default(),
    };
    // A window the plugin's host side owns says so itself.
    if let Window::Floating { plugin, .. } = window
        && let Ok(mut s) = app.session.try_borrow_mut()
        && s.plugin_editor(*plugin).is_some_and(|ed| ed.set_above(on))
    {
        return Outcome::Done;
    }
    if route() == Route::Hyprland {
        #[cfg(all(unix, not(target_os = "macos")))]
        return match crate::hyprland::set_pinned(
            &title,
            on,
            if matches!(window, Window::Floating { .. }) {
                &ours_or_helpers
            } else {
                &|pid| pid == std::process::id()
            },
        ) {
            Ok(true) => Outcome::Done,
            Ok(false) => Outcome::NotYet,
            Err(e) => {
                tracing::warn!("keeping '{title}' on top: {e}");
                Outcome::Done
            }
        };
    }
    match window {
        Window::Native { handle, .. } => match with_parents(|x| {
            if x.mapped(*handle) {
                x.set_above(*handle, on);
                true
            } else {
                false
            }
        }) {
            Some(true) => Outcome::Done,
            Some(false) => Outcome::NotYet,
            None => Outcome::Done,
        },
        Window::Gtk(w) => {
            let Some(surface) = w.surface() else {
                return Outcome::NotYet;
            };
            super::ensure_parents();
            match with_parents(|x| x.set_surface_above(&surface, on)).flatten() {
                Some(true) => Outcome::Done,
                Some(false) => Outcome::NotYet,
                // A Wayland window: nothing to take back; to keep it on
                // top it moves to XWayland.
                None if on && supported() => Outcome::Reopen,
                None => Outcome::Done,
            }
        }
        // X11: found among the window manager's windows by title.
        Window::Floating { .. } => {
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                super::ensure_parents();
                let found = with_parents(|x| {
                    let windows = x.windows_titled(&title, &ours_or_helpers);
                    for w in &windows {
                        x.set_above(*w, on);
                    }
                    !windows.is_empty()
                });
                match found {
                    Some(true) => Outcome::Done,
                    Some(false) => Outcome::NotYet,
                    None => Outcome::Done,
                }
            }
            // Windows and macOS: another program's window (unless its
            // host side keeps it, above).
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            Outcome::Done
        }
    }
}

/// Bring the open editor windows in line with the session's wishes (a
/// window opens not on top; each is asked once a wish changes).
pub(super) fn keep(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let mut windows: Vec<(u64, PluginInstanceId, bool, Window, bool)> = Vec::new();
    EDITORS.with(|e| {
        let frames = e.frames.borrow();
        for n in e.native.borrow().iter() {
            let on = s.plugin_window_on_top(n.plugin);
            match n.host {
                Host::Framed(top) => {
                    if let Some(f) = frames.get(&n.plugin) {
                        let title = f
                            .window()
                            .title()
                            .map(|t| t.to_string())
                            .unwrap_or_default();
                        windows.push((
                            top,
                            n.plugin,
                            on,
                            Window::Native { handle: top, title },
                            false,
                        ));
                    }
                }
                Host::Parent(win) => {
                    let title = super::title_of(app, n.plugin)
                        .map(|(_, t)| t)
                        .unwrap_or_default();
                    windows.push((
                        win,
                        n.plugin,
                        on,
                        Window::Native { handle: win, title },
                        false,
                    ));
                }
                // The plugin's own window (no header, not ours).
                Host::Floating => {
                    let title = super::title_of(app, n.plugin)
                        .map(|(_, t)| t)
                        .unwrap_or_default();
                    windows.push((
                        n.plugin.raw() | (1 << 63),
                        n.plugin,
                        on,
                        Window::Floating {
                            plugin: n.plugin,
                            title,
                        },
                        false,
                    ));
                }
            }
        }
        for (device, map) in [(true, &e.devices), (false, &e.generic)] {
            for (plugin, w) in map.borrow().iter() {
                let key = w.as_ptr() as usize as u64;
                let on = s.plugin_window_on_top(*plugin);
                windows.push((key, *plugin, on, Window::Gtk(w.clone()), device));
            }
        }
    });
    drop(s);
    let mut reopen = Vec::new();
    EDITORS.with(|e| {
        let mut applied = e.on_top.borrow_mut();
        // Windows gone.
        applied.retain(|key, _| windows.iter().any(|w| w.0 == *key));
        for (key, plugin, on, window, device) in &windows {
            let now = applied.get(key).copied().unwrap_or(Applied {
                on: false,
                tries: 0,
            });
            if now.on == *on {
                continue;
            }
            match apply(app, window, *on) {
                Outcome::Done => {
                    applied.insert(*key, Applied { on: *on, tries: 0 });
                }
                Outcome::NotYet => {
                    let tries = now.tries + 1;
                    // Given up on: as if done (a wish changed again tries
                    // anew).
                    let on = if tries >= PATIENCE { *on } else { now.on };
                    applied.insert(
                        *key,
                        Applied {
                            on,
                            tries: if tries >= PATIENCE { 0 } else { tries },
                        },
                    );
                }
                Outcome::Reopen => reopen.push((*plugin, *device)),
            }
        }
    });
    for (plugin, device) in reopen {
        super::reopen_on_its_display(app, plugin, device);
    }
}

/// The header's pin: the plugin's windows on top. It follows the wish set
/// elsewhere (the mixer's insert menu, another of its windows).
pub(super) fn button(app: &Rc<AppState>, plugin: PluginInstanceId) -> gtk::ToggleButton {
    let pin = gtk::ToggleButton::new();
    pin.set_icon_name("view-pin-symbolic");
    let on = app
        .session
        .try_borrow()
        .is_ok_and(|s| s.plugin_window_on_top(plugin));
    pin.set_active(on);
    if supported() {
        pin.set_tooltip_text(Some(
            "Keep on Top: this window stays above every other window",
        ));
    } else {
        pin.set_sensitive(false);
        pin.set_tooltip_text(Some(
            "Keep on Top: this desktop lets no program keep its windows on top \
             (Wayland without XWayland)",
        ));
    }
    let following = Rc::new(std::cell::Cell::new(false));
    let weak = Rc::downgrade(app);
    let quiet = Rc::clone(&following);
    pin.connect_toggled(move |b| {
        if quiet.get() {
            return;
        }
        if let Some(a) = weak.upgrade() {
            a.dispatch(faderframe_session::Action::SetPluginWindowOnTop {
                plugin,
                on: b.is_active(),
            });
        }
    });
    let button = pin.downgrade();
    let weak = Rc::downgrade(app);
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let (Some(b), Some(a)) = (button.upgrade(), weak.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if let Ok(s) = a.session.try_borrow() {
            let now = s.plugin_window_on_top(plugin);
            if now != b.is_active() {
                following.set(true);
                b.set_active(now);
                following.set(false);
            }
        }
        glib::ControlFlow::Continue
    });
    pin
}
