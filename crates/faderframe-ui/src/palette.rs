//! The command palette (Ctrl+Shift+P) and the keyboard shortcut editor.
//! Both list the commands of the menus — walked from the menu model, so
//! they always match them — by their place ("Track › Add Instrument
//! Track…"). Shortcuts the user sets are kept in the preferences
//! (`Preferences::shortcuts`, by detailed action name) and applied over
//! the defaults at start-up. The single-key shortcuts (Space, L, Home, …)
//! are fixed: they leave text fields their keys.

use crate::state::AppState;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::collections::BTreeMap;
use std::rc::Rc;

/// One command of the menus.
#[derive(Clone, Debug)]
pub(crate) struct Command {
    /// Where it is: "File › Save Version…".
    pub path: String,
    /// "app.save-version".
    pub action: String,
    pub target: Option<glib::Variant>,
}

impl Command {
    /// The action with its target ("app.edit::separate").
    pub fn detailed(&self) -> String {
        gio::Action::print_detailed_name(&self.action, self.target.as_ref()).to_string()
    }
}

/// The shortcuts FaderFrame starts with (detailed action, accelerators).
pub(crate) const DEFAULT_SHORTCUTS: &[(&str, &[&str])] = &[
    ("app.new", &["<Control>n"]),
    ("app.open", &["<Control>o"]),
    ("app.save", &["<Control>s"]),
    ("app.toggle-edit-toolbar", &["<Control>e"]),
    ("app.import-audio", &["<Control>i"]),
    ("app.save-as", &["<Control><Shift>s"]),
    ("app.quit", &["<Control>q"]),
    ("app.render", &["<Control><Shift>r"]),
    ("app.capture-midi", &["<Control><Shift>c"]),
    ("app.save-version", &["<Control><Alt>s"]),
    ("app.command-palette", &["<Control><Shift>p"]),
    ("app.preferences", &["<Control>comma"]),
    ("app.undo", &["<Control>z"]),
    ("app.redo", &["<Control><Shift>z", "<Control>y"]),
    ("app.add-audio", &["<Control>t"]),
    ("app.add-instrument", &["<Control><Shift>t"]),
    ("app.dock-bottom", &["F2"]),
    ("app.view-mixer", &["F3"]),
    ("app.view-piano-roll", &["F4"]),
    ("app.view-performance", &["F8"]),
    ("app.view-tools", &["F12"]),
    ("app.workspace-1", &["<Control>1"]),
    ("app.workspace-2", &["<Control>2"]),
    ("app.workspace-3", &["<Control>3"]),
    ("app.workspace-4", &["<Control>4"]),
    ("app.workspace-5", &["<Control>5"]),
    // Edit modes and tools (wherever the keyboard is: not only in the
    // arranger).
    ("app.edit-mode::shuffle", &["<Alt>1"]),
    ("app.edit-mode::slip", &["<Alt>2"]),
    ("app.edit-mode::spot", &["<Alt>3"]),
    ("app.edit-mode::grid", &["<Alt>4"]),
    ("app.edit-tool::zoom", &["<Alt>5"]),
    ("app.edit-tool::trim", &["<Alt>6"]),
    ("app.edit-tool::select", &["<Alt>7"]),
    ("app.edit-tool::grab", &["<Alt>8"]),
    ("app.edit-tool::scrub", &["<Alt>9"]),
    ("app.edit-tool::pencil", &["<Alt>0"]),
    ("app.edit-tool::smart", &["<Alt>s"]),
];

/// The fixed single-key shortcuts, for the editor's note.
const FIXED: &str = "Fixed: Space play/stop · Home to start · L loop · K metronome · Shift+R record · T take lanes · A automation · Esc ends MIDI learn";

