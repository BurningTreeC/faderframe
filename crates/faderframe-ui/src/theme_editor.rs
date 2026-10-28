//! View → Theme → Edit Themes… (also Preferences → General): themes of the
//! user's own. A theme starts from a built-in skin (its knobs, meters and
//! fonts) or another of the user's themes; every colour the app paints
//! with can be changed, by section, and is seen at once. Each theme is a
//! file in the themes folder ([`crate::user_themes`]).

use crate::state::AppState;
use crate::user_themes;
use faderframe_ui_canvas::custom::{self, COLORS, CustomTheme, GROUPS};
use faderframe_ui_canvas::{Color, Theme};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn rgba(c: Color) -> gdk::RGBA {
    gdk::RGBA::new(c.r, c.g, c.b, 1.0)
}

fn color(c: &gdk::RGBA) -> Color {
    Color::rgb(c.red(), c.green(), c.blue())
}

/// The editor's state: the user's themes, the one shown.
struct Editor {
    app: std::rc::Weak<AppState>,
    themes: RefCell<Vec<CustomTheme>>,
    shown: RefCell<Option<CustomTheme>>,
    /// Widgets are being set from the theme (their signals do nothing).
    loading: Cell<bool>,
    chooser: gtk::DropDown,
    name: gtk::Entry,
    base: gtk::DropDown,
    rows: RefCell<Vec<(&'static str, gtk::ColorDialogButton, gtk::Button)>>,
    editing: gtk::Box,
    empty: gtk::Box,
    duplicate: gtk::Button,
    delete: gtk::Button,
}

impl Editor {
    /// Write the shown theme, register it, wear it, list it.
    fn commit(&self) {
        let Some(t) = self.shown.borrow().clone() else {
            return;
        };
        if let Err(e) = user_themes::save(&t) {
            tracing::warn!("cannot save the theme {}: {e}", t.name);
        }
        if let Some(app) = self.app.upgrade() {
            user_themes::rebuild_menu(&app.custom_themes_menu);
            app.set_theme(&t.id());
        }
        let mut list = self.themes.borrow_mut();
        match list.iter_mut().find(|x| x.slug == t.slug) {
            Some(x) => *x = t,
            None => list.push(t),
        }
    }

    /// Show `slug` (or the first theme; none: the empty page).
    fn show(&self, slug: Option<&str>) {
        self.loading.set(true);
        let list = self.themes.borrow().clone();
        let names: Vec<&str> = list.iter().map(|t| t.name.as_str()).collect();
        self.chooser.set_model(Some(&gtk::StringList::new(&names)));
        let i = slug
            .and_then(|s| list.iter().position(|t| t.slug == s))
            .unwrap_or(0);
        let shown = list.get(i).cloned();
        self.editing.set_visible(shown.is_some());
        self.empty.set_visible(shown.is_none());
        self.duplicate.set_sensitive(shown.is_some());
        self.delete.set_sensitive(shown.is_some());
        self.chooser.set_sensitive(shown.is_some());
        if let Some(t) = &shown {
            self.chooser.set_selected(i as u32);
            self.name.set_text(&t.name);
            let bases = Theme::builtin();
            self.base
                .set_selected(bases.iter().position(|b| b.id == t.base).unwrap_or(0) as u32);
            let theme = t.build();
            for (id, button, reset) in self.rows.borrow().iter() {
                if let Some(k) = custom::color_key(id) {
                    button.set_rgba(&rgba(k.get(&theme)));
                }
                reset.set_sensitive(t.colors.iter().any(|(k, _)| k == id));
            }
        }
        *self.shown.borrow_mut() = shown;
        self.loading.set(false);
    }

    /// A new theme from `from` (or the theme in use), shown and worn.
    fn create(&self, from: Option<CustomTheme>) {
        let from = from.unwrap_or_else(|| {
            let current = self
                .app
                .upgrade()
                .map(|a| a.theme.borrow().id)
                .unwrap_or("studio");
            self.themes
                .borrow()
                .iter()
                .find(|t| t.id() == current)
                .cloned()
                .unwrap_or_else(|| CustomTheme {
                    slug: String::new(),
                    name: Theme::by_id(current).name.to_string(),
                    base: current.to_string(),
                    colors: Vec::new(),
                })
        });
        let name = user_themes::new_name(&from.name);
        let t = CustomTheme {
            slug: user_themes::new_slug(&name),
            name,
            ..from
        };
        *self.shown.borrow_mut() = Some(t.clone());
        self.commit();
        self.show(Some(&t.slug));
    }
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("heading");
    l.set_margin_top(10);
    l
}

/// Open the theme editor.
pub fn open(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Themes")
        .application(&app.app)
        .transient_for(&main)
        .default_width(560)
        .default_height(720)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);

