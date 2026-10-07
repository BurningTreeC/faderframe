//! Project templates in the shell: File → Save as Template… (a name, asking
//! before one is replaced), File → New from Template… (the templates: a new
//! project from one, the one New Project starts from, delete), and New
//! Project itself, which starts from the default template when there is one.

use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::templates::{self, ProjectTemplate};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::Path;
use std::rc::Rc;

/// The template New Project starts from, if it is set and still there.
pub fn default_template() -> Option<ProjectTemplate> {
    let name = crate::prefs::Preferences::load().default_template?;
    templates::templates()
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(&name))
}

fn set_default(name: Option<&str>) {
    let mut p = crate::prefs::Preferences::load();
    p.default_template = name.map(str::to_string);
    if let Err(e) = p.save() {
        tracing::warn!("could not save the preferences: {e}");
    }
}

/// New Project: from the default template, or the empty project.
pub fn new_project(app: &Rc<AppState>) {
    match default_template() {
        Some(t) => from_template(app, &t.path),
        None => {
            app.with_session(|s| s.new_project(false));
        }
    }
}

fn from_template(app: &Rc<AppState>, path: &Path) {
    app.with_session(|s| s.new_from_template(path));
}

/// Ask for the template's name (offering the project's), then save it.
pub fn save_prompt(app: &Rc<AppState>) {
    let initial = {
        let s = app.session.borrow();
        s.path()
            .and_then(|p| p.file_stem())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "My Template".into())
    };
    crate::dialogs::name_prompt_checked(
        app,
        "Save as Template",
        "Keep this project's tracks, routing, devices and settings, without its clips, as the template:",
        &initial,
        "Save Template",
        |_, name| {
            templates::existing_template(name).map(|t| {
                (
                    format!("Replace the template ‘{}’?", t.name),
                    "It is saved again from this project.".to_string(),
                )
            })
        },
        |name| Action::SaveTemplate { name },
    );
}

/// What the list shows, to see when it changed.
fn signature(list: &[ProjectTemplate]) -> Vec<(String, Option<std::time::SystemTime>)> {
    list.iter().map(|t| (t.name.clone(), t.saved)).collect()
}

