//! Plugin editor windows.
//!
//! Native editors (CLAP, VST3) embed into a plain top-level window of the
//! platform's own window system — an X11 window on Linux (through XWayland:
//! `x11.rs`), a Win32 window on Windows (`win32.rs`), an `NSWindow`'s
//! content view on macOS (`cocoa.rs`). Each backend offers the same
//! `Parents` API;
//! everything else here is shared. Plugins that cannot embed but can open
//! their own window do that (floating). Everything else — built-ins,
//! plugins without a GUI, no X server — gets the generic parameter window,
//! which also exists for every plugin on request.
//!
//! Plugin GUIs run on the host's main loop: on Linux through the CLAP
//! posix-fd and timer extensions (registered descriptors become glib fd
//! sources, timers glib timeouts, reconciled with what the plugins
//! registered every UI tick); on Windows and macOS GTK's main loop also
//! dispatches the native window messages the plugin windows receive.

#[cfg(target_os = "macos")]
mod cocoa;
#[cfg(windows)]
mod win32;
#[cfg(all(unix, not(target_os = "macos")))]
mod x11;

#[cfg(target_os = "macos")]
use cocoa::Parents;
#[cfg(windows)]
use win32::Parents;
#[cfg(all(unix, not(target_os = "macos")))]
use x11::Parents;

use crate::state::AppState;
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_plugin_host::{ParameterInfo, ParameterUnit, PluginFd, WindowApi};
use faderframe_project::Command;
use faderframe_session::{Action, PluginParameterView};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static EDITORS: Editors = Editors::default();
}

#[derive(Default)]
struct Editors {
    /// The platform's parent windows: lazily connected; a failure is
    /// remembered (and reported once).
    parents: RefCell<Option<Result<Parents, String>>>,
    native: RefCell<Vec<NativeEditor>>,
    generic: RefCell<HashMap<PluginInstanceId, gtk::Window>>,
    #[cfg(unix)]
    fds: RefCell<HashMap<(PluginInstanceId, i32), FdWatch>>,
    timers: RefCell<HashMap<(PluginInstanceId, u32), (u32, glib::SourceId)>>,
    ticks: Cell<u64>,
}

/// What a backend reports about its windows.
enum ParentEvent {
    /// The user closed the window.
    Close(u64),
    /// The window's content area has a new size.
    Resized(u64, (u32, u32)),
}

/// Save every embedded plugin editor next to `path` (`<stem>-plugin<n>.png`);
/// part of the `screenshot` action.
pub fn screenshot(path: &std::path::Path) {
    let editors: Vec<(u64, (u32, u32))> = EDITORS.with(|e| {
        e.native
            .borrow()
            .iter()
            .filter_map(|n| match n.host {
                Host::Parent(win) => Some((win, n.size)),
                Host::Floating => None,
            })
            .collect()
    });
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().to_string());
    for (i, (win, size)) in editors.into_iter().enumerate() {
        let file = path.with_file_name(format!("{stem}-plugin{}.png", i + 1));
        match with_parents(|x| x.capture(win, size)) {
            Some(Ok(tex)) => match tex.save_to_png(&file) {
                Ok(()) => tracing::info!("screenshot saved to {}", file.display()),
                Err(e) => tracing::warn!("plugin editor screenshot failed: {e}"),
            },
            Some(Err(e)) => tracing::warn!("plugin editor screenshot failed: {e}"),
            None => {}
        }
    }
}

// --- native editors ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Host {
    /// A window of ours (the backend's id).
    Parent(u64),
    /// The plugin's own top-level window.
    Floating,
}

struct NativeEditor {
    plugin: PluginInstanceId,
    host: Host,
    size: (u32, u32),
    resizable: bool,
    /// Last known top-left corner (embedded editors).
    pos: Option<(i32, i32)>,
}

type Rect = (i32, i32, i32, i32);

/// Monitor rectangles in the backend's screen coordinates and the one
/// showing the main window (X11: the X server's own RandR geometry matched
/// to the Wayland monitor by connector name). GDK's logical geometry is the
/// fallback.
fn monitors(app: &AppState) -> (Vec<Rect>, Option<Rect>) {
    let Some(display) = gtk::gdk::Display::default() else {
        return (Vec::new(), None);
    };
    let main = app
        .window
        .borrow()
        .as_ref()
        .and_then(|w| w.surface())
        .and_then(|s| display.monitor_at_surface(&s));
    let connector = main
        .as_ref()
        .and_then(|m| m.connector())
        .map(|c| c.to_string());
    if let Some((all, main_rect)) = with_parents(|x| x.monitors(connector.as_deref()))
        && !all.is_empty()
    {
        return (all, main_rect);
    }
    let rect = |m: &gtk::gdk::Monitor| {
        let g = m.geometry();
        (g.x(), g.y(), g.width(), g.height())
    };
    let list = display.monitors();
    let all: Vec<_> = (0..list.n_items())
        .filter_map(|i| list.item(i).and_downcast::<gtk::gdk::Monitor>())
        .map(|m| rect(&m))
        .collect();
    (all, main.map(|m| rect(&m)))
}

