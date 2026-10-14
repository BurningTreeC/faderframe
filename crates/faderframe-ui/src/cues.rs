//! Audio → Cue Mixes: new cue mixes, the Cue Mixes window (names,
//! outputs, pre/post-fader, levels, mutes, clicks), what the mixer's
//! faders set (Mixer Shows: the main mix or a cue mix), and the dev
//! actions `cue-mix:…`, `cue-mode:…` and `cue-send:…` for scripted runs.

use crate::state::AppState;
use faderframe_core::{ChannelLayout, CueId};
use faderframe_project::{Command, SendTap};
use faderframe_session::Action;
use faderframe_session::cues::CueOp;
use gtk::gio;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The Mixer Shows submenu and the cue mixes it was made for.
type Shows = Option<(gio::Menu, Vec<(CueId, String)>)>;

thread_local! {
    /// The Mixer Shows submenu (made again when the cue mixes change) and
    /// what it was made for.
    static SHOWS: RefCell<Shows> = const { RefCell::new(None) };
    /// The open Cue Mixes window.
    static WINDOW: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// The cue mix numbered `n` (1-based, as the menus and the bar count).
fn nth(app: &AppState, n: &str) -> Option<CueId> {
    let i = n.trim().parse::<usize>().ok()?.checked_sub(1)?;
    app.session.borrow().cue_mixes().get(i).map(|c| c.id)
}

/// The `cue-mode` action's state: "main" or the shown cue mix's number.
fn mode_state(app: &AppState) -> String {
    let s = app.session.borrow();
    s.mixer_cue()
        .and_then(|c| s.cue_mixes().iter().position(|x| x.id == c.id))
        .map_or_else(|| "main".to_string(), |i| (i + 1).to_string())
}

fn on_off(v: &str) -> Option<bool> {
    match v.trim() {
        "on" | "1" | "true" => Some(true),
        "off" | "0" | "false" => Some(false),
        _ => None,
    }
}

fn db(v: &str) -> Option<f32> {
    v.trim().trim_end_matches("dB").trim().parse::<f32>().ok()
}

/// `cue-mix:<new|remove=<n>|rename=<n>=<name>|output=<n>=<out>[/mono]|pre=<n>|post=<n>|click=<n>=<on|off>[@dB]|level=<n>=<dB>|mute=<n>=<on|off>|copy-main[=<n>]|main-click=<on|off>>`.
fn cue_mix(app: &Rc<AppState>, arg: &str) {
    let (what, rest) = arg.split_once('=').unwrap_or((arg, ""));
    let (n, value) = rest.split_once('=').unwrap_or((rest, ""));
    let cue = nth(app, n);
    let warn = || tracing::warn!("cue-mix: cannot do '{arg}'");
    let action = match (what, cue) {
        ("new", _) => Action::Cue(CueOp::New),
        ("remove", Some(cue)) => Action::Cue(CueOp::Remove(cue)),
        ("rename", Some(cue)) => Action::Cue(CueOp::Rename {
            cue,
            name: value.to_string(),
        }),
        ("output", Some(cue)) => {
            let (out, mono) = match value.split_once('/') {
                Some((o, m)) => (o, m.trim() == "mono"),
                None => (value, false),
            };
            let Some(first) = out
                .trim()
                .parse::<u16>()
                .ok()
                .and_then(|o| o.checked_sub(1))
            else {
                return warn();
            };
            Action::Cue(CueOp::SetOutput {
                cue,
                first_channel: first,
                layout: if mono {
                    ChannelLayout::Mono
                } else {
                    ChannelLayout::Stereo
                },
            })
        }
        ("pre" | "post", Some(cue)) => Action::Cue(CueOp::SetTap {
            cue,
            tap: if what == "post" {
                SendTap::PostFader
            } else {
                SendTap::PreFader
            },
        }),
        ("click", Some(cue)) => {
            let (switch, level) = match value.split_once('@') {
                Some((s, l)) => (s, db(l)),
                None => (value, None),
            };
            let Some(c) = app.session.borrow().project().cue(cue).cloned() else {
                return warn();
            };
            Action::Edit(Command::SetCueClick {
                cue,
                on: on_off(switch).unwrap_or(c.click),
                db: level.unwrap_or(c.click_db),
            })
        }
        ("level", Some(cue)) => match db(value) {
            Some(db) => Action::Edit(Command::SetCueLevel { cue, db }),
            None => return warn(),
        },
        ("mute", Some(cue)) => match on_off(value) {
            Some(on) => Action::Edit(Command::SetCueMute { cue, on }),
            None => return warn(),
        },
        ("copy-main", _) => {
            let shown = app.session.borrow().mixer_cue().map(|c| c.id);
            match cue.or(shown) {
                Some(cue) => Action::Cue(CueOp::CopyMainMix(cue)),
                None => return warn(),
            }
        }
        ("main-click", _) => {
            let Some(on) = on_off(rest) else {
                return warn();
            };
            set_main_click(app, on);
            return;
        }
        _ => return warn(),
    };
    app.dispatch(action);
}

/// The metronome on the main outputs or not (Preferences → Metronome).
fn set_main_click(app: &Rc<AppState>, on: bool) {
    let mut m = app.session.borrow().metronome_settings().clone();
    m.main_outputs = on;
    crate::metronome_prefs::apply(app, m);
}

/// `cue-send:<track>@<n>=<dB|on|off>[/<pan>]`.
fn cue_send(app: &Rc<AppState>, arg: &str) {
    let parsed = (|| {
        let (name, rest) = arg.split_once('@')?;
        let (n, value) = rest.split_once('=')?;
        let cue = nth(app, n)?;
        let track = app
            .session
            .borrow()
            .project()
            .tracks
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.id)?;
        Some((track, cue, value.to_string()))
    })();
    let Some((track, cue, value)) = parsed else {
        tracing::warn!("cue-send: '{arg}' is not <track>@<cue>=<dB|on|off>[/<pan>]");
        return;
    };
    let (level, pan) = match value.split_once('/') {
        Some((l, p)) => (l.to_string(), faderframe_core::pan::parse_pan(p)),
        None => (value, None),
    };
    let mut commands = Vec::new();
    match (on_off(&level), db(&level)) {
        (Some(on), _) => commands.push(Command::SetCueSendOn { track, cue, on }),
        (None, Some(db)) => commands.push(Command::SetCueSendLevel { track, cue, db }),
        _ => {}
    }
    if let Some(pan) = pan {
        commands.push(Command::SetCueSendPan { track, cue, pan });
    }
    for c in commands {
        app.dispatch(Action::Edit(c));
    }
}

