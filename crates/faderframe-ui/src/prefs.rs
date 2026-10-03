//! User preferences persisted in `$XDG_CONFIG_HOME/faderframe/preferences.json`.

use crate::state::BackendChoice;
use gtk::glib;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// "auto", "jack" or "dummy".
    pub backend: String,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    pub snap: bool,
    pub follow_playhead: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            sample_rate: None,
            buffer_size: None,
            snap: true,
            follow_playhead: true,
        }
    }
}

impl Preferences {
    pub fn path() -> PathBuf {
        glib::user_config_dir()
            .join("faderframe")
            .join("preferences.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn backend(&self) -> BackendChoice {
        match self.backend.as_str() {
            "jack" => BackendChoice::Jack,
            "dummy" => BackendChoice::Dummy,
            _ => BackendChoice::Auto,
        }
    }

    pub fn set_backend(&mut self, b: BackendChoice) {
        self.backend = match b {
            BackendChoice::Auto => "auto",
            BackendChoice::Jack => "jack",
            BackendChoice::Dummy => "dummy",
        }
        .into();
    }
}
