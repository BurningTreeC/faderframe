//! Capture Audio (Transport → Capture Audio, Ctrl+Alt+C): what the armed
//! tracks were played while nobody recorded becomes takes. Its settings
//! (`Preferences::audio_capture`: on/off — also Transport → Keep Input for
//! Capture Audio, a row in [`crate::toggles`] —, how long, the memory
//! limit, which tracks) are on the Recording page of the Preferences,
//! given to the session at start-up and saved whenever the session's
//! change. See `faderframe_session::audio_capture`.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_session::audio_capture::{
    AudioCaptureSettings, CaptureTracks, MAX_MEGABYTES, MAX_SECONDS, MIN_MEGABYTES, MIN_SECONDS,
};
use faderframe_session::{Action, Session};
use gtk::gio;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// The settings last written (or read).
    static SAVED: RefCell<Option<AudioCaptureSettings>> = const { RefCell::new(None) };
}

/// Give the session the saved settings (start-up).
pub fn apply(session: &mut Session, prefs: &Preferences) {
    SAVED.with(|s| *s.borrow_mut() = Some(prefs.audio_capture));
    session.set_audio_capture(prefs.audio_capture);
}

/// Save the session's settings if they changed since the last call.
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let now = s.audio_capture_settings();
    drop(s);
    if SAVED.with(|saved| *saved.borrow() == Some(now)) {
        return;
    }
    let mut p = Preferences::load();
    p.audio_capture = now;
    if let Err(e) = p.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
    SAVED.with(|saved| *saved.borrow_mut() = Some(now));
}

/// The settings with `change` made, given to the session (saved by
/// [`follow`]).
pub(crate) fn change(app: &Rc<AppState>, change: impl FnOnce(&mut AudioCaptureSettings)) {
    let mut s = app.session.borrow().audio_capture_settings();
    change(&mut s);
    app.with_session(|session| {
        session.set_audio_capture(s);
        Ok(())
    });
    follow(app);
}

/// Development aid: `audio-capture:<on|off|seconds=<n>|limit=<MB>|
/// tracks=<armed|monitored|all>|status>` sets what Capture Audio keeps
/// (and saves it) or logs what it keeps.
fn dev(app: &Rc<AppState>, arg: &str) {
    match arg.split_once('=') {
        None if arg == "on" => change(app, |s| s.on = true),
        None if arg == "off" => change(app, |s| s.on = false),
        None if arg == "status" => {
            let s = app.session.borrow();
            let st = s.audio_capture_status();
            tracing::info!(
                "audio capture: {} · {} channel(s) of {} track(s) · {:.1} s kept · {} of {} bytes · {} frames lost · can capture: {}",
                if st.on { "on" } else { "off" },
                st.channels,
                st.tracks.len(),
                st.seconds,
                st.bytes,
                st.limit_bytes,
                st.lost_frames,
                s.can_capture_audio()
            );
        }
        Some(("seconds", v)) => match v.trim().parse::<u32>() {
            Ok(n) => change(app, |s| s.seconds = n),
            Err(_) => tracing::warn!("audio-capture: seconds '{v}'"),
        },
        Some(("limit", v)) => match v.trim().parse::<u32>() {
            Ok(n) => change(app, |s| s.max_megabytes = n),
            Err(_) => tracing::warn!("audio-capture: limit '{v}'"),
        },
        Some(("tracks", v)) => match CaptureTracks::from_id(v.trim()) {
            Some(t) => change(app, |s| s.tracks = t),
            None => tracing::warn!("audio-capture: tracks '{v}'"),
        },
        _ => tracing::warn!("audio-capture: unknown '{arg}'"),
    }
}

pub fn install_actions(app: &Rc<AppState>) {
    let weak = Rc::downgrade(app);
    let capture = gio::ActionEntry::builder("capture-audio")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                a.dispatch(Action::CaptureAudio);
            }
        })
        .build();
    let weak = Rc::downgrade(app);
    let dev_action = gio::ActionEntry::builder("audio-capture")
        .parameter_type(Some(&String::static_variant_type()))
        .activate(move |_, _, param| {
            if let (Some(a), Some(arg)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) {
                dev(&a, &arg);
            }
        })
        .build();
    app.app.add_action_entries([capture, dev_action]);
}

/// "2.5 minutes", "1 minute", "45 seconds".
fn length_text(seconds: u32) -> String {
    if seconds.is_multiple_of(60) {
        let m = seconds / 60;
        format!("{m} minute{}", if m == 1 { "" } else { "s" })
    } else if seconds > 60 {
        format!("{:.1} minutes", f64::from(seconds) / 60.0)
    } else {
        format!("{seconds} seconds")
    }
}