pub fn install_actions(app: &Rc<AppState>) {
    let named = |name: &'static str, f: fn(&Rc<AppState>, &str)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                if let (Some(a), Some(arg)) =
                    (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                {
                    f(&a, &arg);
                }
            })
            .build()
    };
    // `cue-mode:<n|main>`: what the mixer's faders set.
    let weak = Rc::downgrade(app);
    let mode = gio::ActionEntry::builder("cue-mode")
        .parameter_type(Some(&String::static_variant_type()))
        .state(mode_state(app).to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            let cue = if v == "main" { None } else { nth(&a, &v) };
            if v != "main" && cue.is_none() {
                tracing::warn!("cue-mode: no cue mix '{v}'");
                return;
            }
            a.dispatch(Action::Cue(CueOp::Show(cue)));
        })
        .build();
    let weak = Rc::downgrade(app);
    let window = gio::ActionEntry::builder("cue-mixes")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                open(&a);
            }
        })
        .build();
    app.app.add_action_entries([
        named("cue-mix", cue_mix),
        named("cue-send", cue_send),
        mode,
        window,
    ]);
}

/// The Audio menu's Cue Mixes submenu.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let top = gio::Menu::new();
    let item = |label: &str, action: &str, target: Option<&str>| {
        let i = gio::MenuItem::new(Some(label), None);
        match target {
            Some(t) => i.set_action_and_target_value(Some(action), Some(&t.to_variant())),
            None => i.set_detailed_action(action),
        }
        i
    };
    top.append_item(&item("New Cue Mix", "app.cue-mix", Some("new")));
    top.append_item(&item("Cue Mixes…", "app.cue-mixes", None));
    top.append_item(&item(
        "Copy Main Mix into the Shown Cue Mix",
        "app.cue-mix",
        Some("copy-main"),
    ));
    m.append_section(None, &top);
    let shows = gio::Menu::new();
    fill_shows(&shows, &[]);
    m.append_submenu(Some("Mixer Shows"), &shows);
    SHOWS.with(|s| *s.borrow_mut() = Some((shows, Vec::new())));
    m
}

