//! Keyboard focus, selection and the editing context: which editor the
//! editing keys and commands work on.
//!
//! Selection lives in the session; keyboard focus is GTK's. They meet in
//! the *editing context*: the editor view the user last worked in (pressed
//! or focused) — the arranger, piano roll, event list, pitch or spectral
//! editor, … ([`is_editor`]). Clicking a toolbar button, a tool selector,
//! a transport control or another non-editor view (the mixer, meters)
//! moves the keyboard focus but not the context, and selects nothing.
//!
//! * **Commands with shortcuts** (Edit → Cut, Copy, Paste, Duplicate,
//!   Select All, Delete Selection, Undo, Redo: application accelerators
//!   and menu items, which GTK runs
//!   before the focused widget sees the key) go, by what has the focus: to
//!   a focused text field (its own clipboard, selection and undo); to a
//!   focused editor (the key as the view knows it, and only to it); to a
//!   focused other view and, when it does not use the key, to the editing
//!   context. In plugin editors and dialogs they never reach an editor
//!   behind them.
//! * **Plain editing keys** (Delete, S, B, Q, the nudge keys, …) that the
//!   focused widget leaves unused go to the editing context — in the main
//!   window and detached view windows, unless a text field or another
//!   editor has the focus. Tab, the arrows, Enter, Space, Escape and the
//!   page keys stay with focus navigation and the focused control (Escape
//!   also ends MIDI learn and leaves full screen in the window).
//! * The launcher and the automation view have no keys of their own: they
//!   do not take the context (the arranger keeps it).
//! * Without a context yet, or when it is not shown, the arranger is the
//!   context (if it is shown); with no editor shown, nothing happens.

use crate::canvas::CanvasWidget;
use crate::state::AppState;
use faderframe_session::Action;
use faderframe_ui_canvas::{Key, Modifiers, ViewEvent};
use faderframe_workspace::ViewKind;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The editor the user last worked in.
    static CONTEXT: RefCell<glib::WeakRef<CanvasWidget>> = RefCell::new(glib::WeakRef::new());
}

/// Views with a selection of their own that editing keys work on.
pub(crate) fn is_editor(kind: ViewKind) -> bool {
    matches!(
        kind,
        ViewKind::Arranger
            | ViewKind::PianoRoll
            | ViewKind::Album
            | ViewKind::Pitch
            | ViewKind::Events
            | ViewKind::Spectral
            | ViewKind::Setlist
    )
}

/// The editing commands with application shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditCommand {
    Cut,
    Copy,
    Paste,
    Duplicate,
    SelectAll,
    /// Edit → Delete Selection (the Delete key of the editor it goes to).
    Delete,
}

impl EditCommand {
    pub(crate) const ALL: [(EditCommand, &'static str); 6] = [
        (Self::Cut, "cut"),
        (Self::Copy, "copy"),
        (Self::Paste, "paste"),
        (Self::Duplicate, "duplicate"),
        (Self::SelectAll, "select-all"),
        (Self::Delete, "delete"),
    ];

    /// The key the editors know it by.
    pub(crate) fn key(self) -> ViewEvent {
        let c = match self {
            Self::Cut => 'x',
            Self::Copy => 'c',
            Self::Paste => 'v',
            Self::Duplicate => 'd',
            Self::SelectAll => 'a',
            Self::Delete => {
                return ViewEvent::Key {
                    key: Key::Delete,
                    modifiers: Modifiers::NONE,
                };
            }
        };
        ViewEvent::Key {
            key: Key::Char(c),
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        }
    }

    /// GTK's action for it in a text field (`None`: text has none).
    pub(crate) fn text_action(self) -> Option<&'static str> {
        match self {
            Self::Cut => Some("clipboard.cut"),
            Self::Copy => Some("clipboard.copy"),
            Self::Paste => Some("clipboard.paste"),
            Self::Duplicate => None,
            Self::SelectAll => Some("selection.select-all"),
            Self::Delete => Some("selection.delete"),
        }
    }
}