/// Where an editor of `size` opens: its last position if that is still on a
/// monitor, else centred on the monitor showing FaderFrame.
fn placement(app: &AppState, plugin: PluginInstanceId, size: (u32, u32)) -> (i32, i32) {
    let stored = app
        .session
        .try_borrow()
        .ok()
        .and_then(|s| s.workspace().plugin_windows.get(&plugin).copied());
    let (all, main) = monitors(app);
    place(stored, size, &all, main)
}

fn place(
    stored: Option<(i32, i32)>,
    (w, h): (u32, u32),
    monitors: &[Rect],
    main: Option<Rect>,
) -> (i32, i32) {
    // The title bar (top-left area) must be reachable on some monitor.
    let visible = |(x, y): (i32, i32)| {
        monitors.iter().any(|&(mx, my, mw, mh)| {
            x + 40 >= mx && x + 40 < mx + mw && y >= my && y + 20 < my + mh
        })
    };
    if let Some(p) = stored
        && visible(p)
    {
        return p;
    }
    let Some((mx, my, mw, mh)) = main.or_else(|| monitors.first().copied()) else {
        return (0, 0);
    };
    let (w, h) = (w.min(i32::MAX as u32) as i32, h.min(i32::MAX as u32) as i32);
    (mx + ((mw - w) / 2).max(0), my + ((mh - h) / 2).max(0))
}

/// Remember an editor's position in the project's layout.
fn store_position(app: &Rc<AppState>, plugin: PluginInstanceId, pos: (i32, i32)) {
    if let Ok(mut s) = app.session.try_borrow_mut() {
        let _ = s.dispatch(Action::SetPluginWindowPosition {
            plugin,
            x: pos.0,
            y: pos.1,
        });
    }
}

fn with_parents<R>(f: impl FnOnce(&Parents) -> R) -> Option<R> {
    EDITORS.with(|e| match e.parents.borrow().as_ref() {
        Some(Ok(x)) => Some(f(x)),
        _ => None,
    })
}

fn drop_host(host: Host) {
    if let Host::Parent(win) = host {
        with_parents(|x| x.destroy(win));
    }
}

/// The track a plugin's commands name and the window title. An album
/// song's insert names the master (the commands find the song's slot).
fn title_of(app: &AppState, plugin: PluginInstanceId) -> Option<(TrackId, String)> {
    let s = app.session.try_borrow().ok()?;
    if let Some((song, slot)) = s.song_insert(plugin) {
        let master = s.project().master_id()?;
        return Some((
            master,
            format!("{} — {} (album)", slot.plugin.name, song.title),
        ));
    }
    let (t, slot) = s.plugin_slot(plugin)?;
    Some((t.id, format!("{} — {}", slot.plugin.name, t.name)))
}

/// Show `plugin`'s editor: its own GUI unless `generic` (or it has none).
pub fn open(app: &Rc<AppState>, plugin: PluginInstanceId, generic: bool) {
    if !generic && open_native(app, plugin) {
        return;
    }
    open_generic(app, plugin);
}

/// Whether the plugin has a GUI of its own.
fn has_native(app: &AppState, plugin: PluginInstanceId) -> bool {
    app.session
        .try_borrow_mut()
        .is_ok_and(|mut s| s.plugin_editor(plugin).is_some())
}

