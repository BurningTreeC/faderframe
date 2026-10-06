//! User preferences persisted in `$XDG_CONFIG_HOME/faderframe/preferences.json`
//! (portable mode: the portable folder's `Settings`).

use crate::state::BackendChoice;
use faderframe_engine::MetronomeMode;
use faderframe_session::{LoopRecordMode, RecordMode, RecordSettings};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// "auto", "pipewire", "jack", "system", "asio" or "dummy".
    pub backend: String,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// Processing threads (`None`: one per core).
    pub threads: Option<u16>,
    /// The edit toolbar is shown.
    pub show_edit_toolbar: bool,
    /// What a new start opens: "last", "new" or "demo".
    pub startup_project: String,
    /// Recently used projects, most recent first (absolute paths).
    pub recent_projects: Vec<String>,
    /// The skin (`Theme::id`).
    pub theme: String,
    pub snap: bool,
    pub follow_playhead: bool,
    /// "takes" or "replace".
    pub record_mode: String,
    /// "takes", "last-pass" or "new-tracks".
    pub loop_record_mode: String,
    /// "off", "recording" or "always".
    pub metronome: String,
    /// The mode the metronome button switches on ("recording" or
    /// "always").
    pub metronome_on: String,
    pub preroll_bars: u32,
    /// Extra recording latency compensation in frames.
    pub record_latency_offset: i64,
    /// MIDI inputs (port keys) not to use.
    pub midi_disabled_inputs: Vec<String>,
    /// MIDI outputs not to use.
    pub midi_disabled_outputs: Vec<String>,
    /// MIDI outputs that get MIDI clock.
    pub midi_clock_outputs: Vec<String>,
    /// Mackie Control, HUI and OSC surfaces.
    pub control_surfaces: Vec<faderframe_session::control::SurfaceSettings>,
    /// "internal", "midi-clock" or "mtc".
    pub sync_source: String,
    /// Input port key to follow (none: any).
    pub sync_port: Option<String>,
    /// MTC timecode of the project start.
    pub mtc_offset: String,
    /// Run each CLAP/VST3 plugin in a helper process of its own.
    pub sandbox_plugins: bool,
    /// Process plugins in 64-bit floating point where they can.
    pub plugin_double_precision: bool,
    /// Render tracks nobody plays live this many milliseconds ahead (0:
    /// off).
    pub render_ahead_ms: u32,
    /// Keyboard shortcuts changed in the shortcut editor (detailed action
    /// → accelerators; empty: none), over the defaults.
    pub shortcuts: std::collections::BTreeMap<String, Vec<String>>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            sample_rate: None,
            buffer_size: None,
            threads: None,
            show_edit_toolbar: false,
            startup_project: crate::recent::StartupProject::default().id().into(),
            recent_projects: Vec::new(),
            theme: "studio".into(),
            snap: true,
            follow_playhead: true,
            record_mode: RecordMode::default().id().into(),
            loop_record_mode: LoopRecordMode::default().id().into(),
            metronome: MetronomeMode::Recording.id().into(),
            metronome_on: MetronomeMode::Always.id().into(),
            preroll_bars: 0,
            record_latency_offset: 0,
            midi_disabled_inputs: Vec::new(),
            midi_disabled_outputs: Vec::new(),
            midi_clock_outputs: Vec::new(),
            control_surfaces: Vec::new(),
            sync_source: "internal".into(),
            sync_port: None,
            mtc_offset: "00:00:00:00".into(),
            sandbox_plugins: true,
            plugin_double_precision: false,
            render_ahead_ms: 200,
            shortcuts: Default::default(),
        }
    }
}

impl Preferences {
    /// External synchronisation as stored.
    pub fn sync_settings(&self) -> faderframe_session::SyncSettings {
        faderframe_session::SyncSettings {
            source: faderframe_session::SyncSource::from_id(&self.sync_source),
            port: self.sync_port.clone(),
            offset: faderframe_session::Timecode::parse(&self.mtc_offset).unwrap_or_default(),
            ..Default::default()
        }
    }

    pub fn set_sync_settings(&mut self, s: &faderframe_session::SyncSettings) {
        self.sync_source = s.source.id().into();
        self.sync_port = s.port.clone();
        self.mtc_offset = s.offset.to_string();
    }

    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("preferences.json")
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
            "pipewire" => BackendChoice::PipeWire,
            "system" => BackendChoice::System,
            "asio" => BackendChoice::Asio,
            "dummy" => BackendChoice::Dummy,
            _ => BackendChoice::Auto,
        }
    }

    pub fn set_backend(&mut self, b: BackendChoice) {
        self.backend = match b {
            BackendChoice::Auto => "auto",
            BackendChoice::Jack => "jack",
            BackendChoice::PipeWire => "pipewire",
            BackendChoice::System => "system",
            BackendChoice::Asio => "asio",
            BackendChoice::Dummy => "dummy",
        }
        .into();
    }
}
