//! The Collaborate window (File → Collaborate…): host the project or join
//! someone's, who is in the session and where they are, chat, Listen
//! Along, leave. Every control is an `Action::Collab`; the window reads
//! `Session::collaboration` ten times a second.
//!
//! Development aid: `collab:<host|host@<port>|host@<ip>:<port>|join=<address>/<code>|leave|chat=<text>|listen|listen-off|invite|resync>`
//! (the name is `$FADERFRAME_COLLAB_NAME`, else the user's), e.g.
//! `collab:host@127.0.0.1:47400` in one FaderFrame and
//! `collab:join=127.0.0.1:47400/<code>` in another (the host logs its
//! invitation).

use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::collab::{CollabOp, CollabRole, CollabStatus, Connection, parse_invite};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

pub(crate) fn install(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let open_window = gio::ActionEntry::builder("collaborate")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                open(&a);
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let dev = gio::ActionEntry::builder("collab")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            if let (Some(a), Some(arg)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                dev_collab(&a, &arg);
            }
        })
        .build();
    app.app.add_action_entries([open_window, dev]);
}

/// The name others see: `$FADERFRAME_COLLAB_NAME`, else the user's.
fn default_name() -> String {
    if let Ok(n) = std::env::var("FADERFRAME_COLLAB_NAME")
        && !n.trim().is_empty()
    {
        return n;
    }
    let real = glib::real_name().to_string_lossy().into_owned();
    if !real.trim().is_empty() && real != "Unknown" {
        return real;
    }
    glib::user_name().to_string_lossy().into_owned()
}

/// `collab:<…>` (see the module documentation).
pub(crate) fn dev_collab(app: &Rc<AppState>, arg: &str) {
    let Some(op) = faderframe_session::collab::parse_dev(arg, &default_name()) else {
        tracing::warn!(
            "collab: expected host[@port|@ip:port], join=<address>/<code>, leave, chat=…, listen, listen-off, invite or resync; got '{arg}'"
        );
        return;
    };
    app.dispatch(Action::Collab(op));
    if let Some(s) = app.session.borrow().collaboration()
        && let Some(i) = s.invite
    {
        tracing::info!("collab: invite {}", i.text());
    }
}

struct Ui {
    status: gtk::Label,
    start: gtk::Box,
    live: gtk::Box,
    invite: gtk::Label,
    copy: gtk::Button,
    new_code: gtk::Button,
    people: gtk::ListBox,
    chat: gtk::ListBox,
    chat_scroll: gtk::ScrolledWindow,
    listen: gtk::CheckButton,
    leave: gtk::Button,
    join: gtk::Button,
    /// The join button asked once to discard unsaved changes.
    join_armed: Cell<bool>,
    shown: RefCell<Option<CollabStatus>>,
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l
}

fn heading(text: &str) -> gtk::Label {
    let l = label(text);
    l.add_css_class("heading");
    l
}

