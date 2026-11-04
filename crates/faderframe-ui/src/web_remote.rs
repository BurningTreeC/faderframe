//! The phone and web remote (Preferences → Remote; Transport → Web Remote
//! On = `app.toggle-web-remote`, a row in [`crate::toggles`]; Transport →
//! Phone and Web Remote… = `app.web-remote-settings`): on/off, the port, who may
//! connect, pairing (a code and its QR code for a role), the paired
//! devices (renamed, their roles changed, forgotten). The settings with
//! the paired devices are remembered (`Preferences::web_remote`, saved by
//! [`follow`] whenever the session's change — a device pairing on its
//! own included). See `faderframe_session::web_remote`.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_remote::Role;
use faderframe_remote::qr::QrCode;
use faderframe_session::web_remote::{RemoteListen, RemoteOp, WebRemoteSettings};
use faderframe_session::{Action, Session};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    /// The settings last written (or read).
    static SAVED: RefCell<Option<WebRemoteSettings>> = const { RefCell::new(None) };
}

/// Give the session the saved settings (start-up).
pub fn apply(session: &mut Session, prefs: &Preferences) {
    SAVED.with(|s| *s.borrow_mut() = Some(prefs.web_remote.clone()));
    session.set_web_remote(prefs.web_remote.clone());
}

/// Save the session's settings if they changed since the last call.
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let now = s.web_remote_settings().clone();
    drop(s);
    if SAVED.with(|saved| saved.borrow().as_ref() == Some(&now)) {
        return;
    }
    let mut p = Preferences::load();
    p.web_remote = now.clone();
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
    SAVED.with(|saved| *saved.borrow_mut() = Some(now));
}

/// The settings with `change` made, given to the session (saved by
/// [`follow`]).
pub(crate) fn change(app: &Rc<AppState>, change: impl FnOnce(&mut WebRemoteSettings)) {
    let mut s = app.session.borrow().web_remote_settings().clone();
    change(&mut s);
    app.dispatch(Action::WebRemote(RemoteOp::Settings(s)));
}

/// Development aid: `web-remote:<on|off>[@<port>]`, `web-remote:local`
/// (this computer only), `web-remote:network`, `web-remote:code[=full|
/// viewer|performer=<cue mix>]` (a new code, logged with the address),
/// `web-remote:status`, `web-remote:forget=<device name>`.
pub(crate) fn dev(app: &Rc<AppState>, arg: &str) {
    let (what, port) = arg.split_once('@').unwrap_or((arg, ""));
    let (what, value) = what.split_once('=').unwrap_or((what, ""));
    match what.trim() {
        "on" | "off" => {
            let on = what.trim() == "on";
            let port = port.trim().parse::<u16>().ok();
            change(app, |s| {
                s.on = on;
                if let Some(p) = port {
                    s.port = p;
                }
            });
            log_status(&app.session.borrow());
        }
        "local" => change(app, |s| s.listen = RemoteListen::ThisComputer),
        "network" => change(app, |s| s.listen = RemoteListen::Network),
        "code" => {
            let role = match value.split_once('=') {
                Some(("performer", cue)) => Role::Performer { cue: cue.into() },
                _ if value == "viewer" => Role::Viewer,
                _ => Role::Full,
            };
            app.dispatch(Action::WebRemote(RemoteOp::NewCode(role)));
            log_status(&app.session.borrow());
        }
        "status" => log_status(&app.session.borrow()),
        "forget" => {
            let device = app
                .session
                .borrow()
                .web_remote_settings()
                .devices
                .iter()
                .find(|d| d.name == value)
                .map(|d| d.id);
            match device {
                Some(d) => app.dispatch(Action::WebRemote(RemoteOp::Forget(d))),
                None => tracing::warn!("web-remote: no device '{value}'"),
            }
        }
        _ => tracing::warn!("web-remote: '{arg}' is not on|off[@port]|local|network|code|status"),
    }
}

