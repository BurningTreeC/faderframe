//! Recording options: Transport-menu radio items, the Preferences page,
//! persistence. Every change goes through [`apply`], which updates the
//! session, the saved preferences and the menu state together.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_engine::MetronomeMode;
use faderframe_session::{
    Action, LoopRecordMode, RecordMode, RecordSettings, Roll, RollUnit, TransportAction,
};
use gtk::gio;
use gtk::prelude::*;
use std::rc::Rc;

/// The units the Preferences page offers.
const UNITS: [RollUnit; 3] = [RollUnit::Bars, RollUnit::Beats, RollUnit::Seconds];

fn unit_label(u: RollUnit) -> String {
    match u {
        RollUnit::Bars => "bars",
        RollUnit::Beats => "beats",
        RollUnit::Seconds => "seconds",
    }
    .into()
}

/// The menus' choices: the presets, and the current length if it is none.
fn roll_choices(current: Roll, off: &str) -> Vec<(String, String)> {
    let mut v: Vec<Roll> = Roll::PRESETS.to_vec();
    if !v.contains(&current) {
        v.push(current);
    }
    v.into_iter()
        .map(|r| {
            let label = if r.is_off() {
                off.to_string()
            } else {
                r.label()
            };
            (label, r.id())
        })
        .collect()
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

/// Metronome off, or back on in the mode it had (always, by default).
pub fn toggle_metronome(app: &Rc<AppState>) {
    let mut r = app.session.borrow().record;
    let mut p = Preferences::load();
    r.metronome = if r.metronome == MetronomeMode::Off {
        MetronomeMode::from_id(&p.metronome_on)
            .filter(|m| *m != MetronomeMode::Off)
            .unwrap_or(MetronomeMode::Always)
    } else {
        p.metronome_on = r.metronome.id().into();
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
        MetronomeMode::Off
    };
    apply(app, r);
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
    set_state(app, "preroll", &r.preroll.id());
    set_state(app, "postroll", &r.postroll.id());
    for (name, on) in [
        ("preroll-on-bar", r.preroll_on_bar),
        ("count-in", r.count_in),
    ] {
        if let Some(a) = app
            .app
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            a.set_state(&on.to_variant());
        }
    }
}

