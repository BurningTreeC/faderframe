//! Audio → Listen → Codec Monitor: the master heard through Opus at a
//! bitrate or as a streaming platform plays it (its loudness and codec),
//! the codec's decoded signal, the original or their difference; and the
//! "Preview on Platforms" report (also from the Render window, with its
//! settings). The settings (all but whether it is on) are remembered in
//! the preferences.
//!
//! The menu's states are rows of [`crate::toggles`] (`codec-monitor`,
//! `codec-hear`, `codec-vbr`); scripts drive everything through
//! `codec-monitor:<off|toggle|opus@<kbps>[/cbr]|platform=<id>|codec|
//! original|difference|vbr|cbr|reset|measure|preview[=<id>|<id>…]>`.

use crate::prefs::Preferences;
use crate::state::AppState;
use faderframe_codec::{BITRATES, PLATFORMS, Platform};
use faderframe_session::codec::{CodecHear, CodecMonitorSettings, CodecOp, PreviewStatus};
use faderframe_session::render::RenderSettings;
use faderframe_session::{Action, Session};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

/// The `codec-monitor` choice's ids for the bitrates offered
/// ([`BITRATES`], same order).
pub(crate) const OPUS_IDS: [&str; 8] = [
    "opus@48", "opus@64", "opus@96", "opus@128", "opus@160", "opus@192", "opus@256", "opus@320",
];
/// … and for the platforms ([`PLATFORMS`], same order).
pub(crate) const PLATFORM_IDS: [&str; 6] = [
    "platform=spotify",
    "platform=apple-music",
    "platform=youtube",
    "platform=tidal",
    "platform=amazon-music",
    "platform=deezer",
];

/// The `codec-monitor` choice now: `off`, `opus@<kbps>` (`opus` for a
/// bitrate not offered) or `platform=<id>`.
pub(crate) fn choice_now(s: &Session) -> &'static str {
    if !s.codec_monitor_on() {
        return "off";
    }
    let set = s.codec_settings();
    if let Some(p) = set.platform() {
        return PLATFORMS
            .iter()
            .position(|q| q.id == p.id)
            .map_or("opus", |i| PLATFORM_IDS[i]);
    }
    BITRATES
        .iter()
        .position(|k| *k == set.kbps)
        .map_or("opus", |i| OPUS_IDS[i])
}

/// What `codec-monitor:<arg>` does (a menu choice or a script).
pub(crate) fn choose(app: &Rc<AppState>, arg: &str) {
    let arg = arg.trim();
    let vbr = app.session.borrow().codec_settings().vbr;
    let op = match arg {
        "off" => CodecOp::Off,
        "toggle" | "on" => CodecOp::Toggle,
        "vbr" => CodecOp::Vbr(true),
        "cbr" => CodecOp::Vbr(false),
        "reset" => CodecOp::ResetMeters,
        "measure" => CodecOp::Measure,
        "preview" => return preview(app, None, Vec::new()),
        other => {
            if let Some(h) = CodecHear::from_id(other) {
                CodecOp::Hear(h)
            } else if let Some(ids) = other.strip_prefix("preview=") {
                let ids = ids
                    .split('|')
                    .map(str::trim)
                    .filter(|i| !i.is_empty() && *i != "all")
                    .map(String::from)
                    .collect();
                return preview(app, None, ids);
            } else if let Some(id) = other.strip_prefix("platform=") {
                CodecOp::Platform(id.trim().to_string())
            } else if let Some(rest) = other.strip_prefix("opus@") {
                let (kbps, mode) = rest.split_once('/').unwrap_or((rest, ""));
                let Ok(kbps) = kbps.trim().parse::<u32>() else {
                    return tracing::warn!("codec-monitor: '{other}' is no bitrate");
                };
                CodecOp::Opus {
                    kbps,
                    vbr: match mode {
                        "cbr" => false,
                        "vbr" => true,
                        _ => vbr,
                    },
                }
            } else {
                return tracing::warn!(
                    "codec-monitor: '{other}' is not off|toggle|opus@<kbps>[/cbr]|platform=<id>|\
                     codec|original|difference|vbr|cbr|reset|measure|preview[=<ids>]"
                );
            }
        }
    };
    app.dispatch(Action::Codec(op));
}