/// What has the keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    /// An editable text field.
    Text,
    /// An editor view.
    Editor,
    /// Another view (the mixer, the edit toolbar, meters).
    OtherView,
    /// A button, a menu, nothing.
    Other,
}

/// Where a command goes, in order, until one takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The focused text field's own action.
    Text,
    /// The focused view.
    Focused,
    /// The editing context.
    Context,
}

/// The routes of a command for `focus` in a main or detached view window
/// (`workspace`) or another window (plugin editors, dialogs).
pub(crate) fn command_routes(focus: Focus, workspace: bool) -> &'static [Route] {
    match (focus, workspace) {
        (Focus::Text, _) => &[Route::Text],
        (Focus::Editor, _) => &[Route::Focused],
        (Focus::OtherView, true) => &[Route::Focused, Route::Context],
        (Focus::OtherView, false) => &[Route::Focused],
        (Focus::Other, true) => &[Route::Context],
        (Focus::Other, false) => &[],
    }
}

/// Does a key the focused widget left unused go on to the editing
/// context?
pub(crate) fn forwards(key: Key, focus: Focus, workspace: bool) -> bool {
    let navigation = matches!(
        key,
        Key::Tab
            | Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Enter
            | Key::Space
            | Key::Escape
            | Key::PageUp
            | Key::PageDown
            | Key::Other
    );
    workspace && !navigation && matches!(focus, Focus::OtherView | Focus::Other)
}

/// The kind of view `canvas` hosts (views of the workspace only).
fn kind_of(app: &AppState, canvas: &CanvasWidget) -> Option<ViewKind> {
    let dock = app.dock.try_borrow().ok()?;
    dock.hosts
        .values()
        .find(|(_, h)| h.canvas == *canvas)
        .map(|(k, _)| *k)
}

/// The view a widget is in.
fn canvas_of(widget: &gtk::Widget) -> Option<CanvasWidget> {
    widget.clone().downcast::<CanvasWidget>().ok().or_else(|| {
        widget
            .ancestor(CanvasWidget::static_type())
            .and_then(|w| w.downcast::<CanvasWidget>().ok())
    })
}

/// The main window or a detached view window?
fn is_workspace(app: &AppState, window: &gtk::Window) -> bool {
    app.window
        .borrow()
        .as_ref()
        .is_some_and(|w| w.upcast_ref::<gtk::Window>() == window)
        || window.has_css_class("ff-window")
}

/// What has the focus in `window`, the view it is in, the widget.
fn focus_in(
    app: &AppState,
    window: &gtk::Window,
) -> (Focus, Option<CanvasWidget>, Option<gtk::Widget>) {
    let focus = gtk::prelude::GtkWindowExt::focus(window);
    if crate::transport_keys::editing_text(focus.clone()) {
        return (Focus::Text, None, focus);
    }
    let canvas = focus.as_ref().and_then(canvas_of);
    let kind = match &canvas {
        Some(c) if kind_of(app, c).is_some_and(is_editor) => Focus::Editor,
        Some(_) => Focus::OtherView,
        None => Focus::Other,
    };
    (kind, canvas, focus)
}

/// The user works in `canvas` (pressed or focused it): an editor becomes
/// the editing context.
pub(crate) fn worked_in(app: &AppState, canvas: &CanvasWidget) {
    if kind_of(app, canvas).is_some_and(is_editor) {
        CONTEXT.with(|c| c.borrow().set(Some(canvas)));
    }
}

/// The editing context, if shown; else the arranger, if shown.
fn context(app: &AppState) -> Option<CanvasWidget> {
    CONTEXT
        .with(|c| c.borrow().upgrade())
        .filter(|c| c.is_mapped())
        .or_else(|| {
            let dock = app.dock.try_borrow().ok()?;
            dock.hosts
                .values()
                .find(|(k, h)| *k == ViewKind::Arranger && h.canvas.is_mapped())
                .map(|(_, h)| h.canvas.clone())
        })
}

