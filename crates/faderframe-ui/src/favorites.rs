//! Favorite plugins on disk: `plugin-favorites.json` in the configuration
//! folder (portable mode: the portable folder's `Settings`), next to the
//! preferences. The session holds them while FaderFrame runs
//! ([`faderframe_session::plugin_favorites`]); this saves them whenever
//! they changed.

use crate::state::AppState;
use faderframe_session::Session;
use faderframe_session::plugin_favorites::PluginFavorites;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    /// The session's favorites revision last written (or read).
    static SAVED: Cell<u64> = const { Cell::new(0) };
}

pub fn path() -> PathBuf {
    crate::paths::config_dir().join("plugin-favorites.json")
}

/// Give the session the favorites saved last time (start-up).
pub fn load_into(session: &mut Session) {
    let (favorites, warning) = PluginFavorites::load(&path());
    if let Some(w) = warning {
        tracing::warn!("plugin favorites: {w}");
        session.notify(
            faderframe_session::NoticeLevel::Warning,
            format!("Favorite plugins: {w}"),
        );
    }
    SAVED.with(|s| s.set(session.favorites_revision()));
    // (Names the catalogue changed since count as a change: saved.)
    session.set_plugin_favorites(favorites);
}

/// Save the favorites if they changed since the last call (after every
/// change the shell sees).
pub fn follow(app: &Rc<AppState>) {
    let Ok(s) = app.session.try_borrow() else {
        return;
    };
    let revision = s.favorites_revision();
    if SAVED.with(Cell::get) == revision {
        return;
    }
    match s.plugin_favorites().save(&path()) {
        Ok(()) => SAVED.with(|c| c.set(revision)),
        Err(e) => {
            tracing::warn!("cannot save the plugin favorites: {e}");
            // Not again for this revision (the next change tries again).
            SAVED.with(|c| c.set(revision));
        }
    }
}