/// The `codec-hear` choice now.
pub(crate) fn hear_now(s: &Session) -> &'static str {
    s.codec_settings().hear.id()
}

pub(crate) fn choose_hear(app: &Rc<AppState>, id: &str) {
    match CodecHear::from_id(id) {
        Some(h) => app.dispatch(Action::Codec(CodecOp::Hear(h))),
        None => tracing::warn!("codec-hear: unknown '{id}'"),
    }
}

/// The actions the menu names besides the toggles' rows.
pub(crate) fn install(app: &Rc<AppState>) {
    let simple = |name: &'static str, op: CodecOp| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    a.dispatch(Action::Codec(op.clone()));
                }
            })
            .build()
    };
    let weak = Rc::downgrade(app);
    let preview_entry = gio::ActionEntry::builder("codec-preview")
        .activate(move |_, _, _| {
            if let Some(a) = weak.upgrade() {
                preview(&a, None, Vec::new());
            }
        })
        .build();
    app.app.add_action_entries([
        simple("codec-reset", CodecOp::ResetMeters),
        simple("codec-measure", CodecOp::Measure),
        preview_entry,
    ]);
}

fn platform_label(p: &Platform) -> String {
    let native = if p.native { "" } else { ", emulated" };
    format!(
        "{} ({} LUFS · {} {} kb/s{native})",
        p.name, p.target_lufs, p.codec, p.kbps
    )
    .replace('-', "−")
}

/// Audio → Listen's Codec Monitor submenu.
pub fn menu() -> gio::Menu {
    let item = |label: &str, action: &str, target: &str| {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        i
    };
    let m = gio::Menu::new();
    let off = gio::Menu::new();
    off.append_item(&item("Off", "app.codec-monitor", "off"));
    m.append_section(None, &off);
    let opus = gio::Menu::new();
    for (k, id) in BITRATES.iter().zip(OPUS_IDS) {
        opus.append_item(&item(&format!("Opus {k} kb/s"), "app.codec-monitor", id));
    }
    opus.append(Some("Variable Bitrate"), Some("app.codec-vbr"));
    m.append_section(Some("Codec"), &opus);
    let platforms = gio::Menu::new();
    for (p, id) in PLATFORMS.iter().zip(PLATFORM_IDS) {
        platforms.append_item(&item(&platform_label(p), "app.codec-monitor", id));
    }
    m.append_section(Some("As a Platform Plays It"), &platforms);
    let hear = gio::Menu::new();
    for h in CodecHear::ALL {
        let label = match h {
            CodecHear::Codec => "Hear the Codec",
            CodecHear::Original => "Hear the Original (Lossless)",
            CodecHear::Difference => "Hear the Difference (Artefacts Alone)",
        };
        hear.append_item(&item(label, "app.codec-hear", h.id()));
    }
    m.append_section(None, &hear);
    let more = gio::Menu::new();
    more.append(Some("Reset Codec Meters"), Some("app.codec-reset"));
    more.append(
        Some("Measure the Song's Loudness"),
        Some("app.codec-measure"),
    );
    more.append(Some("Preview on Platforms…"), Some("app.codec-preview"));
    m.append_section(None, &more);
    m
}

/// Apply the remembered settings (the monitor stays off).
pub fn apply_preferences(session: &mut Session, prefs: &Preferences) {
    if let Err(e) = session.dispatch(Action::Codec(CodecOp::Settings(
        prefs.codec_monitor.clone(),
    ))) {
        tracing::warn!("codec monitor: {e}");
    }
}

thread_local! {
    /// The settings as last remembered by [`follow`].
    static LAST: RefCell<Option<CodecMonitorSettings>> = const { RefCell::new(None) };
    /// The preview window open (one at a time).
    static PREVIEW: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

/// After a change: new settings are remembered.
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let now = s.codec_settings().clone();
    drop(s);
    let changed = LAST.with(|l| {
        let mut l = l.borrow_mut();
        let first = l.is_none();
        let changed = l.as_ref() != Some(&now);
        *l = Some(now.clone());
        changed && !first
    });
    if changed {
        let mut p = Preferences::load();
        if p.codec_monitor != now {
            p.codec_monitor = now;
            if let Err(e) = p.save() {
                tracing::warn!("cannot save preferences: {e}");
            }
        }
    }
}

