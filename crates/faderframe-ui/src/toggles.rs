//! The menus' toggles and choices with their state (issue #6): every
//! command that switches something on and off is a stateful GIO action
//! (boolean state, no parameter), every one-of-several choice a string
//! state with the choice as the menu item's target — GTK's menus then
//! draw the check or the radio dot themselves, and give screen readers
//! check-box and radio menu items with their state.
//!
//! One table ([`TOGGLES`], [`CHOICES`]) names each action, reads its state
//! from the session and says what a press does; [`follow`] brings every
//! state in line with the session after each change and with the tick,
//! whatever changed it (a menu, a toolbar button, a shortcut, the command
//! palette, a control surface, MIDI learn, the Preferences window, which
//! binds its check boxes to the same actions). A new toggle is one row
//! here and a menu item naming its action.

use crate::state::AppState;
use faderframe_engine::MetronomeMode;
use faderframe_session::{Action, EditFlag, EditMode, EditTool, Session, TransportAction};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

/// A toggle: its action's name, its state now, what a press does
/// (`None`: the action lives with its setting elsewhere; only its state
/// is kept in step here).
pub(crate) struct Toggle {
    pub name: &'static str,
    pub on: fn(&Session) -> bool,
    pub press: Option<fn(&Rc<AppState>)>,
}

/// A choice among a few: its action's name, the chosen id now, what
/// choosing an id does.
pub(crate) struct Choice {
    pub name: &'static str,
    pub now: fn(&Session) -> &'static str,
    pub choose: fn(&Rc<AppState>, &str),
}