/// Run a command where it belongs (see the module docs).
pub(crate) fn command(app: &Rc<AppState>, cmd: EditCommand) {
    let Some(window) = app.app.active_window() else {
        // No window has the focus (scripts): the editing context.
        if let Some(c) = context(app) {
            c.deliver(cmd.key());
        }
        return;
    };
    let (focus, canvas, widget) = focus_in(app, &window);
    let workspace = is_workspace(app, &window);
    tracing::debug!("edit command {cmd:?}: focus {focus:?}, workspace window {workspace}");
    for route in command_routes(focus, workspace) {
        let taken = match route {
            Route::Text => {
                if let (Some(w), Some(name)) = (&widget, cmd.text_action()) {
                    let _ = w.activate_action(name, None);
                }
                true
            }
            Route::Focused => canvas.as_ref().is_some_and(|c| c.deliver(cmd.key())),
            Route::Context => context(app)
                .filter(|c| Some(c) != canvas.as_ref())
                .is_some_and(|c| c.deliver(cmd.key())),
        };
        if taken {
            return;
        }
    }
    // A list (the plugin browser's) selects all its rows itself.
    if cmd == EditCommand::SelectAll
        && let Some(w) = widget
    {
        let _ = w.activate_action("list.select-all", None);
    }
}

/// Undo or redo: the focused text field's own while typing, else the
/// project's.
pub(crate) fn undo(app: &Rc<AppState>, redo: bool) {
    if let Some(window) = app.app.active_window() {
        let focus = gtk::prelude::GtkWindowExt::focus(&window);
        if crate::transport_keys::editing_text(focus.clone()) {
            if let Some(w) = focus {
                let _ = w.activate_action(if redo { "text.redo" } else { "text.undo" }, None);
            }
            return;
        }
    }
    app.dispatch(if redo { Action::Redo } else { Action::Undo });
}

/// Edit → Cut, Copy, Paste, Duplicate, Select All, Delete Selection (their
/// shortcuts are in `palette::DEFAULT_SHORTCUTS`; Delete is the key).
pub(crate) fn install_actions(app: &Rc<AppState>) {
    for (cmd, name) in EditCommand::ALL {
        let weak = Rc::downgrade(app);
        let entry = gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    command(&a, cmd);
                }
            })
            .build();
        app.app.add_action_entries([entry]);
    }
}

