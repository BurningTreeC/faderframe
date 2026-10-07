//! Audio → Listen: speakers or headphones (the master rendered
//! binaurally, with a room), whose ears (a built-in head or a SOFA file),
//! the headphone correction — all remembered — and the mono check (per
//! session). Listening only: renders stay as mixed.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_binaural::{BUILTIN_HEADS, Room};
use faderframe_session::Action;
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

/// The `listen` action's state for a setting.
fn id(room: Option<Room>) -> &'static str {
    room.map_or("speakers", Room::id)
}

/// The `headphone-correction` action's state: the file, or "" for none.
fn correction_state(app: &AppState) -> String {
    app.session
        .borrow()
        .headphone_correction()
        .map(|(p, _)| p.display().to_string())
        .unwrap_or_default()
}

pub fn install_actions(app: &Rc<AppState>) {
    let (room, mono, head) = {
        let s = app.session.borrow();
        (s.headphones(), s.mono_check(), s.head().id().to_string())
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
    // `listen-head:<id>` (a built-in head, or `sofa:<path>`).
    let weak = Rc::downgrade(app);
    let head = gio::ActionEntry::builder("listen-head")
        .parameter_type(Some(&String::static_variant_type()))
        .state(head.to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            a.dispatch(Action::SetHead(v));
        })
        .build();
    // `headphone-correction:<path>` ("": none).
    let weak = Rc::downgrade(app);
    let correction = gio::ActionEntry::builder("headphone-correction")
        .parameter_type(Some(&String::static_variant_type()))
        .state(correction_state(app).to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            let path = (!v.is_empty()).then(|| std::path::PathBuf::from(v));
            a.dispatch(Action::SetHeadphoneCorrection(path));
        })
        .build();
    let weak = Rc::downgrade(app);
    let load_sofa = gio::ActionEntry::builder("load-sofa")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                choose(&a, Choice::Sofa);
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let load_correction = gio::ActionEntry::builder("load-headphone-correction")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                choose(&a, Choice::Correction);
            }
        })
        .build();
    app.app
        .add_action_entries([listen, mono, head, correction, load_sofa, load_correction]);
}

#[derive(Clone, Copy)]
enum Choice {
    Sofa,
    Correction,
}

/// A file chooser for a SOFA head or a headphone correction.
fn choose(app: &Rc<AppState>, what: Choice) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    let f = gtk::FileFilter::new();
    let title = match what {
        Choice::Sofa => {
            f.set_name(Some("SOFA HRIR sets (.sofa)"));
            f.add_suffix("sofa");
            f.add_suffix("SOFA");
            "Listen Through a SOFA File"
        }
        Choice::Correction => {
            f.set_name(Some(
                "Headphone corrections (EqualizerAPO/AutoEq .txt, impulse responses)",
            ));
            for s in ["txt", "TXT", "wav", "WAV", "flac", "aiff", "aif"] {
                f.add_suffix(s);
            }
            "Load a Headphone Correction"
        }
    };
    filters.append(&f);
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    filters.append(&all);
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .accept_label("Load")
        .modal(true)
        .filters(&filters)
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        app.dispatch(match what {
            Choice::Sofa => Action::SetHead(faderframe_session::listening::sofa_id(&path)),
            Choice::Correction => Action::SetHeadphoneCorrection(Some(path)),
        });
    });
}

/// The Audio menu's Listen section.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let item = |label: &str, action: &str, target: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        i
    };
    let modes = gio::Menu::new();
    modes.append_item(&item("Speakers", "app.listen", "speakers"));
    for r in Room::ALL {
        modes.append_item(&item(
            &format!("Headphones · {} (binaural)", r.name()),
            "app.listen",
            r.id(),
        ));
    }
    m.append_section(None, &modes);
    let mono = gio::Menu::new();
    mono.append(Some("Mono Check"), Some("app.mono-check"));
    m.append_section(None, &mono);
    let heads = gio::Menu::new();
    let dummies = gio::Menu::new();
    let people = gio::Menu::new();
    for (i, h) in BUILTIN_HEADS.iter().enumerate() {
        let target = if i < 2 { &dummies } else { &people };
        target.append_item(&item(h.name, "app.listen-head", h.id));
    }
    heads.append_section(None, &dummies);
    heads.append_section(Some("SADIE II listeners"), &people);
    let sofa = gio::Menu::new();
    sofa.append(Some("From a SOFA File…"), Some("app.load-sofa"));
    heads.append_section(None, &sofa);
    let more = gio::Menu::new();
    more.append_submenu(Some("Head"), &heads);
    let correction = gio::Menu::new();
    correction.append_item(&item("None", "app.headphone-correction", ""));
    correction.append(
        Some("Load (EqualizerAPO, AutoEq, Impulse Response)…"),
        Some("app.load-headphone-correction"),
    );
    more.append_submenu(Some("Headphone Correction"), &correction);
    m.append_section(None, &more);
    m
}

/// Apply the remembered head and correction (before the headphones are
/// switched on, so the graph is built once).
pub fn apply_preferences(session: &mut faderframe_session::Session, prefs: &Preferences) {
    if let Some(id) = prefs.headphone_head.as_deref()
        && let Err(e) = session.set_head(id)
    {
        tracing::warn!("head {id}: {e}");
    }
    if let Some(path) = prefs.headphone_correction.as_deref()
        && let Err(e) = session.set_headphone_correction(Some(std::path::Path::new(path)))
    {
        tracing::warn!("headphone correction: {e}");
    }
}

/// After a change: the menu shows what the session does, and new
/// headphone settings are remembered.
pub fn follow(app: &Rc<AppState>) {
    let (room, mono, head) = {
        let s = app.session.borrow();
        (s.headphones(), s.mono_check(), s.head().id().to_string())
    };
    let correction = correction_state(app);
    let action = |name: &str| {
        app.app
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    };
    let changed = |name: &str, now: &str| -> bool {
        match action(name) {
            Some(a) if a.state().and_then(|v| v.get::<String>()).as_deref() != Some(now) => {
                a.set_state(&now.to_variant());
                true
            }
            _ => false,
        }
    };
    let mut save = changed("listen", id(room));
    save |= changed("listen-head", &head);
    save |= changed("headphone-correction", &correction);
    if save {
        let mut p = Preferences::load();
        p.headphones = room.map(|r| r.id().to_string());
        p.headphone_head = (head != "ku100").then_some(head);
        p.headphone_correction = (!correction.is_empty()).then_some(correction);
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