fn persist(update: impl FnOnce(&mut crate::prefs::Preferences)) {
    let mut p = crate::prefs::Preferences::load();
    update(&mut p);
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

fn flag(a: &Rc<AppState>, flag: EditFlag, on: fn(&Session) -> bool) {
    let now = on(&a.session.borrow());
    a.dispatch(Action::SetEditFlag(flag, !now));
}

/// The edit flags the Edit menu's Options list (the edit toolbar shows
/// them too).
pub(crate) const FLAG_ITEMS: [(&str, &str); 8] = [
    ("Link Timeline and Edit Selection", "app.flag-link"),
    ("Insertion Follows Playback", "app.flag-insertion-follows"),
    ("Snap Free Edits to Samples", "app.flag-samples"),
    ("Cut at Zero Crossings", "app.flag-zero-crossings"),
    ("Tab to Transients", "app.flag-tab-transients"),
    ("Show Transients", "app.flag-transients"),
    ("Warp View", "app.flag-warp"),
    ("Polyphonic Pitch Editing", "app.flag-polyphonic-pitch"),
];

pub(crate) const TOGGLES: &[Toggle] = &[
    Toggle {
        name: "toggle-snap",
        on: |s| s.editor.snap,
        press: Some(|a| {
            a.dispatch(Action::ToggleSnap);
            let on = a.session.borrow().editor.snap;
            persist(|p| p.snap = on);
        }),
    },
    Toggle {
        name: "toggle-snap-objects",
        on: |s| s.editor.snap_objects,
        // Remembered by the dispatch (`EditFlag::SnapToObjects`).
        press: Some(|a| flag(a, EditFlag::SnapToObjects, |s| s.editor.snap_objects)),
    },
    Toggle {
        name: "toggle-follow",
        on: |s| s.editor.follow_playhead,
        press: Some(|a| {
            a.dispatch(Action::ToggleFollowPlayhead);
            let on = a.session.borrow().editor.follow_playhead;
            persist(|p| p.follow_playhead = on);
        }),
    },
    Toggle {
        name: "toggle-edit-toolbar",
        on: |s| s.editor.show_edit_toolbar,
        press: Some(|a| {
            let on = !a.session.borrow().editor.show_edit_toolbar;
            a.dispatch(Action::SetEditFlag(EditFlag::EditToolbar, on));
            persist(|p| p.show_edit_toolbar = on);
        }),
    },
    Toggle {
        name: "loop",
        on: |s| s.project().loop_enabled,
        press: Some(|a| a.dispatch(Action::Transport(TransportAction::ToggleLoop))),
    },
    Toggle {
        name: "record",
        on: |s| s.transport().recording,
        press: Some(|a| a.dispatch(Action::Transport(TransportAction::ToggleRecord))),
    },
    Toggle {
        name: "punch",
        on: |s| s.project().punch_enabled,
        press: Some(|a| a.dispatch(Action::Transport(TransportAction::TogglePunch))),
    },
    Toggle {
        name: "input-quantize",
        on: |s| s.record.input_quantize,
        press: Some(|a| a.dispatch(Action::Transport(TransportAction::ToggleInputQuantize))),
    },
    Toggle {
        name: "toggle-metronome",
        on: |s| s.record.metronome != MetronomeMode::Off,
        press: Some(crate::recording::toggle_metronome),
    },
    Toggle {
        name: "arm-selected",
        on: |s| {
            let mut armable = s
                .selection
                .tracks
                .iter()
                .filter_map(|t| s.project().track(*t))
                .filter(|t| t.kind.has_clips())
                .peekable();
            armable.peek().is_some() && armable.all(|t| t.record_arm)
        },
        press: Some(|a| a.dispatch(Action::ToggleArmSelected)),
    },
    Toggle {
        name: "flag-link",
        on: |s| s.editor.link_timeline,
        press: Some(|a| flag(a, EditFlag::LinkTimeline, |s| s.editor.link_timeline)),
    },
    Toggle {
        name: "flag-insertion-follows",
        on: |s| s.editor.insertion_follows_playback,
        press: Some(|a| {
            flag(a, EditFlag::InsertionFollowsPlayback, |s| {
                s.editor.insertion_follows_playback
            })
        }),
    },
    Toggle {
        name: "flag-samples",
        on: |s| s.editor.snap_samples,
        press: Some(|a| flag(a, EditFlag::SnapToSamples, |s| s.editor.snap_samples)),
    },
    Toggle {
        name: "flag-zero-crossings",
        on: |s| s.editor.zero_crossings,
        press: Some(|a| {
            flag(a, EditFlag::SnapToZeroCrossings, |s| {
                s.editor.zero_crossings
            })
        }),
    },
    Toggle {
        name: "flag-tab-transients",
        on: |s| s.editor.tab_to_transients,
        press: Some(|a| flag(a, EditFlag::TabToTransients, |s| s.editor.tab_to_transients)),
    },
    Toggle {
        name: "flag-transients",
        on: |s| s.editor.show_transients,
        press: Some(|a| flag(a, EditFlag::ShowTransients, |s| s.editor.show_transients)),
    },
    Toggle {
        name: "flag-warp",
        on: |s| s.editor.warp,
        press: Some(|a| flag(a, EditFlag::Warp, |s| s.editor.warp)),
    },
    Toggle {
        name: "flag-polyphonic-pitch",
        on: |s| s.editor.pitch_polyphonic,
        press: Some(|a| flag(a, EditFlag::PolyphonicPitch, |s| s.editor.pitch_polyphonic)),
    },
    // The active workspace (View › Workspace; Ctrl+1 … Ctrl+5).
    Toggle {
        name: "workspace-1",
        on: |s| s.workspace().active == 0,
        press: Some(|a| {
            a.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Switch(0),
            ))
        }),
    },
    Toggle {
        name: "workspace-2",
        on: |s| s.workspace().active == 1,
        press: Some(|a| {
            a.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Switch(1),
            ))
        }),
    },
    Toggle {
        name: "workspace-3",
        on: |s| s.workspace().active == 2,
        press: Some(|a| {
            a.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Switch(2),
            ))
        }),
    },
    Toggle {
        name: "workspace-4",
        on: |s| s.workspace().active == 3,
        press: Some(|a| {
            a.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Switch(3),
            ))
        }),
    },
    Toggle {
        name: "workspace-5",
        on: |s| s.workspace().active == 4,
        press: Some(|a| {
            a.dispatch(Action::Workspace(
                faderframe_session::WorkspaceAction::Switch(4),
            ))
        }),
    },
    // Defined with their settings (recording.rs, the dock), kept in step
    // here whatever changed them.
    Toggle {
        name: "stop-to-start",
        on: |s| s.editor.stop_to_start,
        press: None,
    },
    Toggle {
        name: "preroll-on-bar",
        on: |s| s.record.preroll_on_bar,
        press: None,
    },
    Toggle {
        name: "count-in",
        on: |s| s.record.count_in,
        press: None,
    },
    Toggle {
        name: "master-panel",
        on: |s| s.master_panel(),
        press: None,
    },
];

