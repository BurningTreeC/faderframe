//! Self-freezing's settings (`Preferences::self_freeze`, Preferences →
//! Audio): given to the session at start-up and saved whenever the
//! session's change (the page, a plugin kept live from the mixer, a dev
//! action).

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::Session;
use faderframe_session::self_freeze::{SelfFreezeMode, SelfFreezeSettings};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The settings last written (or read).
    static SAVED: RefCell<Option<SelfFreezeSettings>> = const { RefCell::new(None) };
}

/// Give the session the saved settings (start-up).
pub fn apply(session: &mut Session, prefs: &Preferences) {
    SAVED.with(|s| *s.borrow_mut() = Some(prefs.self_freeze.clone()));
    session.set_self_freeze(prefs.self_freeze.clone());
}

/// Save the session's settings if they changed since the last call.
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let now = s.self_freeze_settings().clone();
    drop(s);
    if SAVED.with(|saved| saved.borrow().as_ref() == Some(&now)) {
        return;
    }
    let mut p = Preferences::load();
    p.self_freeze = now.clone();
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
    SAVED.with(|saved| *saved.borrow_mut() = Some(now));
}

/// The settings with `change` made, given to the session (saved by
/// [`follow`]).
fn change(app: &Rc<AppState>, change: impl FnOnce(&mut SelfFreezeSettings)) {
    let mut s = app.session.borrow().self_freeze_settings().clone();
    change(&mut s);
    app.with_session(|session| {
        session.set_self_freeze(s);
        Ok(())
    });
}

/// The choices of when caches are made, in the drop-down's order.
const WHEN: [&str; 3] = ["Off", "When Needed", "Always"];

fn when_index(s: &SelfFreezeSettings) -> u32 {
    match (s.on, s.mode) {
        (false, _) => 0,
        (true, SelfFreezeMode::WhenNeeded) => 1,
        (true, SelfFreezeMode::Always) => 2,
    }
}

fn set_when(s: &mut SelfFreezeSettings, i: u32) {
    match i {
        0 => s.on = false,
        2 => {
            s.on = true;
            s.mode = SelfFreezeMode::Always;
        }
        _ => {
            s.on = true;
            s.mode = SelfFreezeMode::WhenNeeded;
        }
    }
}

/// The page's controls: off, when needed or always, the cache folder's
/// size, the cheapest chain worth a cache.
pub fn controls(app: &Rc<AppState>) -> gtk::Box {
    let settings = app.session.borrow().self_freeze_settings().clone();
    // Off, When Needed (the default) or Always.
    let when = gtk::DropDown::from_strings(&WHEN);
    when.set_selected(when_index(&settings));
    when.set_tooltip_text(Some(
        "Tracks whose devices are left alone are rendered in the background and played \
         from a file of what their devices make, so a heavy project plays on a small \
         machine. Fader, pan and sends stay live; an edit, an open editor, arming or \
         live MIDI brings a track's devices back at once. The files go into the \
         project's Cache folder.\n\nWhen Needed: only while the machine struggles (the \
         audio load high for a few seconds, or a dropout while playing) — a machine \
         that copes hears every track's devices live. Always: every track worth it. \
         Cached tracks stop with a short fade instead of their devices' tails.",
    ));
    {
        let weak = Rc::downgrade(app);
        when.connect_selected_notify(move |d| {
            let i = d.selected();
            if let Some(app) = weak.upgrade() {
                change(&app, |s| set_when(s, i));
            }
        });
    }
    let size = gtk::SpinButton::with_range(0.5, 512.0, 0.5);
    size.set_digits(1);
    size.set_value(settings.max_megabytes as f64 / 1024.0);
    size.set_tooltip_text(Some(
        "The cache folder's size: over it, the files played least recently go first",
    ));
    {
        let weak = Rc::downgrade(app);
        size.connect_value_changed(move |b| {
            let mb = (b.value() * 1024.0).round() as u64;
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.max_megabytes = mb);
            }
        });
    }
    let cost = gtk::SpinButton::with_range(0.0, 100.0, 0.5);
    cost.set_digits(1);
    cost.set_value(f64::from(settings.min_cost_percent));
    cost.set_tooltip_text(Some(
        "Tracks whose devices take less of one processor core than this play live \
         (not worth a file)",
    ));
    {
        let weak = Rc::downgrade(app);
        cost.connect_value_changed(move |b| {
            let v = b.value() as f32;
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.min_cost_percent = v);
            }
        });
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&gtk::Label::new(Some(
        "Play tracks nobody touches from a cache",
    )));
    row.append(&when);
    row.append(&gtk::Label::new(Some("up to")));
    row.append(&size);
    row.append(&gtk::Label::new(Some("GB, chains from")));
    row.append(&cost);
    row.append(&gtk::Label::new(Some("% of a core")));
    if !settings.never.is_empty() {
        let kept = gtk::Button::with_label(&format!(
            "Let {} plugin{} kept live be cached",
            settings.never.len(),
            if settings.never.len() == 1 { "" } else { "s" }
        ));
        kept.set_tooltip_text(Some(&settings.never.join(", ")));
        let weak = Rc::downgrade(app);
        kept.connect_clicked(move |b| {
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.never.clear());
                b.set_visible(false);
            }
        });
        row.append(&kept);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drop_down_reads_and_sets_off_when_needed_and_always() {
        let mut s = SelfFreezeSettings::default();
        assert_eq!(WHEN[when_index(&s) as usize], "When Needed");
        for (i, name) in WHEN.iter().enumerate() {
            set_when(&mut s, i as u32);
            assert_eq!(WHEN[when_index(&s) as usize], *name);
        }
        set_when(&mut s, 0);
        assert!(!s.on);
        set_when(&mut s, 2);
        assert_eq!((s.on, s.mode), (true, SelfFreezeMode::Always));
    }
}
