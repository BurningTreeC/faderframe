//! MIDI learn on the shell's own buttons (the transport, an editor's
//! Bypass): a right-click shows "MIDI Learn…" and the removal of each
//! mapping the control has, as the canvases' context menus do.

use crate::state::AppState;
use faderframe_project::MappingTarget;
use gtk::prelude::*;
use std::rc::Rc;

/// Right-click on `widget` offers MIDI learn for `target` and removes its
/// mappings.
pub fn on_right_click(app: &Rc<AppState>, widget: &impl IsA<gtk::Widget>, target: MappingTarget) {
    on_right_click_with(app, widget, target, |_| Vec::new());
}

/// [`on_right_click`] with the control's own choices first (`first`: what
/// to offer, read from the session when the menu opens).
pub fn on_right_click_with(
    app: &Rc<AppState>,
    widget: &impl IsA<gtk::Widget>,
    target: MappingTarget,
    first: fn(&faderframe_session::Session) -> Vec<(String, faderframe_session::Action)>,
) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let weak = Rc::downgrade(app);
    let anchor = widget.as_ref().downgrade();
    click.connect_pressed(move |g, _, _, _| {
        let (Some(app), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else {
            return;
        };
        g.set_state(gtk::EventSequenceState::Claimed);
        // Read what to offer first: dispatching must not hold the session.
        let (own, entries) = {
            let s = app.session.borrow();
            (first(&s), s.midi_learn_menu(target))
        };
        let gap = own.len();
        let entries: Vec<_> = own.into_iter().chain(entries).collect();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        popover.set_parent(&anchor);
        popover.set_has_arrow(true);
        for (k, (label, action)) in entries.into_iter().enumerate() {
            if k == gap && gap > 0 {
                list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            let b = gtk::Button::with_label(&label);
            b.add_css_class("flat");
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let weak = Rc::downgrade(&app);
            let pop = popover.downgrade();
            b.connect_clicked(move |_| {
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
                if let Some(a) = weak.upgrade() {
                    a.dispatch(action.clone());
                }
            });
            list.append(&b);
        }
        popover.connect_closed(|p| {
            let p = p.clone();
            gtk::glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    });
    widget.add_controller(click);
}
