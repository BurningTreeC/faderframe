//! Video in the shell: the import and export choosers (File menu, the video
//! window's menu) and full screen -- the view in a window of its own,
//! detached first when it sits in the main window; Escape or a double-click
//! leaves it.

use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::video::VideoOp;
use faderframe_video::mux::Container;
use faderframe_workspace::ViewId;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

/// Files offered for import.
const PATTERNS: [&str; 10] = [
    "*.mov", "*.mp4", "*.m4v", "*.mkv", "*.webm", "*.avi", "*.mxf", "*.mts", "*.m2ts", "*.mpg",
];

fn filters(name: &str, patterns: &[String]) -> gio::ListStore {
    let list = gio::ListStore::new::<gtk::FileFilter>();
    let f = gtk::FileFilter::new();
    f.set_name(Some(name));
    for p in patterns {
        f.add_pattern(p);
        f.add_pattern(&p.to_uppercase());
    }
    list.append(&f);
    list
}

/// Choose a video and import it with its sound.
pub fn import(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let patterns: Vec<String> = PATTERNS.iter().map(|s| s.to_string()).collect();
    let dialog = gtk::FileDialog::builder()
        .title("Import Video")
        .accept_label("Import")
        .modal(true)
        .filters(&filters("Video", &patterns))
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        if let (Ok(file), Some(app)) = (res, weak.upgrade())
            && let Some(path) = file.path()
        {
            app.dispatch(Action::Video(VideoOp::Import { path, sound: true }));
        }
    });
}

/// Conform to a new cut: choose the old cut list, then the new one (EDL
/// or OpenTimelineIO).
pub fn conform_lists(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let lists = ["*.edl", "*.otio"].map(String::from);
    let chooser = |title: &str| {
        gtk::FileDialog::builder()
            .title(title)
            .accept_label("Choose")
            .modal(true)
            .filters(&filters("Cut lists (EDL, OpenTimelineIO)", &lists))
            .build()
    };
    let new_dialog = chooser("The New Cut (EDL or OpenTimelineIO)");
    let weak = Rc::downgrade(app);
    let w2 = win.clone();
    chooser("The Old Cut — the One the Sound Follows Now").open(
        Some(&win),
        gio::Cancellable::NONE,
        move |res| {
            let Some(old) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            new_dialog.open(Some(&w2), gio::Cancellable::NONE, move |res| {
                if let (Some(new), Some(app)) = (res.ok().and_then(|f| f.path()), weak.upgrade()) {
                    app.dispatch(Action::Conform(
                        faderframe_session::conform::ConformOp::Lists { old, new },
                    ));
                }
            });
        },
    );
}

/// Choose where the movie goes (the container by its extension).
pub fn export(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = {
        let s = app.session.borrow();
        s.path()
            .and_then(|p| p.file_stem())
            .map_or_else(|| "Movie".into(), |n| n.to_string_lossy().into_owned())
    };
    let list = gio::ListStore::new::<gtk::FileFilter>();
    for c in Container::ALL {
        let f = gtk::FileFilter::new();
        f.set_name(Some(c.label()));
        f.add_pattern(&format!("*.{}", c.extension()));
        list.append(&f);
    }
    let dialog = gtk::FileDialog::builder()
        .title("Export Movie (the picture is copied untouched)")
        .accept_label("Export")
        .modal(true)
        .initial_name(format!("{name}.mov"))
        .filters(&list)
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(mut path) = file.path() else { return };
        let container = Container::for_path(&path).unwrap_or_else(|| {
            path.set_extension("mov");
            Container::Mov
        });
        app.dispatch(Action::Video(VideoOp::Export {
            clip: None,
            path,
            container,
        }));
    });
}

/// The window holding `view`'s host, if shown.
fn window_of(app: &Rc<AppState>, view: &ViewId) -> Option<gtk::Window> {
    let dock = app.dock.borrow();
    let (_, host) = dock.hosts.get(view)?;
    host.root.root().and_downcast::<gtk::Window>()
}

/// Show `view` full screen in a window of its own, or leave full screen.
pub fn full_screen(app: &Rc<AppState>, view: ViewId) {
    let main = app
        .window
        .borrow()
        .clone()
        .map(|w| w.upcast::<gtk::Window>());
    match window_of(app, &view) {
        Some(w) if Some(&w) != main.as_ref() => {
            if w.is_fullscreen() {
                w.unfullscreen();
            } else {
                leave_on_escape(&w);
                w.fullscreen();
            }
        }
        // Docked in the main window (or not shown): detach it, then make
        // its window full screen once it is there.
        _ => {
            app.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Detach(view.clone()),
            ));
            let weak = Rc::downgrade(app);
            let main = main.clone();
            let mut tries = 0;
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                let Some(app) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                tries += 1;
                match window_of(&app, &view) {
                    Some(w) if Some(&w) != main.as_ref() => {
                        leave_on_escape(&w);
                        w.fullscreen();
                        glib::ControlFlow::Break
                    }
                    _ if tries > 40 => glib::ControlFlow::Break,
                    _ => glib::ControlFlow::Continue,
                }
            });
        }
    }
}

/// Escape leaves full screen (and F11 toggles it) in a video window.
fn leave_on_escape(w: &gtk::Window) {
    // One controller per window.
    if w.widget_name() == "faderframe-video-window" {
        return;
    }
    w.set_widget_name("faderframe-video-window");
    let keys = gtk::EventControllerKey::new();
    let win = w.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(w) = win.upgrade() else {
            return glib::Propagation::Proceed;
        };
        match key {
            gtk::gdk::Key::Escape if w.is_fullscreen() => {
                w.unfullscreen();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::F11 => {
                if w.is_fullscreen() {
                    w.unfullscreen();
                } else {
                    w.fullscreen();
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    w.add_controller(keys);
}