fn fill_shows(menu: &gio::Menu, cues: &[(CueId, String)]) {
    menu.remove_all();
    let i = gio::MenuItem::new(Some("Main Mix"), None);
    i.set_action_and_target_value(Some("app.cue-mode"), Some(&"main".to_variant()));
    menu.append_item(&i);
    for (n, (_, name)) in cues.iter().enumerate() {
        let i = gio::MenuItem::new(Some(name), None);
        i.set_action_and_target_value(
            Some("app.cue-mode"),
            Some(&(n + 1).to_string().to_variant()),
        );
        menu.append_item(&i);
    }
}

/// After a change: the menus show the cue mixes and what the mixer
/// shows; an open Cue Mixes window follows.
pub fn follow(app: &Rc<AppState>) {
    let cues: Vec<(CueId, String)> = app
        .session
        .borrow()
        .cue_mixes()
        .iter()
        .map(|c| (c.id, c.name.clone()))
        .collect();
    SHOWS.with(|s| {
        if let Some((menu, shown)) = s.borrow_mut().as_mut()
            && *shown != cues
        {
            fill_shows(menu, &cues);
            *shown = cues;
        }
    });
    if let Some(a) = app
        .app
        .lookup_action("cue-mode")
        .and_downcast::<gio::SimpleAction>()
    {
        let now = mode_state(app);
        if a.state().and_then(|v| v.get::<String>()).as_deref() != Some(now.as_str()) {
            a.set_state(&now.to_variant());
        }
    }
    let ui = WINDOW.with(|w| w.borrow().clone());
    if let Some(ui) = ui {
        ui.refresh(false);
    }
}

/// The Cue Mixes window.
struct Ui {
    app: std::rc::Weak<AppState>,
    win: gtk::Window,
    rows: gtk::Grid,
    main_click: gtk::Switch,
    /// What the rows were made for (rebuilt when it changes).
    made_for: RefCell<String>,
    /// Filling the widgets: their signals do nothing.
    filling: Cell<bool>,
}

impl Ui {
    fn dispatch(&self, action: Action) {
        if self.filling.get() {
            return;
        }
        if let Some(app) = self.app.upgrade() {
            app.dispatch(action);
        }
    }

