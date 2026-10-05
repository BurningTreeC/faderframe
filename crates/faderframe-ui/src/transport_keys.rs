//! Space belongs to the transport before GTK can activate a focused control.

use crate::state::AppState;
use faderframe_session::{Action, TransportAction};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;
use std::rc::Rc;

/// Install on existing and future application windows, including detached
/// views, preferences and built-in/generic plugin editors.
pub fn install(app: &Rc<AppState>) {
    for window in app.app.windows() {
        install_window(app, &window);
    }
    let weak = Rc::downgrade(app);
    app.app.connect_window_added(move |_, window| {
        if let Some(app) = weak.upgrade() {
            install_window(&app, window);
        }
    });
}

fn install_window(app: &Rc<AppState>, window: &gtk::Window) {
    let weak = Rc::downgrade(app);
    window.add_controller(controller(window, move || {
        if let Some(app) = weak.upgrade() {
            let recording = app.session.borrow().transport().recording;
            app.dispatch(Action::Transport(if recording {
                TransportAction::Stop
            } else {
                TransportAction::TogglePlay
            }));
        }
    }));
}

/// Entries focus their inner GtkText, while other editable widgets can
/// delegate to a child. Check the focus ancestry, not just GtkEntry itself.
fn editing_text(mut focus: Option<gtk::Widget>) -> bool {
    while let Some(widget) = focus {
        if widget
            .downcast_ref::<gtk::Editable>()
            .is_some_and(|e| e.is_editable())
            || widget
                .downcast_ref::<gtk::TextView>()
                .is_some_and(|e| e.is_editable())
        {
            return true;
        }
        focus = widget.parent();
    }
    false
}

#[derive(Debug, PartialEq, Eq)]
enum Press {
    Pass,
    Toggle,
    Held,
}

#[derive(Default)]
struct SpaceKey(Cell<Option<u32>>);

impl SpaceKey {
    fn press(
        &self,
        key: gdk::Key,
        code: u32,
        modifiers: gdk::ModifierType,
        editing: bool,
    ) -> Press {
        // Swallow auto-repeat, even if focus/modifiers changed while held.
        if self.0.get() == Some(code) {
            return Press::Held;
        }
        let modified = modifiers.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK
                | gdk::ModifierType::HYPER_MASK,
        );
        if key != gdk::Key::space || modified || editing {
            return Press::Pass;
        }
        self.0.set(Some(code));
        Press::Toggle
    }

    fn release(&self, code: u32) {
        if self.0.get() == Some(code) {
            self.0.set(None);
        }
    }
}

fn controller(window: &gtk::Window, toggle: impl Fn() + 'static) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let space = Rc::new(SpaceKey::default());
    let pressed = Rc::clone(&space);
    keys.connect_key_pressed(move |controller, key, code, modifiers| {
        let focus = controller
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.focus());
        match pressed.press(key, code, modifiers, editing_text(focus)) {
            Press::Pass => glib::Propagation::Proceed,
            Press::Toggle => {
                toggle();
                glib::Propagation::Stop
            }
            Press::Held => glib::Propagation::Stop,
        }
    });
    let released = Rc::clone(&space);
    keys.connect_key_released(move |_, _, code, _| released.release(code));
    // GTK consumes the matching release of a handled press as well, so a
    // focused button cannot activate when the user lets go of Space.
    window.connect_is_active_notify(move |w| {
        if !w.is_active() {
            space.0.set(None);
        }
    });
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_toggles_once_per_press_and_never_while_typing() {
        let space = SpaceKey::default();
        let none = gdk::ModifierType::empty();
        assert_eq!(space.press(gdk::Key::space, 65, none, true), Press::Pass);
        assert_eq!(space.press(gdk::Key::space, 65, none, false), Press::Toggle);
        assert_eq!(space.press(gdk::Key::space, 65, none, false), Press::Held);
        assert_eq!(space.press(gdk::Key::space, 65, none, true), Press::Held);
        space.release(99);
        assert_eq!(space.press(gdk::Key::space, 65, none, false), Press::Held);
        space.release(65);
        assert_eq!(space.press(gdk::Key::space, 65, none, false), Press::Toggle);
    }

    #[test]
    fn other_keys_and_modified_space_are_left_to_the_widget() {
        let space = SpaceKey::default();
        for modifiers in [
            gdk::ModifierType::CONTROL_MASK,
            gdk::ModifierType::ALT_MASK,
            gdk::ModifierType::SUPER_MASK,
            gdk::ModifierType::META_MASK,
        ] {
            assert_eq!(
                space.press(gdk::Key::space, 65, modifiers, false),
                Press::Pass
            );
        }
        assert_eq!(
            space.press(gdk::Key::Return, 36, gdk::ModifierType::empty(), false),
            Press::Pass
        );
        assert_eq!(
            space.press(gdk::Key::space, 65, gdk::ModifierType::LOCK_MASK, false),
            Press::Toggle
        );
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn capture_precedes_buttons_and_preserves_editable_focus() {
        gtk::init().expect("GTK display");
        let window = gtk::Window::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let button = gtk::Button::with_label("Preferences");
        let entry = gtk::Entry::new();
        let search = gtk::SearchEntry::new();
        let text = gtk::TextView::new();
        let spin = gtk::SpinButton::with_range(0.0, 10.0, 1.0);
        for widget in [
            button.upcast_ref::<gtk::Widget>(),
            entry.upcast_ref(),
            search.upcast_ref(),
            text.upcast_ref(),
            spin.upcast_ref(),
        ] {
            body.append(widget);
        }
        window.set_child(Some(&body));
        let toggles = Rc::new(Cell::new(0));
        let count = Rc::clone(&toggles);
        let keys = controller(&window, move || count.set(count.get() + 1));
        assert_eq!(keys.propagation_phase(), gtk::PropagationPhase::Capture);
        window.add_controller(keys.clone());
        let press = || {
            keys.emit_by_name::<bool>(
                "key-pressed",
                &[&gdk::Key::space, &65u32, &gdk::ModifierType::empty()],
            )
        };
        let release = || {
            keys.emit_by_name::<()>(
                "key-released",
                &[&gdk::Key::space, &65u32, &gdk::ModifierType::empty()],
            )
        };
        gtk::prelude::GtkWindowExt::set_focus(&window, Some(&button));
        assert!(press());
        assert!(press());
        assert_eq!(toggles.get(), 1);
        release();
        assert!(press());
        assert_eq!(toggles.get(), 2);
        release();
        for widget in [
            entry.upcast_ref::<gtk::Widget>(),
            search.upcast_ref(),
            text.upcast_ref(),
            spin.upcast_ref(),
        ] {
            gtk::prelude::GtkWindowExt::set_focus(&window, Some(widget));
            assert!(editing_text(gtk::prelude::GtkWindowExt::focus(&window)));
            assert!(!press(), "text input must keep its space");
            release();
        }
        assert_eq!(toggles.get(), 2);
        window.destroy();
    }
}
