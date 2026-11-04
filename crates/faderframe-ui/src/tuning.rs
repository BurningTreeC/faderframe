//! Audio → Tuning: the project's tuning (twelve-tone equal temperament,
//! other equal temperaments, Scala scales and keyboard mappings) and its
//! reference pitch; the Scala file choosers tracks' menus ask for too
//! (`UiRequest::ChooseTuning`). The settings are the project's
//! (undoable; see `faderframe_project::tuning`).
//!
//! `app.tuning` takes `12tet`, `edo=<n>` or `scl=<path>[;kbm=<path>]`
//! and `app.tuning-ref` a frequency in Hz — also as the dev actions
//! `tuning:` and `tuning-ref:`.

use crate::state::AppState;
use faderframe_core::TrackId;
use faderframe_project::Command;
use faderframe_project::tuning::{
    EQUAL_TEMPERAMENTS, MORE_EQUAL_TEMPERAMENTS, REFERENCE_RANGE, ScaleSource, Tuning, trim_hz,
};
use faderframe_session::tuning::{REFERENCE_PITCHES, TuningFile, parse_tuning_files};
use faderframe_session::{Action, NoticeLevel};
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

/// The `tuning` action's state: `12tet`, `edo=<n>` or `scala`.
fn state(app: &AppState) -> (String, String, bool) {
    let s = app.session.borrow();
    let p = s.project();
    let tuning = match &p.tuning {
        None => "12tet".to_string(),
        Some(Tuning {
            scale: ScaleSource::Equal { steps },
            mapping: None,
            ..
        }) => format!("edo={steps}"),
        Some(_) => "scala".to_string(),
    };
    let mapped = p.tuning.as_ref().is_some_and(|t| t.mapping.is_some());
    (tuning, trim_hz(p.reference_hz()), mapped)
}

/// What `app.tuning`'s parameter asks for.
fn action_for(arg: &str) -> Option<Action> {
    let arg = arg.trim();
    if arg == "12tet" || arg == "12-tet" {
        return Some(Action::Edit(Command::SetTuning {
            track: None,
            tuning: None,
        }));
    }
    if let Some(n) = arg.strip_prefix("edo=") {
        let steps = n
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|s| (1..=1024).contains(s))?;
        return Some(Action::Edit(Command::SetTuning {
            track: None,
            tuning: (steps != 12).then(|| Tuning::equal(steps)),
        }));
    }
    let (scale, mapping) = parse_tuning_files(arg)?;
    Some(Action::LoadTuning {
        track: None,
        scale,
        mapping,
    })
}

pub fn install_actions(app: &Rc<AppState>) {
    let (tuning, reference, _) = state(app);
    let weak = Rc::downgrade(app);
    let set = gio::ActionEntry::builder("tuning")
        .parameter_type(Some(&String::static_variant_type()))
        .state(tuning.to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            match action_for(&v) {
                Some(action) => a.dispatch(action),
                None => {
                    tracing::warn!("tuning: '{v}' is not 12tet, edo=<n> or scl=<path>[;kbm=<path>]")
                }
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let reference = gio::ActionEntry::builder("tuning-ref")
        .parameter_type(Some(&String::static_variant_type()))
        .state(reference.to_variant())
        .activate(move |_, _, param| {
            let (Some(a), Some(v)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else {
                return;
            };
            match v.trim().trim_end_matches("Hz").trim().parse::<f64>() {
                Ok(hz) if (REFERENCE_RANGE.0..=REFERENCE_RANGE.1).contains(&hz) => {
                    a.dispatch(Action::Edit(Command::SetReferencePitch { hz }));
                }
                _ => tracing::warn!(
                    "tuning-ref: '{v}' is not a frequency from {} to {} Hz",
                    REFERENCE_RANGE.0,
                    REFERENCE_RANGE.1
                ),
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let other = gio::ActionEntry::builder("tuning-ref-other")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                reference_dialog(&a);
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let scale = gio::ActionEntry::builder("tuning-load-scale")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::PromptTuning {
                    track: None,
                    file: TuningFile::Scale,
                });
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let mapping = gio::ActionEntry::builder("tuning-load-mapping")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::PromptTuning {
                    track: None,
                    file: TuningFile::Mapping,
                });
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let standard = gio::ActionEntry::builder("tuning-standard-mapping")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::LoadKeyboardMapping {
                    track: None,
                    mapping: None,
                });
            }
        })
        .build();
    app.app
        .add_action_entries([set, reference, other, scale, mapping, standard]);
}

