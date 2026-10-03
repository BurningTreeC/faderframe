//! User preferences persisted in `$XDG_CONFIG_HOME/faderframe/preferences.json`.

use crate::state::BackendChoice;
use faderframe_engine::MetronomeMode;
use faderframe_session::{LoopRecordMode, RecordMode, RecordSettings};
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
    /// "takes" or "replace".
    pub record_mode: String,
    /// "takes", "last-pass" or "new-tracks".
    pub loop_record_mode: String,
    /// "off", "recording" or "always".
    pub metronome: String,
    pub preroll_bars: u32,
    /// Extra recording latency compensation in frames.
    pub record_latency_offset: i64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            sample_rate: None,
            buffer_size: None,
            snap: true,
            follow_playhead: true,
            record_mode: RecordMode::default().id().into(),
            loop_record_mode: LoopRecordMode::default().id().into(),
            metronome: MetronomeMode::Recording.id().into(),
            preroll_bars: 0,
            record_latency_offset: 0,
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

    /// Recording settings from these preferences.
    pub fn record_settings(&self) -> RecordSettings {
        let d = RecordSettings::default();
        RecordSettings {
            mode: RecordMode::from_id(&self.record_mode).unwrap_or(d.mode),
            loop_mode: LoopRecordMode::from_id(&self.loop_record_mode).unwrap_or(d.loop_mode),
            metronome: MetronomeMode::from_id(&self.metronome).unwrap_or(d.metronome),
            preroll_bars: self.preroll_bars.min(8),
            latency_offset: self.record_latency_offset,
            ..d
        }
    }

    pub fn set_record_settings(&mut self, r: &RecordSettings) {
        self.record_mode = r.mode.id().into();
        self.loop_record_mode = r.loop_mode.id().into();
        self.metronome = r.metronome.id().into();
        self.preroll_bars = r.preroll_bars;
        self.record_latency_offset = r.latency_offset;
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