    /// The rows from the session (made again only when the cue mixes'
    /// structure changed, or `force`).
    fn refresh(self: &Rc<Self>, force: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let (cues, shown, main_click) = {
            let s = app.session.borrow();
            (
                s.cue_mixes().to_vec(),
                s.mixer_cue().map(|c| c.id),
                s.metronome_settings().main_outputs,
            )
        };
        self.filling.set(true);
        self.main_click.set_active(main_click);
        let signature = format!(
            "{:?}",
            cues.iter()
                .map(|c| (
                    c.id,
                    &c.name,
                    c.first_channel,
                    c.layout,
                    c.tap,
                    c.click,
                    c.mute
                ))
                .collect::<Vec<_>>()
        ) + &format!("{shown:?}");
        if !force && *self.made_for.borrow() == signature {
            self.filling.set(false);
            return;
        }
        *self.made_for.borrow_mut() = signature;
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        let head = [
            "Cue mix", "Outputs", "Taps", "Level dB", "Mute", "Click", "Click dB", "", "", "",
        ];
        for (c, h) in head.iter().enumerate() {
            let l = gtk::Label::new(Some(h));
            l.add_css_class("dim-label");
            l.set_xalign(0.0);
            self.rows.attach(&l, c as i32, 0, 1, 1);
        }
        if cues.is_empty() {
            let l = gtk::Label::new(Some(
                "No cue mixes yet: a new one is a copy of the main mix on the next free outputs.",
            ));
            l.add_css_class("dim-label");
            self.rows.attach(&l, 0, 1, 10, 1);
        }
        for (i, c) in cues.iter().enumerate() {
            let row = i as i32 + 1;
            let id = c.id;
            let name = gtk::Entry::new();
            name.set_text(&c.name);
            name.set_width_chars(14);
            {
                let ui = Rc::clone(self);
                name.connect_activate(move |e| {
                    ui.dispatch(Action::Cue(CueOp::Rename {
                        cue: id,
                        name: e.text().to_string(),
                    }));
                });
            }
            self.rows.attach(&name, 0, row, 1, 1);
            let choices = app.session.borrow().cue_output_choices(id);
            let labels: Vec<String> = choices.iter().map(|o| o.label.clone()).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let outputs = gtk::DropDown::from_strings(&refs);
            if let Some(at) = choices.iter().position(|o| o.checked) {
                outputs.set_selected(at as u32);
            }
            {
                let ui = Rc::clone(self);
                let actions: Vec<Action> = choices.into_iter().map(|o| o.action).collect();
                outputs.connect_selected_notify(move |d| {
                    if let Some(a) = actions.get(d.selected() as usize) {
                        ui.dispatch(a.clone());
                    }
                });
            }
            self.rows.attach(&outputs, 1, row, 1, 1);
            let tap = gtk::DropDown::from_strings(&["Pre-fader", "Post-fader"]);
            tap.set_selected(u32::from(c.post_fader()));
            tap.set_tooltip_text(Some(
                "Pre-fader: independent of the main faders. Post-fader: follows them (the levels move so it sounds the same). Mutes and solos never reach a cue mix.",
            ));
            {
                let ui = Rc::clone(self);
                tap.connect_selected_notify(move |d| {
                    ui.dispatch(Action::Cue(CueOp::SetTap {
                        cue: id,
                        tap: if d.selected() == 1 {
                            SendTap::PostFader
                        } else {
                            SendTap::PreFader
                        },
                    }));
                });
            }
            self.rows.attach(&tap, 2, row, 1, 1);
            let level = gtk::SpinButton::with_range(-60.0, 12.0, 0.5);
            level.set_digits(1);
            level.set_value(f64::from(c.level_db.max(-60.0)));
            {
                let ui = Rc::clone(self);
                level.connect_value_changed(move |s| {
                    ui.dispatch(Action::Edit(Command::SetCueLevel {
                        cue: id,
                        db: s.value() as f32,
                    }));
                });
            }
            self.rows.attach(&level, 3, row, 1, 1);
            let mute = gtk::CheckButton::new();
            mute.set_active(c.mute);
            {
                let ui = Rc::clone(self);
                mute.connect_toggled(move |b| {
                    ui.dispatch(Action::Edit(Command::SetCueMute {
                        cue: id,
                        on: b.is_active(),
                    }));
                });
            }
            self.rows.attach(&mute, 4, row, 1, 1);
            let click = gtk::Switch::new();
            click.set_active(c.click);
            click.set_valign(gtk::Align::Center);
            click.set_tooltip_text(Some(
                "The metronome in this cue mix (when it clicks; a count-in too)",
            ));
            let click_db = gtk::SpinButton::with_range(-60.0, 12.0, 0.5);
            click_db.set_digits(1);
            click_db.set_value(f64::from(c.click_db.max(-60.0)));
            click_db.set_tooltip_text(Some(
                "The click's level here, against the metronome's volume",
            ));
            {
                let ui = Rc::clone(self);
                let db = click_db.clone();
                click.connect_active_notify(move |s| {
                    ui.dispatch(Action::Edit(Command::SetCueClick {
                        cue: id,
                        on: s.is_active(),
                        db: db.value() as f32,
                    }));
                });
            }
            {
                let ui = Rc::clone(self);
                let on = click.clone();
                click_db.connect_value_changed(move |s| {
                    ui.dispatch(Action::Edit(Command::SetCueClick {
                        cue: id,
                        on: on.is_active(),
                        db: s.value() as f32,
                    }));
                });
            }
            self.rows.attach(&click, 5, row, 1, 1);
            self.rows.attach(&click_db, 6, row, 1, 1);
            let show = gtk::CheckButton::with_label("In the mixer");
            show.set_active(shown == Some(id));
            show.set_tooltip_text(Some("The mixer's faders, pans and mutes set this cue mix"));
            {
                let ui = Rc::clone(self);
                show.connect_toggled(move |b| {
                    ui.dispatch(Action::Cue(CueOp::Show(b.is_active().then_some(id))));
                });
            }
            self.rows.attach(&show, 7, row, 1, 1);
            let copy = gtk::Button::with_label("Copy Main Mix");
            {
                let ui = Rc::clone(self);
                copy.connect_clicked(move |_| ui.dispatch(Action::Cue(CueOp::CopyMainMix(id))));
            }
            self.rows.attach(&copy, 8, row, 1, 1);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_tooltip_text(Some("Remove this cue mix (undoable)"));
            {
                let ui = Rc::clone(self);
                remove.connect_clicked(move |_| ui.dispatch(Action::Cue(CueOp::Remove(id))));
            }
            self.rows.attach(&remove, 9, row, 1, 1);
        }
        self.filling.set(false);
    }
}