fn open_native(app: &Rc<AppState>, plugin: PluginInstanceId) -> bool {
    let existing = EDITORS.with(|e| {
        e.native
            .borrow()
            .iter()
            .find(|n| n.plugin == plugin)
            .map(|n| n.host)
    });
    if let Some(host) = existing {
        match host {
            Host::Parent(win) => {
                with_parents(|x| x.raise(win));
            }
            Host::Floating => {
                if let Ok(mut s) = app.session.try_borrow_mut()
                    && let Some(ed) = s.plugin_editor(plugin)
                {
                    ed.raise();
                }
            }
        }
        return true;
    }
    let Some((_, title)) = title_of(app, plugin) else {
        return false;
    };
    EDITORS.with(|e| {
        e.parents.borrow_mut().get_or_insert_with(|| {
            let r = Parents::connect();
            if let Err(err) = &r {
                tracing::warn!("no windows for plugin editors ({err}); using generic editors");
            }
            r
        });
    });
    let mut failure = None;
    let opened = {
        let Ok(mut s) = app.session.try_borrow_mut() else {
            return false;
        };
        let Some(ed) = s.plugin_editor(plugin) else {
            tracing::debug!("{title}: no editor of its own");
            return false;
        };
        let mut opened = None;
        let api = WindowApi::NATIVE;
        let (embed, float) = (ed.can_embed(api), ed.can_float(api));
        tracing::debug!("{title}: editor embeds ({api:?}): {embed}, floats: {float}");
        if embed {
            let embedded = EDITORS.with(|e| {
                let parents = e.parents.borrow();
                let Some(Ok(x)) = parents.as_ref() else {
                    return None;
                };
                let size = match ed.open_embedded(api, x.scale()) {
                    Ok(size) => size,
                    Err(err) => {
                        failure = Some(err.to_string());
                        return None;
                    }
                };
                let resizable = ed.can_resize();
                let pos = placement(app, plugin, size);
                let win = match x.create_window(&title, size, resizable, pos) {
                    Ok(win) => win,
                    Err(err) => {
                        ed.close();
                        failure = Some(err);
                        return None;
                    }
                };
                match ed.attach(x.parent(win)) {
                    Ok(()) => {
                        x.map_at(win, pos);
                        tracing::debug!("plugin editor {plugin}: {size:?} placed at {pos:?}");
                        Some(NativeEditor {
                            plugin,
                            host: Host::Parent(win),
                            size,
                            resizable,
                            pos: Some(pos),
                        })
                    }
                    Err(err) => {
                        x.destroy(win);
                        failure = Some(err.to_string());
                        None
                    }
                }
            });
            opened = embedded;
        }
        if opened.is_none() && float {
            match ed.open_floating(api, &title) {
                Ok(()) => {
                    opened = Some(NativeEditor {
                        plugin,
                        host: Host::Floating,
                        size: (0, 0),
                        resizable: false,
                        pos: None,
                    });
                }
                Err(err) => failure = Some(err.to_string()),
            }
        }
        opened
    };
    if let Some(err) = &failure {
        tracing::warn!("{title}: the plugin's editor failed: {err}");
    }
    if let Some(err) = failure
        && let Ok(mut s) = app.session.try_borrow_mut()
    {
        s.notify(
            faderframe_session::NoticeLevel::Warning,
            format!("{title}: {err}"),
        );
    }
    match opened {
        Some(n) => {
            EDITORS.with(|e| e.native.borrow_mut().push(n));
            true
        }
        None => false,
    }
}

fn close_native(app: &Rc<AppState>, plugin: PluginInstanceId) {
    let host = EDITORS.with(|e| {
        let mut native = e.native.borrow_mut();
        let i = native.iter().position(|n| n.plugin == plugin)?;
        Some(native.remove(i).host)
    });
    let Some(host) = host else { return };
    if let Host::Parent(win) = host
        && let Some(Some(pos)) = with_parents(|x| x.position(win))
    {
        store_position(app, plugin, pos);
    }
    if let Ok(mut s) = app.session.try_borrow_mut()
        && let Some(ed) = s.plugin_editor(plugin)
    {
        ed.close();
    }
    drop_host(host);
}

/// The parent windows' events: close buttons and resizes.
fn pump_parents(app: &Rc<AppState>) {
    let events = with_parents(|x| x.events()).unwrap_or_default();
    for ev in events {
        let find = |win: u64| {
            EDITORS.with(|e| {
                e.native
                    .borrow()
                    .iter()
                    .find(|n| n.host == Host::Parent(win))
                    .map(|n| (n.plugin, n.size, n.resizable))
            })
        };
        match ev {
            ParentEvent::Close(win) => {
                if let Some((plugin, ..)) = find(win) {
                    close_native(app, plugin);
                }
            }
            ParentEvent::Resized(win, new) => {
                let Some((plugin, size, resizable)) = find(win) else {
                    continue;
                };
                if !resizable || new == size || new.0 == 0 || new.1 == 0 {
                    continue;
                }
                let applied = app
                    .session
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut s| s.plugin_editor(plugin)?.set_size(new.0, new.1));
                EDITORS.with(|e| {
                    if let Some(n) = e
                        .native
                        .borrow_mut()
                        .iter_mut()
                        .find(|n| n.plugin == plugin)
                    {
                        n.size = applied.unwrap_or(new);
                    }
                });
                if let Some(a) = applied
                    && a != new
                {
                    with_parents(|x| x.resize(win, a, true));
                }
            }
        }
    }
}

