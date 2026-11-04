//! Scripts and macros in the shell (see `faderframe_session::script` and
//! docs/SCRIPTING.md): the Scripts menu — its commands, then the
//! library's scripts and macros as `app.script::<key>` items, so the
//! command palette lists them and the shortcut editor gives them keys —,
//! the console window (output and a line to type Lua into), the prompts
//! scripts ask, the Macros window (run, rename, delete, MIDI learn), and
//! the development aids:
//!
//! * `run-script:<path>` runs a Lua file;
//! * `macro:<record|stop|discard|play=<name or number>>` records, keeps or
//!   throws away a macro, or plays one;
//! * `script-line:<lua>` runs a line in the console (no commas: start-up
//!   actions split on them).

use crate::state::AppState;
use faderframe_project::MappingTarget;
use faderframe_session::Action;
use faderframe_session::script::{
    ConsoleKind, FieldKind, Prompt, ScriptEntry, ScriptOp, ScriptOrigin,
};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    /// The Scripts menu's library section (rebuilt as the library changes).
    static LIBRARY: gio::Menu = gio::Menu::new();
    static SHOWN_LIBRARY: Cell<u64> = const { Cell::new(u64::MAX) };
    static CONSOLE: RefCell<Option<Console>> = const { RefCell::new(None) };
    static MACROS: RefCell<Option<(gtk::Window, gtk::Box)>> = const { RefCell::new(None) };
}

/// The Scripts menu.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let section = |items: &[(&str, &str)]| {
        let s = gio::Menu::new();
        for (label, action) in items {
            s.append(Some(label), Some(action));
        }
        s
    };
    m.append_section(
        None,
        &section(&[
            ("Script Console…", "app.script-console"),
            ("Run Script File…", "app.run-script-file"),
            ("Open Scripts Folder", "app.scripts-folder"),
            ("Reload Scripts", "app.reload-scripts"),
        ]),
    );
    m.append_section(
        None,
        &section(&[
            ("Record Macro", "app.macro-record"),
            ("Macros…", "app.macros"),
            ("Stop All Scripts and Timers", "app.stop-scripts"),
        ]),
    );
    LIBRARY.with(|l| m.append_section(None, l));
    m
}

fn library_item(e: &ScriptEntry) -> gio::MenuItem {
    let label = match e.number {
        Some(n) => format!("{n} · {}", e.name),
        None => e.name.clone(),
    };
    let detailed = gio::Action::print_detailed_name("app.script", Some(&e.key.to_variant()));
    gio::MenuItem::new(Some(&label), Some(detailed.as_str()))
}

fn rebuild_library(app: &Rc<AppState>) {
    let entries: Vec<ScriptEntry> = app.session.borrow().script_library().to_vec();
    LIBRARY.with(|l| {
        l.remove_all();
        for (origin, title) in [
            (ScriptOrigin::User, "Scripts"),
            (ScriptOrigin::Project, "Project Scripts"),
            (ScriptOrigin::Macro, "Macros"),
        ] {
            let items: Vec<&ScriptEntry> = entries.iter().filter(|e| e.origin == origin).collect();
            if items.is_empty() {
                continue;
            }
            let sub = gio::Menu::new();
            for e in items {
                sub.append_item(&library_item(e));
            }
            l.append_submenu(Some(title), &sub);
        }
    });
}

/// Bring the menu, the console and the Macros window in line with the
/// session (from `AppState::after_change`).
pub(crate) fn follow(app: &Rc<AppState>) {
    let (library, recording) = {
        let s = app.session.borrow();
        (s.script_library_revision(), s.macro_recording().is_some())
    };
    if SHOWN_LIBRARY.with(|c| c.replace(library)) != library {
        rebuild_library(app);
        refill_macros(app);
    }
    if let Some(a) = app
        .app
        .lookup_action("macro-record")
        .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
        && a.state().and_then(|v| v.get::<bool>()) != Some(recording)
    {
        a.set_state(&recording.to_variant());
    }
    refresh_console(app);
}

