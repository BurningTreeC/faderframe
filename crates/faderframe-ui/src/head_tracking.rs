//! Head tracking for binaural listening (Audio → Listen): on and off
//! (`app.toggle-head-tracking`, a row in [`crate::toggles`]), Recentre Head
//! (`app.recentre-head`), Straighten Head (`app.straighten-head`) and the
//! settings window (`app.head-tracking-settings`: the UDP port, the glide,
//! the axes a tracker counts the other way, radians). The settings are
//! remembered (`Preferences::head_tracking`); the head itself is per
//! session. See `faderframe_session::headtrack`.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::headtrack::{HeadOp, HeadTrackingSettings};
use faderframe_session::{Action, Session};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The settings last written (or read).
    static SAVED: RefCell<Option<HeadTrackingSettings>> = const { RefCell::new(None) };
}

/// Give the session the saved settings (start-up).
pub fn apply(session: &mut Session, prefs: &Preferences) {
    SAVED.with(|s| *s.borrow_mut() = Some(prefs.head_tracking.clone()));
    session.set_head_tracking(prefs.head_tracking.clone());
}

/// Save the session's settings if they changed since the last call.
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let now = s.head_tracking().clone();
    drop(s);
    if SAVED.with(|saved| saved.borrow().as_ref() == Some(&now)) {
        return;
    }
    let mut p = Preferences::load();
    p.head_tracking = now.clone();
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
    SAVED.with(|saved| *saved.borrow_mut() = Some(now));
}

pub fn install_actions(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let recentre = gio::ActionEntry::builder("recentre-head")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Head(HeadOp::Recentre));
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let straighten = gio::ActionEntry::builder("straighten-head")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Head(HeadOp::Straight));
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let settings = gio::ActionEntry::builder("head-tracking-settings")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                open(&a);
            }
        })
        .build();
    app.app.add_action_entries([recentre, straighten, settings]);
}

/// The Listen menu's head-tracking section.
pub fn menu() -> gio::Menu {
    let m = gio::Menu::new();
    m.append(Some("Head Tracking"), Some("app.toggle-head-tracking"));
    m.append(Some("Recentre Head"), Some("app.recentre-head"));
    m.append(Some("Straighten Head"), Some("app.straighten-head"));
    m.append(
        Some("Head Tracking Settings…"),
        Some("app.head-tracking-settings"),
    );
    m
}

/// The settings with `change` made, given to the session (saved by
/// [`follow`]).
fn change(app: &Rc<AppState>, change: impl FnOnce(&mut HeadTrackingSettings)) {
    let mut s = app.session.borrow().head_tracking().clone();
    change(&mut s);
    app.dispatch(Action::Head(HeadOp::Tracking(s)));
}

/// What the status line says.
fn status_text(s: &Session) -> String {
    let st = s.head_status();
    let (yaw, pitch, roll) = st.ypr;
    let head = format!("Head: yaw {yaw:+.0}°, pitch {pitch:+.0}°, roll {roll:+.0}°");
    if let Some(e) = st.error {
        return format!("{head}\n{e}");
    }
    let Some(port) = st.listening else {
        return format!("{head}\nNot listening for a tracker.");
    };
    let how = if st.shared {
        format!("on UDP port {port} (with the OSC control surface there)")
    } else {
        format!("on UDP port {port}")
    };
    match st.last_heard {
        Some(t) if t < faderframe_session::headtrack::TRACKER_QUIET => {
            format!("{head}\nListening {how}: the tracker is sending.")
        }
        Some(t) => format!(
            "{head}\nListening {how}: nothing for {:.0} s.",
            t.as_secs_f32()
        ),
        None => format!("{head}\nListening {how}: no tracker yet."),
    }
}