fn log_status(s: &Session) {
    let st = s.web_remote_status();
    if let Some(e) = &st.error {
        tracing::info!("web-remote: not running: {e}");
        return;
    }
    if !st.running {
        tracing::info!("web-remote: off");
        return;
    }
    tracing::info!(
        "web-remote: {} code {} ({}), {} device(s) paired, {} connected",
        st.url.as_deref().unwrap_or("(no network address)"),
        st.code.as_deref().unwrap_or("withdrawn"),
        st.code_role.label(),
        s.web_remote_settings().devices.len(),
        st.connected.len()
    );
}

pub fn install_actions(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let open = gio::ActionEntry::builder("web-remote-settings")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                crate::preferences::open(&a, Some("remote"));
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let dev_action = gio::ActionEntry::builder("web-remote")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            if let (Some(a), Some(arg)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                dev(&a, &arg);
            }
        })
        .build();
    app.app.add_action_entries([open, dev_action]);
}

/// The roles a device can have in this project: full control, the
/// viewer, a performer of each cue mix (and `keep`, a performer's role
/// whose cue mix is not here).
fn roles(s: &Session, keep: Option<&Role>) -> Vec<Role> {
    let mut v = vec![Role::Full, Role::Viewer];
    v.extend(s.project().cue_mixes.iter().map(|c| Role::Performer {
        cue: c.name.clone(),
    }));
    if let Some(r) = keep
        && !v.contains(r)
    {
        v.push(r.clone());
    }
    v
}

fn role_menu(roles: &[Role], current: &Role) -> gtk::DropDown {
    let labels: Vec<String> = roles.iter().map(Role::label).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let d = gtk::DropDown::from_strings(&refs);
    d.set_selected(roles.iter().position(|r| r == current).unwrap_or(0) as u32);
    d
}

/// The QR code drawn dark on light (as a camera reads it, whatever the
/// theme), with its quiet zone.
fn qr_area(code: &Rc<RefCell<Option<QrCode>>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(232);
    area.set_content_height(232);
    area.set_halign(gtk::Align::Start);
    let code = Rc::clone(code);
    area.set_draw_func(move |_, cr, w, h| {
        let Some(qr) = code.borrow().clone() else {
            return;
        };
        let n = qr.size() + 8;
        let side = w.min(h);
        let unit = (side / n as i32).max(1);
        let size = unit * n as i32;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(0.0, 0.0, f64::from(size), f64::from(size));
        let _ = cr.fill();
        cr.set_source_rgb(0.0, 0.0, 0.0);
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                if qr.dark(x, y) {
                    cr.rectangle(
                        f64::from(unit * (x as i32 + 4)),
                        f64::from(unit * (y as i32 + 4)),
                        f64::from(unit),
                        f64::from(unit),
                    );
                }
            }
        }
        let _ = cr.fill();
    });
    area
}

