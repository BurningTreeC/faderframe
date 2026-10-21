//! User preferences persisted in `$XDG_CONFIG_HOME/faderframe/preferences.json`
//! (portable mode: the portable folder's `Settings`).

use crate::state::BackendChoice;
use faderframe_engine::MetronomeMode;
use faderframe_session::{AudioPreferences, LoopRecordMode, RecordMode, RecordSettings};
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
    /// The audio devices chosen for each backend ("pipewire", "jack",
    /// "system", "asio", "dummy"), by the backend's device ids.
    pub audio_devices: std::collections::BTreeMap<String, faderframe_session::DeviceChoice>,
    /// Device channels to open (`None`: as many as the devices have).
    pub audio_input_channels: Option<u16>,
    pub audio_output_channels: Option<u16>,
    /// PipeWire: hold the graph at the rate and buffer size while running.
    pub audio_force_format: bool,
    /// The edit toolbar is shown.
    pub show_edit_toolbar: bool,
    /// What a new start opens: "last", "new" or "demo".
    pub startup_project: String,
    /// Recently used projects, most recent first (absolute paths).
    pub recent_projects: Vec<String>,
    /// Media no saved state uses, when a project closes: "ask", "trash"
    /// or "keep".
    pub unused_media: String,
    /// The skin (`Theme::id`).
    pub theme: String,
    pub snap: bool,
    /// Drags snap to clip edges, markers, sections, the loop, the
    /// selection and the playhead (Snap to Objects).
    pub snap_objects: bool,
    /// Where ripples reach and what follows them (the switch itself is
    /// not kept: editing starts without rippling).
    pub ripple: faderframe_session::RippleSettings,
    /// How strongly the editors draw their time grid, 0…1 (Preferences →
    /// Editing → Grid lines; `GridLook`).
    pub grid_strength: f32,
    pub follow_playhead: bool,
    /// Which global lanes the arranger shows, across projects and restarts.
    pub global_lanes: faderframe_session::lanes::GlobalLanes,
    /// "takes" or "replace".
    pub record_mode: String,
    /// "takes", "last-pass" or "new-tracks".
    pub loop_record_mode: String,
    /// "off", "recording" or "always".
    pub metronome: String,
    /// The mode the metronome button switches on ("recording" or
    /// "always").
    pub metronome_on: String,
    /// The pre-roll in bars of older preferences (`preroll` replaces it).
    pub preroll_bars: u32,
    /// Input quantize (the transport's Q) and its grid (None: the edit
    /// grid).
    pub input_quantize: bool,
    pub input_grid: Option<faderframe_timeline::GridDivision>,
    /// The pre-roll (`Roll::id`); empty: `preroll_bars`.
    pub preroll: String,
    /// The pre-roll starts on a bar line.
    pub preroll_on_bar: bool,
    /// The pre-roll plays the click alone.
    pub count_in: bool,
    /// Played after a punch-out (`Roll::id`).
    pub postroll: String,
    /// How the metronome sounds (Preferences → Metronome).
    pub metronome_sound: faderframe_session::MetronomeSettings,
    /// The talkback microphone (the Cue Mixes window).
    pub talkback: faderframe_session::cues::TalkbackSettings,
    /// Stop puts the playhead at the start of the project.
    pub stop_to_start: bool,
    /// Stop puts the playhead back where playback started (Return to
    /// Start on Stop; off: it stays where it stopped).
    pub return_to_play_start: bool,
    /// Extra recording latency compensation in frames.
    pub record_latency_offset: i64,
    /// MIDI inputs (port keys) not to use.
    pub midi_disabled_inputs: Vec<String>,
    /// MIDI outputs not to use.
    pub midi_disabled_outputs: Vec<String>,
    /// MIDI outputs that get MIDI clock.
    pub midi_clock_outputs: Vec<String>,
    /// MIDI outputs that get MIDI time code, and its rate.
    pub midi_mtc_outputs: Vec<String>,
    pub mtc_out_rate: String,
    /// The timecode MTC output starts from at the project start.
    pub mtc_out_offset: String,
    /// Follow an external master by varispeed.
    pub sync_varispeed: bool,
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
    /// Render buses ahead too (in a shallow second stage: faders stay
    /// immediate).
    pub render_ahead_buses: bool,
    /// Draw dense views (analysers, meters, curves) on the GPU.
    pub gpu_painter: bool,
    /// The GPU painter was set in Preferences (until then it is on, the
    /// default, whatever an older file says).
    pub gpu_painter_chosen: bool,
    /// Listening on headphones: the room of the binaural render ("near",
    /// "mid", "far"); `None`: speakers.
    pub headphones: Option<String>,
    /// Whose ears on headphones: a built-in head's id ("ku100" when
    /// unset) or `sofa:<path>`.
    pub headphone_head: Option<String>,
    /// The headphone correction's file.
    pub headphone_correction: Option<String>,
    /// Bass management of the speakers (crossover, redirect, LFE
    /// low-pass; off by default).
    pub bass_management: faderframe_core::surround::BassManagement,
    /// Low-latency monitoring (tracks heard live skip plugin delay
    /// compensation).
    pub low_latency_monitoring: bool,
    /// Effects on a monitored path with at most this latency stay on (ms).
    pub low_latency_limit_ms: f32,
    /// Keyboard shortcuts changed in the shortcut editor (detailed action
    /// → accelerators; empty: none), over the defaults.
    pub shortcuts: std::collections::BTreeMap<String, Vec<String>>,
    /// The project template New Project starts from (its name).
    pub default_template: Option<String>,
    /// Video proxies' height (0: no proxies).
    pub video_proxy_height: u32,
    /// Where video indexes and proxies go (`None`: the cache folder).
    pub video_cache_dir: Option<String>,
    /// Video frames shown straight from the decoder (dmabufs, Linux).
    pub video_zero_copy: bool,
    /// The monitor Video Full Screen goes to (connector and model).
    pub video_fullscreen_monitor: Option<String>,
    /// The picture on a Blackmagic DeckLink output: device and mode.
    pub video_output: Option<(u32, String)>,
    /// How voice ports hear singing (Preferences → MIDI).
    pub voice: faderframe_session::voice::VoiceSettings,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            backend: "auto".into(),
            sample_rate: None,
            buffer_size: None,
            threads: None,
            audio_devices: Default::default(),
            audio_input_channels: None,
            audio_output_channels: None,
            audio_force_format: false,
            show_edit_toolbar: false,
            startup_project: crate::recent::StartupProject::default().id().into(),
            recent_projects: Vec::new(),
            theme: "studio".into(),
            snap: true,
            snap_objects: true,
            ripple: Default::default(),
            grid_strength: faderframe_ui_canvas::GridLook::DEFAULT,
            follow_playhead: true,
            global_lanes: Default::default(),
            record_mode: RecordMode::default().id().into(),
            loop_record_mode: LoopRecordMode::default().id().into(),
            metronome: MetronomeMode::Recording.id().into(),
            metronome_on: MetronomeMode::Always.id().into(),
            preroll_bars: 0,
            preroll: String::new(),
            input_quantize: false,
            input_grid: None,
            unused_media: "ask".into(),
            preroll_on_bar: true,
            count_in: false,
            postroll: String::new(),
            metronome_sound: Default::default(),
            talkback: Default::default(),
            stop_to_start: false,
            return_to_play_start: false,
            record_latency_offset: 0,
            midi_disabled_inputs: Vec::new(),
            midi_disabled_outputs: Vec::new(),
            midi_clock_outputs: Vec::new(),
            midi_mtc_outputs: Vec::new(),
            mtc_out_rate: "25".into(),
            mtc_out_offset: String::new(),
            sync_varispeed: true,
            control_surfaces: Vec::new(),
            sync_source: "internal".into(),
            sync_port: None,
            mtc_offset: "00:00:00:00".into(),
            sandbox_plugins: true,
            plugin_double_precision: false,
            render_ahead_ms: 200,
            render_ahead_buses: true,
            gpu_painter: true,
            gpu_painter_chosen: false,
            headphones: None,
            headphone_head: None,
            headphone_correction: None,
            bass_management: Default::default(),
            low_latency_monitoring: false,
            low_latency_limit_ms: faderframe_session::DEFAULT_LOW_LATENCY_LIMIT_MS,
            shortcuts: Default::default(),
            default_template: None,
            video_proxy_height: 540,
            video_cache_dir: None,
            video_zero_copy: true,
            video_fullscreen_monitor: None,
            video_output: None,
            voice: Default::default(),
        }
    }
}