/// The settings window.
fn open(app: &Rc<AppState>) {
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let settings = app.session.borrow().head_tracking().clone();
    let win = gtk::Window::builder()
        .title("Head Tracking")
        .application(&app.app)
        .transient_for(&main)
        .default_width(520)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for m in [
        gtk::Box::set_margin_top,
        gtk::Box::set_margin_bottom,
        gtk::Box::set_margin_start,
        gtk::Box::set_margin_end,
    ] {
        m(&body, 16);
    }
    let intro = gtk::Label::new(Some(
        "On headphones, a head tracker turns the scene as your head turns: beds, objects and \
         ambisonic fields stay where they are in the room. Point the tracker (or a phone app \
         that sends its orientation) at this computer's UDP port. FaderFrame reads \
         /SceneRotator/ypr, /ypr (yaw, pitch, roll), /SceneRotator/quaternions and \
         /quaternion (w, x, y, z), and the single angles and components.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.set_max_width_chars(64);
    body.append(&intro);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);
    let label = |text: &str| {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(1.0);
        l
    };
    let on = gtk::CheckButton::with_label("Listen for a head tracker");
    on.set_active(settings.enabled);
    {
        let weak = Rc::downgrade(app);
        on.connect_toggled(move |b| {
            let v = b.is_active();
            if let Some(a) = weak.upgrade() {
                change(&a, |s| s.enabled = v);
            }
        });
    }
    grid.attach(&on, 1, 0, 1, 1);
    let port = gtk::SpinButton::with_range(1024.0, 65535.0, 1.0);
    port.set_value(f64::from(settings.port));
    {
        let weak = Rc::downgrade(app);
        port.connect_value_changed(move |b| {
            let v = b.value() as u16;
            if let Some(a) = weak.upgrade() {
                change(&a, |s| s.port = v);
            }
        });
    }
    grid.attach(&label("UDP port"), 0, 1, 1, 1);
    grid.attach(&port, 1, 1, 1, 1);
    let smoothing = gtk::SpinButton::with_range(0.0, 500.0, 5.0);
    smoothing.set_value(f64::from(settings.smoothing_ms));
    smoothing.set_tooltip_text(Some(
        "How long the head glides to where the tracker points it (a tracker sends 30 to 100 \
         times a second; a little smoothing hides its steps)",
    ));
    {
        let weak = Rc::downgrade(app);
        smoothing.connect_value_changed(move |b| {
            let v = b.value() as f32;
            if let Some(a) = weak.upgrade() {
                change(&a, |s| s.smoothing_ms = v);
            }
        });
    }
    grid.attach(&label("Smoothing (ms)"), 0, 2, 1, 1);
    grid.attach(&smoothing, 1, 2, 1, 1);
    let axes = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let check = |text: &str, active: bool, set: fn(&mut HeadTrackingSettings, bool)| {
        let b = gtk::CheckButton::with_label(text);
        b.set_active(active);
        let weak = Rc::downgrade(app);
        b.connect_toggled(move |b| {
            let v = b.is_active();
            if let Some(a) = weak.upgrade() {
                change(&a, |s| set(s, v));
            }
        });
        b
    };
    axes.append(&check("Yaw", settings.invert_yaw, |s, v| s.invert_yaw = v));
    axes.append(&check("Pitch", settings.invert_pitch, |s, v| {
        s.invert_pitch = v
    }));
    axes.append(&check("Roll", settings.invert_roll, |s, v| {
        s.invert_roll = v
    }));
    grid.attach(&label("Invert"), 0, 3, 1, 1);
    grid.attach(&axes, 1, 3, 1, 1);
    let radians = check("Angles in radians", settings.radians, |s, v| s.radians = v);
    grid.attach(&radians, 1, 4, 1, 1);
    body.append(&grid);
    let status = gtk::Label::new(Some(&status_text(&app.session.borrow())));
    status.set_xalign(0.0);
    status.set_wrap(true);
    body.append(&status);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    for (text, op) in [
        ("Straighten", HeadOp::Straight),
        ("Recentre", HeadOp::Recentre),
    ] {
        let b = gtk::Button::with_label(text);
        let weak = Rc::downgrade(app);
        b.connect_clicked(move |_| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::Head(op.clone()));
            }
        });
        buttons.append(&b);
    }
    body.append(&buttons);
    win.set_child(Some(&body));
    // The status follows the tracker while the window is open.
    let weak = Rc::downgrade(app);
    let label = status.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        let (Some(a), Some(l)) = (weak.upgrade(), label.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if let Ok(s) = a.session.try_borrow() {
            l.set_text(&status_text(&s));
        }
        glib::ControlFlow::Continue
    });
    win.present();
}
