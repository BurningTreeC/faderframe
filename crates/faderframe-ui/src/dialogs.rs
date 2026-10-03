//! Native file dialogs, confirmations and the about dialog.

use crate::state::AppState;
use faderframe_project::file::FILE_EXTENSION;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

fn project_filters() -> gio::ListStore {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("FaderFrame projects"));
    filter.add_pattern(&format!("*.{FILE_EXTENSION}"));
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&filter);
    store.append(&all);
    store
}

pub fn open_project(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Open Project")
        .modal(true)
        .filters(&project_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            app.with_session(|s| s.open(&path));
        }
    });
}

/// Save to the current file, or ask for one.
pub fn save(app: &Rc<AppState>) {
    let has_path = app.session.borrow().path().is_some();
    if has_path {
        app.with_session(|s| s.save());
    } else {
        save_as(app, None);
    }
}

/// Continuation run after a successful "save as".
pub type AfterSave = Box<dyn FnOnce(&Rc<AppState>)>;

/// Ask for a file name and save; run `then` after a successful save.
pub fn save_as(app: &Rc<AppState>, then: Option<AfterSave>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = format!("{}.{FILE_EXTENSION}", app.session.borrow().project().name);
    let dialog = gtk::FileDialog::builder()
        .title("Save Project")
        .modal(true)
        .initial_name(name)
        .filters(&project_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        if app.with_session(|s| s.save_as(&path)).is_some()
            && let Some(then) = then
        {
            then(&app);
        }
    });
}

/// Run `next` immediately, or after asking what to do with unsaved changes.
pub fn confirm_discard(app: &Rc<AppState>, next: impl Fn(&Rc<AppState>) + 'static) {
    if !app.session.borrow().is_dirty() {
        next(app);
        return;
    }
    let Some(win) = app.window.borrow().clone() else {
        next(app);
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message("Save changes to the current project?")
        .detail("Unsaved changes will be lost otherwise.")
        .buttons(["Cancel", "Discard", "Save"])
        .cancel_button(0)
        .default_button(2)
        .modal(true)
        .build();
    let weak = Rc::downgrade(app);
    let next = Rc::new(next);
    dialog.choose(Some(&win), gio::Cancellable::NONE, move |res| {
        let Some(app) = weak.upgrade() else { return };
        match res {
            Ok(1) => next(&app),
            Ok(2) => {
                let has_path = app.session.borrow().path().is_some();
                if has_path {
                    if app.with_session(|s| s.save()).is_some() {
                        next(&app);
                    }
                } else {
                    let next = Rc::clone(&next);
                    save_as(&app, Some(Box::new(move |a: &Rc<AppState>| next(a))));
                }
            }
            _ => {}
        }
    });
}

pub fn about(app: &Rc<AppState>) {
    let dialog = gtk::AboutDialog::builder()
        .program_name("FaderFrame")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A multitrack recording, editing and mixing environment.\nLinux first · Wayland native · GTK 4")
        .website("https://github.com/BurningTreeC/faderframe")
        .website_label("github.com/BurningTreeC/faderframe")
        .license_type(gtk::License::MitX11)
        .authors(["BurningTreeC and the FaderFrame contributors"])
        .modal(true)
        .build();
    if let Some(win) = app.window.borrow().as_ref() {
        dialog.set_transient_for(Some(win));
    }
    dialog.present();
}

/// Ask before closing the main window with unsaved changes.
pub fn install_close_guard(app: &Rc<AppState>, window: &gtk::ApplicationWindow) {
    let weak = Rc::downgrade(app);
    let confirmed = Rc::new(std::cell::Cell::new(false));
    window.connect_close_request(move |w| {
        let Some(app) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if confirmed.get() || !app.session.borrow().is_dirty() {
            app.app.quit();
            return glib::Propagation::Proceed;
        }
        let confirmed = Rc::clone(&confirmed);
        let w = w.clone();
        confirm_discard(&app, move |a| {
            confirmed.set(true);
            a.session.borrow_mut().stop_audio();
            w.close();
        });
        glib::Propagation::Stop
    });
}