/// What is kept now, in a line.
fn status_text(s: &Session) -> String {
    let st = s.audio_capture_status();
    if !st.on {
        return "Off: nothing is kept.".into();
    }
    if st.channels == 0 {
        return "Nothing is kept now: no track the setting names has an input on the running \
                audio device."
            .into();
    }
    format!(
        "Keeping {} track{} ({} input channel{}): {:.0} s so far, {:.0} of {:.0} MB{}.",
        st.tracks.len(),
        if st.tracks.len() == 1 { "" } else { "s" },
        st.channels,
        if st.channels == 1 { "" } else { "s" },
        st.seconds,
        st.bytes as f64 / 1_048_576.0,
        st.limit_bytes as f64 / 1_048_576.0,
        if st.lost_frames > 0 {
            format!(", {} frames lost", st.lost_frames)
        } else {
            String::new()
        }
    )
}

/// The Recording page's Capture Audio rows, from grid row `y` on.
pub fn page_rows(
    app: &Rc<AppState>,
    g: &gtk::Grid,
    y: i32,
    row: &impl Fn(&gtk::Grid, i32, &str, &gtk::Widget),
) {
    let settings = app.session.borrow().audio_capture_settings();
    let title = gtk::Label::new(Some("Capture Audio"));
    title.set_xalign(0.0);
    title.add_css_class("heading");
    g.attach(&title, 0, y, 2, 1);
    let on = gtk::Switch::new();
    on.set_active(settings.on);
    on.set_halign(gtk::Align::Start);
    on.set_tooltip_text(Some(
        "The inputs of the tracks below are kept in memory while nobody records, so a take \
         played without recording can still be placed: Transport → Capture Audio (Ctrl+Alt+C).",
    ));
    row(g, y + 1, "Keep input for Capture Audio", on.upcast_ref());
    let minutes = gtk::SpinButton::with_range(
        f64::from(MIN_SECONDS) / 60.0,
        f64::from(MAX_SECONDS) / 60.0,
        0.5,
    );
    minutes.set_digits(1);
    minutes.set_value(f64::from(settings.seconds) / 60.0);
    minutes.set_tooltip_text(Some(&format!(
        "How long each track's input is kept ({} to {}), as far as the memory limit allows",
        length_text(MIN_SECONDS),
        length_text(MAX_SECONDS)
    )));
    row(g, y + 2, "Keep per track (minutes)", minutes.upcast_ref());
    let limit = gtk::SpinButton::with_range(
        f64::from(MIN_MEGABYTES.max(16)),
        f64::from(MAX_MEGABYTES),
        16.0,
    );
    limit.set_value(f64::from(settings.max_megabytes));
    limit.set_tooltip_text(Some(
        "All kept input together at most this much memory: 48 kHz is 11.5 MB per channel and \
         minute. Over it, each track keeps less.",
    ));
    row(g, y + 3, "Memory limit (MB)", limit.upcast_ref());
    let names: Vec<&str> = CaptureTracks::ALL.iter().map(|t| t.label()).collect();
    let tracks = gtk::DropDown::from_strings(&names);
    tracks.set_selected(
        CaptureTracks::ALL
            .iter()
            .position(|t| *t == settings.tracks)
            .unwrap_or(0) as u32,
    );
    row(g, y + 4, "Keep the input of", tracks.upcast_ref());
    let status = gtk::Label::new(Some(&status_text(&app.session.borrow())));
    status.set_wrap(true);
    status.set_max_width_chars(80);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    g.attach(&status, 0, y + 5, 2, 1);
    let refresh = {
        let (weak, status) = (Rc::downgrade(app), status.clone());
        move || {
            if let Some(app) = weak.upgrade()
                && let Ok(s) = app.session.try_borrow()
            {
                status.set_text(&status_text(&s));
            }
        }
    };
    {
        let weak = Rc::downgrade(app);
        let refresh = refresh.clone();
        on.connect_active_notify(move |b| {
            let v = b.is_active();
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.on = v);
            }
            refresh();
        });
    }
    {
        let weak = Rc::downgrade(app);
        minutes.connect_value_changed(move |b| {
            let v = (b.value() * 60.0).round() as u32;
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.seconds = v);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        limit.connect_value_changed(move |b| {
            let v = b.value().round() as u32;
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.max_megabytes = v);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        let refresh = refresh.clone();
        tracks.connect_selected_notify(move |d| {
            let t = CaptureTracks::ALL[d.selected() as usize % CaptureTracks::ALL.len()];
            if let Some(app) = weak.upgrade() {
                change(&app, |s| s.tracks = t);
            }
            refresh();
        });
    }
    // What is kept grows while the page is open.
    let weak = status.downgrade();
    gtk::glib::timeout_add_seconds_local(1, move || {
        if weak.upgrade().is_none() {
            return gtk::glib::ControlFlow::Break;
        }
        refresh();
        gtk::glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_as_minutes_or_seconds() {
        assert_eq!(length_text(MIN_SECONDS), "10 seconds");
        assert_eq!(length_text(60), "1 minute");
        assert_eq!(length_text(150), "2.5 minutes");
        assert_eq!(length_text(MAX_SECONDS), "10 minutes");
    }
}