/// Per UI frame: plugin GUI requests, parent window events, fd and timer
/// sources, windows of removed plugins.
pub fn tick(app: &Rc<AppState>) {
    pump_parents(app);
    // Follow where the user moves editors (twice a second).
    let n = EDITORS.with(|e| {
        let n = e.ticks.get() + 1;
        e.ticks.set(n);
        n
    });
    if n.is_multiple_of(30) {
        track_positions(app);
    }
    // Plugins being edited play on the audio thread (not rendered ahead):
    // what is turned is heard at once.
    let edited: std::collections::HashSet<PluginInstanceId> = EDITORS.with(|e| {
        e.native
            .borrow()
            .iter()
            .map(|n| n.plugin)
            .chain(e.generic.borrow().keys().copied())
            .collect()
    });
    if let Ok(mut s) = app.session.try_borrow_mut() {
        s.set_plugins_being_edited(&edited);
    }
    let natives: Vec<(PluginInstanceId, Host, bool)> = EDITORS.with(|e| {
        e.native
            .borrow()
            .iter()
            .map(|n| (n.plugin, n.host, n.resizable))
            .collect()
    });
    for (plugin, host, resizable) in natives {
        let Ok(mut s) = app.session.try_borrow_mut() else {
            return;
        };
        let requests = match s.plugin_editor(plugin) {
            Some(ed) if ed.is_open() => ed.take_requests(),
            // Plugin removed, or the engine (and its instances) recreated.
            _ => {
                drop(s);
                EDITORS.with(|e| e.native.borrow_mut().retain(|n| n.plugin != plugin));
                drop_host(host);
                continue;
            }
        };
        drop(s);
        if requests.closed {
            close_native(app, plugin);
            continue;
        }
        if let Host::Parent(win) = host {
            if let Some(size) = requests.resize {
                with_parents(|x| x.resize(win, size, resizable));
                EDITORS.with(|e| {
                    if let Some(n) = e
                        .native
                        .borrow_mut()
                        .iter_mut()
                        .find(|n| n.plugin == plugin)
                    {
                        n.size = size;
                    }
                });
            }
            if requests.hide {
                with_parents(|x| x.unmap(win));
            }
            if requests.show {
                with_parents(|x| x.raise(win));
            }
        }
    }
    reconcile_sources(app);
    // Generic windows of plugins that are gone.
    let stale: Vec<PluginInstanceId> = EDITORS.with(|e| {
        let s = app.session.try_borrow();
        e.generic
            .borrow()
            .keys()
            .filter(|p| {
                s.as_ref()
                    .is_ok_and(|s| s.plugin_slot(**p).is_none() && s.song_insert(**p).is_none())
            })
            .copied()
            .collect()
    });
    for p in stale {
        if let Some(w) = EDITORS.with(|e| e.generic.borrow_mut().remove(&p)) {
            w.close();
        }
    }
}

fn track_positions(app: &Rc<AppState>) {
    type Open = (PluginInstanceId, u64, Option<(i32, i32)>);
    let open: Vec<Open> = EDITORS.with(|e| {
        e.native
            .borrow()
            .iter()
            .filter_map(|n| match n.host {
                Host::Parent(win) => Some((n.plugin, win, n.pos)),
                Host::Floating => None,
            })
            .collect()
    });
    for (plugin, win, last) in open {
        let Some(Some(pos)) = with_parents(|x| x.position(win)) else {
            continue;
        };
        if Some(pos) == last {
            continue;
        }
        tracing::debug!("plugin editor {plugin} now at {pos:?} (was {last:?})");
        EDITORS.with(|e| {
            if let Some(n) = e
                .native
                .borrow_mut()
                .iter_mut()
                .find(|n| n.plugin == plugin)
            {
                n.pos = Some(pos);
            }
        });
        store_position(app, plugin, pos);
    }
}

// --- plugin event sources -----------------------------------------------------------

#[cfg(unix)]
struct FdWatch {
    fd: PluginFd,
    source: Option<glib::SourceId>,
    /// The source removed itself (descriptor closed under it).
    gone: Rc<Cell<bool>>,
}

#[cfg(unix)]
fn watch_fd(app: &Rc<AppState>, plugin: PluginInstanceId, fd: PluginFd) -> FdWatch {
    use glib::IOCondition as C;
    let mut cond = C::empty();
    if fd.read {
        cond |= C::IN | C::PRI;
    }
    if fd.write {
        cond |= C::OUT;
    }
    if fd.error {
        cond |= C::ERR | C::HUP;
    }
    let gone = Rc::new(Cell::new(false));
    let flag = Rc::clone(&gone);
    let weak = Rc::downgrade(app);
    let source = glib_unix::unix_fd_add_local(fd.fd, cond, move |_, c| {
        if c.contains(C::NVAL) {
            flag.set(true);
            return glib::ControlFlow::Break;
        }
        if let Some(a) = weak.upgrade()
            && let Ok(mut s) = a.session.try_borrow_mut()
        {
            s.plugin_on_fd(
                plugin,
                PluginFd {
                    fd: fd.fd,
                    read: c.intersects(C::IN | C::PRI),
                    write: c.contains(C::OUT),
                    error: c.intersects(C::ERR | C::HUP),
                },
            );
        }
        glib::ControlFlow::Continue
    });
    FdWatch {
        fd,
        source: Some(source),
        gone,
    }
}