/// Show the window (raised if it is open).
pub(crate) fn open(app: &Rc<AppState>) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        w.present();
        return;
    }
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Collaborate")
        .transient_for(&main)
        .default_width(520)
        .default_height(620)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for m in [0, 1, 2, 3] {
        match m {
            0 => body.set_margin_top(14),
            1 => body.set_margin_bottom(14),
            2 => body.set_margin_start(14),
            _ => body.set_margin_end(14),
        }
    }
    let status = label("");
    status.set_wrap(true);
    body.append(&status);

    // --- not in a session: host or join -------------------------------------
    let start = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let name = gtk::Entry::new();
    name.set_text(&default_name());
    name.set_tooltip_text(Some("The name the others see"));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&label("Your name"));
    name.set_hexpand(true);
    row.append(&name);
    start.append(&row);

    start.append(&heading("Host this project"));
    let host_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
    port.set_value(f64::from(faderframe_session::collab::DEFAULT_PORT));
    port.set_tooltip_text(Some(
        "The TCP port people connect to (any free one when it is taken)",
    ));
    host_row.append(&label("Port"));
    host_row.append(&port);
    let host = gtk::Button::with_label("Host");
    host.add_css_class("suggested-action");
    host.set_tooltip_text(Some(
        "Others join with this machine's address and the invite code shown then",
    ));
    host_row.append(&host);
    start.append(&host_row);
    let lan = label(
        "On a local network that is all. Over the internet, forward the port \
         (TCP) on the host's router to this machine, or have everyone join \
         one VPN (WireGuard, Tailscale, ZeroTier …). Everything sent is \
         encrypted and needs the invite code.",
    );
    lan.set_wrap(true);
    lan.add_css_class("dim-label");
    start.append(&lan);

    start.append(&heading("Join a session"));
    let invite_entry = gtk::Entry::new();
    invite_entry.set_placeholder_text(Some("address/CODE (the invitation)  or  address"));
    let code_entry = gtk::Entry::new();
    code_entry.set_placeholder_text(Some("Invite code (when not in the line above)"));
    start.append(&invite_entry);
    start.append(&code_entry);
    let join = gtk::Button::with_label("Join");
    join.set_halign(gtk::Align::End);
    join.set_tooltip_text(Some(
        "The host's project replaces the one open here (its media come in the background)",
    ));
    start.append(&join);
    body.append(&start);

    // --- in a session ---------------------------------------------------------
    let live = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let invite = label("");
    invite.set_selectable(true);
    invite.set_wrap(true);
    let invite_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    invite.set_hexpand(true);
    invite_row.append(&invite);
    let copy = gtk::Button::with_label("Copy Invitation");
    let new_code = gtk::Button::with_label("New Code");
    new_code.set_tooltip_text(Some(
        "Who joins from now on needs the new code (those in the session stay)",
    ));
    invite_row.append(&copy);
    invite_row.append(&new_code);
    live.append(&invite_row);
    live.append(&heading("People"));
    let people = gtk::ListBox::new();
    people.set_selection_mode(gtk::SelectionMode::None);
    live.append(&people);
    let listen = gtk::CheckButton::with_label("Listen Along (hear the host's mix here)");
    listen.set_tooltip_text(Some(
        "The host's mix streamed (Opus) and played in place of this session's own: \
         for missing media or plug-ins",
    ));
    live.append(&listen);
    live.append(&heading("Chat"));
    let chat = gtk::ListBox::new();
    chat.set_selection_mode(gtk::SelectionMode::None);
    let chat_scroll = gtk::ScrolledWindow::builder()
        .child(&chat)
        .min_content_height(160)
        .vexpand(true)
        .build();
    live.append(&chat_scroll);
    let say = gtk::Entry::new();
    say.set_placeholder_text(Some("Say something to everyone"));
    live.append(&say);
    let leave = gtk::Button::with_label("Leave");
    leave.set_halign(gtk::Align::End);
    live.append(&leave);
    body.append(&live);

    win.set_child(Some(&body));
    let ui = Rc::new(Ui {
        status,
        start,
        live,
        invite,
        copy,
        new_code,
        people,
        chat,
        chat_scroll,
        listen,
        leave,
        join,
        join_armed: Cell::new(false),
        shown: RefCell::new(None),
    });
    {
        let (app, name, port) = (Rc::clone(app), name.clone(), port.clone());
        host.connect_clicked(move |_| {
            let bind = std::net::SocketAddr::from(([0, 0, 0, 0], port.value() as u16));
            app.dispatch(Action::Collab(CollabOp::Host {
                bind: Some(bind),
                name: name.text().to_string(),
            }));
        });
    }
    {
        let (app, u) = (Rc::clone(app), Rc::downgrade(&ui));
        let (name, invite_entry, code_entry) = (name, invite_entry, code_entry);
        ui.join.connect_clicked(move |b| {
            let Some(u) = u.upgrade() else {
                return;
            };
            let line = invite_entry.text().to_string();
            let (address, code) = match parse_invite(&line) {
                Some(x) => x,
                None => (line.trim().to_string(), code_entry.text().trim().to_string()),
            };
            if address.is_empty() || code.is_empty() {
                u.status
                    .set_text("Paste the invitation (address/CODE), or the address and the code.");
                return;
            }
            if app.session.borrow().is_dirty() && !u.join_armed.get() {
                u.join_armed.set(true);
                b.set_label("Discard Changes and Join");
                u.status.set_text(
                    "This project has unsaved changes, and the host's replaces it: save it first, or join anyway.",
                );
                return;
            }
            u.join_armed.set(false);
            b.set_label("Join");
            app.dispatch(Action::Collab(CollabOp::Join {
                address,
                code,
                name: name.text().to_string(),
            }));
        });
    }
    {
        let u = Rc::downgrade(&ui);
        ui.copy.connect_clicked(move |b| {
            if let Some(u) = u.upgrade()
                && let Some(Some(i)) = u.shown.borrow().as_ref().map(|s| s.invite.clone())
            {
                b.clipboard().set_text(&i.text());
            }
        });
    }
    {
        let app = Rc::clone(app);
        ui.new_code
            .connect_clicked(move |_| app.dispatch(Action::Collab(CollabOp::NewInvite)));
    }
    {
        let app = Rc::clone(app);
        ui.leave
            .connect_clicked(move |_| app.dispatch(Action::Collab(CollabOp::Leave)));
    }
    {
        let app = Rc::clone(app);
        ui.listen.connect_toggled(move |c| {
            let on = c.is_active();
            let now = app
                .session
                .borrow()
                .collaboration()
                .is_some_and(|s| s.listening);
            if now != on {
                app.dispatch(Action::Collab(CollabOp::ListenAlong(on)));
            }
        });
    }
    {
        let app = Rc::clone(app);
        say.connect_activate(move |e| {
            let text = e.text().to_string();
            if !text.trim().is_empty() {
                app.dispatch(Action::Collab(CollabOp::Chat(text)));
                e.set_text("");
            }
        });
    }
    win.connect_close_request(|_| {
        OPEN.with(|o| o.borrow_mut().take());
        glib::Propagation::Proceed
    });
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    refresh(app, &ui);
    {
        let (app, ui, weak) = (Rc::downgrade(app), Rc::clone(&ui), win.downgrade());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(app), Some(_)) = (app.upgrade(), weak.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            refresh(&app, &ui);
            glib::ControlFlow::Continue
        });
    }
    win.present();
}