/// A command's default shortcuts.
fn defaults(detailed: &str) -> Vec<String> {
    DEFAULT_SHORTCUTS
        .iter()
        .find(|(a, _)| *a == detailed)
        .map(|(_, k)| k.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// The defaults, then the user's shortcuts over them.
pub(crate) fn apply_shortcuts(app: &gtk::Application, user: &BTreeMap<String, Vec<String>>) {
    for (action, keys) in DEFAULT_SHORTCUTS {
        app.set_accels_for_action(action, keys);
    }
    for (action, keys) in user {
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        app.set_accels_for_action(action, &keys);
    }
}

/// The commands of a menu model, depth first.
pub(crate) fn menu_commands(model: &gio::MenuModel) -> Vec<Command> {
    fn walk(model: &gio::MenuModel, prefix: &str, out: &mut Vec<Command>) {
        for i in 0..model.n_items() {
            let label = model
                .item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
                .map(|l| l.replace('_', ""));
            let action = model
                .item_attribute_value(i, "action", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>());
            let path = match &label {
                Some(l) if prefix.is_empty() => l.clone(),
                Some(l) => format!("{prefix} › {l}"),
                None => prefix.to_string(),
            };
            if let (Some(_), Some(action)) = (&label, action) {
                out.push(Command {
                    path: path.clone(),
                    action,
                    target: model.item_attribute_value(i, "target", None),
                });
            }
            if let Some(sub) = model.item_link(i, "submenu") {
                walk(&sub, &path, out);
            }
            if let Some(section) = model.item_link(i, "section") {
                walk(&section, prefix, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(model, "", &mut out);
    out
}

/// How well `query` fits `label` (higher is better; `None`: not at all).
/// Every word of the query must be in it — as a whole (better at a word's
/// start) or its letters in order; shorter labels win ties.
pub(crate) fn score(query: &str, label: &str) -> Option<i32> {
    let label = label.to_lowercase();
    let mut total = 0;
    for word in query.to_lowercase().split_whitespace() {
        total += match label.find(word) {
            Some(at) => {
                let start = at == 0
                    || label[..at]
                        .chars()
                        .last()
                        .is_some_and(|c| !c.is_alphanumeric());
                if start { 15 } else { 10 }
            }
            None => {
                let mut rest = label.chars();
                if word.chars().all(|c| rest.any(|l| l == c)) {
                    2
                } else {
                    return None;
                }
            }
        };
    }
    Some(total * 100 - label.len() as i32)
}

/// A shortcut as the menus show it ("Ctrl+Shift+P").
fn shortcut_label(accel: &str) -> String {
    gtk::accelerator_parse(accel)
        .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string())
        .unwrap_or_else(|| accel.to_string())
}

fn commands(app: &Rc<AppState>) -> Vec<Command> {
    let model = crate::window::menu_model(&app.recent_menu);
    menu_commands(model.upcast_ref())
}

/// Run a command (an `app.` action with its target).
fn run(app: &Rc<AppState>, c: &Command) {
    if let Some(name) = c.action.strip_prefix("app.") {
        app.app.activate_action(name, c.target.as_ref());
    }
}

fn margins(w: &impl IsA<gtk::Widget>, m: i32) {
    w.set_margin_top(m);
    w.set_margin_bottom(m);
    w.set_margin_start(m);
    w.set_margin_end(m);
}

/// The command palette: type to find a command of the menus, Enter runs
/// it.
pub fn open(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let all = commands(app);
    let win = gtk::Window::builder()
        .title("Commands")
        .application(&app.app)
        .transient_for(&main)
        .modal(true)
        .default_width(600)
        .default_height(440)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    margins(&body, 12);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Type a command (Enter runs it, Esc closes)"));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    body.append(&search);
    body.append(&scroll);
    win.set_child(Some(&body));
    let shown: Rc<std::cell::RefCell<Vec<Command>>> = Rc::default();
    let fill = {
        let (app, list, shown) = (Rc::clone(app), list.clone(), Rc::clone(&shown));
        move |query: &str| {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let mut found: Vec<(i32, &Command)> = all
                .iter()
                .filter_map(|c| score(query, &c.path).map(|s| (s, c)))
                .collect();
            if query.trim().is_empty() {
                found = all.iter().map(|c| (0, c)).collect();
            } else {
                found.sort_by_key(|(s, _)| -s);
            }
            let mut kept = Vec::new();
            for (_, c) in found.into_iter().take(80) {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                margins(&row, 6);
                let label = gtk::Label::new(Some(&c.path));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                row.append(&label);
                if let Some(k) = app.app.accels_for_action(&c.detailed()).first() {
                    let keys = gtk::Label::new(Some(&shortcut_label(k)));
                    keys.add_css_class("dim-label");
                    row.append(&keys);
                }
                list.append(&row);
                kept.push(c.clone());
            }
            *shown.borrow_mut() = kept;
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    fill("");
    let fill = Rc::new(fill);
    {
        let fill = Rc::clone(&fill);
        search.connect_search_changed(move |e| fill(&e.text()));
    }
    let activate = {
        let (app, shown, win) = (Rc::clone(app), Rc::clone(&shown), win.clone());
        Rc::new(move |index: i32| {
            let c = shown.borrow().get(index.max(0) as usize).cloned();
            win.close();
            if let Some(c) = c {
                run(&app, &c);
            }
        })
    };
    {
        let (activate, list) = (Rc::clone(&activate), list.clone());
        search.connect_activate(move |_| {
            activate(list.selected_row().map_or(0, |r| r.index()));
        });
    }
    {
        let activate = Rc::clone(&activate);
        list.connect_row_activated(move |_, row| activate(row.index()));
    }
    // Up and down move through the list while typing; Esc closes.
    let keys = gtk::EventControllerKey::new();
    {
        let (list, win) = (list.clone(), win.clone());
        keys.connect_key_pressed(move |_, key, _, _| {
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                gdk::Key::Escape => {
                    win.close();
                    return glib::Propagation::Stop;
                }
                _ => return glib::Propagation::Proceed,
            };
            let at = list.selected_row().map_or(-1, |r| r.index()) + step;
            if let Some(row) = list.row_at_index(at.max(0)) {
                list.select_row(Some(&row));
                row.grab_focus();
            }
            glib::Propagation::Stop
        });
    }
    search.add_controller(keys);
    win.present();
    search.grab_focus();
}

/// Change shortcut `accel` to run `detailed` (`None`: clear `detailed`'s);
/// the command that had it loses it. Saved in the preferences.
fn assign(app: &Rc<AppState>, detailed: &str, accel: Option<String>) {
    let mut prefs = crate::prefs::Preferences::load();
    if let Some(a) = &accel {
        for other in app.app.actions_for_accel(a) {
            if other == detailed {
                continue;
            }
            let rest: Vec<String> = app
                .app
                .accels_for_action(&other)
                .iter()
                .map(|k| k.to_string())
                .filter(|k| k != a)
                .collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            app.app.set_accels_for_action(&other, &refs);
            prefs.shortcuts.insert(other.to_string(), rest);
        }
    }
    let keys: Vec<String> = accel.into_iter().collect();
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    app.app.set_accels_for_action(detailed, &refs);
    prefs.shortcuts.insert(detailed.to_string(), keys);
    if let Err(e) = prefs.save() {
        tracing::warn!("shortcuts: {e}");
    }
}

/// Back to the default shortcut (`None`: every command).
fn reset(app: &Rc<AppState>, detailed: Option<&str>) {
    let mut prefs = crate::prefs::Preferences::load();
    match detailed {
        Some(d) => {
            prefs.shortcuts.remove(d);
            let keys = defaults(d);
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            app.app.set_accels_for_action(d, &refs);
        }
        None => {
            for action in prefs.shortcuts.keys() {
                app.app.set_accels_for_action(action, &[]);
            }
            prefs.shortcuts.clear();
            apply_shortcuts(&app.app, &prefs.shortcuts);
        }
    }
    if let Err(e) = prefs.save() {
        tracing::warn!("shortcuts: {e}");
    }
}

/// Rebuilds a list for a search.
type Filler = Rc<dyn Fn(&str)>;

/// The keyboard shortcut editor: every command of the menus with its
/// shortcut; click one and press the new keys (Esc: cancel, Backspace:
/// none).
pub fn shortcuts(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    let all: Vec<Command> = commands(app)
        .into_iter()
        .filter(|c| c.action != "app.open-recent" && seen.insert(c.detailed()))
        .collect();
    let win = gtk::Window::builder()
        .title("Keyboard Shortcuts")
        .application(&app.app)
        .transient_for(&main)
        .default_width(640)
        .default_height(560)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    margins(&body, 12);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let search = gtk::SearchEntry::new();
    search.set_hexpand(true);
    search.set_placeholder_text(Some("Find a command"));
    let reset_all = gtk::Button::with_label("Reset All");
    top.append(&search);
    top.append(&reset_all);
    body.append(&top);
    let fixed = gtk::Label::new(Some(FIXED));
    fixed.set_wrap(true);
    fixed.set_xalign(0.0);
    fixed.add_css_class("dim-label");
    body.append(&fixed);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    body.append(&scroll);
    win.set_child(Some(&body));
    // The command waiting for its new keys, and the button that shows it.
    let waiting: Rc<std::cell::RefCell<Option<(String, gtk::Button)>>> = Rc::default();
    let fill: Rc<std::cell::RefCell<Option<Filler>>> = Rc::default();
    let filler: Rc<dyn Fn(&str)> = {
        let (app, list, waiting, fill) = (
            Rc::clone(app),
            list.clone(),
            Rc::clone(&waiting),
            Rc::clone(&fill),
        );
        Rc::new(move |query: &str| {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            for c in all
                .iter()
                .filter(|c| query.trim().is_empty() || score(query, &c.path).is_some())
            {
                let detailed = c.detailed();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                margins(&row, 4);
                let label = gtk::Label::new(Some(&c.path));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                let keys: Vec<String> = app
                    .app
                    .accels_for_action(&detailed)
                    .iter()
                    .map(|k| shortcut_label(k))
                    .collect();
                let text = if keys.is_empty() {
                    "—".to_string()
                } else {
                    keys.join(", ")
                };
                let button = gtk::Button::with_label(&text);
                button.set_width_request(150);
                let reset_one = gtk::Button::from_icon_name("edit-undo-symbolic");
                reset_one.set_tooltip_text(Some("Back to the default"));
                row.append(&label);
                row.append(&button);
                row.append(&reset_one);
                list.append(&row);
                {
                    let (waiting, detailed) = (Rc::clone(&waiting), detailed.clone());
                    button.connect_clicked(move |b| {
                        b.set_label("Press keys… (Esc, Backspace: none)");
                        *waiting.borrow_mut() = Some((detailed.clone(), b.clone()));
                    });
                }
                {
                    let (app, fill, detailed) = (Rc::clone(&app), Rc::clone(&fill), detailed);
                    reset_one.connect_clicked(move |_| {
                        reset(&app, Some(&detailed));
                        if let Some(f) = fill.borrow().as_ref() {
                            f("");
                        }
                    });
                }
            }
        })
    };
    *fill.borrow_mut() = Some(Rc::clone(&filler));
    filler("");
    {
        let filler = Rc::clone(&filler);
        search.connect_search_changed(move |e| filler(&e.text()));
    }
    {
        let (app, filler) = (Rc::clone(app), Rc::clone(&filler));
        reset_all.connect_clicked(move |_| {
            reset(&app, None);
            filler("");
        });
    }
    // The new keys of the command waiting for them.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (app, waiting, filler, search) = (
            Rc::clone(app),
            Rc::clone(&waiting),
            Rc::clone(&filler),
            search.clone(),
        );
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some((detailed, _)) = waiting.borrow().clone() else {
                return glib::Propagation::Proceed;
            };
            let modifier = matches!(
                key,
                gdk::Key::Shift_L
                    | gdk::Key::Shift_R
                    | gdk::Key::Control_L
                    | gdk::Key::Control_R
                    | gdk::Key::Alt_L
                    | gdk::Key::Alt_R
                    | gdk::Key::Super_L
                    | gdk::Key::Super_R
                    | gdk::Key::Meta_L
                    | gdk::Key::Meta_R
                    | gdk::Key::ISO_Level3_Shift
            );
            if modifier {
                return glib::Propagation::Stop;
            }
            let mods = state & gtk::accelerator_get_default_mod_mask();
            match key {
                gdk::Key::Escape => {}
                gdk::Key::BackSpace if mods.is_empty() => assign(&app, &detailed, None),
                _ => {
                    let accel = gtk::accelerator_name(key.to_lower(), mods).to_string();
                    assign(&app, &detailed, Some(accel));
                }
            }
            *waiting.borrow_mut() = None;
            filler(&search.text());
            glib::Propagation::Stop
        });
    }
    win.add_controller(keys);
    win.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menus_become_commands_with_their_places() {
        let menu = gio::Menu::new();
        let file = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("Save"), Some("app.save"));
        section.append(Some("Save Version…"), Some("app.save-version"));
        file.append_section(None, &section);
        let tool = gio::MenuItem::new(Some("Separate"), None);
        tool.set_action_and_target_value(Some("app.edit"), Some(&"separate".to_variant()));
        file.append_item(&tool);
        menu.append_submenu(Some("_File"), &file);
        let all = menu_commands(menu.upcast_ref());
        let paths: Vec<&str> = all.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            ["File › Save", "File › Save Version…", "File › Separate"]
        );
        assert_eq!(all[2].detailed(), "app.edit::separate");
    }

    #[test]
    fn queries_find_commands_by_words() {
        let a = "File › Save Version…";
        let b = "Track › Move to Folder";
        assert!(score("save ver", a).is_some());
        assert!(score("save ver", b).is_none());
        assert!(score("sv", a).is_some(), "letters in order");
        // A word's start beats its middle; shorter wins ties.
        assert!(score("ver", a) > score("ers", a));
        assert!(score("save", "File › Save") > score("save", a));
    }
}