    // The theme shown, and making or removing one.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let chooser = gtk::DropDown::from_strings(&[]);
    chooser.set_hexpand(true);
    let new = gtk::Button::with_label("New");
    new.set_tooltip_text(Some("A theme of your own from the one in use"));
    let duplicate = gtk::Button::with_label("Duplicate");
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete the theme"));
    top.append(&chooser);
    top.append(&new);
    top.append(&duplicate);
    top.append(&delete);
    body.append(&top);

    // Its name, the skin it starts from, its colours.
    let editing = gtk::Box::new(gtk::Orientation::Vertical, 8);
    editing.set_vexpand(true);
    let form = gtk::Grid::new();
    form.set_column_spacing(10);
    form.set_row_spacing(6);
    let name = gtk::Entry::new();
    name.set_hexpand(true);
    let name_label = gtk::Label::new(Some("Name"));
    name_label.set_xalign(0.0);
    form.attach(&name_label, 0, 0, 1, 1);
    form.attach(&name, 1, 0, 1, 1);
    let bases = Theme::builtin();
    let base_names: Vec<&str> = bases.iter().map(|t| t.name).collect();
    let base = gtk::DropDown::from_strings(&base_names);
    base.set_tooltip_text(Some(
        "Knobs, meters, fonts and the colours not changed come from this skin",
    ));
    let base_label = gtk::Label::new(Some("Based on"));
    base_label.set_xalign(0.0);
    form.attach(&base_label, 0, 1, 1, 1);
    form.attach(&base, 1, 1, 1, 1);
    editing.append(&form);
    let colours = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let mut rows = Vec::new();
    for group in GROUPS {
        colours.append(&heading(group));
        let grid = gtk::Grid::new();
        grid.set_column_spacing(10);
        grid.set_row_spacing(4);
        for (r, k) in COLORS.iter().filter(|k| k.group == group).enumerate() {
            let label = gtk::Label::new(Some(k.label));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            let dialog = gtk::ColorDialog::new();
            dialog.set_with_alpha(false);
            dialog.set_title(k.label);
            let button = gtk::ColorDialogButton::new(Some(dialog));
            let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
            reset.add_css_class("flat");
            reset.set_tooltip_text(Some("As the skin it is based on has it"));
            grid.attach(&label, 0, r as i32, 1, 1);
            grid.attach(&button, 1, r as i32, 1, 1);
            grid.attach(&reset, 2, r as i32, 1, 1);
            rows.push((k.id, button, reset));
        }
        colours.append(&grid);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&colours)
        .build();
    editing.append(&scroll);
    body.append(&editing);