/// The actions of the Scripts menu and the development aids.
pub(crate) fn install(app: &Rc<AppState>) {
    let plain = |name: &'static str, f: fn(&Rc<AppState>)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    f(&a);
                }
            })
            .build()
    };
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
    let weak = Rc::downgrade(app);
    let record = gio::ActionEntry::builder("macro-record")
        .state(false.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                let recording = a.session.borrow().macro_recording().is_some();
                a.dispatch(Action::Script(if recording {
                    ScriptOp::StopRecording { keep: true }
                } else {
                    ScriptOp::RecordMacro
                }));
            }
        })
        .build();
    app.app.add_action_entries([
        record,
        named("script", |a, key| {
            a.dispatch(Action::Script(ScriptOp::Run(key.to_string())));
        }),
        plain("script-console", open_console),
        plain("run-script-file", choose_file),
        plain("scripts-folder", |a| {
            let dir = faderframe_session::script::user_dir();
            let _ = std::fs::create_dir_all(&dir);
            let win = a.window.borrow().clone();
            gtk::FileLauncher::new(Some(&gio::File::for_path(&dir))).launch(
                win.as_ref(),
                gio::Cancellable::NONE,
                |res| {
                    if let Err(e) = res {
                        tracing::warn!("scripts: could not open the folder: {e}");
                    }
                },
            );
        }),
        plain("reload-scripts", |a| {
            a.dispatch(Action::Script(ScriptOp::Rescan))
        }),
        plain("stop-scripts", |a| {
            a.dispatch(Action::Script(ScriptOp::StopAll))
        }),
        plain("macros", open_macros),
        // Development aids.
        named("run-script", |a, path| {
            a.dispatch(Action::Script(ScriptOp::RunFile(path.trim().into())));
        }),
        named("script-line", |a, line| {
            a.dispatch(Action::Script(ScriptOp::Console(line.to_string())));
        }),
        named("macro", |a, arg| {
            let arg = arg.trim();
            let op = match arg {
                "record" => ScriptOp::RecordMacro,
                "stop" => ScriptOp::StopRecording { keep: true },
                "discard" => ScriptOp::StopRecording { keep: false },
                _ => match arg.strip_prefix("play=") {
                    Some(n) => match n.trim().parse::<u16>() {
                        Ok(n) => ScriptOp::RunMacro(n),
                        Err(_) => {
                            let found = a
                                .session
                                .borrow()
                                .script_library()
                                .iter()
                                .find(|e| e.name.eq_ignore_ascii_case(n.trim()))
                                .map(|e| e.key.clone());
                            match found {
                                Some(key) => ScriptOp::Run(key),
                                None => {
                                    return tracing::warn!("macro: no macro named '{n}'");
                                }
                            }
                        }
                    },
                    None => {
                        return tracing::warn!(
                            "macro: '{arg}' is not record, stop, discard or play=<name or number>"
                        );
                    }
                },
            };
            a.dispatch(Action::Script(op));
        }),
    ]);
}

fn choose_file(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Lua scripts"));
    filter.add_pattern("*.lua");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Run Script File")
        .filters(&filters)
        .initial_folder(&gio::File::for_path(faderframe_session::script::user_dir()))
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Some(a), Ok(file)) = (weak.upgrade(), res)
            && let Some(path) = file.path()
        {
            a.dispatch(Action::Script(ScriptOp::RunFile(path)));
        }
    });
}

fn margins(w: &impl IsA<gtk::Widget>, m: i32) {
    w.set_margin_top(m);
    w.set_margin_bottom(m);
    w.set_margin_start(m);
    w.set_margin_end(m);
}

// --- prompts ------------------------------------------------------------------

/// How a prompt's field gives its answer (`None`: not acceptable yet).
type Reader = Box<dyn Fn() -> Option<String>>;

