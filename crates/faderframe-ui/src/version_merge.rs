//! Merging versions in the shell (see `faderframe_session::version_merge`):
//! File → Merge Project File… (a chooser), the Versions window's Merge
//! buttons, and the Merge window that lists the last merge's conflicts with
//! a choice each (ours, theirs, both where it makes sense) and a button to
//! hear the other choice against the merge as it is (the A/B compare
//! stage).
//!
//! Development aids:
//!
//! * `merge-version:<theirs n>[@<base n>]` merges version n (the base
//!   found by where the two come from, or version b);
//! * `merge-file:<path>` merges a project file from elsewhere;
//! * `merge-resolve:<n>=<ours|theirs|both>` chooses for conflict n (from
//!   1, the Merge window's order);
//! * `merge-audition:<n>` hears conflict n's other choice;
//! * `merge-log:x` logs the merge (changes, conflicts, media).

use crate::state::AppState;
use faderframe_project::merge::{Conflict, Resolution};
use faderframe_session::{Action, NoticeLevel};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) fn install(app: &Rc<AppState>) {
    let named = |name: &'static str, f: fn(&Rc<AppState>, &str)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .parameter_type(Some(&String::static_variant_type()))
            .activate(move |_, _, param| {
                if let (Some(a), Some(arg)) =
                    (weak.upgrade(), param.and_then(|p| p.get::<String>()))
                {
                    f(&a, &arg);
                }
            })
            .build()
    };
    let plain = |name: &'static str, f: fn(&Rc<AppState>)| {
        let weak = Rc::downgrade(app);
        gio::ActionEntry::builder(name)
            .activate(move |_, _, _| {
                if let Some(a) = weak.upgrade() {
                    f(&a);
                }
            })
            .build()
    };
    app.app.add_action_entries([
        plain("merge-project-file", |a| {
            a.dispatch(Action::PromptMergeProjectFile)
        }),
        plain("merge-conflicts", window),
        named("merge-version", |a, arg| {
            let (theirs, base) = match arg.split_once('@') {
                Some((t, b)) => (t, Some(b)),
                None => (arg, None),
            };
            let Some(theirs) = version_path(a, theirs) else {
                return tracing::warn!("merge-version: no version '{theirs}'");
            };
            let base = match base {
                Some(b) => match version_path(a, b) {
                    Some(p) => Some(p),
                    None => return tracing::warn!("merge-version: no version '{b}'"),
                },
                None => None,
            };
            a.dispatch(Action::MergeVersion { theirs, base });
        }),
        named("merge-file", |a, arg| {
            a.dispatch(Action::MergeProjectFile(PathBuf::from(arg.trim())));
        }),
        named("merge-resolve", |a, arg| {
            let parsed = arg
                .split_once('=')
                .and_then(|(n, r)| Some((n.trim().parse::<usize>().ok()?, Resolution::parse(r)?)));
            let Some((n, resolution)) = parsed else {
                return tracing::warn!("merge-resolve: '{arg}' is not <n>=<ours|theirs|both>");
            };
            match conflict_key(a, n) {
                Some(key) => a.dispatch(Action::ResolveMergeConflict { key, resolution }),
                None => tracing::warn!("merge-resolve: the merge has no conflict {n}"),
            }
        }),
        named("merge-audition", |a, arg| {
            match arg.trim().parse().ok().and_then(|n| conflict_key(a, n)) {
                Some(key) => a.dispatch(Action::AuditionMergeConflict { key }),
                None => tracing::warn!("merge-audition: the merge has no conflict '{arg}'"),
            }
        }),
        named("merge-log", |a, _| {
            let Some(m) = a.session.borrow().merge_status() else {
                return tracing::info!("merge: none");
            };
            tracing::info!("merge: {} (from {})", m.label, m.base);
            for c in &m.changes {
                tracing::info!("merge: change: {c}");
            }
            for (i, c) in m.conflicts.iter().enumerate() {
                tracing::info!(
                    "merge: conflict {}: {} [{}]",
                    i + 1,
                    c.summary(),
                    c.chosen.label()
                );
            }
            for (theirs, here) in &m.media.found {
                tracing::info!("merge: media: {theirs} found as {here}");
            }
            for f in &m.media.elsewhere {
                tracing::info!("merge: media: used where it is: {f}");
            }
            for f in &m.media.missing {
                tracing::info!("merge: media: missing: {f}");
            }
        }),
    ]);
}

