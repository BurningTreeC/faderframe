//! Recently used projects (File → Open Recent) and what a new start of the
//! app opens.

use crate::prefs::Preferences;
use crate::state::AppState;
use gtk::gio;
use gtk::prelude::*;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Projects remembered.
pub const MAX_RECENT: usize = 10;

/// What a new start of the app opens (Preferences → General).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartupProject {
    /// The project used last (the demo session on a first start).
    #[default]
    Last,
    /// A new, empty project.
    New,
    /// The demo session.
    Demo,
}

impl StartupProject {
    pub const ALL: [StartupProject; 3] = [Self::Last, Self::New, Self::Demo];

    pub fn id(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::New => "new",
            Self::Demo => "demo",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Last => "Open the last project",
            Self::New => "Start a new project (from the default template, if one is set)",
            Self::Demo => "Open the demo session",
        }
    }
}

/// Put `path` first (absolute, without duplicates, at most [`MAX_RECENT`]).
pub fn push(list: &mut Vec<String>, path: &Path) {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let key = path.to_string_lossy().to_string();
    list.retain(|p| *p != key);
    list.insert(0, key);
    list.truncate(MAX_RECENT);
}

/// "Song — ~/Music/Projects" for a menu entry.
pub fn label(path: &str) -> String {
    let p = Path::new(path);
    let name = p
        .file_stem()
        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().to_string());
    let dir = p
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let home = gtk::glib::home_dir().to_string_lossy().to_string();
    let dir = match dir.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => dir,
    };
    // Menu labels treat '_' as a mnemonic.
    format!("{name} — {dir}").replace('_', "__")
}

/// Fill the File → Open Recent menu.
pub fn rebuild_menu(menu: &gio::Menu, list: &[String]) {
    menu.remove_all();
    let entries = gio::Menu::new();
    if list.is_empty() {
        entries.append(Some("No recent projects"), None);
    }
    for path in list {
        let item = gio::MenuItem::new(Some(&label(path)), None);
        item.set_action_and_target_value(Some("app.open-recent"), Some(&path.to_variant()));
        entries.append_item(&item);
    }
    menu.append_section(None, &entries);
    if !list.is_empty() {
        let clear = gio::Menu::new();
        clear.append(Some("Clear Recent Projects"), Some("app.clear-recent"));
        menu.append_section(None, &clear);
    }
}

/// Remember the session's file when it changed (opened, saved under a new
/// name, loaded at start-up). Called after every change.
pub fn note_session_path(app: &Rc<AppState>) {
    let path = app.session.borrow().path().map(Path::to_path_buf);
    if path.is_none() || *app.shown_path.borrow() == path {
        return;
    }
    app.shown_path.replace(path.clone());
    if let Some(path) = path {
        let mut prefs = Preferences::load();
        push(&mut prefs.recent_projects, &path);
        if let Err(e) = prefs.save() {
            tracing::warn!("cannot save preferences: {e}");
        }
        rebuild_menu(&app.recent_menu, &prefs.recent_projects);
    }
}

/// Forget a project in the saved list (before the menu exists).
pub fn forget_path(path: &Path) {
    let key = path.to_string_lossy();
    let mut prefs = Preferences::load();
    prefs.recent_projects.retain(|p| *p != key);
    if let Err(e) = prefs.save() {
        tracing::warn!("cannot save preferences: {e}");
    }
}

/// Forget a project (it could not be opened).
pub fn forget(app: &Rc<AppState>, path: &str) {
    let mut prefs = Preferences::load();
    prefs.recent_projects.retain(|p| p != path);
    let _ = prefs.save();
    rebuild_menu(&app.recent_menu, &prefs.recent_projects);
}

/// File → Open Recent → a project (after asking about unsaved changes).
pub fn open(app: &Rc<AppState>, path: String) {
    crate::dialogs::confirm_discard(app, move |a| {
        let p = PathBuf::from(&path);
        if !p.exists() {
            a.session.borrow_mut().notify(
                faderframe_session::NoticeLevel::Warning,
                format!(
                    "{} no longer exists; removed from the recent projects",
                    p.display()
                ),
            );
            forget(a, &path);
            a.after_change();
            return;
        }
        a.with_session(|s| s.open(&p));
    });
}

pub fn clear(app: &Rc<AppState>) {
    let mut prefs = Preferences::load();
    prefs.recent_projects.clear();
    let _ = prefs.save();
    rebuild_menu(&app.recent_menu, &prefs.recent_projects);
}

/// The project a new start opens: `Some(path)` for the last project when
/// that is the choice and it still exists.
pub fn startup_path(prefs: &Preferences) -> Option<PathBuf> {
    (StartupProject::from_id(&prefs.startup_project).unwrap_or_default() == StartupProject::Last)
        .then(|| prefs.recent_projects.first().map(PathBuf::from))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_list_is_most_recent_first_without_duplicates() {
        let mut l = Vec::new();
        for i in 0..14 {
            push(&mut l, Path::new(&format!("/p/song{i}.ffproj")));
        }
        // Stored absolute (on Windows with the current drive).
        let abs = |p: &str| {
            std::path::absolute(p)
                .unwrap()
                .to_string_lossy()
                .to_string()
        };
        assert_eq!(l.len(), MAX_RECENT);
        assert_eq!(l[0], abs("/p/song13.ffproj"));
        push(&mut l, Path::new("/p/song8.ffproj"));
        assert_eq!(l[0], abs("/p/song8.ffproj"));
        assert_eq!(l.iter().filter(|p| p.ends_with("song8.ffproj")).count(), 1);
        assert_eq!(label("/x/my_song.ffproj"), "my__song — /x");
    }

    #[test]
    fn startup_choice_and_fallbacks() {
        let mut p = Preferences::default();
        assert_eq!(startup_path(&p), None, "nothing used yet");
        p.recent_projects = vec!["/a/b.ffproj".into()];
        assert_eq!(startup_path(&p), Some(PathBuf::from("/a/b.ffproj")));
        p.startup_project = "new".into();
        assert_eq!(startup_path(&p), None);
        assert_eq!(StartupProject::from_id("demo"), Some(StartupProject::Demo));
    }

    #[test]
    fn the_menu_lists_projects_and_a_clear_entry() {
        let m = gio::Menu::new();
        rebuild_menu(&m, &[]);
        assert_eq!(m.n_items(), 1, "a placeholder section");
        rebuild_menu(&m, &["/a/one.ffproj".into(), "/b/two.ffproj".into()]);
        assert_eq!(m.n_items(), 2, "projects and the clear section");
        let projects = m
            .item_link(0, gio::MENU_LINK_SECTION)
            .and_then(|l| l.downcast::<gio::Menu>().ok())
            .unwrap();
        assert_eq!(projects.n_items(), 2);
        let target = projects
            .item_attribute_value(1, gio::MENU_ATTRIBUTE_TARGET, None)
            .and_then(|v| v.get::<String>());
        assert_eq!(target.as_deref(), Some("/b/two.ffproj"));
    }
}