impl Preferences {
    /// How video is cached, as stored.
    pub fn video_settings(&self) -> faderframe_session::video::VideoSettings {
        faderframe_session::video::VideoSettings {
            proxy_height: (self.video_proxy_height > 0).then_some(self.video_proxy_height),
            cache_dir: self.video_cache_dir.as_ref().map(std::path::PathBuf::from),
        }
    }

    /// External synchronisation as stored.
    pub fn sync_settings(&self) -> faderframe_session::SyncSettings {
        faderframe_session::SyncSettings {
            source: faderframe_session::SyncSource::from_id(&self.sync_source),
            port: self.sync_port.clone(),
            offset: faderframe_session::Timecode::parse(&self.mtc_offset).unwrap_or_default(),
            mtc_out_rate: faderframe_session::MtcRate::from_id(&self.mtc_out_rate),
            varispeed: self.sync_varispeed,
            mtc_out_offset: faderframe_session::Timecode::parse(&self.mtc_out_offset)
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    pub fn set_sync_settings(&mut self, s: &faderframe_session::SyncSettings) {
        self.sync_source = s.source.id().into();
        self.sync_port = s.port.clone();
        self.mtc_offset = s.offset.to_string();
        self.mtc_out_rate = s.mtc_out_rate.id().into();
        self.sync_varispeed = s.varispeed;
        self.mtc_out_offset = s.mtc_out_offset.to_string();
    }

    pub fn path() -> PathBuf {
        crate::paths::config_dir().join("preferences.json")
    }

    pub fn load() -> Self {
        let mut p: Self = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // Drawing on the GPU is the default; files from before it was
        // stored false for everyone.
        if !p.gpu_painter_chosen {
            p.gpu_painter = true;
        }
        p
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
            preroll: faderframe_session::Roll::from_id(&self.preroll).unwrap_or(
                faderframe_session::Roll::bars(f64::from(self.preroll_bars.min(8))),
            ),
            preroll_on_bar: self.preroll_on_bar,
            count_in: self.count_in,
            input_quantize: self.input_quantize,
            input_grid: self.input_grid,
            postroll: faderframe_session::Roll::from_id(&self.postroll).unwrap_or(d.postroll),
            latency_offset: self.record_latency_offset,
            ..d
        }
    }

    pub fn set_record_settings(&mut self, r: &RecordSettings) {
        self.record_mode = r.mode.id().into();
        self.loop_record_mode = r.loop_mode.id().into();
        self.metronome = r.metronome.id().into();
        self.preroll = r.preroll.id();
        self.preroll_on_bar = r.preroll_on_bar;
        self.count_in = r.count_in;
        self.input_quantize = r.input_quantize;
        self.input_grid = r.input_grid;
        self.postroll = r.postroll.id();
        self.record_latency_offset = r.latency_offset;
    }

    /// The audio device settings as stored, with the rate, buffer size and
    /// threads `options` start with (the command line wins over these).
    pub fn audio_preferences(&self, options: &crate::RunOptions) -> AudioPreferences {
        AudioPreferences {
            sample_rate: options.sample_rate,
            buffer_size: options.buffer_size,
            threads: options.threads,
            devices: self.audio_devices.clone(),
            input_channels: self.audio_input_channels,
            output_channels: self.audio_output_channels,
            force_format: self.audio_force_format,
        }
    }

    /// Store audio device settings (the backend's choice under its id).
    pub fn set_audio_preferences(&mut self, backend: BackendChoice, a: &AudioPreferences) {
        self.set_backend(backend);
        self.sample_rate = a.sample_rate;
        self.buffer_size = a.buffer_size;
        self.threads = a.threads;
        self.audio_devices = a.devices.clone();
        self.audio_devices.retain(|_, c| !c.is_empty());
        self.audio_input_channels = a.input_channels;
        self.audio_output_channels = a.output_channels;
        self.audio_force_format = a.force_format;
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

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_session::lanes::{GlobalLane, GlobalLanes};

    #[test]
    fn snapping_and_grid_preferences_round_trip_and_default_for_older_files() {
        let old: Preferences = serde_json::from_str(r#"{"snap":false}"#).unwrap();
        assert!(old.snap_objects, "Snap to Objects is on by default");
        assert_eq!(old.grid_strength, faderframe_ui_canvas::GridLook::DEFAULT);
        let mut prefs = old;
        prefs.snap_objects = false;
        prefs.grid_strength = 0.8;
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(restored, prefs);
    }

    #[test]
    fn audio_device_settings_round_trip_and_older_files_use_the_defaults() {
        let old: Preferences =
            serde_json::from_str(r#"{"backend":"pipewire","buffer_size":128}"#).unwrap();
        assert!(old.audio_devices.is_empty());
        assert_eq!(old.audio_input_channels, None);
        let options = crate::RunOptions {
            buffer_size: old.buffer_size,
            ..crate::RunOptions::default()
        };
        assert_eq!(
            old.audio_preferences(&options),
            AudioPreferences {
                buffer_size: Some(128),
                ..AudioPreferences::default()
            }
        );
        let mut prefs = old;
        let chosen = AudioPreferences {
            sample_rate: Some(96_000),
            buffer_size: Some(64),
            output_channels: Some(10),
            force_format: true,
            ..AudioPreferences::default()
        }
        .with_devices(
            "pipewire",
            &["alsa_output.usb-desk"],
            &["alsa_input.usb-desk"],
        )
        .with_devices("system", &[], &[]);
        prefs.set_audio_preferences(BackendChoice::PipeWire, &chosen);
        assert_eq!(prefs.audio_devices.len(), 1, "empty choices are not kept");
        let saved = serde_json::to_string(&prefs).unwrap();
        let restored: Preferences = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, prefs);
        assert_eq!(restored.backend(), BackendChoice::PipeWire);
        let options = crate::RunOptions {
            sample_rate: restored.sample_rate,
            buffer_size: restored.buffer_size,
            ..crate::RunOptions::default()
        };
        let back = restored.audio_preferences(&options);
        assert_eq!(
            back.devices_for("pipewire").outputs,
            vec!["alsa_output.usb-desk"]
        );
        assert_eq!(back.output_channels, Some(10));
        assert!(back.force_format);
    }

    #[test]
    fn lane_preferences_round_trip_and_accept_older_files() {
        let old: Preferences = serde_json::from_str(r#"{"snap":false}"#).unwrap();
        assert!(!old.snap);
        assert_eq!(old.global_lanes, GlobalLanes::default());
        let mut prefs = old;
        for lane in GlobalLane::ALL {
            prefs.global_lanes.set(lane, false);
            let saved = serde_json::to_string(&prefs).unwrap();
            let restored: Preferences = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored, prefs);
        }
        let partial: Preferences =
            serde_json::from_str(r#"{"global_lanes":{"tempo":false}}"#).unwrap();
        assert!(!partial.global_lanes.tempo);
        assert!(partial.global_lanes.markers);
    }
}