/// Match glib sources to the descriptors and timers plugins registered.
fn reconcile_sources(app: &Rc<AppState>) {
    let Ok(sources) = app.session.try_borrow().map(|s| s.plugin_event_sources()) else {
        return;
    };
    // Descriptors exist only on Unix (the CLAP posix-fd extension).
    #[cfg_attr(not(unix), allow(unused_mut, unused_variables))]
    let mut want_fds: HashMap<(PluginInstanceId, i32), PluginFd> = HashMap::new();
    let mut want_timers = HashMap::new();
    for (plugin, src) in sources {
        for fd in src.fds {
            want_fds.insert((plugin, fd.fd), fd);
        }
        for (id, period) in src.timers {
            want_timers.insert((plugin, id), period);
        }
    }
    EDITORS.with(|e| {
        #[cfg(unix)]
        {
            let mut fds = e.fds.borrow_mut();
            fds.retain(|k, w| {
                let keep = want_fds.get(k) == Some(&w.fd) && !w.gone.get();
                if !keep
                    && !w.gone.get()
                    && let Some(source) = w.source.take()
                {
                    source.remove();
                }
                keep
            });
            for (k, fd) in want_fds {
                fds.entry(k).or_insert_with(|| watch_fd(app, k.0, fd));
            }
        }
        let mut timers = e.timers.borrow_mut();
        let stale: Vec<_> = timers
            .iter()
            .filter(|(k, (period, _))| want_timers.get(k) != Some(period))
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            if let Some((_, source)) = timers.remove(&k) {
                source.remove();
            }
        }
        for ((plugin, id), period) in want_timers {
            timers.entry((plugin, id)).or_insert_with(|| {
                let weak = Rc::downgrade(app);
                let source = glib::timeout_add_local(
                    Duration::from_millis(u64::from(period.max(1))),
                    move || {
                        if let Some(a) = weak.upgrade()
                            && let Ok(mut s) = a.session.try_borrow_mut()
                        {
                            s.plugin_on_timer(plugin, id);
                        }
                        glib::ControlFlow::Continue
                    },
                );
                (period, source)
            });
        }
    });
}

// --- generic editor ------------------------------------------------------------------

/// Fallback text for a value in its unit.
fn unit_text(info: &ParameterInfo, v: f64) -> String {
    match info.unit {
        ParameterUnit::Decibels if v <= -96.0 => "-∞ dB".into(),
        ParameterUnit::Decibels => format!("{v:.1} dB"),
        ParameterUnit::Milliseconds if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
        ParameterUnit::Milliseconds => format!("{v:.1} ms"),
        ParameterUnit::Hertz if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
        ParameterUnit::Hertz => format!("{v:.0} Hz"),
        ParameterUnit::Percent => format!("{:.0} %", v * 100.0),
        ParameterUnit::Samples => format!("{v:.0} smp"),
        _ if info.stepped && info.min == 0.0 && info.max == 1.0 => {
            if v >= 0.5 { "On" } else { "Off" }.into()
        }
        ParameterUnit::None if info.stepped => format!("{v:.0}"),
        ParameterUnit::None => format!("{v:.3}"),
    }
}

fn value_text(app: &AppState, plugin: PluginInstanceId, info: &ParameterInfo, v: f64) -> String {
    app.session
        .try_borrow_mut()
        .ok()
        .and_then(|mut s| s.format_plugin_parameter(plugin, info.id, v))
        .unwrap_or_else(|| unit_text(info, v))
}

enum Control {
    Scale(gtk::Scale),
    Switch(gtk::Switch),
}

struct Row {
    info: ParameterInfo,
    control: Control,
    value: gtk::Label,
    row: gtk::ListBoxRow,
    shown: Cell<f64>,
}

/// One undo step per burst of changes: a gesture opens with the first change
/// and closes once the controls are quiet for a moment.
struct Gesture {
    open: Cell<bool>,
    generation: Cell<u64>,
}

struct Generic {
    app: std::rc::Weak<AppState>,
    track: TrackId,
    plugin: PluginInstanceId,
    rows: Vec<Row>,
    gesture: Gesture,
}

const GESTURE_QUIET_MS: u64 = 450;

impl Generic {
    fn set(self: &Rc<Self>, i: usize, v: f64) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let row = &self.rows[i];
        let v = row.info.clamp(if row.info.stepped { v.round() } else { v });
        if !self.gesture.open.get() {
            app.dispatch(Action::BeginGesture(format!("Change {}", row.info.name)));
            self.gesture.open.set(true);
        }
        app.dispatch(Action::Edit(Command::SetPluginParameter {
            track: self.track,
            plugin: self.plugin,
            parameter: row.info.id,
            value: Some(v),
        }));
        row.shown.set(v);
        row.value
            .set_text(&value_text(&app, self.plugin, &row.info, v));
        let generation = self.gesture.generation.get() + 1;
        self.gesture.generation.set(generation);
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(GESTURE_QUIET_MS), move || {
            if let Some(me) = me.upgrade()
                && me.gesture.generation.get() == generation
                && me.gesture.open.replace(false)
                && let Some(app) = me.app.upgrade()
            {
                app.dispatch(Action::EndGesture);
            }
        });
    }

    /// Follow values changed elsewhere (automation, the plugin's own GUI).
    fn refresh(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if self.gesture.open.get() {
            return;
        }
        for row in &self.rows {
            let Some(v) = app
                .session
                .try_borrow_mut()
                .ok()
                .and_then(|mut s| s.plugin_parameter_value(self.plugin, row.info.id))
            else {
                continue;
            };
            if (v - row.shown.get()).abs() <= f64::EPSILON * v.abs().max(1.0) {
                continue;
            }
            row.shown.set(v);
            match &row.control {
                Control::Scale(s) => s.set_value(v),
                Control::Switch(s) => s.set_active(v >= 0.5),
            }
            row.value
                .set_text(&value_text(&app, self.plugin, &row.info, v));
        }
    }
}

