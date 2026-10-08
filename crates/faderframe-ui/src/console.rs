//! Audio → Console: run the mix through a console's circuits (or in the
//! box), how hard the channels drive it, and whether the mixer takes the
//! console's look. The settings are the project's (undoable).

use crate::state::AppState;
use faderframe_project::Command;
use faderframe_project::console::{DRIVE_DB, FAMILIES, IDS, family_of};
use faderframe_session::Action;
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

/// The `console` action's state: the family's id, or "off".
fn state(app: &AppState) -> (String, String, bool) {
    let c = app.session.borrow().console();
    (
        c.map_or("off", |c| c.id()).to_string(),
        c.map_or_else(|| "0".to_string(), |c| format!("{}", c.drive_db.round())),
        c.is_some_and(|c| c.look),
    )
}

pub fn install_actions(app: &Rc<AppState>) {
    let (family, drive, look) = state(app);
    let weak = Rc::downgrade(app);
    let console = gio::ActionEntry::builder("console")
        .parameter_type(Some(&String::static_variant_type()))
        .state(family.to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            a.dispatch(Action::SetConsole {
                family: family_of(&v),
            });
        })
        .build();
    // `console-drive:<dB>`.
    let weak = Rc::downgrade(app);
    let drive = gio::ActionEntry::builder("console-drive")
        .parameter_type(Some(&String::static_variant_type()))
        .state(drive.to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            match v.trim().trim_end_matches("dB").trim().parse::<f64>() {
                Ok(db) => a.dispatch(Action::Edit(Command::SetConsoleDrive {
                    drive_db: db.clamp(-DRIVE_DB, DRIVE_DB),
                })),
                Err(_) => tracing::warn!("console-drive: not a level: '{v}'"),
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let look = gio::ActionEntry::builder("console-look")
        .state(look.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                let Some(c) = a.session.borrow().console() else {
                    return;
                };
                a.dispatch(Action::Edit(Command::SetConsoleLook { look: !c.look }));
            }
        })
        .build();
    app.app.add_action_entries([console, drive, look]);
}

/// The Audio menu's Console section.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let item = |label: &str, action: &str, target: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        i
    };
    let families = gio::Menu::new();
    families.append_item(&item("Off (in the Box)", "app.console", "off"));
    for (name, id) in FAMILIES.iter().zip(IDS) {
        families.append_item(&item(name, "app.console", id));
    }
    m.append_section(None, &families);
    let drive = gio::Menu::new();
    for db in [-6, -3, 0, 3, 6, 9, 12] {
        drive.append_item(&item(
            &format!("{db:+} dB"),
            "app.console-drive",
            &db.to_string(),
        ));
    }
    let more = gio::Menu::new();
    more.append_submenu(Some("Channel Drive"), &drive);
    more.append(
        Some("Mixer Takes the Console's Look"),
        Some("app.console-look"),
    );
    m.append_section(None, &more);
    m
}

/// After a change: the menu shows the project's console.
pub fn follow(app: &Rc<AppState>) {
    let (family, drive, look) = state(app);
    let action = |name: &str| {
        app.app
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    };
    for (name, now) in [("console", family), ("console-drive", drive)] {
        if let Some(a) = action(name)
            && a.state().and_then(|v| v.get::<String>()).as_deref() != Some(now.as_str())
        {
            a.set_state(&now.to_variant());
        }
    }
    if let Some(a) = action("console-look") {
        a.set_enabled(app.session.borrow().console().is_some());
        if a.state().and_then(|v| v.get::<bool>()) != Some(look) {
            a.set_state(&look.to_variant());
        }
    }
}
