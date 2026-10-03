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
        entry(app, "import-audio", crate::dialogs::import_audio),
        entry(app, "cancel-import", |a| {
            a.session.borrow().cancel_imports()
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
        // Development aid: render the main window into the PNG named by
        // $FADERFRAME_SCREENSHOT (the app's own pixels only).
        entry(app, "screenshot", |a| {
            let Ok(pattern) = std::env::var("FADERFRAME_SCREENSHOT") else {
                tracing::warn!("screenshot: set FADERFRAME_SCREENSHOT to a .png path");
                return;
            };
            // "{n}" in the path numbers successive screenshots.
            static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = pattern.replace("{n}", &n.to_string());
            let canvases: Vec<_> = {
                let dock = a.dock.borrow();
                let mut hosts: Vec<_> = dock.hosts.values().collect();
                // Arranger first, then the other editors.
                hosts.sort_by_key(|(kind, _)| {
                    !matches!(kind, faderframe_workspace::ViewKind::Arranger)
                });
                hosts
                    .into_iter()
                    .filter(|(_, h)| h.canvas.is_mapped())
                    .map(|(_, h)| h.canvas.clone())
                    .collect()
            };
            if let Some(w) = a.window.borrow().as_ref() {
                crate::screenshot::capture(w, &canvases, std::path::Path::new(&path));
            }
        }),
        dispatch(app, "undo", A::Undo),
        dispatch(app, "redo", A::Redo),
        dispatch(app, "delete", A::DeleteSelection),
        dispatch(app, "split", A::SplitSelectedAtPlayhead),
        dispatch(app, "toggle-snap", A::ToggleSnap),
        dispatch(app, "toggle-follow", A::ToggleFollowPlayhead),
        dispatch(app, "add-audio", A::AddTrack(TrackKind::Audio)),
        dispatch(
            app,
            "add-audio-stereo",
            A::AddTrackWithLayout(TrackKind::Audio, faderframe_core::ChannelLayout::Stereo),
        ),
        dispatch(app, "add-instrument", A::AddTrack(TrackKind::Instrument)),
        dispatch(app, "add-midi", A::AddTrack(TrackKind::Midi)),
        dispatch(app, "add-bus", A::AddTrack(TrackKind::Bus)),
        dispatch(app, "add-aux", A::AddTrack(TrackKind::Aux)),
        dispatch(app, "remove-tracks", A::RemoveSelectedTracks),
        dispatch(app, "arm-selected", A::ToggleArmSelected),
        entry(app, "save-track-preset", |a| {
            let tracks: Vec<_> = a
                .session
                .borrow()
                .selection
                .tracks
                .iter()
                .copied()
                .collect();
            if tracks.is_empty() {
                a.report(
                    faderframe_session::SessionError::Other(
                        "select a track to save as a preset".into(),
                    ),
                    false,
                );
            }
            for track in tracks {
                a.dispatch(Action::SaveTrackPreset { track });
            }
        }),
        entry(app, "track-from-preset", |a| {
            crate::dialogs::track_preset(a, false)
        }),
        entry(app, "apply-track-preset", |a| {
            crate::dialogs::track_preset(a, true)
        }),
        entry(
            app,
            "export-track-preset",
            crate::dialogs::export_track_preset,
        ),
        entry(app, "toggle-automation", |a| {
            let tracks: Vec<_> = {
                let s = a.session.borrow();
                let sel: Vec<_> = s.selection.tracks.iter().copied().collect();
                if sel.is_empty() {
                    s.project()
                        .tracks
                        .iter()
                        .filter(|t| t.automation.lanes.iter().any(|l| !l.curve.is_empty()))
                        .map(|t| t.id)
                        .collect()
                } else {
                    sel
                }
            };
            for t in tracks {
                a.dispatch(Action::ToggleTrackAutomation(t));
            }
        }),
        entry(app, "toggle-take-lanes", |a| {
            let folders: Vec<_> = {
                let s = a.session.borrow();
                s.selection
                    .clips
                    .iter()
                    .copied()
                    .filter(|c| s.project().clip(*c).is_some_and(|c| c.as_takes().is_some()))
                    .collect()
            };
            for c in folders {
                a.dispatch(Action::ToggleTakeLanes(c));
            }
        }),
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
    for (i, (_, h)) in faderframe_view_arranger::TRACK_HEIGHTS.iter().enumerate() {
        entries.push(dispatch(
            app,
            &format!("track-height-{i}"),
            A::SetTrackHeight {
                track: None,
                height: *h,
            },
        ));
    }
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
        ("app.import-audio", &["<Control>i"]),
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
            gdk::Key::t | gdk::Key::T if !shift => {
                app.app.activate_action("toggle-take-lanes", None);
                return glib::Propagation::Stop;
            }
            gdk::Key::a | gdk::Key::A if !shift => {
                app.app.activate_action("toggle-automation", None);
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Proceed,
        };
        app.dispatch(Action::Transport(action));
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}
