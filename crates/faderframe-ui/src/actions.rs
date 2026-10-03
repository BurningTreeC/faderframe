//! Application actions (menus, buttons, shortcuts).

use crate::state::AppState;
use faderframe_project::TrackKind;
use faderframe_session::{Action, TransportAction, WorkspaceAction};
use faderframe_workspace::{DockAreaId, ViewId};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

fn entry(
    app: &Rc<AppState>,
    name: &str,
    f: impl Fn(&Rc<AppState>) + 'static,
) -> gio::ActionEntry<gtk::Application> {
    let weak = Rc::downgrade(app);
    gio::ActionEntry::builder(name)
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                f(&app);
            }
        })
        .build()
}

fn dispatch(app: &Rc<AppState>, name: &str, action: Action) -> gio::ActionEntry<gtk::Application> {
    entry(app, name, move |a| a.dispatch(action.clone()))
}

pub fn install(app: &Rc<AppState>) {
    use Action as A;
    use TransportAction as T;
    use WorkspaceAction as W;
    let mut entries = vec![
        entry(app, "new", |a| {
            crate::dialogs::confirm_discard(a, |a| {
                a.with_session(|s| s.new_project(false));
            })
        }),
        entry(app, "new-demo", |a| {
            crate::dialogs::confirm_discard(a, |a| {
                a.with_session(|s| s.new_project(true));
            })
        }),
        entry(app, "open", |a| {
            crate::dialogs::confirm_discard(a, crate::dialogs::open_project)
        }),
        entry(app, "save", crate::dialogs::save),
        entry(app, "save-as", |a| crate::dialogs::save_as(a, None)),
        entry(app, "quit", |a| {
            if let Some(w) = a.window.borrow().as_ref() {
                w.close();
            }
        }),
        entry(app, "render", crate::render::open),
        entry(app, "preferences", |a| crate::preferences::open(a, None)),
        entry(app, "audio-settings", |a| {
            crate::preferences::open(a, Some("audio"))
        }),
        entry(app, "restart-audio", |a| a.start_audio()),
        entry(app, "about", crate::dialogs::about),
        dispatch(app, "undo", A::Undo),
        dispatch(app, "redo", A::Redo),
        dispatch(app, "delete", A::DeleteSelection),
        dispatch(app, "split", A::SplitSelectedAtPlayhead),
        dispatch(app, "toggle-snap", A::ToggleSnap),
        dispatch(app, "toggle-follow", A::ToggleFollowPlayhead),
        dispatch(app, "add-audio", A::AddTrack(TrackKind::Audio)),
        dispatch(app, "add-instrument", A::AddTrack(TrackKind::Instrument)),
        dispatch(app, "add-midi", A::AddTrack(TrackKind::Midi)),
        dispatch(app, "add-bus", A::AddTrack(TrackKind::Bus)),
        dispatch(app, "add-aux", A::AddTrack(TrackKind::Aux)),
        dispatch(app, "remove-tracks", A::RemoveSelectedTracks),
        dispatch(app, "play", A::Transport(T::TogglePlay)),
        dispatch(app, "stop", A::Transport(T::Stop)),
        dispatch(app, "to-start", A::Transport(T::ReturnToStart)),
        dispatch(app, "loop", A::Transport(T::ToggleLoop)),
        dispatch(app, "record", A::Transport(T::ToggleRecord)),
        entry(app, "panic", |a| {
            a.with_session(|s| {
                s.engine_reset_processors()?;
                Ok(())
            });
        }),
        dispatch(
            app,
            "show-mixer",
            A::Workspace(W::ShowView(ViewId::mixer())),
        ),
        dispatch(
            app,
            "show-piano-roll",
            A::Workspace(W::ShowView(ViewId::piano_roll())),
        ),
        dispatch(
            app,
            "show-automation",
            A::Workspace(W::ShowView(ViewId::automation())),
        ),
        dispatch(
            app,
            "toggle-dock",
            A::Workspace(W::ToggleArea(DockAreaId::bottom())),
        ),
        dispatch(
            app,
            "detach-mixer",
            A::Workspace(W::Detach(ViewId::mixer())),
        ),
        dispatch(
            app,
            "detach-piano-roll",
            A::Workspace(W::Detach(ViewId::piano_roll())),
        ),
        entry(app, "dock-all", |a| {
            let ids: Vec<_> = a
                .session
                .borrow()
                .workspace()
                .active_layout()
                .floating
                .iter()
                .map(|f| f.id)
                .collect();
            for id in ids {
                a.dispatch(Action::Workspace(WorkspaceAction::CloseWindow(id)));
            }
        }),
        dispatch(app, "workspace-reset", A::Workspace(W::ResetActive)),
    ];
    for i in 0..5 {
        entries.push(dispatch(
            app,
            &format!("workspace-{}", i + 1),
            A::Workspace(W::Switch(i)),
        ));
    }
    app.app.add_action_entries(entries);

    let accels: &[(&str, &[&str])] = &[
        ("app.new", &["<Control>n"]),
        ("app.open", &["<Control>o"]),
        ("app.save", &["<Control>s"]),
        ("app.save-as", &["<Control><Shift>s"]),
        ("app.quit", &["<Control>q"]),
        ("app.render", &["<Control><Shift>r"]),
        ("app.preferences", &["<Control>comma"]),
        ("app.undo", &["<Control>z"]),
        ("app.redo", &["<Control><Shift>z", "<Control>y"]),
        ("app.add-audio", &["<Control>t"]),
        ("app.add-instrument", &["<Control><Shift>t"]),
        ("app.toggle-dock", &["F2"]),
        ("app.show-mixer", &["F3"]),
        ("app.show-piano-roll", &["F4"]),
        ("app.workspace-1", &["<Control>1"]),
        ("app.workspace-2", &["<Control>2"]),
        ("app.workspace-3", &["<Control>3"]),
        ("app.workspace-4", &["<Control>4"]),
        ("app.workspace-5", &["<Control>5"]),
    ];
    for (action, keys) in accels {
        app.app.set_accels_for_action(action, keys);
    }
}

/// Single-key transport shortcuts, handled in the bubble phase so focused
/// text fields always get their keys first.
pub fn install_window_keys(app: &Rc<AppState>, window: &impl IsA<gtk::Widget>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let weak = Rc::downgrade(app);
    keys.connect_key_pressed(move |_, key, _, state| {
        let Some(app) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let mods = state
            & (gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK);
        if !mods.is_empty() {
            return glib::Propagation::Proceed;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let action = match key {
            gdk::Key::space => TransportAction::TogglePlay,
            gdk::Key::Home => TransportAction::ReturnToStart,
            gdk::Key::l | gdk::Key::L if !shift => TransportAction::ToggleLoop,
            gdk::Key::R if shift => TransportAction::ToggleRecord,
            gdk::Key::KP_0 | gdk::Key::KP_Insert => TransportAction::Stop,
            _ => return glib::Propagation::Proceed,
        };
        app.dispatch(Action::Transport(action));
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}