/// Audio → Cue Mixes…
pub fn open(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    if let Some(ui) = WINDOW.with(|w| w.borrow().clone()) {
        ui.refresh(true);
        ui.win.present();
        return;
    }
    let win = gtk::Window::builder()
        .title("Cue Mixes")
        .application(&app.app)
        .transient_for(&main)
        .default_width(1240)
        .default_height(380)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let intro = gtk::Label::new(Some(
        "Headphone mixes for the performers, each on its own outputs. Every track sends to every cue mix with its own level, pan and switch — set them in the mixer (the bar over the strips, or Audio → Cue Mixes → Mixer Shows). Cue mixes ignore the main mix's mutes and solos, never reach renders, and come out with the master, early with low-latency monitoring.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    body.append(&intro);
    let rows = gtk::Grid::new();
    rows.set_row_spacing(6);
    rows.set_column_spacing(10);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&rows)
        .build();
    body.append(&scroll);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let new = gtk::Button::with_label("New Cue Mix");
    foot.append(&new);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    foot.append(&spacer);
    let main_label = gtk::Label::new(Some("Click on the main outputs"));
    foot.append(&main_label);
    let main_click = gtk::Switch::new();
    main_click.set_valign(gtk::Align::Center);
    main_click.set_tooltip_text(Some(
        "Off: the click plays only in the cue mixes whose click is on (not on the speakers)",
    ));
    foot.append(&main_click);
    body.append(&foot);
    win.set_child(Some(&body));
    let ui = Rc::new(Ui {
        app: Rc::downgrade(app),
        win: win.clone(),
        rows,
        main_click: main_click.clone(),
        made_for: RefCell::new(String::new()),
        filling: Cell::new(false),
    });
    {
        let ui = Rc::clone(&ui);
        new.connect_clicked(move |_| ui.dispatch(Action::Cue(CueOp::New)));
    }
    {
        let ui2 = Rc::clone(&ui);
        main_click.connect_active_notify(move |s| {
            if ui2.filling.get() {
                return;
            }
            if let Some(app) = ui2.app.upgrade() {
                set_main_click(&app, s.is_active());
            }
        });
    }
    ui.refresh(true);
    WINDOW.with(|w| *w.borrow_mut() = Some(Rc::clone(&ui)));
    win.connect_close_request(|_| {
        WINDOW.with(|w| *w.borrow_mut() = None);
        gtk::glib::Propagation::Proceed
    });
    win.present();
}
