//! Audio → Low-Latency Monitoring: tracks heard live (a monitored input,
//! a MIDI keyboard) skip plugin delay compensation while it is on — their
//! effects with latency bypassed, summed as they come — so the player
//! hears themselves at once. Remembered; playback and renders stay
//! aligned.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::Action;
use gtk::gio;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

thread_local! {
    /// What was last shown and saved.
    static LAST: Cell<Option<bool>> = const { Cell::new(None) };
    static LAST_LIMIT: Cell<Option<f32>> = const { Cell::new(None) };
}

pub fn install_actions(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let on = app.session.borrow().low_latency_monitoring();
    let toggle = gio::ActionEntry::builder("low-latency-monitoring")
        .state(on.to_variant())
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                let on = a.session.borrow().low_latency_monitoring();
                a.dispatch(Action::SetLowLatencyMonitoring(!on));
            }
        })
        .build();
    app.app.add_action_entries([toggle]);
}

/// The Audio menu's section.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    m.append(
        Some("Low-Latency Monitoring"),
        Some("app.low-latency-monitoring"),
    );
    m
}

/// Show the setting, save it when it changed, and say what it does when it
/// is turned on (after a change).
pub fn follow(app: &Rc<AppState>) {
    let limit = app.session.borrow().low_latency_limit();
    if LAST_LIMIT.with(|l| l.replace(Some(limit))) != Some(limit) {
        let mut p = Preferences::load();
        if p.low_latency_limit_ms != limit {
            p.low_latency_limit_ms = limit;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
        }
    }
    let on = app.session.borrow().low_latency_monitoring();
    if let Some(a) = app
        .app
        .lookup_action("low-latency-monitoring")
        .and_downcast::<gio::SimpleAction>()
        && a.state().and_then(|v| v.get::<bool>()) != Some(on)
    {
        a.set_state(&on.to_variant());
    }
    let last = LAST.with(|l| l.replace(Some(on)));
    if last == Some(on) {
        return;
    }
    if last.is_some()
        && on
        && let Ok(mut s) = app.session.try_borrow_mut()
    {
        s.notify(
            faderframe_session::NoticeLevel::Info,
            "Low-latency monitoring: tracks heard live skip plugin delay compensation, and \
             effects with latency on their way to the outputs are bypassed. Playback and \
             renders stay aligned.",
        );
    }
    let mut p = Preferences::load();
    if p.low_latency_monitoring != on {
        p.low_latency_monitoring = on;
        if let Err(e) = p.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
    }
}
