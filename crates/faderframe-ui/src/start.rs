//! The start window: what to work on — a recent project, a new one (empty
//! or from a template), a project file, the demo session. Shown when the
//! app starts if Preferences → General says so (the default), and by
//! File → Start Window… at any time (asking about unsaved changes first).

use crate::prefs::Preferences;
use crate::recent::StartupProject;
use crate::state::AppState;
use faderframe_session::templates;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Whether this start of the app shows the start window: the preference
/// says so and nothing else chose the project (the command line, a
/// scripted run).
pub fn wanted_at_start(chosen_elsewhere: bool, prefs: &Preferences) -> bool {
    !chosen_elsewhere
        && !crate::scripted_run()
        && StartupProject::from_id(&prefs.startup_project).unwrap_or_default()
            == StartupProject::Ask
}

/// A recent project as the window lists it.
struct Recent {
    path: PathBuf,
    name: String,
    folder: String,
    saved: Option<std::time::SystemTime>,
    there: bool,
}

fn recent_projects() -> Vec<Recent> {
    Preferences::load()
        .recent_projects
        .iter()
        .map(|p| {
            let path = PathBuf::from(p);
            let meta = std::fs::metadata(&path).ok();
            let name = path
                .file_stem()
                .map_or_else(|| p.clone(), |s| s.to_string_lossy().into_owned());
            Recent {
                folder: crate::recent::folder_label(&path),
                saved: meta.as_ref().and_then(|m| m.modified().ok()),
                there: meta.is_some(),
                name,
                path,
            }
        })
        .collect()
}

/// After asking about unsaved changes: `then`, and the window closes.
fn go(
    app: &Rc<AppState>,
    win: &glib::WeakRef<gtk::Window>,
    then: impl Fn(&Rc<AppState>) + 'static,
) {
    let win = win.clone();
    crate::dialogs::confirm_discard(app, move |a| {
        then(a);
        if let Some(w) = win.upgrade() {
            w.close();
        }
    });
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("heading");
    l.set_margin_top(6);
    l
}

/// A full-width button with a title and a line under it.
fn choice(title: &str, detail: &str, icon: &str) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(22);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.add_css_class("heading");
    let d = gtk::Label::new(Some(detail));
    d.set_xalign(0.0);
    d.set_ellipsize(gtk::pango::EllipsizeMode::End);
    d.add_css_class("dim-label");
    d.add_css_class("caption");
    text.append(&t);
    text.append(&d);
    row.append(&image);
    row.append(&text);
    let b = gtk::Button::new();
    b.set_child(Some(&row));
    b.add_css_class("flat");
    b.add_css_class("start-choice");
    b
}