/// A version by its number (or name).
fn version_path(app: &Rc<AppState>, which: &str) -> Option<PathBuf> {
    let which = which.trim();
    let versions = app.session.borrow().versions();
    versions
        .iter()
        .find(|v| which.parse::<u32>().is_ok_and(|n| n == v.number))
        .or_else(|| versions.iter().find(|v| v.name == which))
        .map(|v| v.path.clone())
}

/// Conflict `n` (from 1) of the last merge.
fn conflict_key(app: &Rc<AppState>, n: usize) -> Option<String> {
    let m = app.session.borrow().merge_status()?;
    m.conflicts.get(n.checked_sub(1)?).map(|c| c.key.clone())
}

/// `UiRequest::MergeProjectFile`: pick a project file, then merge it.
pub fn choose_file(app: &Rc<AppState>) {
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Merge Project File")
        .accept_label("Merge")
        .modal(true)
        .filters(&crate::dialogs::project_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(f), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        match f.path() {
            Some(path) => app.dispatch(Action::MergeProjectFile(path)),
            None => app.session.borrow_mut().notify(
                NoticeLevel::Warning,
                "only local project files can be merged",
            ),
        }
    });
}

/// What the Merge window shows: the merge, its choices, whether open.
type Shown = (String, Vec<(String, Resolution)>, bool);

thread_local! {
    /// The Merge window while it is open.
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
}

/// The last merge: what it brought, its conflicts with a choice each.
pub fn window(app: &Rc<AppState>) {
    if let Some(win) = OPEN.with(|o| o.borrow().as_ref().and_then(glib::WeakRef::upgrade)) {
        win.present();
        return;
    }
    let Some(main) = app.window.borrow().clone() else {
        return;
    };
    let win = gtk::Window::builder()
        .title("Merge")
        .application(&app.app)
        .transient_for(&main)
        .default_width(720)
        .default_height(560)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    for m in [
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
    ] {
        m(body.upcast_ref(), 14);
    }
    let heading = gtk::Label::new(None);
    heading.set_xalign(0.0);
    heading.add_css_class("title-3");
    let intro = gtk::Label::new(None);
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.add_css_class("dim-label");
    body.append(&heading);
    body.append(&intro);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    body.append(&scroll);
    let details = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(8)
        .top_margin(6)
        .build();
    let details_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(130)
        .child(&details)
        .build();
    body.append(&details_scroll);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let mix = gtk::Button::with_label("Hear the Merge");
    mix.set_tooltip_text(Some("Back to the mix as merged (stops comparing)"));
    let done = gtk::Button::with_label("Done");
    done.add_css_class("suggested-action");
    buttons.append(&mix);
    buttons.append(&done);
    body.append(&buttons);
    win.set_child(Some(&body));

    // What is shown, rebuilt when the merge changes.
    let shown: Rc<RefCell<Option<Shown>>> = Rc::new(RefCell::new(None));
    let fill: Rc<dyn Fn()> = {
        let (weak, list, heading, intro, details, shown) = (
            Rc::downgrade(app),
            list.clone(),
            heading.clone(),
            intro.clone(),
            details.clone(),
            Rc::clone(&shown),
        );
        Rc::new(move || {
            let Some(app) = weak.upgrade() else { return };
            let status = app.session.borrow().merge_status();
            let key = status.as_ref().map(|m| {
                (
                    m.label.clone(),
                    m.conflicts
                        .iter()
                        .map(|c| (c.key.clone(), c.chosen))
                        .collect::<Vec<_>>(),
                    m.open,
                )
            });
            if *shown.borrow() == key {
                return;
            }
            *shown.borrow_mut() = key;
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let Some(m) = status else {
                heading.set_text("No merge");
                intro.set_text("Merge a version (File → Versions…) or a project file (File → Merge Project File…).");
                details.buffer().set_text("");
                return;
            };
            heading.set_text(&m.label);
            intro.set_text(&format!(
                "From {} — {} change{}, {} conflict{}. {}",
                m.base,
                m.changes.len(),
                if m.changes.len() == 1 { "" } else { "s" },
                m.conflicts.len(),
                if m.conflicts.len() == 1 { "" } else { "s" },
                if !m.open {
                    "The project changed since: undo back to the merge to change its choices."
                } else if m.conflicts.is_empty() {
                    "Everything merged; one undo takes it back."
                } else {
                    "Where both sides changed the same thing this project's version is kept; choose otherwise below. One undo takes the merge back."
                }
            ));
            if m.conflicts.is_empty() {
                let none = gtk::Label::new(Some("No conflicts."));
                none.set_margin_top(18);
                none.set_margin_bottom(18);
                none.add_css_class("dim-label");
                list.append(&none);
            }
            for (i, c) in m.conflicts.iter().enumerate() {
                list.append(&conflict_row(&app, i, c, m.open));
            }
            let mut text = String::new();
            if !m.changes.is_empty() {
                text.push_str("What the merge changed:\n• ");
                text.push_str(&m.changes.join("\n• "));
                text.push('\n');
            }
            let media = &m.media;
            if !media.found.is_empty() {
                text.push_str("\nMedia found here by content:\n");
                for (theirs, here) in &media.found {
                    text.push_str(&format!("• {theirs} → {here}\n"));
                }
            }
            if !media.elsewhere.is_empty() {
                text.push_str("\nMedia used where they are (not in this project's folder):\n• ");
                text.push_str(&media.elsewhere.join("\n• "));
                text.push('\n');
            }
            if !media.missing.is_empty() {
                text.push_str("\nMedia missing (silent until found):\n• ");
                text.push_str(&media.missing.join("\n• "));
                text.push('\n');
            }
            if !m.notes.is_empty() {
                text.push_str("\nPut right afterwards:\n• ");
                text.push_str(&m.notes.join("\n• "));
            }
            details.buffer().set_text(text.trim());
        })
    };
    fill();
    let weak = Rc::downgrade(app);
    mix.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::Compare(faderframe_session::compare::CompareOp::Off));
        }
    });
    let (weak, w) = (Rc::downgrade(app), win.downgrade());
    done.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::CloseMerge);
        }
        if let Some(w) = w.upgrade() {
            w.close();
        }
    });
    // Choices made here or by a script, undo/redo: the rows follow.
    let (weak, w) = (Rc::downgrade(app), win.downgrade());
    glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
        if weak.upgrade().is_none() || w.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        fill();
        glib::ControlFlow::Continue
    });
    OPEN.with(|o| *o.borrow_mut() = Some(win.downgrade()));
    win.present();
}