fn open_generic(app: &Rc<AppState>, plugin: PluginInstanceId) {
    if let Some(w) = EDITORS.with(|e| e.generic.borrow().get(&plugin).cloned()) {
        w.present();
        return;
    }
    let Some((track, title)) = title_of(app, plugin) else {
        return;
    };
    let (params, vendor, bypassed, name) = {
        let mut s = app.session.borrow_mut();
        let params: Vec<PluginParameterView> = s.plugin_parameter_views(plugin);
        let info = s.plugin_owner(plugin).map(|(_, slot)| {
            let vendor = s
                .available_plugins()
                .into_iter()
                .find(|p| p.plugin.id == slot.plugin.id && p.plugin.format == slot.plugin.format)
                .map(|p| p.vendor)
                .unwrap_or_default();
            (vendor, slot.bypass, slot.plugin.name.clone())
        });
        let Some((vendor, bypassed, name)) = info else {
            return;
        };
        (params, vendor, bypassed, name)
    };

    let window = gtk::Window::builder()
        .application(&app.app)
        .title(&title)
        .default_width(520)
        .default_height(640)
        .build();
    window.add_css_class("plugin-editor");
    if let Some(main) = app.window.borrow().as_ref() {
        window.set_transient_for(Some(main));
    }

    let header = gtk::HeaderBar::new();
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let name_label = gtk::Label::new(Some(&name));
    name_label.add_css_class("title");
    let sub = gtk::Label::new(Some(&title_of(app, plugin).map_or_else(
        String::new,
        |(_, t)| {
            let track_name = t.rsplit(" — ").next().unwrap_or_default().to_string();
            if vendor.is_empty() {
                track_name
            } else {
                format!("{vendor} · {track_name}")
            }
        },
    )));
    sub.add_css_class("subtitle");
    heading.append(&name_label);
    heading.append(&sub);
    header.set_title_widget(Some(&heading));

    let bypass = gtk::ToggleButton::with_label("Bypass");
    bypass.set_active(bypassed);
    bypass.add_css_class("bypass-toggle");
    {
        let weak = Rc::downgrade(app);
        bypass.connect_toggled(move |b| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Edit(Command::SetPluginBypass {
                    track,
                    plugin,
                    bypass: b.is_active(),
                }));
            }
        });
    }
    header.pack_start(&bypass);
    // Presets: rebuilt each time the menu opens.
    let presets = gtk::MenuButton::new();
    presets.set_label("Presets");
    let weak = Rc::downgrade(app);
    presets.set_create_popup_func(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let menu = gtk::gio::Menu::new();
        let save = gtk::gio::Menu::new();
        let item = gtk::gio::MenuItem::new(Some("Save Preset…"), None);
        item.set_action_and_target_value(
            Some("app.save-preset"),
            Some(&plugin.raw().to_string().to_variant()),
        );
        save.append_item(&item);
        menu.append_section(None, &save);
        let list = gtk::gio::Menu::new();
        let found = app.session.borrow().plugin_presets(plugin);
        if found.is_empty() {
            list.append(Some("No presets yet"), None);
        }
        for p in found {
            let label = if p.factory {
                format!("{} (factory)", p.name)
            } else {
                p.name.clone()
            }
            .replace('_', "__");
            let item = gtk::gio::MenuItem::new(Some(&label), None);
            let target = format!("{}\n{}", plugin.raw(), p.path.display());
            item.set_action_and_target_value(Some("app.load-preset"), Some(&target.to_variant()));
            list.append_item(&item);
        }
        menu.append_section(None, &list);
        // The plugin's own programs.
        let (programs, current) = {
            let s = app.session.borrow();
            (s.plugin_programs(plugin), s.plugin_current_program(plugin))
        };
        if !programs.is_empty() {
            let section = gtk::gio::Menu::new();
            for (i, name) in programs.iter().enumerate() {
                let mark = if current == Some(i) { "● " } else { "" };
                let label = format!("{mark}{name}").replace('_', "__");
                let item = gtk::gio::MenuItem::new(Some(&label), None);
                let target = format!("{}\n{i}", plugin.raw());
                item.set_action_and_target_value(
                    Some("app.select-program"),
                    Some(&target.to_variant()),
                );
                section.append_item(&item);
            }
            menu.append_section(Some("Programs"), &section);
        }
        button.set_menu_model(Some(&menu));
    });
    header.pack_start(&presets);
    if has_native(app, plugin) {
        let native = gtk::Button::with_label("Plugin GUI");
        native.set_tooltip_text(Some("Open the plugin's own editor"));
        let weak = Rc::downgrade(app);
        native.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade()
                && !open_native(&a, plugin)
            {
                tracing::warn!("the plugin editor could not be opened");
            }
        });
        header.pack_end(&native);
    }
    window.set_titlebar(Some(&header));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&format!("Filter {} parameters…", params.len())));
    search.add_css_class("param-search");
    body.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("param-list");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    body.append(&scroller);
    if params.is_empty() {
        let empty = gtk::Label::new(Some("This plugin has no parameters."));
        empty.add_css_class("dim-label");
        empty.set_margin_top(24);
        list.append(&empty);
    }

    let mut rows = Vec::new();
    let mut module = None;
    for p in &params {
        let info = p.info.clone();
        // CLAP parameters carry a module path ("Filter/Cutoff"): section
        // headers instead of repeated prefixes.
        let (group, short) = match info.name.rsplit_once('/') {
            Some((g, n)) => (Some(g.to_string()), n.to_string()),
            None => (None, info.name.clone()),
        };
        if group.is_some() && group != module {
            let heading = gtk::Label::new(group.as_deref().map(str::to_uppercase).as_deref());
            heading.set_xalign(0.0);
            heading.add_css_class("param-module");
            let hrow = gtk::ListBoxRow::new();
            hrow.set_activatable(false);
            hrow.set_selectable(false);
            hrow.set_child(Some(&heading));
            list.append(&hrow);
            module = group.clone();
        }
        let grid = gtk::Grid::new();
        grid.set_column_spacing(12);
        grid.add_css_class("param-row");
        let label = gtk::Label::new(Some(&short));
        label.set_xalign(0.0);
        label.set_width_chars(16);
        label.set_max_width_chars(22);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_tooltip_text(Some(&format!(
            "{} · double-click to reset{}",
            info.name,
            if p.explicit {
                " · stored in the project"
            } else {
                ""
            }
        )));
        label.add_css_class("param-name");
        let value = gtk::Label::new(Some(&value_text(app, plugin, &info, p.value)));
        value.set_xalign(1.0);
        value.set_width_chars(10);
        value.add_css_class("param-value");
        let toggle = info.stepped && (info.max - info.min - 1.0).abs() < f64::EPSILON;
        let control = if toggle {
            let sw = gtk::Switch::new();
            sw.set_active(p.value >= 0.5);
            sw.set_halign(gtk::Align::Start);
            sw.set_valign(gtk::Align::Center);
            sw.set_hexpand(true);
            grid.attach(&sw, 1, 0, 1, 1);
            Control::Switch(sw)
        } else {
            let step = if info.stepped {
                1.0
            } else {
                ((info.max - info.min) / 1000.0).max(f64::EPSILON)
            };
            let scale =
                gtk::Scale::with_range(gtk::Orientation::Horizontal, info.min, info.max, step);
            scale.set_draw_value(false);
            scale.set_hexpand(true);
            scale.set_value(p.value);
            grid.attach(&scale, 1, 0, 1, 1);
            Control::Scale(scale)
        };
        grid.attach(&label, 0, 0, 1, 1);
        grid.attach(&value, 2, 0, 1, 1);
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_child(Some(&grid));
        list.append(&row);
        rows.push(Row {
            info,
            control,
            value,
            row,
            shown: Cell::new(p.value),
        });
    }

    let editor = Rc::new(Generic {
        app: Rc::downgrade(app),
        track,
        plugin,
        rows,
        gesture: Gesture {
            open: Cell::new(false),
            generation: Cell::new(0),
        },
    });
    for (i, row) in editor.rows.iter().enumerate() {
        match &row.control {
            Control::Scale(scale) => {
                let me = Rc::downgrade(&editor);
                scale.connect_change_value(move |_, _, v| {
                    if let Some(me) = me.upgrade() {
                        me.set(i, v);
                    }
                    glib::Propagation::Proceed
                });
            }
            Control::Switch(sw) => {
                let me = Rc::downgrade(&editor);
                sw.connect_state_set(move |_, on| {
                    if let Some(me) = me.upgrade() {
                        me.set(i, if on { 1.0 } else { 0.0 });
                    }
                    glib::Propagation::Proceed
                });
            }
        }
        // Double-click the name: back to the default.
        let click = gtk::GestureClick::new();
        let me = Rc::downgrade(&editor);
        click.connect_pressed(move |_, n, _, _| {
            if n == 2
                && let Some(me) = me.upgrade()
            {
                let row = &me.rows[i];
                let d = row.info.default;
                match &row.control {
                    Control::Scale(s) => s.set_value(d),
                    Control::Switch(s) => s.set_active(d >= 0.5),
                }
                me.set(i, d);
            }
        });
        row.row.add_controller(click);
        // Right-click: MIDI learn for this parameter.
        let menu = gtk::GestureClick::new();
        menu.set_button(3);
        let me = Rc::downgrade(&editor);
        menu.connect_pressed(move |g, _, x, y| {
            let Some(me) = me.upgrade() else { return };
            let Some(app) = me.app.upgrade() else { return };
            let Some(widget) = g.widget() else { return };
            let target = faderframe_project::MappingTarget::Parameter {
                track: me.track,
                target: faderframe_automation::AutomationTarget::PluginParameter {
                    plugin: me.plugin,
                    parameter: me.rows[i].info.id,
                },
            };
            let entries = app.session.borrow().midi_learn_menu(target);
            let popover = gtk::Popover::new();
            popover.add_css_class("ff-menu");
            let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for (label, action) in entries {
                let b = gtk::Button::with_label(&label);
                b.add_css_class("flat");
                if let Some(l) = b.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                    l.set_xalign(0.0);
                }
                let weak = Rc::downgrade(&app);
                let pop = popover.downgrade();
                b.connect_clicked(move |_| {
                    if let Some(p) = pop.upgrade() {
                        p.popdown();
                    }
                    if let Some(a) = weak.upgrade() {
                        a.dispatch(action.clone());
                    }
                });
                list.append(&b);
            }
            popover.set_child(Some(&list));
            popover.set_parent(&widget);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.connect_closed(|p| p.unparent());
            popover.popup();
        });
        row.row.add_controller(menu);
    }
    {
        let me = Rc::downgrade(&editor);
        search.connect_search_changed(move |e| {
            let Some(me) = me.upgrade() else { return };
            let q = e.text().to_lowercase();
            for row in &me.rows {
                row.row
                    .set_visible(q.is_empty() || row.info.name.to_lowercase().contains(&q));
            }
        });
    }
    window.set_child(Some(&body));
    {
        let me = Rc::downgrade(&editor);
        glib::timeout_add_local(Duration::from_millis(100), move || match me.upgrade() {
            Some(me) => {
                me.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }
    // The window owns the editor state.
    let holder = RefCell::new(Some(editor));
    window.connect_close_request(move |_| {
        holder.borrow_mut().take();
        EDITORS.with(|e| e.generic.borrow_mut().remove(&plugin));
        glib::Propagation::Proceed
    });
    let key = gtk::EventControllerKey::new();
    {
        let w = window.downgrade();
        key.connect_key_pressed(move |_, k, _, _| {
            if k == gtk::gdk::Key::Escape
                && let Some(w) = w.upgrade()
            {
                w.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(key);
    EDITORS.with(|e| e.generic.borrow_mut().insert(plugin, window.clone()));
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ParameterId;

    fn info(unit: ParameterUnit, stepped: bool) -> ParameterInfo {
        ParameterInfo {
            id: ParameterId(0),
            name: "P".into(),
            min: 0.0,
            max: 10_000.0,
            default: 0.0,
            unit,
            automatable: true,
            stepped,
        }
    }

    #[test]
    fn editors_open_centred_or_where_they_were() {
        let monitors = [(0, 0, 2560, 1440), (2560, 0, 1920, 1080)];
        // Centred on the monitor showing FaderFrame.
        assert_eq!(
            place(None, (800, 600), &monitors, Some(monitors[1])),
            (3120, 240)
        );
        // Larger than the monitor: pinned to its corner.
        assert_eq!(
            place(None, (3000, 2000), &monitors, Some(monitors[0])),
            (0, 0)
        );
        // The stored position wins while it is on a monitor …
        assert_eq!(
            place(Some((100, 80)), (800, 600), &monitors, Some(monitors[1])),
            (100, 80)
        );
        // … but not once that monitor is gone.
        assert_eq!(
            place(Some((5000, 80)), (800, 600), &monitors, Some(monitors[0])),
            (880, 420)
        );
    }

    #[test]
    fn fallback_value_text() {
        assert_eq!(
            unit_text(&info(ParameterUnit::Hertz, false), 1500.0),
            "1.50 kHz"
        );
        assert_eq!(
            unit_text(&info(ParameterUnit::Hertz, false), 440.0),
            "440 Hz"
        );
        assert_eq!(
            unit_text(&info(ParameterUnit::Milliseconds, false), 2500.0),
            "2.50 s"
        );
        assert_eq!(
            unit_text(&info(ParameterUnit::Decibels, false), -120.0),
            "-∞ dB"
        );
        assert_eq!(unit_text(&info(ParameterUnit::None, true), 3.0), "3");
        let mut mix = info(ParameterUnit::Percent, false);
        mix.max = 1.0;
        assert_eq!(unit_text(&mix, 0.38), "38 %");
        let mut toggle = info(ParameterUnit::None, true);
        toggle.max = 1.0;
        assert_eq!(unit_text(&toggle, 1.0), "On");
    }
}