/// A recording setting switched on and off from the menu.
fn toggle(
    app: &Rc<AppState>,
    name: &str,
    initial: bool,
    update: impl Fn(&mut RecordSettings) + 'static,
) -> gio::ActionEntry<gtk::Application> {
    let weak = Rc::downgrade(app);
    gio::ActionEntry::builder(name)
        .state(initial.to_variant())
        .activate(move |_, _, _| {
            let Some(app) = weak.upgrade() else { return };
            let mut r = app.session.borrow().record;
            update(&mut r);
            apply(&app, r);
        })
        .build()
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
    let weak = Rc::downgrade(app);
    let toggle_metronome = gio::ActionEntry::builder("toggle-metronome")
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                toggle_metronome(&app);
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let metronome_settings = gio::ActionEntry::builder("metronome-settings")
        .activate(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                crate::preferences::open(&app, Some("metronome"));
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let stop_to_start = gio::ActionEntry::builder("stop-to-start")
        .state(app.session.borrow().editor.stop_to_start.to_variant())
        .activate(move |_, action, _| {
            let Some(app) = weak.upgrade() else { return };
            let on = !app.session.borrow().editor.stop_to_start;
            app.dispatch(Action::SetEditFlag(
                faderframe_session::EditFlag::StopToStart,
                on,
            ));
            action.set_state(&on.to_variant());
            let mut p = Preferences::load();
            p.stop_to_start = on;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
        })
        .build();
    app.app.add_action_entries([
        metronome_settings,
        stop_to_start,
        toggle_metronome,
        radio(app, "record-mode", r.mode.id(), |r, id| {
            r.mode = RecordMode::from_id(id).unwrap_or(r.mode);
        }),
        radio(app, "loop-record-mode", r.loop_mode.id(), |r, id| {
            r.loop_mode = LoopRecordMode::from_id(id).unwrap_or(r.loop_mode);
        }),
        radio(app, "metronome", r.metronome.id(), |r, id| {
            r.metronome = MetronomeMode::from_id(id).unwrap_or(r.metronome);
        }),
        radio(app, "preroll", &r.preroll.id(), |r, id| {
            r.preroll = Roll::from_id(id).unwrap_or(r.preroll);
        }),
        radio(app, "postroll", &r.postroll.id(), |r, id| {
            r.postroll = Roll::from_id(id).unwrap_or(r.postroll);
        }),
        toggle(app, "preroll-on-bar", r.preroll_on_bar, |r| {
            r.preroll_on_bar = !r.preroll_on_bar;
        }),
        toggle(app, "count-in", r.count_in, |r| r.count_in = !r.count_in),
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

/// The recording part of the Transport menu (its choices as `r` has them:
/// a length set in the Preferences shows too).
pub fn menu_for(r: &RecordSettings) -> gio::Menu {
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
    opts.append(Some("Metronome Settings…"), Some("app.metronome-settings"));
    let pre = radio_menu("app.preroll", roll_choices(r.preroll, "No pre-roll"));
    let how = gio::Menu::new();
    how.append(Some("Start on a Bar Line"), Some("app.preroll-on-bar"));
    how.append(Some("Count-in (Click Only)"), Some("app.count-in"));
    pre.append_section(None, &how);
    opts.append_submenu(Some("Pre-roll"), &pre);
    opts.append_submenu(
        Some("Post-roll (after Punch-out)"),
        &radio_menu("app.postroll", roll_choices(r.postroll, "No post-roll")),
    );
    m.append_section(None, &opts);
    m
}

/// An amount and its unit (pre- and post-roll on the Preferences page).
fn roll_editor(r: Roll) -> (gtk::Box, gtk::SpinButton, gtk::DropDown) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let amount = gtk::SpinButton::with_range(0.0, 64.0, 0.5);
    amount.set_digits(1);
    amount.set_value(r.amount);
    let unit = dropdown(&UNITS, unit_label, r.unit);
    b.append(&amount);
    b.append(&unit);
    (b, amount, unit)
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
    let (preroll_box, pre_amount, pre_unit) = roll_editor(r.preroll);
    row(&g, 2, "Pre-roll (0: none)", preroll_box.upcast_ref());
    let on_bar = gtk::Switch::new();
    on_bar.set_active(r.preroll_on_bar);
    on_bar.set_halign(gtk::Align::Start);
    row(&g, 5, "Pre-roll starts on a bar line", on_bar.upcast_ref());
    let count_in = gtk::Switch::new();
    count_in.set_active(r.count_in);
    count_in.set_halign(gtk::Align::Start);
    count_in.set_tooltip_text(Some(
        "During the pre-roll only the click plays; the song starts with the take.",
    ));
    row(&g, 6, "Count-in (click only)", count_in.upcast_ref());
    let (postroll_box, post_amount, post_unit) = roll_editor(r.postroll);
    postroll_box.set_tooltip_text(Some(
        "With a punch range: playback goes on this long after the punch-out, then stops.",
    ));
    row(
        &g,
        7,
        "Post-roll after punch-out",
        postroll_box.upcast_ref(),
    );
    let offset = gtk::SpinButton::with_range(-4096.0, 4096.0, 1.0);
    offset.set_value(r.latency_offset as f64);
    offset.set_tooltip_text(Some(
        "Added to the latency the audio driver reports. Positive values move new takes earlier.",
    ));
    row(
        &g,
        3,
        "Extra latency compensation (frames)",
        offset.upcast_ref(),
    );
    let help = gtk::Label::new(Some(
        "Takes: recording over existing clips keeps them as takes in a take folder and comps the new \
         take in — open the folder's take lanes to comp by clicking or swiping. Replace: existing \
         clips are cut back underneath the new take.\n\nLoop recording keeps every pass as a take, \
         only the last pass, or puts earlier passes on new muted tracks. Takes are written as 32-bit \
         float WAV into the project's Audio folder and compensated for input and output latency. \
         The metronome has a page of its own.",
    ));
    help.set_wrap(true);
    // Bounded line length: consistent height-for-width measuring.
    help.set_max_width_chars(80);
    help.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    g.attach(&help, 0, 8, 2, 1);

    let update = {
        let weak = Rc::downgrade(app);
        let (mode, loop_mode, offset) = (mode.clone(), loop_mode.clone(), offset.clone());
        let (pre_amount, pre_unit, post_amount, post_unit) = (
            pre_amount.clone(),
            pre_unit.clone(),
            post_amount.clone(),
            post_unit.clone(),
        );
        let (on_bar, count_in) = (on_bar.clone(), count_in.clone());
        move || {
            let Some(app) = weak.upgrade() else { return };
            let mut r = app.session.borrow().record;
            r.mode = RecordMode::ALL[mode.selected() as usize % RecordMode::ALL.len()];
            r.loop_mode =
                LoopRecordMode::ALL[loop_mode.selected() as usize % LoopRecordMode::ALL.len()];
            let unit = |d: &gtk::DropDown| UNITS[d.selected() as usize % UNITS.len()];
            r.preroll = Roll {
                amount: pre_amount.value(),
                unit: unit(&pre_unit),
            };
            r.postroll = Roll {
                amount: post_amount.value(),
                unit: unit(&post_unit),
            };
            r.preroll_on_bar = on_bar.is_active();
            r.count_in = count_in.is_active();
            r.latency_offset = offset.value() as i64;
            if r != app.session.borrow().record {
                apply(&app, r);
            }
        }
    };
    let update = Rc::new(update);
    for d in [&mode, &loop_mode, &pre_unit, &post_unit] {
        let u = Rc::clone(&update);
        d.connect_selected_notify(move |_| u());
    }
    for s in [&pre_amount, &post_amount] {
        let u = Rc::clone(&update);
        s.connect_value_changed(move |_| u());
    }
    for s in [&on_bar, &count_in] {
        let u = Rc::clone(&update);
        s.connect_active_notify(move |_| u());
    }
    offset.connect_value_changed(move |_| update());
    g.upcast()
}
