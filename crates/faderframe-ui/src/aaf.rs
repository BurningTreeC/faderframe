//! AAF from the File menu and scripts (`FADERFRAME_STARTUP_ACTIONS`):
//!
//! * `app.export-aaf` — target `@embed` or `@link` (File → Export AAF…):
//!   a save dialog, then [`Action::ExportAaf`]; `export-aaf:<path>[@embed|link]`
//!   in scripts exports at once (a file there is asked about first);
//! * `app.import-aaf` — target `""` (File → Import AAF…): an open dialog;
//!   `import-aaf:<path>` imports at once;
//! * `relink-aaf:<folder|none>` answers an import's missing media
//!   (otherwise [`UiRequest::RelinkAaf`] asks with [`relink`]).
//!
//! [`UiRequest::RelinkAaf`]: faderframe_session::UiRequest::RelinkAaf

use crate::state::AppState;
use faderframe_session::Action;
use faderframe_session::aaf::AafMedia;
use gtk::gio;
use gtk::prelude::*;
use std::path::{Path, PathBuf};
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
    app.app.add_action_entries([
        named("export-aaf", export),
        named("import-aaf", import),
        named("relink-aaf", |a, arg| {
            let folder = match arg.trim() {
                "" | "none" => None,
                p => Some(PathBuf::from(p)),
            };
            a.dispatch(Action::RelinkAaf(folder));
        }),
    ]);
}

/// `<path>[@embed|link]`, or only `@embed`/`@link` (ask for the path).
fn parse_export(arg: &str) -> (Option<PathBuf>, AafMedia) {
    let (path, media) = match arg.rsplit_once('@') {
        Some((p, "link")) => (p, AafMedia::Link),
        Some((p, "embed")) => (p, AafMedia::Embed),
        _ => (arg, AafMedia::Embed),
    };
    let path = path.trim();
    ((!path.is_empty()).then(|| PathBuf::from(path)), media)
}

fn aaf_filters() -> gio::ListStore {
    let f = gtk::FileFilter::new();
    f.set_name(Some("AAF compositions (.aaf)"));
    f.add_suffix("aaf");
    f.add_suffix("AAF");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&f);
    store
}

fn export(app: &Rc<AppState>, arg: &str) {
    let (path, media) = parse_export(arg);
    if let Some(path) = path {
        app.dispatch(Action::ExportAaf { path, media });
        return;
    }
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let name = format!("{}.aaf", app.session.borrow().project().name);
    let dialog = gtk::FileDialog::builder()
        .title(match media {
            AafMedia::Embed => "Export AAF with the Audio Inside",
            AafMedia::Link => "Export AAF with Linked WAVE Files",
        })
        .accept_label("Export")
        .modal(true)
        .initial_name(name)
        .filters(&aaf_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        let path = with_extension(&path);
        app.dispatch_chosen(
            Action::ExportAaf {
                path: path.clone(),
                media,
            },
            &path,
        );
    });
}

/// `name` → `name.aaf`.
fn with_extension(path: &Path) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("aaf"))
    {
        path.to_path_buf()
    } else {
        let mut p = path.as_os_str().to_owned();
        p.push(".aaf");
        PathBuf::from(p)
    }
}

fn import(app: &Rc<AppState>, arg: &str) {
    if !arg.trim().is_empty() {
        app.dispatch(Action::ImportAaf(PathBuf::from(arg.trim())));
        return;
    }
    let Some(win) = app.window.borrow().clone() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import AAF")
        .accept_label("Import")
        .modal(true)
        .filters(&aaf_filters())
        .build();
    let weak = Rc::downgrade(app);
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let (Ok(file), Some(app)) = (res, weak.upgrade()) else {
            return;
        };
        if let Some(path) = file.path() {
            app.dispatch(Action::ImportAaf(path));
        }
    });
}

/// An import's linked media were not found: look in a folder, or import
/// without them (they stay offline).
pub(crate) fn relink(app: &Rc<AppState>, file: &Path, missing: &[String]) {
    let Some(win) = app.window.borrow().clone() else {
        app.dispatch(Action::RelinkAaf(None));
        return;
    };
    let shown: Vec<&str> = missing.iter().take(8).map(String::as_str).collect();
    let more = missing.len().saturating_sub(shown.len());
    let dialog = gtk::AlertDialog::builder()
        .message(format!(
            "{} media file{} of “{}” {} not where the AAF says",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            file.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().to_string()),
            if missing.len() == 1 { "is" } else { "are" }
        ))
        .detail(format!(
            "{}{}\n\nLook for them in a folder (and the folders in it), or import without \
             them: their clips stay offline until the files are found.",
            shown.join("\n"),
            if more > 0 {
                format!("\n… and {more} more")
            } else {
                String::new()
            }
        ))
        .buttons(["Import Without Them", "Look in a Folder…"])
        .cancel_button(0)
        .default_button(1)
        .modal(true)
        .build();
    let weak = Rc::downgrade(app);
    let parent = win.clone();
    dialog.choose(Some(&win), gio::Cancellable::NONE, move |res| {
        let Some(app) = weak.upgrade() else { return };
        if res != Ok(1) {
            app.dispatch(Action::RelinkAaf(None));
            return;
        }
        let folder = gtk::FileDialog::builder()
            .title("Where the Media Are")
            .accept_label("Look Here")
            .modal(true)
            .build();
        let weak = Rc::downgrade(&app);
        folder.select_folder(Some(&parent), gio::Cancellable::NONE, move |res| {
            let Some(app) = weak.upgrade() else { return };
            let folder = res.ok().and_then(|f| f.path());
            app.dispatch(Action::RelinkAaf(folder));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_arguments_name_the_file_and_the_media() {
        assert_eq!(parse_export("@link"), (None, AafMedia::Link));
        assert_eq!(parse_export(""), (None, AafMedia::Embed));
        assert_eq!(
            parse_export("/tmp/Reel 1.aaf@embed"),
            (Some(PathBuf::from("/tmp/Reel 1.aaf")), AafMedia::Embed)
        );
        assert_eq!(
            parse_export("/tmp/a.aaf"),
            (Some(PathBuf::from("/tmp/a.aaf")), AafMedia::Embed)
        );
        assert_eq!(
            with_extension(Path::new("/tmp/x")),
            PathBuf::from("/tmp/x.aaf")
        );
        assert_eq!(
            with_extension(Path::new("/tmp/x.AAF")),
            PathBuf::from("/tmp/x.AAF")
        );
    }
}