pub(crate) fn mode_id(m: EditMode) -> &'static str {
    match m {
        EditMode::Shuffle => "shuffle",
        EditMode::Slip => "slip",
        EditMode::Spot => "spot",
        EditMode::Grid => "grid",
    }
}

pub(crate) fn tool_id(t: EditTool) -> &'static str {
    match t {
        EditTool::Smart => "smart",
        EditTool::Trim => "trim",
        EditTool::TrimStretch => "stretch",
        EditTool::Select => "select",
        EditTool::Grab => "grab",
        EditTool::GrabSeparation => "separate",
        EditTool::Scrub => "scrub",
        EditTool::Pencil => "pencil",
        EditTool::Zoom => "zoom",
    }
}

const MODES: [EditMode; 4] = [
    EditMode::Shuffle,
    EditMode::Slip,
    EditMode::Spot,
    EditMode::Grid,
];

const TOOLS: [EditTool; 9] = [
    EditTool::Smart,
    EditTool::Trim,
    EditTool::TrimStretch,
    EditTool::Select,
    EditTool::Grab,
    EditTool::GrabSeparation,
    EditTool::Scrub,
    EditTool::Pencil,
    EditTool::Zoom,
];

/// Edit › Edit Mode and Edit Tool (`edit-mode:<id>`, `edit-tool:<id>` in
/// scripts; Alt+1 … Alt+0, Alt+S).
pub(crate) const CHOICES: &[Choice] = &[
    Choice {
        name: "edit-mode",
        now: |s| mode_id(s.editor.edit_mode),
        choose: |a, id| match MODES.into_iter().find(|m| mode_id(*m) == id) {
            Some(m) => a.dispatch(Action::SetEditMode(m)),
            None => tracing::warn!("edit-mode: unknown '{id}'"),
        },
    },
    Choice {
        name: "edit-tool",
        now: |s| tool_id(s.editor.tool),
        choose: |a, id| match TOOLS.into_iter().find(|t| tool_id(*t) == id) {
            Some(t) => a.dispatch(Action::SetEditTool(t)),
            None => tracing::warn!("edit-tool: unknown '{id}'"),
        },
    },
];