/// On the main window and detached view windows: editing keys the focused
/// widget leaves unused go to the editing context, and an editor focused
/// from the keyboard becomes it.
pub(crate) fn install_window(app: &Rc<AppState>, window: &impl IsA<gtk::Window>) {
    let window = window.upcast_ref::<gtk::Window>();
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let weak = Rc::downgrade(app);
    keys.connect_key_pressed(move |c, key, _, state| {
        let (Some(app), Some(window)) = (
            weak.upgrade(),
            c.widget().and_then(|w| w.downcast::<gtk::Window>().ok()),
        ) else {
            return glib::Propagation::Proceed;
        };
        let k = crate::canvas::map_key(key);
        let (focus, _, _) = focus_in(&app, &window);
        if !forwards(k, focus, is_workspace(&app, &window)) {
            return glib::Propagation::Proceed;
        }
        let ev = ViewEvent::Key {
            key: k,
            modifiers: crate::canvas::modifiers(state),
        };
        if context(&app).is_some_and(|c| c.deliver(ev)) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    let weak = Rc::downgrade(app);
    window.connect_focus_widget_notify(move |w| {
        if let (Some(app), Some(canvas)) = (
            weak.upgrade(),
            gtk::prelude::GtkWindowExt::focus(w)
                .as_ref()
                .and_then(canvas_of),
        ) {
            worked_in(&app, &canvas);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_go_to_text_then_the_focused_editor_then_the_context() {
        // Typing: the text field's clipboard, never an editor.
        assert_eq!(command_routes(Focus::Text, true), &[Route::Text]);
        assert_eq!(command_routes(Focus::Text, false), &[Route::Text]);
        // An editor with the focus takes its own commands (the piano
        // roll's notes, the event list's rows) and nothing goes past it.
        assert_eq!(command_routes(Focus::Editor, true), &[Route::Focused]);
        // A toolbar button, a transport control, the mixer: the editor
        // last worked in (the arranger's selected clips).
        assert_eq!(command_routes(Focus::Other, true), &[Route::Context]);
        assert_eq!(
            command_routes(Focus::OtherView, true),
            &[Route::Focused, Route::Context]
        );
        // A plugin editor or a dialog: never the arranger behind it.
        assert!(command_routes(Focus::Other, false).is_empty());
        assert_eq!(command_routes(Focus::OtherView, false), &[Route::Focused]);
    }

    #[test]
    fn unused_editing_keys_reach_the_context_but_navigation_stays() {
        for k in [
            Key::Delete,
            Key::Backspace,
            Key::Char('s'),
            Key::Char('b'),
            Key::Home,
        ] {
            assert!(forwards(k, Focus::Other, true), "{k:?} from a button");
            assert!(forwards(k, Focus::OtherView, true), "{k:?} from the mixer");
            assert!(!forwards(k, Focus::Text, true), "{k:?} while typing");
            assert!(!forwards(k, Focus::Editor, true), "{k:?} in another editor");
            assert!(
                !forwards(k, Focus::Other, false),
                "{k:?} in a plugin window"
            );
        }
        for k in [
            Key::Tab,
            Key::Left,
            Key::Right,
            Key::Up,
            Key::Down,
            Key::Enter,
            Key::Space,
            Key::Escape,
            Key::PageUp,
            Key::PageDown,
            Key::Other,
        ] {
            assert!(!forwards(k, Focus::Other, true), "{k:?} moves the focus");
        }
    }

    #[test]
    fn the_commands_are_the_keys_editors_know() {
        for (cmd, _) in EditCommand::ALL {
            let ViewEvent::Key { key, modifiers } = cmd.key() else {
                panic!("{cmd:?}");
            };
            if cmd == EditCommand::Delete {
                // The editors' own Delete: notes in the piano roll, the
                // range or the clips in the arranger.
                assert_eq!((key, modifiers), (Key::Delete, Modifiers::NONE));
                continue;
            }
            assert!(matches!(key, Key::Char(_)));
            assert!(modifiers.ctrl && !modifiers.alt && !modifiers.shift);
        }
        assert_eq!(EditCommand::Duplicate.text_action(), None);
        assert_eq!(
            EditCommand::SelectAll.text_action(),
            Some("selection.select-all")
        );
        for v in [
            ViewKind::Arranger,
            ViewKind::PianoRoll,
            ViewKind::Events,
            ViewKind::Pitch,
            ViewKind::Spectral,
        ] {
            assert!(is_editor(v));
        }
        // No keys of their own: the arranger stays the context.
        for v in [
            ViewKind::Mixer,
            ViewKind::Tools,
            ViewKind::History,
            ViewKind::Launcher,
            ViewKind::Automation,
        ] {
            assert!(!is_editor(v));
        }
    }

    /// With a display: a button that has the focus leaves Ctrl+C to the
    /// editing context; an entry keeps its text clipboard.
    #[test]
    #[ignore = "requires a GTK display"]
    fn focused_text_keeps_its_keys() {
        gtk::init().expect("GTK display");
        let window = gtk::Window::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::Button::with_label("Smart");
        let entry = gtk::Entry::new();
        body.append(&button);
        body.append(&entry);
        window.set_child(Some(&body));
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&entry));
        assert!(crate::transport_keys::editing_text(
            gtk::prelude::GtkWindowExt::focus(&window)
        ));
        let text = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
        assert!(text.activate_action("selection.select-all", None).is_ok());
        assert!(text.activate_action("clipboard.copy", None).is_ok());
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&button));
        assert!(!crate::transport_keys::editing_text(
            gtk::prelude::GtkWindowExt::focus(&window)
        ));
        window.destroy();
    }
}
