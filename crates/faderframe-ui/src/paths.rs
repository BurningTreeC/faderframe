//! Where the shell keeps its files: the usual per-user folders, or the
//! portable folder (see [`faderframe_core::paths`]).

use gtk::glib;
use std::path::PathBuf;

/// Preferences.
pub fn config_dir() -> PathBuf {
    faderframe_core::paths::portable("Settings")
        .unwrap_or_else(|| glib::user_config_dir().join("faderframe"))
}

/// Plugin scan caches, generated icons.
pub fn cache_dir() -> PathBuf {
    faderframe_core::paths::portable("Cache")
        .unwrap_or_else(|| glib::user_cache_dir().join("faderframe"))
}