/// Add the table's actions (those it defines) with their states now.
pub fn install(app: &Rc<AppState>) {
    let s = app.session.borrow();
    let mut entries = Vec::new();
    for t in TOGGLES {
        let Some(press) = t.press else { continue };
        let weak = Rc::downgrade(app);
        entries.push(
            gio::ActionEntry::<gtk::Application>::builder(t.name)
                .state((t.on)(&s).to_variant())
                .activate(move |_, _, _| {
                    if let Some(a) = weak.upgrade() {
                        press(&a);
                        follow(&a);
                    }
                })
                .build(),
        );
    }
    for c in CHOICES {
        let weak = Rc::downgrade(app);
        let choose = c.choose;
        entries.push(
            gio::ActionEntry::<gtk::Application>::builder(c.name)
                .parameter_type(Some(&String::static_variant_type()))
                .state((c.now)(&s).to_variant())
                .activate(move |_, _, param| {
                    if let (Some(a), Some(id)) =
                        (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                    {
                        choose(&a, &id);
                        follow(&a);
                    }
                })
                .build(),
        );
    }
    drop(s);
    app.app.add_action_entries(entries);
}

/// Bring the states of `map`'s actions in line with the session.
pub(crate) fn sync(map: &impl IsA<gio::ActionMap>, s: &Session) {
    let set = |name: &str, state: glib::Variant| {
        if let Some(a) = map
            .lookup_action(name)
            .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
            && a.state().as_ref() != Some(&state)
        {
            a.set_state(&state);
        }
    };
    for t in TOGGLES {
        set(t.name, (t.on)(s).to_variant());
    }
    for c in CHOICES {
        set(c.name, (c.now)(s).to_variant());
    }
    let r = s.record;
    for (name, id) in [
        ("record-mode", r.mode.id().to_string()),
        ("loop-record-mode", r.loop_mode.id().to_string()),
        ("metronome", r.metronome.id().to_string()),
        ("preroll", r.preroll.id()),
        ("postroll", r.postroll.id()),
    ] {
        set(name, id.to_variant());
    }
}

/// Every toggle's state as the session has it now (after each change and
/// with the tick, ~10 times a second).
pub fn follow(app: &Rc<AppState>) {
    if let Ok(s) = app.session.try_borrow() {
        sync(&app.app, &s);
    }
}

thread_local! {
    /// A menu of the menu bar opened by `show-menu:<name>` (scripts and
    /// screenshots of its checks).
    static SHOWN: std::cell::RefCell<Option<gtk::PopoverMenu>> = const { std::cell::RefCell::new(None) };
}

/// Development aid: `show-menu:<File|Edit|…>[/<submenu>…]` opens a menu
/// of the menu bar (as a popover under it, with the same model and actions) so a
/// screenshot shows its checks, and logs each entry's state.
pub(crate) fn show_menu(app: &Rc<AppState>, name: &str) {
    if name == "none" {
        return hide_menu();
    }
    // A submenu by its path: "Edit/Edit Options" (inside sections too).
    fn find(model: &gio::MenuModel, label: &str) -> Option<gio::MenuModel> {
        (0..model.n_items()).find_map(|i| {
            if let Some(section) = model.item_link(i, "section")
                && let Some(found) = find(&section, label)
            {
                return Some(found);
            }
            let l = model
                .item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())?;
            l.eq_ignore_ascii_case(label)
                .then(|| model.item_link(i, "submenu"))
                .flatten()
        })
    }
    let model: gio::MenuModel = crate::window::menu_model(&app.recent_menu).upcast();
    let sub = name
        .split('/')
        .try_fold(model, |m, label| find(&m, label.trim()));
    let Some(sub) = sub else {
        tracing::warn!("show-menu: no menu '{name}'");
        return;
    };
    for c in crate::palette::menu_commands(&sub) {
        if let Some(on) = crate::palette::checked(&app.app, &c) {
            tracing::info!(
                "menu {name}: {} = {}",
                c.path,
                if on { "checked" } else { "unchecked" }
            );
        }
    }
    let Some(window) = app.window.borrow().clone() else {
        return;
    };
    if let Some(old) = SHOWN.with(|m| m.borrow_mut().take()) {
        old.popdown();
    }
    let popover = gtk::PopoverMenu::from_model(Some(&sub));
    popover.set_parent(&window);
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(120, 4, 1, 1)));
    popover.popup();
    popover.connect_closed(|p| {
        SHOWN.with(|m| {
            let mut m = m.borrow_mut();
            if m.as_ref() == Some(p) {
                m.take();
            }
        });
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    SHOWN.with(|m| *m.borrow_mut() = Some(popover));
}

/// Close the menu `show-menu` opened (`show-menu:none`).
pub(crate) fn hide_menu() {
    if let Some(old) = SHOWN.with(|m| m.borrow_mut().take()) {
        old.popdown();
    }
}

/// The menu `show-menu` opened, while open.
pub(crate) fn shown_menu() -> Option<gtk::Popover> {
    SHOWN.with(|m| m.borrow().as_ref().map(|p| p.clone().upcast()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The actions as `install` makes them (state only: no app to press
    /// them in a unit test).
    fn group(s: &Session) -> gio::SimpleActionGroup {
        let g = gio::SimpleActionGroup::new();
        for t in TOGGLES {
            g.add_action(&gio::SimpleAction::new_stateful(
                t.name,
                None,
                &(t.on)(s).to_variant(),
            ));
        }
        for c in CHOICES {
            g.add_action(&gio::SimpleAction::new_stateful(
                c.name,
                Some(&String::static_variant_type()),
                &(c.now)(s).to_variant(),
            ));
        }
        g
    }

    fn state(g: &gio::SimpleActionGroup, name: &str) -> glib::Variant {
        g.lookup_action(name).unwrap().state().unwrap()
    }

    /// The actions carry the session's states and follow them, whatever
    /// changed them.
    #[test]
    fn toggles_carry_and_follow_the_sessions_state() {
        let mut s = Session::demo(faderframe_engine::EngineConfig::default()).unwrap();
        let g = group(&s);
        let snap = s.editor.snap;
        assert_eq!(state(&g, "toggle-snap"), snap.to_variant());
        assert_eq!(
            state(&g, "edit-mode"),
            mode_id(s.editor.edit_mode).to_variant()
        );
        s.dispatch(Action::ToggleSnap).unwrap();
        s.dispatch(Action::ToggleFollowPlayhead).unwrap();
        s.dispatch(Action::Transport(TransportAction::ToggleLoop))
            .unwrap();
        s.dispatch(Action::SetEditTool(EditTool::Pencil)).unwrap();
        s.dispatch(Action::SetEditFlag(EditFlag::SnapToZeroCrossings, true))
            .unwrap();
        s.dispatch(Action::Transport(TransportAction::ToggleInputQuantize))
            .unwrap();
        sync(&g, &s);
        assert_eq!(state(&g, "toggle-snap"), (!snap).to_variant());
        assert_eq!(
            state(&g, "toggle-follow"),
            s.editor.follow_playhead.to_variant()
        );
        assert_eq!(state(&g, "loop"), s.project().loop_enabled.to_variant());
        assert_eq!(state(&g, "edit-tool"), "pencil".to_variant());
        // Snap off is the Slip mode; the radio follows.
        assert_eq!(
            state(&g, "edit-mode"),
            mode_id(s.editor.edit_mode).to_variant()
        );
        assert_eq!(state(&g, "flag-zero-crossings"), true.to_variant());
        assert_eq!(state(&g, "input-quantize"), true.to_variant());
        assert_eq!(state(&g, "workspace-1"), true.to_variant());
        assert_eq!(state(&g, "workspace-2"), false.to_variant());
        // Arm Selected: checked while every selected track that records
        // is armed.
        assert_eq!(state(&g, "arm-selected"), false.to_variant());
        let bass = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Bass")
            .unwrap()
            .id;
        s.dispatch(Action::SelectTracks {
            tracks: vec![bass],
            mode: faderframe_session::SelectMode::Replace,
        })
        .unwrap();
        s.dispatch(Action::ToggleArmSelected).unwrap();
        sync(&g, &s);
        assert_eq!(state(&g, "arm-selected"), true.to_variant());
    }

    /// Every toggle and choice of the table is in the menus (so they show
    /// its check), toggles without a target, choices with one of theirs;
    /// and the menus' other on/off items are stateful too.
    #[test]
    fn the_menus_show_every_toggle() {
        let recent = gio::Menu::new();
        let model = crate::window::menu_model(&recent);
        let commands = crate::palette::menu_commands(model.upcast_ref());
        for t in TOGGLES {
            let name = format!("app.{}", t.name);
            let items: Vec<_> = commands.iter().filter(|c| c.action == name).collect();
            assert!(!items.is_empty(), "{name} is in no menu");
            assert!(items.iter().all(|c| c.target.is_none()), "{name}");
        }
        for c in CHOICES {
            let name = format!("app.{}", c.name);
            let items: Vec<_> = commands.iter().filter(|x| x.action == name).collect();
            assert!(items.len() >= 2, "{name}");
            assert!(items.iter().all(|x| x.target.is_some()), "{name}");
        }
        // The View menu's views and docks, the lanes, the recording
        // choices: stateful where they are defined.
        for name in [
            "app.view-inspector",
            "app.view-channel-overview",
            "app.view-mixer",
            "app.dock-bottom",
            "app.master-panel",
            "app.stop-to-start",
            "app.metronome",
            "app.record-mode",
        ] {
            assert!(commands.iter().any(|c| c.action == name), "{name}");
        }
    }
}