fn clear(list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
}

fn swatch(c: faderframe_project::TrackColor) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// The status line in words.
pub(crate) fn status_text(s: &CollabStatus) -> String {
    let mut parts = Vec::new();
    parts.push(match (&s.connection, s.role) {
        (Connection::Connecting, _) => "Connecting…".to_string(),
        (Connection::Connected, CollabRole::Host) => {
            format!(
                "Hosting a live session ({} joined)",
                s.people.len().saturating_sub(1)
            )
        }
        (Connection::Connected, CollabRole::Peer) => format!(
            "In {}’s live session",
            s.people
                .iter()
                .find(|p| p.host)
                .map_or("the host", |p| p.name.as_str())
        ),
        (Connection::Lost { seconds, reason }, _) => {
            format!("Lost the host {seconds} s ago ({reason}); trying again — keep working")
        }
        (Connection::Ended { reason }, _) => format!("The live session ended: {reason}"),
    });
    if s.waiting > 0 {
        parts.push(format!("{} of your edits on their way", s.waiting));
    }
    if s.media.waiting > 0 {
        let mb = |b: u64| b as f64 / 1_048_576.0;
        parts.push(format!(
            "{} media file{} coming ({:.1} of {:.1} MB)",
            s.media.waiting,
            if s.media.waiting == 1 { "" } else { "s" },
            mb(s.media.done),
            mb(s.media.total)
        ));
    }
    if s.media.hashing > 0 {
        parts.push(format!("hashing {} media file(s)", s.media.hashing));
    }
    if !s.media.failed.is_empty() {
        parts.push(format!("could not fetch {}", s.media.failed.join(", ")));
    }
    if !s.missing_plugins.is_empty() {
        parts.push(format!(
            "not installed here: {}",
            s.missing_plugins.join(", ")
        ));
    }
    parts.join(" · ")
}

