//! Recording options: Transport-menu radio items, the Preferences page,
//! persistence. Every change goes through [`apply`], which updates the
//! session, the saved preferences and the menu state together.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_engine::MetronomeMode;
use faderframe_session::{Action, LoopRecordMode, RecordMode, RecordSettings, TransportAction};
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

const PREROLL_BARS: [u32; 4] = [0, 1, 2, 4];

fn preroll_label(bars: u32) -> String {
    match bars {
        0 => "No pre-roll".into(),
        1 => "1 bar".into(),
        n => format!("{n} bars"),
    }
}

/// Change the recording settings everywhere.
pub fn apply(app: &Rc<AppState>, r: RecordSettings) {
    app.dispatch(Action::SetRecordSettings(r));
    let mut p = Preferences::load();
    p.set_record_settings(&r);
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
    sync_actions(app, &r);
}

fn set_state(app: &Rc<AppState>, name: &str, value: &str) {
    if let Some(a) = app
        .app
        .lookup_action(name)
        .and_downcast::<gio::SimpleAction>()
    {
        a.set_state(&value.to_variant());
    }
}

fn sync_actions(app: &Rc<AppState>, r: &RecordSettings) {
    set_state(app, "record-mode", r.mode.id());
    set_state(app, "loop-record-mode", r.loop_mode.id());
    set_state(app, "metronome", r.metronome.id());
    set_state(app, "preroll", &r.preroll_bars.to_string());
}

fn radio(
    app: &Rc<AppState>,
    name: &str,
    initial: &str,
    update: impl Fn(&mut RecordSettings, &str) + 'static,
) -> gio::ActionEntry<gtk::Application> {
    let weak = Rc::downgrade(app);
    gio::ActionEntry::builder(name)
        .parameter_type(Some(&String::static_variant_type()))
        .state(initial.to_variant())
        .activate(move |_, action, param| {
            let (Some(app), Some(id)) = (weak.upgrade(), param.and_then(|p| p.get::<String>()))
            else {
                return;
            };
            action.set_state(&id.to_variant());
            let mut r = app.session.borrow().record;
            update(&mut r, &id);
            apply(&app, r);
        })
        .build()
}

pub fn install_actions(app: &Rc<AppState>) {
    let r = app.session.borrow().record;
    let weak = Rc::downgrade(app);
    let punch_from_loop = gio::ActionEntry::builder("punch-from-loop")
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                let range = app.session.borrow().project().loop_range;
                app.dispatch(Action::Transport(TransportAction::SetPunch(range)));
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let punch = gio::ActionEntry::builder("punch")
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                app.dispatch(Action::Transport(TransportAction::TogglePunch));
            }
        })
        .build();
    app.app.add_action_entries([
        radio(app, "record-mode", r.mode.id(), |r, id| {
            r.mode = RecordMode::from_id(id).unwrap_or(r.mode);
        }),
        radio(app, "loop-record-mode", r.loop_mode.id(), |r, id| {
            r.loop_mode = LoopRecordMode::from_id(id).unwrap_or(r.loop_mode);
        }),
        radio(app, "metronome", r.metronome.id(), |r, id| {
            r.metronome = MetronomeMode::from_id(id).unwrap_or(r.metronome);
        }),
        radio(app, "preroll", &r.preroll_bars.to_string(), |r, id| {
            r.preroll_bars = id.parse().unwrap_or(r.preroll_bars);
        }),
        punch,
        punch_from_loop,
    ]);
    app.app.set_accels_for_action("app.punch", &["<Control>p"]);
}

fn radio_menu(action: &str, items: impl IntoIterator<Item = (String, String)>) -> gio::Menu {
    let m = gio::Menu::new();
    for (label, id) in items {
        let item = gio::MenuItem::new(Some(&label), None);
        item.set_action_and_target_value(Some(action), Some(&id.to_variant()));
        m.append_item(&item);
    }
    m
}