/// One conflict: what, how the sides differ, the choices, a listen.
fn conflict_row(app: &Rc<AppState>, i: usize, c: &Conflict, open: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    for m in [
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
    ] {
        m(row.upcast_ref(), 8);
    }
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let number = gtk::Label::new(Some(&(i + 1).to_string()));
    number.add_css_class("dim-label");
    number.set_width_chars(3);
    let what = gtk::Label::new(Some(&c.what));
    what.set_xalign(0.0);
    what.set_hexpand(true);
    what.set_ellipsize(gtk::pango::EllipsizeMode::End);
    what.add_css_class("heading");
    top.append(&number);
    top.append(&what);
    let mut group: Option<gtk::ToggleButton> = None;
    for r in &c.choices {
        let b = gtk::ToggleButton::with_label(r.label());
        b.set_active(*r == c.chosen);
        b.set_sensitive(open);
        if let Some(g) = &group {
            b.set_group(Some(g));
        } else {
            group = Some(b.clone());
        }
        let (weak, key, r) = (Rc::downgrade(app), c.key.clone(), *r);
        b.connect_toggled(move |b| {
            if !b.is_active() {
                return;
            }
            let Some(app) = weak.upgrade() else { return };
            let chosen = app
                .session
                .borrow()
                .merge_status()
                .and_then(|m| m.conflicts.into_iter().find(|c| c.key == key))
                .map(|c| c.chosen);
            if chosen != Some(r) {
                app.dispatch(Action::ResolveMergeConflict {
                    key: key.clone(),
                    resolution: r,
                });
            }
        });
        top.append(&b);
    }
    let listen = gtk::Button::with_label("Hear Other");
    listen.set_tooltip_text(Some(
        "Render the merge with the other choice for this conflict and hear it against the merge as it is (Ctrl+Alt+A flips)",
    ));
    listen.set_sensitive(open);
    let (weak, key) = (Rc::downgrade(app), c.key.clone());
    listen.connect_clicked(move |_| {
        if let Some(app) = weak.upgrade() {
            app.dispatch(Action::AuditionMergeConflict { key: key.clone() });
        }
    });
    top.append(&listen);
    row.append(&top);
    let how = gtk::Label::new(Some(&how(c)));
    how.set_xalign(0.0);
    how.set_wrap(true);
    how.set_margin_start(34);
    how.add_css_class("dim-label");
    row.append(&how);
    row
}

/// How the two sides differ, in words.
fn how(c: &Conflict) -> String {
    c.summary()
        .split_once(" — ")
        .map_or_else(String::new, |(_, how)| how.to_string())
}
