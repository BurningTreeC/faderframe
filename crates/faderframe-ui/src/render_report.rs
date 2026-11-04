//! After a render: the statistics of what was written (one section per
//! file; measured by `faderframe_session::render::stats` from the files
//! as written, read back from disk on the render's worker), the delivery
//! QC — the files checked against a chosen spec, the stems' null test
//! (or a Null Test of untested stems), Save QC Report… (a PDF and a JSON
//! file, `faderframe_session::qc`) — and the question asked before an
//! existing file is replaced.

use crate::state::AppState;
use faderframe_qc::{QcFile, QcReport, SPECS, Spec, Verdict};
use faderframe_session::delivery::Finished;
use faderframe_session::exports::ReplaceQuestion;
use faderframe_session::render::stats::{FileStats, db_text, duration_text, size_text};
use faderframe_session::render::{RenderSummary, Rendered};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Files listed by name in a question; the rest are counted.
const LISTED: usize = 12;

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// "“Mix.wav”", or for a folder "“Album (DDP)” (a folder, replaced as a
/// whole)".
fn described(path: &Path) -> String {
    if path.is_dir() {
        format!("“{}” (a folder, replaced as a whole)", name_of(path))
    } else {
        format!("“{}”", name_of(path))
    }
}

type Answer = Box<dyn FnOnce(bool)>;
type OpenQuestion = (gio::Cancellable, Rc<RefCell<Option<Answer>>>);

thread_local! {
    /// The Overwrite / Cancel question opened last and not answered: its
    /// answer and what closes its dialog (scripted runs answer it with
    /// [`answer_question`]).
    static OPEN_QUESTION: RefCell<Option<OpenQuestion>> = const { RefCell::new(None) };
}

/// Answer the open Overwrite / Cancel question as its button would
/// (scripted runs: `answer-question:<overwrite|cancel>`); false when none
/// is open.
pub fn answer_question(replace: bool) -> bool {
    let Some((cancellable, answer)) = OPEN_QUESTION.with(|q| q.borrow_mut().take()) else {
        return false;
    };
    let answer = answer.borrow_mut().take();
    cancellable.cancel();
    match answer {
        Some(a) => {
            a(replace);
            true
        }
        None => false,
    }
}

/// Ask whether `paths` (they exist) may be replaced; `appeared`: they
/// turned up while rendering. `answer(true)` replaces them.
pub fn ask_overwrite(
    parent: Option<&gtk::Window>,
    paths: &[PathBuf],
    appeared: bool,
    answer: impl FnOnce(bool) + 'static,
) {
    let after = if appeared {
        "The render is finished and waits beside it. Replacing moves the new file into \
         place in one step; Cancel throws the render away and keeps what is there."
    } else {
        "The new file replaces it only once the render has finished; until then, and if \
         the render is stopped or fails, the existing file stays as it is."
    };
    ask(parent, None, paths, appeared, "rendering", after, answer);
}

/// The session's question about an export ([`ReplaceQuestion`]): the
/// same dialog as a render's, the answer sent back as
/// `Action::AnswerReplace`.
pub fn ask_replace(app: &Rc<AppState>, q: &ReplaceQuestion) {
    let one = q.paths.len() == 1;
    let after = match (q.appeared, one) {
        (true, _) => {
            "The export is finished and waits beside it. Replacing moves the new files into \
             place; Cancel throws the export away and keeps what is there."
        }
        (false, true) => {
            "Nothing is written before you answer. The new one replaces it only once the \
             export has finished — if it is stopped or fails, it stays as it is."
        }
        (false, false) => {
            "Nothing is written before you answer. The new ones replace these only once the \
             export has finished — if it is stopped or fails, they stay as they are."
        }
    };
    let win = app
        .window
        .borrow()
        .clone()
        .map(gtk::prelude::Cast::upcast::<gtk::Window>);
    let weak = Rc::downgrade(app);
    let id = q.id;
    ask(
        win.as_ref(),
        Some(&q.what),
        &q.paths,
        q.appeared,
        "exporting",
        after,
        move |yes| {
            if let Some(app) = weak.upgrade() {
                app.dispatch(faderframe_session::Action::AnswerReplace {
                    question: id,
                    replace: yes,
                });
            }
        },
    );
}