/// The templates: a new project from one, the default for New Project,
/// delete; save the current project as one; any project file's set-up.
pub fn window(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Project Templates")
        .application(&app.app)
        .transient_for(&main)
        .default_width(620)
        .default_height(460)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let intro = gtk::Label::new(Some(
        "Projects set up the way you start: tracks, routing, devices, mappings and settings, without clips. The default one is where New Project starts.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.set_hexpand(true);
    intro.add_css_class("dim-label");
    let save = gtk::Button::with_label("Save Current as Template…");
    save.add_css_class("suggested-action");
    save.set_valign(gtk::Align::Center);
    top.append(&intro);
    top.append(&save);
    body.append(&top);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    body.append(&scroll);
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let from_file = gtk::Button::with_label("From a Project File…");
    from_file.set_tooltip_text(Some(
        "A new project set up like a project file, without its clips",
    ));
    let folder = gtk::Button::with_label("Open Templates Folder");
    let empty = gtk::Button::with_label("New Empty Project");
    empty.set_tooltip_text(Some("A new project with one audio track"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bottom.append(&from_file);
    bottom.append(&folder);
    bottom.append(&spacer);
    bottom.append(&empty);
    body.append(&bottom);
    win.set_child(Some(&body));

    // The rows, rebuilt when the templates or the default change.
    let shown = Rc::new(std::cell::RefCell::new(None));
    let fill: Rc<dyn Fn()> = {
        let (weak, list, win, shown) = (
            Rc::downgrade(app),
            list.clone(),
            win.downgrade(),
            Rc::clone(&shown),
        );
        Rc::new(move || {
            let (Some(app), Some(win)) = (weak.upgrade(), win.upgrade()) else {
                return;
            };
            let all = templates::templates();
            let default = default_template().map(|t| t.name);
            *shown.borrow_mut() = Some((signature(&all), default.clone()));
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            if all.is_empty() {
                let none = gtk::Label::new(Some(
                    "No templates yet: Save Current as Template keeps this project's set-up.",
                ));
                none.set_margin_top(18);
                none.set_margin_bottom(18);
                none.add_css_class("dim-label");
                list.append(&none);
            }
            for t in all {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.set_margin_top(6);
                row.set_margin_bottom(6);
                row.set_margin_start(10);
                row.set_margin_end(10);
                let name = gtk::Label::new(Some(&t.name));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name.add_css_class("heading");
                let when = gtk::Label::new(Some(&crate::dialogs::ago(t.saved)));
                when.add_css_class("dim-label");
                let is_default = default.as_deref() == Some(t.name.as_str());
                let star = gtk::CheckButton::with_label("Default");
                star.set_active(is_default);
                star.set_tooltip_text(Some("New Project starts from this template"));
                let delete = gtk::Button::from_icon_name("user-trash-symbolic");
                delete.add_css_class("flat");
                delete.set_tooltip_text(Some("Delete the template"));
                let open = gtk::Button::with_label("New Project");
                open.add_css_class("suggested-action");
                for w in [
                    &name.upcast::<gtk::Widget>(),
                    &when.upcast(),
                    &star.clone().upcast(),
                    &delete.clone().upcast(),
                    &open.clone().upcast(),
                ] {
                    row.append(w);
                }
                let (weak2, path) = (Rc::downgrade(&app), t.path.clone());
                let w = win.downgrade();
                open.connect_clicked(move |_| {
                    let Some(app) = weak2.upgrade() else { return };
                    let path = path.clone();
                    let w = w.clone();
                    crate::dialogs::confirm_discard(&app, move |a| {
                        from_template(a, &path);
                        if let Some(w) = w.upgrade() {
                            w.close();
                        }
                    });
                });
                let tname = t.name.clone();
                star.connect_toggled(move |b| {
                    set_default(b.is_active().then_some(tname.as_str()));
                });
                let (weak2, path, tname) = (Rc::downgrade(&app), t.path.clone(), t.name.clone());
                let w = win.downgrade();
                delete.connect_clicked(move |_| {
                    let (Some(app), Some(w)) = (weak2.upgrade(), w.upgrade()) else {
                        return;
                    };
                    let dialog = gtk::AlertDialog::builder()
                        .message(format!("Delete the template ‘{tname}’?"))
                        .detail("Its folder and the samples in it are deleted.")
                        .buttons(["Cancel", "Delete"])
                        .cancel_button(0)
                        .default_button(0)
                        .modal(true)
                        .build();
                    let (weak3, path, tname) = (Rc::downgrade(&app), path.clone(), tname.clone());
                    dialog.choose(Some(&w), gio::Cancellable::NONE, move |res| {
                        if res != Ok(1) {
                            return;
                        }
                        if default_template().is_some_and(|d| d.name == tname) {
                            set_default(None);
                        }
                        if let Some(app) = weak3.upgrade() {
                            app.dispatch(Action::DeleteTemplate(path.clone()));
                        }
                    });
                });
                list.append(&row);
            }
        })
    };
    fill();
    let weak = Rc::downgrade(app);
    save.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::PromptSaveTemplate);
        }
    });
    let (weak, w) = (Rc::downgrade(app), win.downgrade());
    from_file.connect_clicked(move |_| {
        let (Some(app), Some(w)) = (weak.upgrade(), w.upgrade()) else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("New Project from a Project File")
            .modal(true)
            .filters(&crate::dialogs::project_filters())
            .build();
        let (weak, w2) = (Rc::downgrade(&app), w.downgrade());
        dialog.open(Some(&w), gio::Cancellable::NONE, move |res| {
            let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
                return;
            };
            let Some(path) = file.path() else { return };
            let w2 = w2.clone();
            crate::dialogs::confirm_discard(&app, move |a| {
                from_template(a, &path);
                if let Some(w) = w2.upgrade() {
                    w.close();
                }
            });
        });
    });
    let w = win.downgrade();
    folder.connect_clicked(move |_| {
        let dir = templates::templates_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("could not make {}: {e}", dir.display());
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
        launcher.launch(w.upgrade().as_ref(), gio::Cancellable::NONE, |res| {
            if let Err(e) = res {
                tracing::warn!("could not open the templates folder: {e}");
            }
        });
    });
    let (weak, w) = (Rc::downgrade(app), win.downgrade());
    empty.connect_clicked(move |_| {
        let Some(app) = weak.upgrade() else { return };
        let w = w.clone();
        crate::dialogs::confirm_discard(&app, move |a| {
            a.with_session(|s| s.new_project(false));
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
    });
    // Templates saved or deleted (here or anywhere) show up.
    let (w, shown) = (win.downgrade(), Rc::clone(&shown));
    glib::timeout_add_local(std::time::Duration::from_millis(700), move || {
        if w.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        let now = (
            signature(&templates::templates()),
            default_template().map(|t| t.name),
        );
        if shown.borrow().as_ref() != Some(&now) {
            fill();
        }
        glib::ControlFlow::Continue
    });
    win.present();
}
