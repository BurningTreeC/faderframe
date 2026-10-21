//! After a render: the statistics of what was written (one section per
//! file; measured by `faderframe_session::render::stats` from the files
//! as written, read back from disk on the render's worker), and the
//! question asked before an existing file is replaced.

use crate::state::AppState;
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

/// One file's facts and levels.
fn file_grid(r: &Rendered) -> gtk::Grid {
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
    grid
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

/// The report of a finished render (`what`: "Track “Bass”", "Master",
/// "Stems").
pub fn show(app: &Rc<AppState>, parent: Option<&gtk::Window>, what: &str, files: &[Rendered]) {
    let win = gtk::Window::builder()
        .application(&app.app)
        .title(format!("Render Report — {what}"))
        .default_width(620)
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

    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for (i, r) in files.iter().enumerate() {
        let grid = file_grid(r);
        if files.len() == 1 {
            list.append(&grid);
            continue;
        }
        let summary = match &r.stats {
            Some(Ok(st)) => st.levels.as_ref().map_or_else(String::new, |l| {
                let lufs = l
                    .loudness
                    .map(|x| format!(" · {}", db_text(x.integrated, "LUFS")))
                    .unwrap_or_default();
                let warn = if st.warnings().is_empty() {
                    ""
                } else {
                    " · clipping"
                };
                format!(" — {}{lufs}{warn}", db_text(l.true_peak, "dBTP"))
            }),
            _ => String::new(),
        };
        let exp = gtk::Expander::new(Some(&format!("{}{summary}", name_of(&r.path))));
        exp.set_expanded(i == 0);
        grid.set_margin_start(16);
        grid.set_margin_top(4);
        exp.set_child(Some(&grid));
        list.append(&exp);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .vexpand(true)
        .build();
    body.append(&scroll);

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