/// Start a preview of the master (`settings`: the Render window's; `None`:
/// the whole project as it is, at its rate) on `platforms` (ids; empty:
/// those chosen last, else all) and show its window.
pub(crate) fn preview(
    app: &Rc<AppState>,
    settings: Option<RenderSettings>,
    platforms: Vec<String>,
) {
    let settings = settings.unwrap_or_else(|| {
        let s = app.session.borrow();
        let mut r = RenderSettings::defaults_for(s.project(), std::path::PathBuf::new());
        r.range = faderframe_session::render::RenderRange::Project;
        r
    });
    app.dispatch(Action::Codec(CodecOp::Preview {
        settings: Box::new(settings),
        platforms,
    }));
    show(app);
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l
}

fn db(v: f64, unit: &str) -> String {
    if v.is_finite() {
        format!("{v:+.1} {unit}").replace('-', "−")
    } else {
        format!("−∞ {unit}")
    }
}

/// One platform's section of the report.
fn platform_section(p: &faderframe_codec::PlatformPreview) -> gtk::Widget {
    let grid = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(3)
        .build();
    let mut y = 0;
    let mut row = |name: &str, value: String, error: bool| {
        let n = label(name);
        n.add_css_class("dim-label");
        let v = label(&value);
        v.set_selectable(true);
        v.set_hexpand(true);
        if error {
            v.add_css_class("error");
        }
        grid.attach(&n, 0, y, 1, 1);
        grid.attach(&v, 1, y, 1, 1);
        y += 1;
    };
    let pl = p.platform;
    row("Codec", pl.codec_text(), false);
    row(
        "Loudness",
        format!("plays at {:.0} LUFS, {}", pl.target_lufs, pl.turn_up_text()).replace('-', "−"),
        false,
    );
    let g = p.gain;
    let limited = if g.peak_limited {
        format!(" (wanted {}, stopped by the peaks)", db(g.wanted_db, "dB"))
    } else {
        String::new()
    };
    row(
        "Gain applied",
        format!("{}{limited}", db(g.db, "dB")),
        false,
    );
    row(
        "True peak decoded",
        format!(
            "{} at the platform's level, {} with normalisation off",
            db(p.heard_true_peak, "dBTP"),
            db(p.decoded_true_peak, "dBTP")
        ),
        p.heard_true_peak > 0.0,
    );
    row(
        "Over full scale",
        format!(
            "{} frames at the platform's level, {} with normalisation off",
            p.overs, p.overs_unnormalised
        ),
        p.overs > 0,
    );
    row(
        "Codec artefacts",
        p.artefacts_db.map_or_else(
            || "—".into(),
            |a| format!("{} against the master", db(a, "dB")),
        ),
        false,
    );
    row(
        "Clipping risk",
        p.risk.label().to_string(),
        p.risk == faderframe_codec::Risk::Clips,
    );
    for a in &p.advice {
        row("Advice", a.clone(), false);
    }
    grid.upcast()
}

