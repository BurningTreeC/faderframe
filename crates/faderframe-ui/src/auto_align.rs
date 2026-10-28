//! The Auto-Align window: the microphones of one source, each track's
//! delay behind the reference (milliseconds and samples), its polarity,
//! how sure the finding is and a tick to include it; the reference
//! chosen (or the first to hear it), how far to search, Analyse and
//! Apply. Everything shown is the session's (`Session::auto_align`),
//! read again while the window is open; every control is an
//! `Action::AutoAlign`.

use crate::state::AppState;
use faderframe_core::TrackId;
use faderframe_session::Action;
use faderframe_session::align::{AlignOp, AlignView};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

/// The reference list's first entry.
const AUTOMATIC: &str = "Automatic — the first to hear it";

struct Ui {
    reference: gtk::DropDown,
    /// The tracks the reference list names (after "Automatic").
    names: RefCell<Vec<(TrackId, String)>>,
    max_lag: gtk::SpinButton,
    list: gtk::Grid,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    span: gtk::Label,
    analyse: gtk::Button,
    apply: gtk::Button,
    /// Set while the window itself changes its controls (their signals
    /// then send nothing).
    updating: Cell<bool>,
    revision: Cell<u64>,
}

fn op(app: &Rc<AppState>, op: AlignOp) {
    app.dispatch(Action::AutoAlign(op));
}

/// Show the window (raised if it is open).
pub fn open(app: &Rc<AppState>) {
    if let Some(w) = OPEN.with(|o| o.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        w.present();
        return;
    }
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .application(&app.app)
        .title("Auto-Align Microphones")
        .transient_for(&main)
        .default_width(640)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let intro = gtk::Label::new(Some(
        "Microphones of one source — a kit's close, overhead and room microphones, a \
         cabinet's close and room ones, a bass's DI and microphone — heard together: each \
         track's own delay and polarity. The reference keeps its place; the others move \
         earlier by how much later they hear it.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.set_max_width_chars(70);
    body.append(&intro);

    let settings = gtk::Grid::new();
    settings.set_row_spacing(6);
    settings.set_column_spacing(10);
    let label = |text: &str, row: i32| {
        let l = gtk::Label::new(Some(text));
        l.set_halign(gtk::Align::End);
        settings.attach(&l, 0, row, 1, 1);
    };
    let reference = gtk::DropDown::from_strings(&[AUTOMATIC]);
    reference.set_hexpand(true);
    label("Reference", 0);
    settings.attach(&reference, 1, 0, 1, 1);
    let max_lag = gtk::SpinButton::with_range(1.0, 200.0, 1.0);
    max_lag.set_digits(0);
    max_lag.set_value(faderframe_session::align::DEFAULT_MAX_LAG_MS);
    max_lag.set_tooltip_text(Some(
        "The furthest a microphone may hear the source after another (30 ms: about ten metres)",
    ));
    label("Search (ms)", 1);
    settings.attach(&max_lag, 1, 1, 1, 1);
    body.append(&settings);

    let list = gtk::Grid::new();
    list.set_row_spacing(4);
    list.set_column_spacing(14);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .min_content_height(160)
        .propagate_natural_height(true)
        .max_content_height(420)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    body.append(&scroller);

    let progress = gtk::ProgressBar::new();
    progress.set_show_text(true);
    progress.set_text(Some("Analysing…"));
    body.append(&progress);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_max_width_chars(70);
    body.append(&status);
    let span = gtk::Label::new(None);
    span.set_xalign(0.0);
    span.add_css_class("dim-label");
    body.append(&span);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let analyse = gtk::Button::with_label("Analyse");
    let close = gtk::Button::with_label("Close");
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    buttons.append(&analyse);
    buttons.append(&close);
    buttons.append(&apply);
    body.append(&buttons);
    win.set_child(Some(&body));

    let ui = Rc::new(Ui {
        reference,
        names: RefCell::new(Vec::new()),
        max_lag,
        list,
        progress,
        status,
        span,
        analyse,
        apply,
        updating: Cell::new(false),
        revision: Cell::new(0),
    });
    {
        let (app, u) = (Rc::clone(app), Rc::clone(&ui));
        ui.reference.connect_selected_notify(move |d| {
            if u.updating.get() {
                return;
            }
            let chosen = (d.selected() as usize)
                .checked_sub(1)
                .and_then(|i| u.names.borrow().get(i).map(|(id, _)| *id));
            op(&app, AlignOp::SetReference(chosen));
        });
    }
    {
        let (app, u) = (Rc::clone(app), Rc::clone(&ui));
        ui.max_lag.connect_value_changed(move |s| {
            if !u.updating.get() {
                op(&app, AlignOp::SetMaxLag(s.value()));
            }
        });
    }
    {
        let app = Rc::clone(app);
        ui.analyse
            .connect_clicked(move |_| op(&app, AlignOp::Analyse));
    }
    {
        let app = Rc::clone(app);
        ui.apply.connect_clicked(move |_| op(&app, AlignOp::Apply));
    }
    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    {
        let app = Rc::clone(app);
        win.connect_close_request(move |_| {
            OPEN.with(|o| o.borrow_mut().take());
            op(&app, AlignOp::Close);
            glib::Propagation::Proceed
        });
    }
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    refresh(app, &ui, &win);
    {
        let (app, ui, weak) = (Rc::downgrade(app), Rc::clone(&ui), win.downgrade());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(app), Some(win)) = (app.upgrade(), weak.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            refresh(&app, &ui, &win);
            glib::ControlFlow::Continue
        });
    }
    win.present();
}