/// Show the start window (one at a time).
pub fn present(app: &Rc<AppState>) {
    if let Some(w) = app.start_window.borrow().as_ref().and_then(|w| w.upgrade()) {
        w.present();
        return;
    }
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Start — FaderFrame")
        .application(&app.app)
        .transient_for(&main)
        .default_width(860)
        .default_height(540)
        .build();
    let weak_win = win.downgrade();
    *app.start_window.borrow_mut() = Some(weak_win.clone());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(14);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let logo = gtk::Image::from_icon_name(crate::APP_ID);
    logo.set_pixel_size(48);
    let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let name = gtk::Label::new(Some("FaderFrame"));
    name.set_xalign(0.0);
    name.add_css_class("title-1");
    let version = gtk::Label::new(Some(&format!(
        "Version {} — what would you like to work on?",
        env!("CARGO_PKG_VERSION")
    )));
    version.set_xalign(0.0);
    version.add_css_class("dim-label");
    names.append(&name);
    names.append(&version);
    title.append(&logo);
    title.append(&names);
    body.append(&title);

    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    columns.set_vexpand(true);

    // Left: new projects, a file, the demo.
    let left = gtk::Box::new(gtk::Orientation::Vertical, 4);
    left.set_size_request(300, -1);
    left.append(&heading("Start"));
    let empty = choice("Empty Project", "One audio track", "document-new-symbolic");
    let open = choice("Open…", "A project file", "document-open-symbolic");
    let demo = choice(
        "Demo Session",
        "A song to look around in",
        "media-playback-start-symbolic",
    );
    for b in [&empty, &open, &demo] {
        left.append(b);
    }
    left.append(&heading("From a Template"));
    let template_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let default = crate::templates::default_template().map(|t| t.name);
    let all = templates::templates();
    if all.is_empty() {
        let none = gtk::Label::new(Some(
            "None yet: File → Save as Template… keeps a project's set-up.",
        ));
        none.set_wrap(true);
        none.set_xalign(0.0);
        none.add_css_class("dim-label");
        none.set_margin_start(8);
        template_list.append(&none);
    }
    for t in all {
        let is_default = default.as_deref() == Some(t.name.as_str());
        let detail = match (is_default, crate::dialogs::ago(t.saved)) {
            (true, ago) if ago.is_empty() => "New Project starts here".to_string(),
            (true, ago) => format!("New Project starts here · saved {ago}"),
            (false, ago) if ago.is_empty() => "Tracks, routing and devices".to_string(),
            (false, ago) => format!("Saved {ago}"),
        };
        let b = choice(&t.name, &detail, "view-paged-symbolic");
        let (weak, path) = (Rc::downgrade(app), t.path.clone());
        let w = weak_win.clone();
        b.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                let path = path.clone();
                go(&app, &w, move |a| crate::templates::from_template(a, &path));
            }
        });
        template_list.append(&b);
    }
    let templates_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&template_list)
        .build();
    left.append(&templates_scroll);
    let manage = gtk::Button::with_label("Manage Templates…");
    manage.add_css_class("flat");
    manage.set_halign(gtk::Align::Start);
    left.append(&manage);
    columns.append(&left);
    columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    // Right: the recent projects, newest first.
    let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    right.set_hexpand(true);
    right.append(&heading("Recent Projects"));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.add_css_class("boxed-list");
    let recent = recent_projects();
    if recent.is_empty() {
        let none = gtk::Label::new(Some("Projects you open or save are listed here."));
        none.set_margin_top(18);
        none.set_margin_bottom(18);
        none.add_css_class("dim-label");
        list.set_placeholder(Some(&none));
    }
    for r in &recent {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(7);
        row.set_margin_bottom(7);
        row.set_margin_start(10);
        row.set_margin_end(10);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
        text.set_hexpand(true);
        let n = gtk::Label::new(Some(&r.name));
        n.set_xalign(0.0);
        n.set_ellipsize(gtk::pango::EllipsizeMode::End);
        n.add_css_class("heading");
        let f = gtk::Label::new(Some(&r.folder));
        f.set_xalign(0.0);
        f.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        f.add_css_class("dim-label");
        f.add_css_class("caption");
        text.append(&n);
        text.append(&f);
        let when = gtk::Label::new(Some(&if r.there {
            crate::dialogs::ago(r.saved)
        } else {
            "missing".to_string()
        }));
        when.add_css_class("dim-label");
        row.append(&text);
        row.append(&when);
        let item = gtk::ListBoxRow::new();
        item.set_child(Some(&row));
        item.set_tooltip_text(Some(&r.path.display().to_string()));
        if !r.there {
            item.set_sensitive(false);
            item.set_tooltip_text(Some(&format!("{} is gone", r.path.display())));
        }
        list.append(&item);
    }
    let paths: Vec<PathBuf> = recent.iter().map(|r| r.path.clone()).collect();
    {
        let (weak, w) = (Rc::downgrade(app), weak_win.clone());
        list.connect_row_activated(move |_, row| {
            let (Some(app), Some(path)) = (
                weak.upgrade(),
                usize::try_from(row.index())
                    .ok()
                    .and_then(|i| paths.get(i))
                    .cloned(),
            ) else {
                return;
            };
            open_recent(&app, &w, path);
        });
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    right.append(&scroll);
    let open_selected = gtk::Button::with_label("Open");
    open_selected.add_css_class("suggested-action");
    open_selected.set_halign(gtk::Align::End);
    open_selected.set_sensitive(false);
    {
        let b = open_selected.clone();
        list.connect_row_selected(move |_, row| b.set_sensitive(row.is_some()));
        let list = list.clone();
        open_selected.connect_clicked(move |_| {
            if let Some(row) = list.selected_row() {
                row.activate();
            }
        });
    }
    right.append(&open_selected);
    columns.append(&right);
    body.append(&columns);

    // Bottom: whether it shows at start-up, and closing it.
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let at_start = gtk::CheckButton::with_label("Show this window when FaderFrame starts");
    at_start.set_active(
        StartupProject::from_id(&Preferences::load().startup_project).unwrap_or_default()
            == StartupProject::Ask,
    );
    at_start.set_hexpand(true);
    at_start.connect_toggled(|b| {
        let mut p = Preferences::load();
        let choice = if b.is_active() {
            StartupProject::Ask
        } else {
            StartupProject::Last
        };
        p.startup_project = choice.id().into();
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    });
    let close = gtk::Button::with_label("Close");
    close.set_tooltip_text(Some("Go on with the project that is open"));
    bottom.append(&at_start);
    bottom.append(&close);
    body.append(&bottom);
    win.set_child(Some(&body));

    // The buttons.
    let (weak, w) = (Rc::downgrade(app), weak_win.clone());
    empty.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            go(&app, &w, |a| {
                a.with_session(|s| s.new_project(false));
            });
        }
    });
    let (weak, w) = (Rc::downgrade(app), weak_win.clone());
    demo.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            go(&app, &w, |a| {
                a.with_session(|s| s.new_project(true));
            });
        }
    });
    let (weak, w) = (Rc::downgrade(app), weak_win.clone());
    open.connect_clicked(move |_| {
        let (Some(app), Some(win)) = (weak.upgrade(), w.upgrade()) else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Open Project")
            .modal(true)
            .filters(&crate::dialogs::project_filters())
            .build();
        let (weak, w) = (Rc::downgrade(&app), w.clone());
        dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
            if let (Ok(file), Some(app)) = (res, weak.upgrade())
                && let Some(path) = file.path()
            {
                go(&app, &w, move |a| {
                    a.with_session(|s| s.open(&path));
                });
            }
        });
    });
    let (weak, w) = (Rc::downgrade(app), weak_win.clone());
    manage.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            if let Some(w) = w.upgrade() {
                w.close();
            }
            crate::templates::window(&app);
        }
    });
    let w = weak_win.clone();
    close.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    let keys = gtk::EventControllerKey::new();
    let w = weak_win.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape
            && let Some(w) = w.upgrade()
        {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    win.present();
    // The newest project is a key press away.
    if let Some(first) = list.row_at_index(0).filter(|r| r.is_sensitive()) {
        list.select_row(Some(&first));
        first.grab_focus();
    }
}

/// Open a recent project (gone: forgotten, with a notice).
fn open_recent(app: &Rc<AppState>, win: &glib::WeakRef<gtk::Window>, path: PathBuf) {
    if !Path::new(&path).exists() {
        crate::recent::open(app, path.to_string_lossy().into_owned());
        return;
    }
    go(app, win, move |a| {
        a.with_session(|s| s.open(&path));
    });
}