/// "123 456".
fn spaced(code: &str) -> String {
    if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// Preferences → Remote.
pub fn page(app: &Rc<AppState>, row: impl Fn(&gtk::Grid, i32, &str, &gtk::Widget)) -> gtk::Widget {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    let settings = app.session.borrow().web_remote_settings().clone();
    // Set while the page brings widgets in line (their signals are not
    // the user's).
    let quiet = Rc::new(Cell::new(false));

    let intro = gtk::Label::new(Some(
        "Run the transport, the mixer, cue mixes, the setlist, the lyrics and the clip \
         launcher from a phone or tablet: open the address below in its browser (on the \
         same network) and pair it with the code, or scan the QR code with its camera.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.set_max_width_chars(70);
    g.attach(&intro, 0, 0, 2, 1);

    let on = gtk::Switch::new();
    on.set_active(settings.on);
    on.set_halign(gtk::Align::Start);
    row(&g, 1, "Serve the remote", on.upcast_ref());
    let port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
    port.set_value(f64::from(settings.port.max(1024)));
    port.set_halign(gtk::Align::Start);
    row(&g, 2, "Port", port.upcast_ref());
    let listen =
        gtk::DropDown::from_strings(&["Phones and tablets on this network", "This computer only"]);
    listen.set_selected(u32::from(settings.listen == RemoteListen::ThisComputer));
    row(&g, 3, "Answer", listen.upcast_ref());
    let any = gtk::CheckButton::with_label("Also devices outside the local network (a VPN)");
    any.set_active(settings.any_network);
    any.set_tooltip_text(Some(
        "The remote speaks plain HTTP with no encryption: keep it to networks you trust. \
         Outside the private address ranges only with a VPN you run yourself.",
    ));
    row(&g, 4, "", any.upcast_ref());
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_selectable(true);
    row(&g, 5, "Status", status.upcast_ref());

    let pair_title = gtk::Label::new(Some("Pair a Device"));
    pair_title.set_xalign(0.0);
    pair_title.add_css_class("heading");
    pair_title.set_margin_top(10);
    g.attach(&pair_title, 0, 6, 2, 1);
    let code_roles = Rc::new(RefCell::new(roles(&app.session.borrow(), None)));
    let pair_as = role_menu(&code_roles.borrow(), &Role::Full);
    let new_code = gtk::Button::with_label("New Code");
    let as_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    as_box.append(&pair_as);
    as_box.append(&new_code);
    row(&g, 7, "As", as_box.upcast_ref());
    let code = gtk::Label::new(None);
    code.set_xalign(0.0);
    code.set_selectable(true);
    code.add_css_class("title-1");
    row(&g, 8, "Code", code.upcast_ref());
    let qr = Rc::new(RefCell::new(None::<QrCode>));
    let area = qr_area(&qr);
    row(&g, 9, "", area.upcast_ref());
    let warning = gtk::Label::new(None);
    warning.set_xalign(0.0);
    warning.set_wrap(true);
    warning.add_css_class("warning");
    row(&g, 10, "", warning.upcast_ref());

    let devices_title = gtk::Label::new(Some("Paired Devices"));
    devices_title.set_xalign(0.0);
    devices_title.add_css_class("heading");
    devices_title.set_margin_top(10);
    g.attach(&devices_title, 0, 11, 2, 1);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    g.attach(&list, 0, 12, 2, 1);
    let note = gtk::Label::new(Some(
        "A performer's device shows only their cue mix (and the lyrics and setlist); a \
         viewer's reads the lyrics and setlist and changes nothing. Each code pairs one \
         device; after ten wrong codes it is withdrawn until you ask for a new one. \
         Forgetting a device disconnects it at once.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    note.set_max_width_chars(70);
    g.attach(&note, 0, 13, 2, 1);

    // The user's changes.
    {
        let app = Rc::downgrade(app);
        let quiet = Rc::clone(&quiet);
        on.connect_active_notify(move |w| {
            let v = w.is_active();
            if let Some(a) = app.upgrade().filter(|_| !quiet.get()) {
                change(&a, |s| s.on = v);
            }
        });
    }
    {
        let app = Rc::downgrade(app);
        let quiet = Rc::clone(&quiet);
        port.connect_value_changed(move |w| {
            let v = w.value() as u16;
            if let Some(a) = app.upgrade().filter(|_| !quiet.get()) {
                change(&a, |s| s.port = v);
            }
        });
    }
    {
        let app = Rc::downgrade(app);
        let quiet = Rc::clone(&quiet);
        listen.connect_selected_notify(move |w| {
            let v = if w.selected() == 1 {
                RemoteListen::ThisComputer
            } else {
                RemoteListen::Network
            };
            if let Some(a) = app.upgrade().filter(|_| !quiet.get()) {
                change(&a, |s| s.listen = v);
            }
        });
    }
    {
        let app = Rc::downgrade(app);
        let quiet = Rc::clone(&quiet);
        any.connect_toggled(move |w| {
            let v = w.is_active();
            if let Some(a) = app.upgrade().filter(|_| !quiet.get()) {
                change(&a, |s| s.any_network = v);
            }
        });
    }
    let ask_code = {
        let app = Rc::downgrade(app);
        let roles = Rc::clone(&code_roles);
        let pair_as = pair_as.clone();
        move || {
            let role = roles
                .borrow()
                .get(pair_as.selected() as usize)
                .cloned()
                .unwrap_or_default();
            if let Some(a) = app.upgrade() {
                a.dispatch(Action::WebRemote(RemoteOp::NewCode(role)));
            }
        }
    };
    let ask_code = Rc::new(ask_code);
    {
        let ask = Rc::clone(&ask_code);
        new_code.connect_clicked(move |_| ask());
    }
    {
        let ask = Rc::clone(&ask_code);
        let quiet = Rc::clone(&quiet);
        pair_as.connect_selected_notify(move |_| {
            if !quiet.get() {
                ask();
            }
        });
    }

    // Live: the status, the code and its picture, the devices.
    let weak = Rc::downgrade(app);
    let alive = g.downgrade();
    let shown_url = RefCell::new(None::<String>);
    let shown_devices = RefCell::new(String::new());
    let refresh = move || {
        let (Some(a), Some(_)) = (weak.upgrade(), alive.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let (st, settings, now_roles) = {
            let s = a.session.borrow();
            let st = s.web_remote_status();
            let keep = st.running.then_some(st.code_role.clone());
            (
                st,
                s.web_remote_settings().clone(),
                roles(&s, keep.as_ref()),
            )
        };
        quiet.set(true);
        if on.is_active() != settings.on {
            on.set_active(settings.on);
        }
        let text = match (&st.error, st.running, &st.url) {
            (Some(e), _, _) => format!("Not running: {e}."),
            (None, false, _) => "Off.".to_string(),
            (None, true, Some(url)) => format!(
                "Open {url} on the device (or scan the code). {} connected.",
                st.connected.len()
            ),
            (None, true, None) => format!(
                "Serving on port {}, but this computer has no network address now.",
                st.port.unwrap_or(0)
            ),
        };
        if status.text() != text {
            status.set_text(&text);
        }
        // The roles the code may give (cue mixes come and go).
        if *code_roles.borrow() != now_roles {
            let labels: Vec<String> = now_roles.iter().map(Role::label).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            pair_as.set_model(Some(&gtk::StringList::new(&refs)));
            *code_roles.borrow_mut() = now_roles.clone();
        }
        let at = now_roles
            .iter()
            .position(|r| *r == st.code_role)
            .unwrap_or(0) as u32;
        if pair_as.selected() != at {
            pair_as.set_selected(at);
        }
        let code_text = match (&st.code, st.running) {
            (Some(c), true) => spaced(c),
            (None, true) => "Withdrawn".to_string(),
            _ => "—".to_string(),
        };
        if code.text() != code_text {
            code.set_text(&code_text);
        }
        if *shown_url.borrow() != st.pair_url {
            *qr.borrow_mut() = st
                .pair_url
                .as_ref()
                .and_then(|u| QrCode::encode(u.as_bytes()));
            area.set_visible(qr.borrow().is_some());
            area.queue_draw();
            *shown_url.borrow_mut() = st.pair_url.clone();
        }
        let warn = match (&st.code, st.wrong_codes) {
            (None, _) if st.running => {
                "Ten wrong codes were tried: the code is withdrawn (someone \
                 may be guessing). New Code offers another."
                    .to_string()
            }
            (_, 0) => String::new(),
            (_, n) => format!("{n} wrong code{} tried.", if n == 1 { "" } else { "s" }),
        };
        if warning.text() != warn {
            warning.set_text(&warn);
        }
        warning.set_visible(!warn.is_empty());
        // The devices (rebuilt when they or who is connected change).
        let connected: Vec<u64> = st.connected.iter().map(|c| c.device).collect();
        let sig = format!("{:?}{:?}{:?}", settings.devices, connected, now_roles);
        if *shown_devices.borrow() != sig {
            *shown_devices.borrow_mut() = sig;
            device_rows(&a, &list, &settings, &connected, &now_roles);
        }
        quiet.set(false);
        glib::ControlFlow::Continue
    };
    refresh();
    glib::timeout_add_local(Duration::from_millis(400), refresh);
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&g)
        .build()
        .upcast()
}

/// A row per paired device: its name (editable), role, whether it is
/// connected, Forget.
fn device_rows(
    app: &Rc<AppState>,
    list: &gtk::ListBox,
    settings: &WebRemoteSettings,
    connected: &[u64],
    roles_here: &[Role],
) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    if settings.devices.is_empty() {
        let empty = gtk::Label::new(Some("No devices paired yet"));
        empty.add_css_class("dim-label");
        empty.set_margin_top(8);
        empty.set_margin_bottom(8);
        list.append(&empty);
        return;
    }
    for d in &settings.devices {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let name = gtk::Entry::new();
        name.set_text(&d.name);
        name.set_hexpand(true);
        name.set_max_length(faderframe_remote::pairing::MAX_NAME as i32);
        let mut roles = roles_here.to_vec();
        if !roles.contains(&d.role) {
            roles.push(d.role.clone());
        }
        let role = role_menu(&roles, &d.role);
        let here = gtk::Label::new(Some(if connected.contains(&d.id) {
            "Connected"
        } else {
            ""
        }));
        here.add_css_class("dim-label");
        here.set_width_chars(10);
        let forget = gtk::Button::with_label("Forget");
        row.append(&name);
        row.append(&role);
        row.append(&here);
        row.append(&forget);
        list.append(&row);
        let id = d.id;
        {
            let app = Rc::downgrade(app);
            name.connect_activate(move |e| {
                if let Some(a) = app.upgrade() {
                    a.dispatch(Action::WebRemote(RemoteOp::Rename {
                        device: id,
                        name: e.text().to_string(),
                    }));
                }
            });
        }
        {
            let app = Rc::downgrade(app);
            role.connect_selected_notify(move |w| {
                let Some(r) = roles.get(w.selected() as usize).cloned() else {
                    return;
                };
                if let Some(a) = app.upgrade() {
                    // Not from inside the handler of a widget the refresh
                    // may remove.
                    glib::idle_add_local_once(move || {
                        a.dispatch(Action::WebRemote(RemoteOp::SetRole {
                            device: id,
                            role: r,
                        }));
                    });
                }
            });
        }
        {
            let app = Rc::downgrade(app);
            forget.connect_clicked(move |_| {
                if let Some(a) = app.upgrade() {
                    glib::idle_add_local_once(move || {
                        a.dispatch(Action::WebRemote(RemoteOp::Forget(id)));
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_settings_keep_in_the_preferences_with_their_devices() {
        let mut p = Preferences::default();
        assert!(!p.web_remote.on);
        p.web_remote.on = true;
        p.web_remote.devices.push(faderframe_remote::PairedDevice {
            id: 9,
            name: "Phone".into(),
            token_hash: "ab".into(),
            role: Role::Performer {
                cue: "Drums".into(),
            },
            paired: 1,
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: Preferences = serde_json::from_str(&json).unwrap();
        assert_eq!(back.web_remote, p.web_remote);
        // Older preferences: off.
        let old: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(old.web_remote, WebRemoteSettings::default());
    }

    #[test]
    fn roles_offer_each_cue_mix_and_keep_one_not_here() {
        let mut s = Session::demo(faderframe_engine::EngineConfig::default()).unwrap();
        s.dispatch(Action::Cue(faderframe_session::cues::CueOp::New))
            .unwrap();
        let away = Role::Performer {
            cue: "Horns".into(),
        };
        let r = roles(&s, Some(&away));
        assert_eq!(r[0], Role::Full);
        assert_eq!(r[1], Role::Viewer);
        assert!(r.contains(&Role::Performer {
            cue: "Cue 1".into()
        }));
        assert_eq!(r.last(), Some(&away));
        assert_eq!(spaced("123456"), "123 456");
    }
}