/// Overwrite / Cancel about `paths` (`what`: the export's name; `doing`:
/// "rendering", "exporting"; `after`: what each answer does).
fn ask(
    parent: Option<&gtk::Window>,
    what: Option<&str>,
    paths: &[PathBuf],
    appeared: bool,
    doing: &str,
    after: &str,
    answer: impl FnOnce(bool) + 'static,
) {
    let items = if paths.iter().any(|p| p.is_dir()) {
        "files and folders"
    } else {
        "files"
    };
    let message = match (paths.len(), appeared) {
        (1, false) => format!("Replace {}?", described(&paths[0])),
        (1, true) => format!(
            "{} appeared while {doing}. Replace it?",
            described(&paths[0])
        ),
        (n, false) => format!("Replace {n} existing {items}?"),
        (n, true) => format!("{n} {items} appeared while {doing}. Replace them?"),
    };
    let mut detail = String::new();
    if let Some(what) = what {
        detail += &format!("{what} export.\n\n");
    }
    if paths.len() > 1 {
        for p in paths.iter().take(LISTED) {
            detail += &format!("• {}\n", described(p));
        }
        if paths.len() > LISTED {
            detail += &format!("… and {} more\n", paths.len() - LISTED);
        }
        detail += "\n";
    }
    let folder = paths
        .first()
        .and_then(|p| p.parent())
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    detail += &format!("In {folder}.\n\n{after}");
    let dialog = gtk::AlertDialog::builder()
        .message(message)
        .detail(detail)
        .buttons(["Cancel", "Overwrite"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();
    let holder: Rc<RefCell<Option<Answer>>> = Rc::new(RefCell::new(Some(Box::new(answer))));
    let cancellable = gio::Cancellable::new();
    OPEN_QUESTION.with(|q| *q.borrow_mut() = Some((cancellable.clone(), Rc::clone(&holder))));
    dialog.choose(parent, Some(&cancellable), move |res| {
        OPEN_QUESTION.with(|q| {
            let mut q = q.borrow_mut();
            if q.as_ref().is_some_and(|(_, h)| Rc::ptr_eq(h, &holder)) {
                *q = None;
            }
        });
        let answer = holder.borrow_mut().take();
        if let Some(a) = answer {
            a(res == Ok(1));
        }
    });
}

/// Tell that a render failed (no report: nothing was written in place).
pub fn show_failure(parent: Option<&gtk::Window>, error: &str) {
    let dialog = gtk::AlertDialog::builder()
        .message("The render failed")
        .detail(format!(
            "{error}\n\nNothing was moved into place: files that were there are unchanged."
        ))
        .buttons(["Close"])
        .modal(true)
        .build();
    dialog.show(parent);
}

/// Open the folder holding `path` with it selected where the platform's
/// file manager can (else the folder itself).
fn show_in_folder(parent: Option<&gtk::Window>, path: &Path) {
    let file = gio::File::for_path(path);
    let folder = path.parent().map(Path::to_path_buf);
    let parent_weak = parent.map(|p| p.downgrade());
    gtk::FileLauncher::new(Some(&file)).open_containing_folder(
        parent,
        gio::Cancellable::NONE,
        move |res| {
            let Err(e) = res else { return };
            tracing::info!("show in folder: {e}; opening the folder");
            let Some(folder) = folder else { return };
            let parent = parent_weak.as_ref().and_then(glib::WeakRef::upgrade);
            gtk::FileLauncher::new(Some(&gio::File::for_path(&folder))).launch(
                parent.as_ref(),
                gio::Cancellable::NONE,
                |res| {
                    if let Err(e) = res {
                        tracing::warn!("could not open the folder: {e}");
                    }
                },
            );
        },
    );
}

fn row(grid: &gtk::Grid, y: &mut i32, label: &str, value: &str) {
    let l = gtk::Label::new(Some(label));
    l.set_xalign(1.0);
    l.set_valign(gtk::Align::Start);
    l.add_css_class("dim-label");
    let v = gtk::Label::new(Some(value));
    v.set_xalign(0.0);
    v.set_wrap(true);
    v.set_selectable(true);
    v.set_hexpand(true);
    grid.attach(&l, 0, *y, 1, 1);
    grid.attach(&v, 1, *y, 1, 1);
    *y += 1;
}

fn rate_text(rate: u32) -> String {
    faderframe_audio::format_sample_rate(rate)
}

fn speed_text(seconds: f64, factor: Option<f64>) -> String {
    match factor {
        Some(f) if f >= 1.0 => format!("{seconds:.2} s ({f:.0}× real time)"),
        Some(f) => format!("{seconds:.2} s ({f:.2}× real time)"),
        None => format!("{seconds:.2} s"),
    }
}

/// One file's facts and levels, and its QC checks that did not pass
/// (`qc`: the file in the report for the chosen spec).
fn file_grid(r: &Rendered, qc: Option<&QcFile>) -> gtk::Grid {
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    let mut y = 0;
    row(&grid, &mut y, "File", &r.path.display().to_string());
    match &r.stats {
        None => row(&grid, &mut y, "Statistics", "not measured"),
        Some(Err(e)) => row(
            &grid,
            &mut y,
            "Statistics",
            &format!("the file could not be read back: {e}"),
        ),
        Some(Ok(st)) => stats_rows(&grid, &mut y, st),
    }
    if let Some(f) = &r.finished {
        row(&grid, &mut y, "Level processing", &finishing_text(f));
    }
    if let Some(q) = qc {
        let flagged: Vec<String> = q
            .checks
            .iter()
            .filter(|c| matches!(c.verdict, Verdict::Warn | Verdict::Fail))
            .map(|c| {
                format!(
                    "{} {}: {} (asked: {})",
                    c.verdict.label(),
                    c.label,
                    c.measured,
                    c.requirement
                )
            })
            .collect();
        let at = y;
        row(
            &grid,
            &mut y,
            "QC",
            &if flagged.is_empty() {
                format!(
                    "{} checks passed{}",
                    q.checks
                        .iter()
                        .filter(|c| c.verdict == Verdict::Pass)
                        .count(),
                    if q.checks.iter().any(|c| c.verdict == Verdict::Info) {
                        " (the rest measured, nothing asked of them)"
                    } else {
                        ""
                    }
                )
            } else {
                flagged.join("\n")
            },
        );
        if let Some(w) = grid.child_at(1, at) {
            w.add_css_class(verdict_class(q.verdict));
        }
    }
    grid
}

/// The style class for a verdict's text.
fn verdict_class(v: Verdict) -> &'static str {
    match v {
        Verdict::Fail => "error",
        Verdict::Warn => "warning",
        Verdict::Pass => "success",
        Verdict::Info => "dim-label",
    }
}

fn finishing_text(f: &Finished) -> String {
    let mut parts = vec![format!("gain {:+.1} dB", f.gain_db)];
    if f.limited_db >= 0.05 {
        parts.push(format!("limited up to {:.1} dB", f.limited_db));
    }
    parts.join(" · ") + " (applied before writing)"
}

fn stats_rows(grid: &gtk::Grid, y: &mut i32, st: &FileStats) {
    let encoder = if st.from_file { "" } else { " (as encoded)" };
    row(
        grid,
        y,
        "Format",
        &format!("{} · {}{encoder}", st.container, st.encoding),
    );
    row(grid, y, "Sample rate", &rate_text(st.sample_rate));
    row(
        grid,
        y,
        "Channels",
        &format!("{} · {}", st.channels, st.layout),
    );
    row(
        grid,
        y,
        "Duration",
        &format!("{} ({} frames)", duration_text(st.seconds()), st.frames),
    );
    row(grid, y, "Size", &size_text(st.bytes));
    row(
        grid,
        y,
        "Render time",
        &speed_text(st.render_seconds, st.realtime_factor()),
    );
    if let Some(l) = &st.levels {
        row(grid, y, "Sample peak", &db_text(l.sample_peak, "dBFS"));
        row(grid, y, "True peak", &db_text(l.true_peak, "dBTP"));
        row(grid, y, "RMS", &db_text(l.rms, "dBFS"));
        if let Some(loud) = &l.loudness {
            row(
                grid,
                y,
                "Integrated loudness",
                &db_text(loud.integrated, "LUFS"),
            );
            row(grid, y, "Loudness range", &format!("{:.1} LU", loud.range));
            row(
                grid,
                y,
                "Max. short-term",
                &db_text(loud.max_short_term, "LUFS"),
            );
        }
        if l.channels.len() > 1 && l.channels.len() <= 16 {
            let per: Vec<String> = l
                .channels
                .iter()
                .map(|c| db_text(c.sample_peak, "").trim_end().to_string())
                .collect();
            row(grid, y, "Peaks per channel", &(per.join(" · ") + " dBFS"));
        }
        let warnings = st.warnings();
        let clip = if warnings.is_empty() {
            "none: no sample at full scale, true peak below 0 dBTP".to_string()
        } else {
            warnings.join("\n")
        };
        let at = *y;
        row(grid, y, "Clipping", &clip);
        if !warnings.is_empty()
            && let Some(w) = grid.child_at(1, at)
        {
            w.add_css_class("error");
        }
    }
    for note in &st.notes {
        row(grid, y, "Note", note);
    }
}

thread_local! {
    /// The spec the report window checked against last.
    static LAST_SPEC: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The report window's QC part: the spec, the verdict and what changes
/// with them.
struct Qc {
    spec: gtk::DropDown,
    verdict: gtk::Label,
    status: gtk::Label,
    null_box: gtk::Box,
    null_button: gtk::Button,
    save: gtk::Button,
    list: gtk::Box,
    files: Vec<Rendered>,
    what: String,
}

impl Qc {
    fn spec(&self) -> &'static Spec {
        SPECS
            .get(self.spec.selected() as usize)
            .unwrap_or(Spec::none())
    }

    /// The delivery is this window's (the session keeps the last one).
    fn report(&self, app: &Rc<AppState>) -> Option<QcReport> {
        let s = app.session.borrow();
        let d = s.delivery()?;
        let same = d.title == self.what
            && d.files.len() == self.files.len()
            && d.files
                .iter()
                .zip(&self.files)
                .all(|(a, b)| a.path == b.path);
        if !same {
            return None;
        }
        s.qc_report(self.spec())
    }

    /// Fill everything from the session's report for the chosen spec.
    fn refresh(&self, app: &Rc<AppState>) {
        let report = self.report(app);
        let can_test = {
            let s = app.session.borrow();
            s.delivery().is_some_and(|d| d.can_null_test()) && report.is_some()
        };
        let busy = app.session.borrow().qc_status();
        for c in ["error", "warning", "success", "dim-label"] {
            self.verdict.remove_css_class(c);
        }
        match &report {
            Some(r) => {
                self.verdict.set_text(&r.summary());
                self.verdict.add_css_class(verdict_class(r.verdict));
            }
            None => self
                .verdict
                .set_text("Not a delivery the QC report can be written of any more"),
        }
        self.save.set_sensitive(report.is_some() && !busy.writing);
        self.null_button
            .set_visible(can_test || busy.null_testing.is_some());
        self.null_button.set_sensitive(busy.null_testing.is_none());
        while let Some(c) = self.null_box.first_child() {
            self.null_box.remove(&c);
        }
        if let Some(n) = report.as_ref().and_then(|r| r.null_test.as_ref()) {
            let title = gtk::Label::new(Some(&format!("Null test — {}", n.verdict.label())));
            title.add_css_class("heading");
            title.set_xalign(0.0);
            self.null_box.append(&title);
            let grid = gtk::Grid::new();
            grid.set_row_spacing(4);
            grid.set_column_spacing(12);
            let mut y = 0;
            let at = y;
            row(&grid, &mut y, "Outcome", &n.summary);
            if let Some(w) = grid.child_at(1, at) {
                w.add_css_class(verdict_class(n.verdict));
            }
            let f = &n.residual;
            row(
                &grid,
                &mut y,
                "Residual",
                &format!(
                    "peak {}, RMS {}{}",
                    faderframe_qc::report::level(f.peak_dbfs, "dBFS"),
                    faderframe_qc::report::level(f.rms_dbfs, "dBFS"),
                    f.depth_db
                        .map_or_else(String::new, |d| format!(" — {d:.1} dB under the mix"))
                ),
            );
            if let (Some(first), Some(worst)) = (f.first_at, f.worst_at) {
                row(
                    &grid,
                    &mut y,
                    "Where",
                    &format!(
                        "first at {}, worst at {} (channel {})",
                        duration_text(first),
                        duration_text(worst),
                        f.worst_channel
                    ),
                );
            }
            row(&grid, &mut y, "Summed", &n.summed.join(", "));
            for l in &n.left_out {
                row(
                    &grid,
                    &mut y,
                    "Left out",
                    &format!("{} — {}", l.name, l.why),
                );
            }
            for c in &n.causes {
                let text = match &c.fix {
                    Some(fix) => format!("{}.\n{fix}.", c.text),
                    None => format!("{}.", c.text),
                };
                row(
                    &grid,
                    &mut y,
                    if c.kind == faderframe_qc::CauseKind::Dither {
                        "Note"
                    } else {
                        "Why"
                    },
                    &text,
                );
            }
            self.null_box.append(&grid);
        } else if can_test {
            let l = gtk::Label::new(Some(
                "The stems were not null-tested. Null Test renders the mix as the stems were \
                 rendered and compares their sum with it.",
            ));
            l.set_wrap(true);
            l.set_xalign(0.0);
            l.add_css_class("dim-label");
            self.null_box.append(&l);
        }
        self.null_box
            .set_visible(self.null_box.first_child().is_some());
        // The files, with what the spec says of each.
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        for (i, r) in self.files.iter().enumerate() {
            let qc = report.as_ref().and_then(|x| x.files.get(i));
            let grid = file_grid(r, qc);
            if self.files.len() == 1 {
                self.list.append(&grid);
                continue;
            }
            let summary = match &r.stats {
                Some(Ok(st)) => st.levels.as_ref().map_or_else(String::new, |l| {
                    let lufs = l
                        .loudness
                        .map(|x| format!(" · {}", db_text(x.integrated, "LUFS")))
                        .unwrap_or_default();
                    format!(" — {}{lufs}", db_text(l.true_peak, "dBTP"))
                }),
                _ => String::new(),
            };
            let verdict = qc
                .filter(|q| matches!(q.verdict, Verdict::Warn | Verdict::Fail))
                .map_or_else(String::new, |q| format!(" · {}", q.verdict.label()));
            let exp = gtk::Expander::new(Some(&format!("{}{summary}{verdict}", name_of(&r.path))));
            exp.set_expanded(i == 0);
            grid.set_margin_start(16);
            grid.set_margin_top(4);
            exp.set_child(Some(&grid));
            self.list.append(&exp);
        }
    }

    /// Follow the running QC work until it is done, then refresh.
    fn follow(self: &Rc<Self>, app: &Rc<AppState>) {
        let weak = Rc::downgrade(app);
        let qc = Rc::clone(self);
        glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            let Some(app) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let busy = app.session.borrow().qc_status();
            match busy.null_testing {
                Some(f) => qc
                    .status
                    .set_text(&format!("Null test: {:.0} %", f * 100.0)),
                None if busy.writing => qc.status.set_text("Writing the QC report…"),
                None => qc.status.set_text(""),
            }
            if busy.null_testing.is_none() && !busy.writing {
                qc.refresh(&app);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
}

/// Ask where the QC report goes, then write it ([`Action::SaveQcReport`]).
fn save_report(app: &Rc<AppState>, parent: Option<&gtk::Window>, qc: &Rc<Qc>) {
    let spec = qc.spec().id.to_string();
    let first = qc.files.first().map(|r| r.path.clone());
    let base = {
        let s = app.session.borrow();
        format!(
            "{} - {} QC Report.pdf",
            faderframe_qc::pdf::safe(&s.project().name).replace(['/', '\\'], "_"),
            qc.what.replace(['/', '\\', '“', '”'], "")
        )
    };
    let dialog = gtk::FileDialog::builder()
        .title("Save QC Report")
        .modal(true)
        .initial_name(&base)
        .build();
    if let Some(dir) = first.as_ref().and_then(|p| p.parent()) {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let weak = Rc::downgrade(app);
    let qc = Rc::clone(qc);
    dialog.save(parent, gio::Cancellable::NONE, move |res| {
        let Some(app) = weak.upgrade() else { return };
        let Some(path) = res.ok().and_then(|f| f.path()) else {
            return;
        };
        app.dispatch_chosen(
            faderframe_session::Action::SaveQcReport {
                path: path.clone(),
                spec,
            },
            &path,
        );
        qc.refresh(&app);
        qc.follow(&app);
    });
}

/// Write the QC report of the window opened last (scripted runs:
/// `qc-report:<path>[@<spec>]`); false when there is nothing to report.
pub fn save_report_to(app: &Rc<AppState>, path: &Path, spec: Option<&str>) -> bool {
    let id = spec
        .map(str::to_string)
        .or_else(|| {
            OPEN_QC
                .with(|q| q.borrow().as_ref().and_then(std::rc::Weak::upgrade))
                .map(|q| q.spec().id.to_string())
        })
        .unwrap_or_else(|| "none".into());
    if app.session.borrow().delivery().is_none() {
        return false;
    }
    app.dispatch(faderframe_session::Action::SaveQcReport {
        path: path.to_path_buf(),
        spec: id,
    });
    if let Some(q) = OPEN_QC.with(|q| q.borrow().as_ref().and_then(std::rc::Weak::upgrade)) {
        q.follow(app);
    }
    true
}

/// Null-test the open report's stems (scripted runs: `null-test:x` with
/// a report open); false when there is nothing to test.
pub fn null_test_now(app: &Rc<AppState>) -> bool {
    let can = app
        .session
        .borrow()
        .delivery()
        .is_some_and(|d| d.can_null_test());
    if !can {
        return false;
    }
    app.dispatch(faderframe_session::Action::NullTestStems);
    if let Some(q) = OPEN_QC.with(|q| q.borrow().as_ref().and_then(std::rc::Weak::upgrade)) {
        q.refresh(app);
        q.follow(app);
    }
    true
}

thread_local! {
    /// The QC part of the report window opened last.
    static OPEN_QC: RefCell<Option<std::rc::Weak<Qc>>> = const { RefCell::new(None) };
}

/// The report of a finished render (`what`: "Track “Bass”", "Master",
/// "Stems"), with its QC checks against a spec, the stems' null test and
/// Save QC Report… (the last delivery's: [`Session::delivery`]).
///
/// [`Session::delivery`]: faderframe_session::Session::delivery
pub fn show(app: &Rc<AppState>, parent: Option<&gtk::Window>, what: &str, files: &[Rendered]) {
    let win = gtk::Window::builder()
        .application(&app.app)
        .title(format!("Render Report — {what}"))
        .default_width(680)
        .build();
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let s = RenderSummary::of(files);
    let heading = gtk::Label::new(Some(&format!(
        "{what}: {} file{} · {} · {}",
        s.files,
        if s.files == 1 { "" } else { "s" },
        duration_text(s.seconds),
        size_text(s.bytes)
    )));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    heading.set_wrap(true);
    body.append(&heading);
    let speed = gtk::Label::new(Some(&format!(
        "Rendered in {}{}",
        speed_text(s.render_seconds, s.realtime_factor()),
        if s.warned > 0 {
            format!(
                " · {} file{} with clipping warnings",
                s.warned,
                if s.warned == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        }
    )));
    speed.set_xalign(0.0);
    body.append(&speed);
    let source = gtk::Label::new(Some(
        "Measured from each file as written — read back from disk after rendering, at the \
         file's rate, channels and format (dither included) — not from the live tracks.",
    ));
    source.set_wrap(true);
    source.set_xalign(0.0);
    source.add_css_class("dim-label");
    body.append(&source);

    // QC: the spec, the verdict, the null test, the report.
    let names: Vec<&str> = SPECS.iter().map(|s| s.name).collect();
    let spec = gtk::DropDown::from_strings(&names);
    spec.set_tooltip_text(Some(
        "The delivery spec the files are checked against (loudness, true peak, format, length) \
         besides the checks every delivery gets",
    ));
    let album_spec = app
        .session
        .borrow()
        .project()
        .album
        .settings
        .qc_spec
        .clone()
        .filter(|_| what.starts_with("Album"))
        .and_then(|id| SPECS.iter().position(|s| s.id == id));
    spec.set_selected(
        album_spec.map_or_else(|| LAST_SPEC.with(std::cell::Cell::get), |i| i as u32),
    );
    let verdict = gtk::Label::new(None);
    verdict.set_xalign(0.0);
    verdict.set_wrap(true);
    verdict.set_hexpand(true);
    let null_button = gtk::Button::with_label("Null Test");
    null_button.set_tooltip_text(Some(
        "Render the mix as the stems were rendered and compare their sum with it",
    ));
    let save = gtk::Button::with_label("Save QC Report…");
    save.set_tooltip_text(Some(
        "A PDF and a JSON file: every file's facts, levels, checks against the spec and \
         SHA-256, the null test, the project and the date",
    ));
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spec_label = gtk::Label::new(Some("Spec"));
    spec_label.add_css_class("dim-label");
    bar.append(&spec_label);
    bar.append(&spec);
    bar.append(&null_button);
    bar.append(&save);
    body.append(&bar);
    body.append(&verdict);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    body.append(&status);
    let null_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.append(&null_box);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .vexpand(true)
        .build();
    body.append(&scroll);
    let qc = Rc::new(Qc {
        spec: spec.clone(),
        verdict,
        status,
        null_box,
        null_button: null_button.clone(),
        save: save.clone(),
        list,
        files: files.to_vec(),
        what: what.to_string(),
    });
    qc.refresh(app);
    // (The handlers hold the QC part weakly: the window holds it.)
    {
        let weak = Rc::downgrade(app);
        let q = Rc::downgrade(&qc);
        spec.connect_selected_notify(move |d| {
            LAST_SPEC.with(|l| l.set(d.selected()));
            if let (Some(app), Some(q)) = (weak.upgrade(), q.upgrade()) {
                q.refresh(&app);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        let q = Rc::downgrade(&qc);
        null_button.connect_clicked(move |_| {
            if let (Some(app), Some(q)) = (weak.upgrade(), q.upgrade()) {
                app.dispatch(faderframe_session::Action::NullTestStems);
                q.refresh(&app);
                q.follow(&app);
            }
        });
    }
    {
        let weak = Rc::downgrade(app);
        let q = Rc::downgrade(&qc);
        let w = win.downgrade();
        save.connect_clicked(move |_| {
            if let (Some(app), Some(q)) = (weak.upgrade(), q.upgrade()) {
                let win = w.upgrade();
                save_report(&app, win.as_ref(), &q);
            }
        });
    }
    OPEN_QC.with(|o| *o.borrow_mut() = Some(Rc::downgrade(&qc)));
    // The window holds the QC part as long as it is open.
    let held = RefCell::new(Some(Rc::clone(&qc)));
    win.connect_close_request(move |_| {
        held.borrow_mut().take();
        glib::Propagation::Proceed
    });

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let reveal = gtk::Button::with_label("Show in Folder");
    reveal.set_tooltip_text(Some("Open the folder the file was written to"));
    let close = gtk::Button::with_label("Close");
    close.add_css_class("suggested-action");
    actions.append(&reveal);
    actions.append(&close);
    body.append(&actions);
    win.set_child(Some(&body));

    let first = files.first().map(|r| r.path.clone());
    reveal.set_sensitive(first.is_some());
    let w = win.downgrade();
    reveal.connect_clicked(move |_| {
        if let Some(path) = &first {
            let win = w.upgrade();
            show_in_folder(win.as_ref(), path);
        }
    });
    let w = win.downgrade();
    close.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    // Escape closes it too.
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
    win.set_default_widget(Some(&close));
    close.grab_focus();
    win.present();
}