fn refresh(app: &Rc<AppState>, ui: &Ui) {
    let status = app.session.borrow().collaboration();
    // Shown or hidden every time: the first refresh finds no session and
    // nothing shown, equal, and the window must still hide the live part.
    let live = status
        .as_ref()
        .is_some_and(|s| !matches!(s.connection, Connection::Ended { .. }));
    ui.start.set_visible(!live);
    ui.live.set_visible(live);
    if *ui.shown.borrow() == status {
        return;
    }
    match &status {
        None => ui
            .status
            .set_text("Work on one project together, live: host it here, or join someone's."),
        Some(s) => ui.status.set_text(&status_text(s)),
    }
    if let Some(s) = &status {
        match &s.invite {
            Some(i) => {
                ui.invite.set_text(&format!(
                    "Invitation: {}\nCode {} · this machine: {}",
                    i.text(),
                    i.code,
                    i.addresses.join(", ")
                ));
                ui.copy.set_visible(true);
                ui.new_code.set_visible(true);
            }
            None => {
                ui.invite.set_text(&format!(
                    "Host: {}",
                    s.host_address.as_deref().unwrap_or("")
                ));
                ui.copy.set_visible(false);
                ui.new_code.set_visible(false);
            }
        }
        ui.leave.set_label(match s.role {
            CollabRole::Host => "End the Session",
            CollabRole::Peer => "Leave",
        });
        ui.listen.set_visible(s.role == CollabRole::Peer);
        if ui.listen.is_active() != s.listening {
            ui.listen.set_active(s.listening);
        }
        clear(&ui.people);
        for p in &s.people {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let dot = gtk::Label::new(None);
            dot.set_markup(&format!(
                "<span foreground=\"{}\">●</span>",
                swatch(p.colour)
            ));
            row.append(&dot);
            let mut text = p.name.clone();
            if p.me {
                text.push_str(" (you)");
            }
            if p.host {
                text.push_str(" · host");
            }
            if !p.connected {
                text.push_str(" · away");
            }
            if p.listening {
                text.push_str(" · listening along");
            }
            if let Some(e) = &p.last_edit {
                text.push_str(&format!(" · last: {e}"));
            }
            if let Some(pr) = &p.presence {
                text.push_str(&format!(
                    " · at {:.1} q{}",
                    pr.playhead.quarters(),
                    if pr.playing { " (playing)" } else { "" }
                ));
            }
            row.append(&label(&text));
            ui.people.append(&row);
        }
        let shown_lines = ui.shown.borrow().as_ref().map_or(0, |o| o.chat.len());
        if s.chat.len() != shown_lines {
            clear(&ui.chat);
            for line in &s.chat {
                let l = gtk::Label::new(None);
                l.set_xalign(0.0);
                l.set_wrap(true);
                l.set_markup(&format!(
                    "<span foreground=\"{}\"><b>{}</b></span>  {}",
                    swatch(line.colour),
                    glib::markup_escape_text(&line.from),
                    glib::markup_escape_text(&line.text)
                ));
                ui.chat.append(&l);
            }
            let adj = ui.chat_scroll.vadjustment();
            glib::idle_add_local_once(move || adj.set_value(adj.upper()));
        }
    }
    *ui.shown.borrow_mut() = status;
}