/// Ask a script's question; the answer (or a cancel, also by closing the
/// window) goes back as `ScriptOp::Answer`.
pub(crate) fn prompt(app: &Rc<AppState>, p: Prompt) {
    let Some(main) = app.window.borrow().clone() else {
        app.dispatch(Action::Script(ScriptOp::Answer {
            prompt: p.id,
            values: None,
        }));
        return;
    };
    let win = gtk::Window::builder()
        .title(format!("{} — {}", p.script, p.title))
        .modal(true)
        .transient_for(&main)
        .resizable(false)
        .default_width(380)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    margins(&body, 14);
    let title = gtk::Label::new(Some(&p.title));
    title.add_css_class("heading");
    title.set_halign(gtk::Align::Start);
    title.set_wrap(true);
    body.append(&title);
    if let Some(t) = &p.text {
        let l = gtk::Label::new(Some(t));
        l.set_wrap(true);
        l.set_halign(gtk::Align::Start);
        l.add_css_class("dim-label");
        body.append(&l);
    }
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    // Each field: how to read its answer.
    let mut readers: Vec<Reader> = Vec::new();
    let mut first: Option<gtk::Widget> = None;
    for (row, f) in p.fields.iter().enumerate() {
        let label = gtk::Label::new(Some(&f.label));
        label.set_halign(gtk::Align::Start);
        let row = row as i32;
        match &f.kind {
            FieldKind::Check => {
                let check = gtk::CheckButton::with_label(&f.label);
                check.set_active(f.default == "true");
                grid.attach(&check, 0, row, 2, 1);
                first.get_or_insert(check.clone().upcast());
                readers.push(Box::new(move || Some(check.is_active().to_string())));
            }
            FieldKind::Choice(choices) => {
                let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
                let drop = gtk::DropDown::from_strings(&refs);
                if let Some(i) = choices.iter().position(|c| *c == f.default) {
                    drop.set_selected(i as u32);
                }
                grid.attach(&label, 0, row, 1, 1);
                grid.attach(&drop, 1, row, 1, 1);
                first.get_or_insert(drop.clone().upcast());
                let choices = choices.clone();
                readers.push(Box::new(move || {
                    choices.get(drop.selected() as usize).cloned()
                }));
            }
            FieldKind::Text | FieldKind::Number => {
                let entry = gtk::Entry::new();
                entry.set_text(&f.default);
                entry.set_hexpand(true);
                entry.set_activates_default(true);
                label.set_mnemonic_widget(Some(&entry));
                grid.attach(&label, 0, row, 1, 1);
                grid.attach(&entry, 1, row, 1, 1);
                first.get_or_insert(entry.clone().upcast());
                let number = matches!(f.kind, FieldKind::Number);
                readers.push(Box::new(move || {
                    let t = entry.text().to_string();
                    if number && t.trim().parse::<f64>().is_err() {
                        entry.add_css_class("error");
                        return None;
                    }
                    entry.remove_css_class("error");
                    Some(t)
                }));
            }
        }
    }
    body.append(&grid);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("OK");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.set_default_widget(Some(&ok));
    let answered = Rc::new(Cell::new(false));
    let id = p.id;
    {
        let (weak, answered) = (Rc::downgrade(app), Rc::clone(&answered));
        win.connect_close_request(move |_| {
            if !answered.replace(true)
                && let Some(a) = weak.upgrade()
            {
                // Later: the window is still closing.
                glib::idle_add_local_once(move || {
                    a.dispatch(Action::Script(ScriptOp::Answer {
                        prompt: id,
                        values: None,
                    }));
                });
            }
            glib::Propagation::Proceed
        });
    }
    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let (weak, w) = (Rc::downgrade(app), win.clone());
    ok.connect_clicked(move |_| {
        let values: Option<Vec<String>> = readers.iter().map(|r| r()).collect();
        let Some(values) = values else {
            return;
        };
        answered.set(true);
        w.close();
        if let Some(a) = weak.upgrade() {
            a.dispatch(Action::Script(ScriptOp::Answer {
                prompt: id,
                values: Some(values),
            }));
        }
    });
    win.present();
    if let Some(f) = first {
        f.grab_focus();
    }
}

// --- the console ------------------------------------------------------------------

struct Console {
    win: gtk::Window,
    view: gtk::TextView,
    shown: Cell<u64>,
    history: RefCell<Vec<String>>,
    back: Cell<usize>,
}

