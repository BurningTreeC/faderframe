//! File → Import/Export of DAWproject files and MusicXML scores
//! (`faderframe_session::{dawproject, musicxml}`): the choosers, the
//! rest is the session's.

use crate::state::AppState;
use faderframe_session::Action;
use gtk::gio;
use gtk::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;

fn filters(name: &str, suffixes: &[&str]) -> gio::ListStore {
    let f = gtk::FileFilter::new();
    f.set_name(Some(name));
    for s in suffixes {
        f.add_suffix(s);
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&f);
    store.append(&all);
    store
}

/// File → Import DAWproject…: the file opens as a new project (unsaved
/// changes were asked about first).
pub fn import_dawproject(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import DAWproject")
        .accept_label("Import")
        .modal(true)
        .filters(&filters("DAWproject files", &["dawproject"]))
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            app.dispatch(Action::ImportDawProject(path));
        }
    });
}

/// File → Export DAWproject…: the project, its media and plug-in states
/// in one file other programs open.
pub fn export_dawproject(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = format!("{}.dawproject", app.session.borrow().project().name);
    let dialog = gtk::FileDialog::builder()
        .title("Export DAWproject")
        .modal(true)
        .initial_name(name)
        .filters(&filters("DAWproject files", &["dawproject"]))
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        app.dispatch_chosen(Action::ExportDawProject(path.clone()), &path);
    });
}

/// File → Import MusicXML Score…: a track a part at the playhead; into a
/// project without clips at bar 1 with the score's tempo and metre.
pub fn import_musicxml(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import MusicXML Score")
        .accept_label("Import")
        .modal(true)
        .filters(&filters("MusicXML scores", &["musicxml", "mxl", "xml"]))
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            import_musicxml_from(&app, path);
        }
    });
}

/// Import the score at `path` as the menu does.
pub fn import_musicxml_from(app: &Rc<AppState>, path: PathBuf) {
    let (empty, playhead) = {
        let s = app.session.borrow();
        (s.project().clips.is_empty(), s.playhead())
    };
    let at = if empty {
        faderframe_timeline::MusicalTime::ZERO
    } else {
        playhead
    };
    app.dispatch(Action::ImportMusicXml {
        path,
        at,
        tempo: empty,
    });
}