/// Fill `body` with the preview as it stands.
fn fill(body: &gtk::Box, status: Option<&PreviewStatus>) {
    while let Some(c) = body.first_child() {
        body.remove(&c);
    }
    let Some(status) = status else {
        body.append(&label("No preview yet."));
        return;
    };
    if let Some(f) = status.progress {
        body.append(&label(status.stage));
        let bar = gtk::ProgressBar::new();
        bar.set_fraction(f);
        body.append(&bar);
        return;
    }
    match &status.result {
        Some(Ok(r)) => {
            let head = label(&format!(
                "The master — {}: {} LUFS, true peak {}",
                r.rendered,
                format!("{:.1}", r.master.integrated).replace('-', "−"),
                db(r.master.true_peak, "dBTP")
            ));
            head.add_css_class("heading");
            body.append(&head);
            let note = label(
                "Rendered, then coded and decoded as each platform streams it (its usual tier; \
                 lossless tiers play the master as it is), at the loudness the platform plays \
                 it. Figures are the platforms' published defaults; services change them.",
            );
            note.add_css_class("dim-label");
            body.append(&note);
            for p in &r.platforms {
                let worst = match p.risk {
                    faderframe_codec::Risk::Clips => " — clips",
                    faderframe_codec::Risk::Close => " — close to clipping",
                    faderframe_codec::Risk::Safe => "",
                };
                let exp = gtk::Expander::new(Some(&format!(
                    "{}: {} · decoded {}{worst}",
                    p.platform.name,
                    db(p.gain.db, "dB"),
                    db(p.heard_true_peak, "dBTP")
                )));
                exp.set_expanded(p.risk != faderframe_codec::Risk::Safe);
                let section = platform_section(p);
                section.set_margin_start(16);
                section.set_margin_top(4);
                exp.set_child(Some(&section));
                body.append(&exp);
            }
        }
        Some(Err(e)) => {
            let l = label(&format!("The preview failed: {e}"));
            l.add_css_class("error");
            body.append(&l);
        }
        None => body.append(&label("No preview yet.")),
    }
}

/// The preview's window (one; brought up again when open).
pub(crate) fn show(app: &Rc<AppState>) {
    if let Some(w) = PREVIEW.with(|p| p.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        w.present();
        return;
    }
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Preview on Platforms")
        .default_width(680)
        .default_height(560)
        .build();
    if let Some(parent) = app.window.borrow().as_ref() {
        win.set_transient_for(Some(parent));
    }
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 10);
    outer.set_margin_top(14);
    outer.set_margin_bottom(14);
    outer.set_margin_start(14);
    outer.set_margin_end(14);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .vexpand(true)
        .build();
    outer.append(&scroll);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let stop = gtk::Button::with_label("Stop");
    let close = gtk::Button::with_label("Close");
    close.add_css_class("suggested-action");
    actions.append(&stop);
    actions.append(&close);
    outer.append(&actions);
    win.set_child(Some(&outer));
    let weak = Rc::downgrade(app);
    stop.connect_clicked(move |_| {
        if let Some(a) = weak.upgrade() {
            a.dispatch(Action::Codec(CodecOp::CancelPreview));
        }
    });
    let w = win.downgrade();
    close.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    // Brought up to date while it is open (the progress, then the report
    // once).
    let last: Rc<RefCell<Option<PreviewStatus>>> = Rc::new(RefCell::new(None));
    let refresh = {
        let weak = Rc::downgrade(app);
        let body = body.clone();
        let stop = stop.clone();
        move || {
            let Some(a) = weak.upgrade() else {
                return;
            };
            let Ok(s) = a.session.try_borrow() else {
                return;
            };
            let now = s.platform_preview();
            drop(s);
            let mut last = last.borrow_mut();
            let same = match (&*last, &now) {
                (Some(x), Some(y)) => x.progress.is_some() == y.progress.is_some() && x == y,
                (None, None) => true,
                _ => false,
            };
            if !same {
                fill(&body, now.as_ref());
                stop.set_sensitive(now.as_ref().is_some_and(|p| p.progress.is_some()));
                *last = now;
            }
        }
    };
    refresh();
    let w = win.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if w.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        refresh();
        glib::ControlFlow::Continue
    });
    let keys = gtk::EventControllerKey::new();
    let w = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape
            && let Some(w) = w.upgrade()
        {
            w.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    PREVIEW.with(|p| *p.borrow_mut() = Some(win.downgrade()));
    win.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_ids_follow_the_lists() {
        for (k, id) in BITRATES.iter().zip(OPUS_IDS) {
            assert_eq!(id, format!("opus@{k}"));
        }
        assert_eq!(BITRATES.len(), OPUS_IDS.len());
        for (p, id) in PLATFORMS.iter().zip(PLATFORM_IDS) {
            assert_eq!(id, format!("platform={}", p.id));
        }
        assert_eq!(PLATFORMS.len(), PLATFORM_IDS.len());
    }
}