fn open_console(app: &Rc<AppState>) {
    if let Some(win) = CONSOLE.with(|c| c.borrow().as_ref().map(|c| c.win.clone())) {
        win.present();
        return;
    }
    let win = gtk::Window::builder()
        .title("Script Console")
        .application(&app.app)
        .default_width(720)
        .default_height(420)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    margins(&body, 8);
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.update_property(&[gtk::accessible::Property::Label("Script output")]);
    let buffer = view.buffer();
    buffer.create_tag(Some("input"), &[("weight", &700i32)]);
    buffer.create_tag(
        Some("error"),
        &[("weight", &700i32), ("style", &gtk::pango::Style::Italic)],
    );
    buffer.create_tag(Some("info"), &[("style", &gtk::pango::Style::Italic)]);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&view)
        .build();
    body.append(&scroll);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some("Lua — e.g. ff.tracks()[1].name   (↑ ↓ earlier lines)"));
    entry.update_property(&[gtk::accessible::Property::Label("Lua line")]);
    let clear = gtk::Button::with_label("Clear");
    let stop = gtk::Button::with_label("Stop All");
    stop.set_tooltip_text(Some(
        "Stop every timer and every script waiting for an answer",
    ));
    line.append(&entry);
    line.append(&clear);
    line.append(&stop);
    body.append(&line);
    win.set_child(Some(&body));
    let console = Console {
        win: win.clone(),
        view,
        shown: Cell::new(u64::MAX),
        history: RefCell::new(Vec::new()),
        back: Cell::new(0),
    };
    CONSOLE.with(|c| *c.borrow_mut() = Some(console));
    {
        let weak = Rc::downgrade(app);
        entry.connect_activate(move |e| {
            let text = e.text().to_string();
            e.set_text("");
            CONSOLE.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    c.history.borrow_mut().push(text.clone());
                    c.back.set(0);
                }
            });
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Script(ScriptOp::Console(text)));
                refresh_console(&a);
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    {
        let entry = entry.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let step: isize = match key {
                gtk::gdk::Key::Up => 1,
                gtk::gdk::Key::Down => -1,
                _ => return glib::Propagation::Proceed,
            };
            CONSOLE.with(|c| {
                if let Some(c) = c.borrow().as_ref() {
                    let h = c.history.borrow();
                    let back = (c.back.get() as isize + step).clamp(0, h.len() as isize) as usize;
                    c.back.set(back);
                    let text = if back == 0 {
                        ""
                    } else {
                        h[h.len() - back].as_str()
                    };
                    entry.set_text(text);
                    entry.set_position(-1);
                }
            });
            glib::Propagation::Stop
        });
    }
    entry.add_controller(keys);
    {
        let weak = Rc::downgrade(app);
        clear.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Script(ScriptOp::ClearConsole));
                refresh_console(&a);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        stop.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Script(ScriptOp::StopAll));
            }
        });
    }
    win.connect_close_request(|_| {
        CONSOLE.with(|c| c.borrow_mut().take());
        glib::Propagation::Proceed
    });
    refresh_console(app);
    win.present();
    entry.grab_focus();
}

fn refresh_console(app: &Rc<AppState>) {
    CONSOLE.with(|c| {
        let c = c.borrow();
        let Some(c) = c.as_ref() else {
            return;
        };
        let Ok(s) = app.session.try_borrow() else {
            return;
        };
        let rev = s.script_console_revision();
        if c.shown.replace(rev) == rev {
            return;
        }
        let buffer = c.view.buffer();
        buffer.set_text("");
        for l in s.script_console() {
            let mut end = buffer.end_iter();
            let tag = match l.kind {
                ConsoleKind::Input => Some("input"),
                ConsoleKind::Error => Some("error"),
                ConsoleKind::Info => Some("info"),
                ConsoleKind::Output | ConsoleKind::Result => None,
            };
            let text = format!("{}\n", l.text);
            match tag {
                Some(t) => buffer.insert_with_tags_by_name(&mut end, &text, &[t]),
                None => buffer.insert(&mut end, &text),
            }
        }
        let mut end = buffer.end_iter();
        c.view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
    });
}

// --- the Macros window ------------------------------------------------------------------