    // No theme of the user's yet.
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
    empty.set_vexpand(true);
    empty.set_valign(gtk::Align::Center);
    let intro = gtk::Label::new(Some(
        "Make a theme of your own: it starts as the one in use, and every colour can be changed.",
    ));
    intro.set_wrap(true);
    intro.add_css_class("dim-label");
    let create = gtk::Button::with_label("Create a Theme");
    create.add_css_class("suggested-action");
    create.set_halign(gtk::Align::Center);
    empty.append(&intro);
    empty.append(&create);
    body.append(&empty);

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let folder = gtk::Button::with_label("Open Themes Folder");
    folder.set_tooltip_text(Some("Theme files can be shared and edited by hand"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close = gtk::Button::with_label("Close");
    bottom.append(&folder);
    bottom.append(&spacer);
    bottom.append(&close);
    body.append(&bottom);
    win.set_child(Some(&body));

    let ed = Rc::new(Editor {
        app: Rc::downgrade(app),
        themes: RefCell::new(user_themes::load()),
        shown: RefCell::new(None),
        loading: Cell::new(false),
        chooser: chooser.clone(),
        name: name.clone(),
        base: base.clone(),
        rows: RefCell::new(rows),
        editing,
        empty,
        duplicate: duplicate.clone(),
        delete: delete.clone(),
    });
    // The theme in use, when it is one of the user's.
    let current = app.theme.borrow().id;
    let slug = current
        .strip_prefix(custom::CUSTOM_PREFIX)
        .map(str::to_string);
    ed.show(slug.as_deref());

    {
        let ed = Rc::clone(&ed);
        chooser.connect_selected_notify(move |d| {
            if ed.loading.get() {
                return;
            }
            let slug = ed
                .themes
                .borrow()
                .get(d.selected() as usize)
                .map(|t| t.slug.clone());
            ed.show(slug.as_deref());
            if let (Some(slug), Some(app)) = (slug, ed.app.upgrade()) {
                app.set_theme(&format!("{}{slug}", custom::CUSTOM_PREFIX));
            }
        });
    }
    for (id, button, reset) in ed.rows.borrow().iter() {
        let (ed2, id) = (Rc::clone(&ed), *id);
        let r = reset.clone();
        button.connect_rgba_notify(move |b| {
            if ed2.loading.get() {
                return;
            }
            if let Some(t) = ed2.shown.borrow_mut().as_mut() {
                t.set_color(id, color(&b.rgba()));
                r.set_sensitive(t.colors.iter().any(|(k, _)| k == id));
            }
            ed2.commit();
        });
        let (ed2, b) = (Rc::clone(&ed), button.clone());
        reset.connect_clicked(move |r| {
            let base = {
                let mut shown = ed2.shown.borrow_mut();
                let Some(t) = shown.as_mut() else { return };
                t.colors.retain(|(k, _)| k != id);
                custom::color_key(id).map(|k| k.get(&t.base_theme()))
            };
            r.set_sensitive(false);
            if let Some(c) = base {
                ed2.loading.set(true);
                b.set_rgba(&rgba(c));
                ed2.loading.set(false);
            }
            ed2.commit();
        });
    }
    {
        let ed = Rc::clone(&ed);
        let rename = move |e: &gtk::Entry| {
            let text = e.text().trim().to_string();
            if ed.loading.get() || text.is_empty() {
                return;
            }
            let slug = {
                let mut shown = ed.shown.borrow_mut();
                let Some(t) = shown.as_mut() else { return };
                if t.name == text {
                    return;
                }
                t.name = text;
                t.slug.clone()
            };
            ed.commit();
            ed.show(Some(&slug));
        };
        let r = rename.clone();
        name.connect_activate(move |e| r(e));
        let focus = gtk::EventControllerFocus::new();
        let n = name.clone();
        focus.connect_leave(move |_| rename(&n));
        name.add_controller(focus);
    }
    {
        let ed = Rc::clone(&ed);
        base.connect_selected_notify(move |d| {
            if ed.loading.get() {
                return;
            }
            let Some(b) = Theme::builtin().get(d.selected() as usize).map(|t| t.id) else {
                return;
            };
            let slug = {
                let mut shown = ed.shown.borrow_mut();
                let Some(t) = shown.as_mut() else { return };
                t.base = b.to_string();
                t.slug.clone()
            };
            ed.commit();
            ed.show(Some(&slug));
        });
    }
    {
        let ed2 = Rc::clone(&ed);
        new.connect_clicked(move |_| ed2.create(None));
        let ed2 = Rc::clone(&ed);
        create.connect_clicked(move |_| ed2.create(None));
        let ed2 = Rc::clone(&ed);
        duplicate.connect_clicked(move |_| {
            let from = ed2.shown.borrow().clone();
            if from.is_some() {
                ed2.create(from);
            }
        });
    }
    {
        let (ed, w) = (Rc::clone(&ed), win.downgrade());
        delete.connect_clicked(move |_| {
            let (Some(t), Some(w)) = (ed.shown.borrow().clone(), w.upgrade()) else {
                return;
            };
            let dialog = gtk::AlertDialog::builder()
                .message(format!("Delete the theme ‘{}’?", t.name))
                .detail("Its file is deleted.")
                .buttons(["Cancel", "Delete"])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let ed = Rc::clone(&ed);
            dialog.choose(Some(&w), gio::Cancellable::NONE, move |res| {
                if res != Ok(1) {
                    return;
                }
                if let Err(e) = user_themes::delete(&t.slug) {
                    tracing::warn!("cannot delete the theme {}: {e}", t.name);
                    return;
                }
                ed.themes.borrow_mut().retain(|x| x.slug != t.slug);
                if let Some(app) = ed.app.upgrade() {
                    user_themes::rebuild_menu(&app.custom_themes_menu);
                    // Worn: the skin it was based on instead.
                    if app.theme.borrow().id == t.id() {
                        app.set_theme(&t.base);
                    }
                }
                ed.show(None);
            });
        });
    }
    let w = win.downgrade();
    folder.connect_clicked(move |_| {
        let dir = user_themes::dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("could not make {}: {e}", dir.display());
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
        launcher.launch(w.upgrade().as_ref(), gio::Cancellable::NONE, |res| {
            if let Err(e) = res {
                tracing::warn!("could not open the themes folder: {e}");
            }
        });
    });
    let w = win.downgrade();
    close.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    let keys = gtk::EventControllerKey::new();
    let w = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape
            && let Some(w) = w.upgrade()
        {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    win.present();
}
