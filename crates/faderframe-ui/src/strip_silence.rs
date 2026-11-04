//! The Strip Silence dialog: threshold, minimum length, padding before
//! and after, minimum gap; every change previews at once on the
//! arranger's clips (`Session::strip_silence_preview`); Strip, Separate
//! and Strip to New Clips each do it as one undo step and leave the
//! dialog on what is left. Every control is an `Action::StripSilence`.

use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::strip_silence::{SilenceSettings, StripMode, StripSilenceOp};
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: std::cell::RefCell<Option<glib::WeakRef<gtk::Window>>> =
        const { std::cell::RefCell::new(None) };
}

struct Ui {
    threshold: gtk::Scale,
    min_length: gtk::SpinButton,
    pre_pad: gtk::SpinButton,
    post_pad: gtk::SpinButton,
    min_gap: gtk::SpinButton,
    summary: gtk::Label,
    buttons: Vec<gtk::Button>,
    revision: Cell<u64>,
}

impl Ui {
    fn settings(&self) -> SilenceSettings {
        SilenceSettings {
            threshold_db: self.threshold.value() as f32,
            min_length: self.min_length.value() / 1000.0,
            pre_pad: self.pre_pad.value() / 1000.0,
            post_pad: self.post_pad.value() / 1000.0,
            min_gap: self.min_gap.value() / 1000.0,
        }
    }
}

fn op(app: &Rc<AppState>, op: StripSilenceOp) {
    app.dispatch(Action::StripSilence(op));
}

/// Show the dialog (raised if it is open) on the session's clips.
pub fn open(app: &Rc<AppState>) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        w.present();
        return;
    }
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let now = app.session.borrow().strip_silence_settings();
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Strip Silence")
        .transient_for(&main)
        .default_width(460)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    let label = |text: &str, row: i32| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::End);
        grid.attach(&l, 0, row, 1, 1);
    };
    let threshold = gtk::Scale::with_range(gtk::Orientation::Horizontal, -96.0, 0.0, 0.5);
    threshold.set_value(f64::from(now.threshold_db));
    threshold.set_draw_value(true);
    threshold.set_digits(1);
    threshold.set_hexpand(true);
    threshold.set_tooltip_text(Some("Sound is at or above this peak level (dBFS)"));
    label("Threshold (dB)", 0);
    grid.attach(&threshold, 1, 0, 1, 1);
    let spin = |row: i32, text: &str, tip: &str, max: f64, value: f64| {
        let s = gtk::SpinButton::with_range(0.0, max, 1.0);
        s.set_digits(0);
        s.set_value(value * 1000.0);
        s.set_tooltip_text(Some(tip));
        label(text, row);
        grid.attach(&s, 1, row, 1, 1);
        s
    };
    let min_length = spin(
        1,
        "Minimum Length (ms)",
        "Sound shorter than this goes with the silence",
        10_000.0,
        now.min_length,
    );
    let pre_pad = spin(
        2,
        "Pad Before (ms)",
        "Kept before each region's first sound (the attack's lead-in)",
        5_000.0,
        now.pre_pad,
    );
    let post_pad = spin(
        3,
        "Pad After (ms)",
        "Kept after each region's last sound (its tail)",
        10_000.0,
        now.post_pad,
    );
    let min_gap = spin(
        4,
        "Minimum Gap (ms)",
        "Silences shorter than this stay inside a region (pauses within a line)",
        10_000.0,
        now.min_gap,
    );
    body.append(&grid);
    let summary = gtk::Label::new(None);
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("dim-label");
    body.append(&summary);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let close = gtk::Button::with_label("Close");
    row.append(&close);
    let mut buttons = Vec::new();
    for mode in [StripMode::Separate, StripMode::NewClips, StripMode::Strip] {
        let b = gtk::Button::with_label(mode.label());
        b.set_tooltip_text(Some(match mode {
            StripMode::Strip => "Take the silence out: each region a clip of its own",
            StripMode::Separate => "Split the clips at the regions' edges, keeping everything",
            StripMode::NewClips => {
                "The regions as numbered clips on a new track (dialogue lines, drum \
                 hits); the clips themselves are muted"
            }
        }));
        if mode == StripMode::Strip {
            b.add_css_class("suggested-action");
        }
        let app = Rc::clone(app);
        b.connect_clicked(move |_| op(&app, StripSilenceOp::Apply(mode)));
        row.append(&b);
        buttons.push(b);
    }
    body.append(&row);
    win.set_child(Some(&body));
    let ui = Rc::new(Ui {
        threshold,
        min_length,
        pre_pad,
        post_pad,
        min_gap,
        summary,
        buttons,
        revision: Cell::new(u64::MAX),
    });
    let changed = {
        let (app, u) = (Rc::clone(app), Rc::downgrade(&ui));
        move || {
            if let Some(u) = u.upgrade() {
                op(&app, StripSilenceOp::Settings(u.settings()));
            }
        }
    };
    {
        let f = changed.clone();
        ui.threshold.connect_value_changed(move |_| f());
    }
    for s in [&ui.min_length, &ui.pre_pad, &ui.post_pad, &ui.min_gap] {
        let f = changed.clone();
        s.connect_value_changed(move |_| f());
    }
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    {
        let app = Rc::clone(app);
        win.connect_close_request(move |_| {
            OPEN.with(|o| o.borrow_mut().take());
            op(&app, StripSilenceOp::Close);
            glib::Propagation::Proceed
        });
    }
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    refresh(app, &ui);
    {
        let (app, ui, weak) = (Rc::downgrade(app), Rc::clone(&ui), win.downgrade());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(app), Some(_)) = (app.upgrade(), weak.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            refresh(&app, &ui);
            glib::ControlFlow::Continue
        });
    }
    win.present();
}

/// The summary line and the buttons from the session's preview.
fn refresh(app: &Rc<AppState>, ui: &Ui) {
    let s = app.session.borrow();
    let revision = s.revision();
    if ui.revision.replace(revision) == revision {
        return;
    }
    let Some(p) = s.strip_silence_preview() else {
        ui.summary
            .set_text("No clips: select audio clips and open Strip Silence again.");
        for b in &ui.buttons {
            b.set_sensitive(false);
        }
        return;
    };
    let (regions, clips) = p.counts();
    let text = if p.measuring {
        format!("Measuring {clips} clip(s)…")
    } else {
        let empty = p.regions.values().filter(|r| r.is_empty()).count();
        let mut t = format!(
            "{regions} region{} in {clips} clip{}",
            if regions == 1 { "" } else { "s" },
            if clips == 1 { "" } else { "s" }
        );
        if empty > 0 {
            t.push_str(&format!(
                " — {empty} clip{} silent throughout (Strip removes {})",
                if empty == 1 { " is" } else { "s are" },
                if empty == 1 { "it" } else { "them" }
            ));
        }
        t
    };
    ui.summary.set_text(&text);
    for b in &ui.buttons {
        b.set_sensitive(!p.measuring && !p.clips.is_empty());
    }
}
