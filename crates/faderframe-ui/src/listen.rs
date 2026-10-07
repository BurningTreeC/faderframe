//! Audio → Listen: speakers or headphones (the master rendered
//! binaurally, with a room — persisted), and the mono check (per session).
//! Listening only: renders stay as mixed.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_binaural::Room;
use faderframe_session::Action;
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

/// The `listen` action's state for a setting.
fn id(room: Option<Room>) -> &'static str {
    room.map_or("speakers", Room::id)
}

pub fn install_actions(app: &Rc<AppState>) {
    let (room, mono) = {
        let s = app.session.borrow();
        (s.headphones(), s.mono_check())
    };
    let weak = Rc::downgrade(app);
    let listen = gio::ActionEntry::builder("listen")
        .parameter_type(Some(&String::static_variant_type()))
        .state(id(room).to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            a.dispatch(Action::SetHeadphones(Room::from_id(&v)));
        })
        .build();
    let weak = Rc::downgrade(app);
    let mono = gio::ActionEntry::builder("mono-check")
        .state(mono.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                let on = a.session.borrow().mono_check();
                a.dispatch(Action::SetMonoCheck(!on));
            }
        })
        .build();
    app.app.add_action_entries([listen, mono]);
}

/// The Audio menu's Listen section.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let item = |label: &str, target: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some("app.listen"), Some(&target.to_variant()));
        i
    };
    m.append_item(&item("Speakers", "speakers"));
    for r in Room::ALL {
        m.append_item(&item(
            &format!("Headphones · {} (binaural)", r.name()),
            r.id(),
        ));
    }
    m.append(Some("Mono Check"), Some("app.mono-check"));
    m
}

/// After a change: the menu shows what the session does, and a new
/// headphone setting is remembered.
pub fn follow(app: &Rc<AppState>) {
    let (room, mono) = {
        let s = app.session.borrow();
        (s.headphones(), s.mono_check())
    };
    let action = |name: &str| {
        app.app
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    };
    if let Some(a) = action("listen")
        && a.state().and_then(|v| v.get::<String>()).as_deref() != Some(id(room))
    {
        a.set_state(&id(room).to_variant());
        let mut p = Preferences::load();
        p.headphones = room.map(|r| r.id().to_string());
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    }
    if let Some(a) = action("mono-check")
        && a.state().and_then(|v| v.get::<bool>()) != Some(mono)
    {
        a.set_state(&mono.to_variant());
    }
}