fn open_macros(app: &Rc<AppState>) {
    if let Some(win) = MACROS.with(|m| m.borrow().as_ref().map(|(w, _)| w.clone())) {
        win.present();
        return;
    }
    let win = gtk::Window::builder()
        .title("Macros")
        .application(&app.app)
        .default_width(560)
        .default_height(380)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    margins(&body, 12);
    let note = gtk::Label::new(Some(
        "Macros and numbered scripts. A MIDI button or a surface (Control + F1…F8, OSC /macro/run n) runs a macro by its number; shortcuts are set in the shortcut editor (Scripts › Macros).",
    ));
    note.set_wrap(true);
    note.set_halign(gtk::Align::Start);
    note.add_css_class("dim-label");
    body.append(&note);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&rows)
        .build();
    body.append(&scroll);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let record = gtk::Button::with_label("Record Macro");
    record.set_action_name(Some("app.macro-record"));
    let keys = gtk::Button::with_label("Shortcuts…");
    {
        let weak = Rc::downgrade(app);
        keys.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                crate::palette::shortcuts(&a);
            }
        });
    }
    buttons.append(&keys);
    buttons.append(&record);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.connect_close_request(|_| {
        MACROS.with(|m| m.borrow_mut().take());
        glib::Propagation::Proceed
    });
    MACROS.with(|m| *m.borrow_mut() = Some((win.clone(), rows)));
    refill_macros(app);
    win.present();
}

fn refill_macros(app: &Rc<AppState>) {
    let Some(rows) = MACROS.with(|m| m.borrow().as_ref().map(|(_, r)| r.clone())) else {
        return;
    };
    while let Some(c) = rows.first_child() {
        rows.remove(&c);
    }
    let entries: Vec<ScriptEntry> = app
        .session
        .borrow()
        .script_library()
        .iter()
        .filter(|e| e.number.is_some())
        .cloned()
        .collect();
    if entries.is_empty() {
        let l = gtk::Label::new(Some("No macros yet: Scripts › Record Macro."));
        l.add_css_class("dim-label");
        rows.append(&l);
    }
    for e in entries {
        let Some(number) = e.number else { continue };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(&format!("{number} · {}", e.name)));
        label.set_hexpand(true);
        label.set_xalign(0.0);
        if let Some(d) = &e.description {
            label.set_tooltip_text(Some(d));
        }
        row.append(&label);
        let run = gtk::Button::with_label("Run");
        run.set_action_name(Some("app.script"));
        run.set_action_target_value(Some(&e.key.to_variant()));
        row.append(&run);
        let rename = gtk::Button::with_label("Rename…");
        {
            let (weak, name) = (Rc::downgrade(app), e.name.clone());
            rename.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    crate::dialogs::name_prompt_checked(
                        &a,
                        "Rename Macro",
                        "Macro name:",
                        &name,
                        "Rename",
                        |_, _| None,
                        move |name| Action::Script(ScriptOp::RenameMacro { number, name }),
                    );
                }
            });
        }
        row.append(&rename);
        let learn = gtk::Button::with_label("MIDI Learn");
        learn.set_tooltip_text(Some(
            "Press a key or button on a controller to run this macro with it",
        ));
        {
            let weak = Rc::downgrade(app);
            learn.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(Action::MidiLearn(MappingTarget::RunMacro { number }));
                }
            });
        }
        row.append(&learn);
        if e.origin == ScriptOrigin::Macro {
            let delete = gtk::Button::with_label("Delete");
            let weak = Rc::downgrade(app);
            delete.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(Action::Script(ScriptOp::DeleteMacro(number)));
                }
            });
            row.append(&delete);
        }
        rows.append(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_menu_lists_its_commands_for_the_palette() {
        let commands = crate::palette::menu_commands(menu().upcast_ref());
        let actions: Vec<&str> = commands.iter().map(|c| c.action.as_str()).collect();
        for a in [
            "app.script-console",
            "app.run-script-file",
            "app.macro-record",
            "app.macros",
            "app.stop-scripts",
        ] {
            assert!(actions.contains(&a), "{a}: {actions:?}");
        }
    }
}