/// Show the session's state when it changed (closing the window when
/// Auto-Align was closed elsewhere).
fn refresh(app: &Rc<AppState>, ui: &Rc<Ui>, win: &gtk::Window) {
    let view = match app.session.try_borrow() {
        Ok(s) => s.auto_align(),
        Err(_) => return,
    };
    let Some(view) = view else {
        OPEN.with(|o| o.borrow_mut().take());
        win.destroy();
        return;
    };
    if view.revision == ui.revision.get() {
        return;
    }
    ui.revision.set(view.revision);
    ui.updating.set(true);
    show(app, ui, &view);
    ui.updating.set(false);
}

/// The rows' columns.
const HEADINGS: [&str; 6] = ["Use", "Track", "Delay", "Samples", "Polarity", "Confidence"];

fn show(app: &Rc<AppState>, ui: &Rc<Ui>, view: &AlignView) {
    // The reference list (rebuilt when the tracks change).
    if *ui.names.borrow() != view.tracks {
        let mut entries = vec![AUTOMATIC.to_string()];
        entries.extend(view.tracks.iter().map(|(_, n)| n.clone()));
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        ui.reference.set_model(Some(&gtk::StringList::new(&refs)));
        *ui.names.borrow_mut() = view.tracks.clone();
    }
    let chosen = view
        .reference
        .and_then(|r| view.tracks.iter().position(|(id, _)| *id == r))
        .map_or(0, |i| i + 1);
    if ui.reference.selected() as usize != chosen {
        ui.reference.set_selected(chosen as u32);
    }
    if (ui.max_lag.value() - view.max_lag_ms).abs() > 1e-9 {
        ui.max_lag.set_value(view.max_lag_ms);
    }

    while let Some(child) = ui.list.first_child() {
        ui.list.remove(&child);
    }
    for (c, h) in HEADINGS.iter().enumerate() {
        let l = gtk::Label::new(None);
        l.set_markup(&format!("<b>{h}</b>"));
        l.set_xalign(if c >= 2 { 1.0 } else { 0.0 });
        ui.list.attach(&l, c as i32, 0, 1, 1);
    }
    for (i, row) in view.rows.iter().enumerate() {
        let y = i as i32 + 1;
        let tick = gtk::CheckButton::new();
        tick.set_active(row.include);
        tick.set_sensitive(!row.reference && row.lag_ms.is_some() && !view.analysing);
        {
            let (app, u, track) = (Rc::clone(app), Rc::clone(ui), row.track);
            tick.connect_toggled(move |t| {
                if !u.updating.get() {
                    op(
                        &app,
                        AlignOp::Include {
                            track,
                            on: t.is_active(),
                        },
                    );
                }
            });
        }
        ui.list.attach(&tick, 0, y, 1, 1);
        let name = if row.via_hub {
            format!("{} (through a shared microphone)", row.name)
        } else {
            row.name.clone()
        };
        let cells = [
            name,
            row.lag_text(),
            row.samples_text(),
            row.polarity_text(),
            row.confidence_text(),
        ];
        for (c, text) in cells.into_iter().enumerate() {
            let l = gtk::Label::new(Some(&text));
            l.set_xalign(if c >= 1 { 1.0 } else { 0.0 });
            if c == 0 {
                l.set_hexpand(true);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            if row.reference {
                l.add_css_class("accent");
            }
            ui.list.attach(&l, c as i32 + 1, y, 1, 1);
        }
    }

    ui.progress.set_visible(view.analysing);
    ui.progress.set_fraction(f64::from(view.progress));
    ui.status.set_text(&view.status);
    ui.span
        .set_text(&view.span.map_or_else(String::new, |(a, b)| {
            format!("Analysed {a:.1}–{b:.1} s (the edit range, else where the clips overlap)")
        }));
    ui.analyse
        .set_sensitive(!view.analysing && view.tracks.len() >= 2);
    // While analysing, Apply waits for the findings.
    ui.apply.set_sensitive(view.can_apply || view.analysing);
}