/// The recording part of the Transport menu.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    let punch = gio::Menu::new();
    punch.append(Some("Punch In/Out"), Some("app.punch"));
    punch.append(Some("Set Punch Range to Loop"), Some("app.punch-from-loop"));
    m.append_section(None, &punch);
    let opts = gio::Menu::new();
    opts.append_submenu(
        Some("Record Mode"),
        &radio_menu(
            "app.record-mode",
            RecordMode::ALL.map(|x| (x.label().to_string(), x.id().to_string())),
        ),
    );
    opts.append_submenu(
        Some("Loop Recording"),
        &radio_menu(
            "app.loop-record-mode",
            LoopRecordMode::ALL.map(|x| (x.label().to_string(), x.id().to_string())),
        ),
    );
    opts.append_submenu(
        Some("Metronome"),
        &radio_menu(
            "app.metronome",
            MetronomeMode::ALL.map(|x| (x.label().to_string(), x.id().to_string())),
        ),
    );
    opts.append_submenu(
        Some("Pre-roll"),
        &radio_menu(
            "app.preroll",
            PREROLL_BARS.map(|b| (preroll_label(b), b.to_string())),
        ),
    );
    m.append_section(None, &opts);
    m
}

fn dropdown<T: Copy + PartialEq + 'static>(
    options: &[T],
    label: impl Fn(T) -> String,
    current: T,
) -> gtk::DropDown {
    let names: Vec<String> = options.iter().map(|o| label(*o)).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let d = gtk::DropDown::from_strings(&refs);
    d.set_selected(options.iter().position(|o| *o == current).unwrap_or(0) as u32);
    d
}

/// The Recording page of the Preferences window.
pub fn page(app: &Rc<AppState>, row: impl Fn(&gtk::Grid, i32, &str, &gtk::Widget)) -> gtk::Widget {
    let g = gtk::Grid::new();
    g.set_row_spacing(10);
    g.set_column_spacing(14);
    g.add_css_class("audio-settings");
    let r = app.session.borrow().record;
    let mode = dropdown(&RecordMode::ALL, |m| m.label().to_string(), r.mode);
    row(&g, 0, "Recording over clips", mode.upcast_ref());
    let loop_mode = dropdown(&LoopRecordMode::ALL, |m| m.label().to_string(), r.loop_mode);
    row(&g, 1, "Loop recording", loop_mode.upcast_ref());
    let metronome = dropdown(&MetronomeMode::ALL, |m| m.label().to_string(), r.metronome);
    row(&g, 2, "Metronome", metronome.upcast_ref());
    let preroll = dropdown(&PREROLL_BARS, preroll_label, r.preroll_bars);
    row(&g, 3, "Pre-roll", preroll.upcast_ref());
    let offset = gtk::SpinButton::with_range(-4096.0, 4096.0, 1.0);
    offset.set_value(r.latency_offset as f64);
    offset.set_tooltip_text(Some(
        "Added to the latency the audio driver reports. Positive values move new takes earlier.",
    ));
    row(
        &g,
        4,
        "Extra latency compensation (frames)",
        offset.upcast_ref(),
    );
    let help = gtk::Label::new(Some(
        "Takes: recording over existing clips keeps them as takes in a take folder and comps the new \
         take in — open the folder's take lanes to comp by clicking or swiping. Replace: existing \
         clips are cut back underneath the new take.\n\nLoop recording keeps every pass as a take, \
         only the last pass, or puts earlier passes on new muted tracks. Takes are written as 32-bit \
         float WAV into the project's Audio folder and compensated for input and output latency.",
    ));
    help.set_wrap(true);
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    g.attach(&help, 0, 5, 2, 1);

    let update = {
        let weak = Rc::downgrade(app);
        let (mode, loop_mode, metronome, preroll, offset) = (
            mode.clone(),
            loop_mode.clone(),
            metronome.clone(),
            preroll.clone(),
            offset.clone(),
        );
        move || {
            let Some(app) = weak.upgrade() else { return };
            let mut r = app.session.borrow().record;
            r.mode = RecordMode::ALL[mode.selected() as usize % RecordMode::ALL.len()];
            r.loop_mode =
                LoopRecordMode::ALL[loop_mode.selected() as usize % LoopRecordMode::ALL.len()];
            r.metronome =
                MetronomeMode::ALL[metronome.selected() as usize % MetronomeMode::ALL.len()];
            r.preroll_bars = PREROLL_BARS[preroll.selected() as usize % PREROLL_BARS.len()];
            r.latency_offset = offset.value() as i64;
            if r != app.session.borrow().record {
                apply(&app, r);
            }
        }
    };
    let update = Rc::new(update);
    for d in [&mode, &loop_mode, &metronome, &preroll] {
        let u = Rc::clone(&update);
        d.connect_selected_notify(move |_| u());
    }
    offset.connect_value_changed(move |_| update());
    g.upcast()
}