/// The Audio menu's Tuning submenu.
pub fn menu() -> gio::Menu {
    let item = |label: &str, action: &str, target: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        i
    };
    let m = gio::Menu::new();
    let equal = gio::Menu::new();
    for steps in EQUAL_TEMPERAMENTS {
        let (label, target) = if steps == 12 {
            ("12-TET (Standard)".to_string(), "12tet".to_string())
        } else {
            (format!("{steps}-EDO"), format!("edo={steps}"))
        };
        equal.append_item(&item(&label, "app.tuning", &target));
    }
    let more = gio::Menu::new();
    for steps in MORE_EQUAL_TEMPERAMENTS {
        more.append_item(&item(
            &format!("{steps}-EDO"),
            "app.tuning",
            &format!("edo={steps}"),
        ));
    }
    equal.append_submenu(Some("More Equal Temperaments"), &more);
    m.append_section(None, &equal);
    let files = gio::Menu::new();
    files.append(Some("Load Scala Scale…"), Some("app.tuning-load-scale"));
    files.append(
        Some("Load Keyboard Mapping…"),
        Some("app.tuning-load-mapping"),
    );
    files.append(
        Some("Standard Keyboard Mapping"),
        Some("app.tuning-standard-mapping"),
    );
    m.append_section(None, &files);
    let pitch = gio::Menu::new();
    for hz in REFERENCE_PITCHES {
        pitch.append_item(&item(
            &format!("A = {} Hz", trim_hz(hz)),
            "app.tuning-ref",
            &trim_hz(hz),
        ));
    }
    let other = gio::Menu::new();
    other.append(Some("Other…"), Some("app.tuning-ref-other"));
    pitch.append_section(None, &other);
    let section = gio::Menu::new();
    section.append_submenu(Some("Reference Pitch"), &pitch);
    m.append_section(None, &section);
    m
}

/// After a change: the menu shows the project's tuning.
pub fn follow(app: &Rc<AppState>) {
    let (tuning, reference, mapped) = state(app);
    let action = |name: &str| {
        app.app
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    };
    for (name, now) in [("tuning", tuning), ("tuning-ref", reference)] {
        if let Some(a) = action(name)
            && a.state().and_then(|v| v.get::<String>()).as_deref() != Some(now.as_str())
        {
            a.set_state(&now.to_variant());
        }
    }
    if let Some(a) = action("tuning-standard-mapping") {
        a.set_enabled(mapped);
    }
}

/// Ask for a reference pitch in Hz.
fn reference_dialog(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let now = app.session.borrow().project().reference_hz();
    let win = gtk::Window::builder()
        .title("Reference Pitch")
        .modal(true)
        .transient_for(&main)
        .resizable(false)
        .default_width(320)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let label = gtk::Label::new(Some("A4 in Hz (every tuning moves with it):"));
    label.set_halign(gtk::Align::Start);
    let spin = gtk::SpinButton::with_range(REFERENCE_RANGE.0, REFERENCE_RANGE.1, 0.5);
    spin.set_digits(2);
    spin.set_value(now);
    spin.set_activates_default(true);
    body.append(&label);
    body.append(&spin);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label("Set");
    confirm.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&confirm);
    body.append(&buttons);
    win.set_child(Some(&body));
    win.set_default_widget(Some(&confirm));
    let w = win.clone();
    cancel.connect_clicked(move |_| w.close());
    let weak = Rc::downgrade(app);
    let w = win.clone();
    confirm.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Edit(Command::SetReferencePitch {
                hz: spin.value(),
            }));
        }
        w.close();
    });
    win.present();
}

fn scala_filters(file: TuningFile) -> gio::ListStore {
    let own = gtk::FileFilter::new();
    match file {
        TuningFile::Scale => {
            own.set_name(Some("Scala scales (.scl)"));
            own.add_suffix("scl");
        }
        TuningFile::Mapping => {
            own.set_name(Some("Scala keyboard mappings (.kbm)"));
            own.add_suffix("kbm");
        }
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&own);
    store.append(&all);
    store
}

/// `UiRequest::ChooseTuning`: pick the file, then load it.
pub fn choose(app: &Rc<AppState>, track: Option<TrackId>, file: TuningFile) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let title = match file {
        TuningFile::Scale => "Load Scala Scale",
        TuningFile::Mapping => "Load Keyboard Mapping",
    };
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .accept_label("Load")
        .modal(true)
        .filters(&scala_filters(file))
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(f), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = f.path() else {
            app.session
                .borrow_mut()
                .notify(NoticeLevel::Warning, "only local files can be loaded");
            return;
        };
        app.dispatch(match file {
            TuningFile::Scale => Action::LoadTuning {
                track,
                scale: path,
                mapping: None,
            },
            TuningFile::Mapping => Action::LoadKeyboardMapping {
                track,
                mapping: Some(path),
            },
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tuning_action_reads_its_parameter() {
        let tuning = |arg: &str| match action_for(arg) {
            Some(Action::Edit(Command::SetTuning {
                track: None,
                tuning,
            })) => Some(tuning),
            _ => None,
        };
        assert_eq!(tuning("12tet"), Some(None));
        assert_eq!(tuning("edo=12"), Some(None), "12 steps: no tuning");
        assert_eq!(tuning("edo=19"), Some(Some(Tuning::equal(19))));
        assert_eq!(tuning("edo=0"), None);
        assert_eq!(tuning("edo=x"), None);
        assert!(matches!(
            action_for("scl=/a/b.scl;kbm=/a/c.kbm"),
            Some(Action::LoadTuning {
                track: None,
                mapping: Some(_),
                ..
            })
        ));
        assert!(action_for("nonsense").is_none());
    }
}
