//! The control-world session.
//!
//! A [`Session`] owns everything the GUI works with — the project, undo
//! history, workspace layout, engine controller, audio stream, decoded
//! sources, waveform peaks, meter ballistics and editor state (selection,
//! the clip open in the piano roll) — and is the single place where editors'
//! [`Action`]s are applied. After every edit it re-synchronises exactly as
//! much engine state as the edit's [`Impact`] requires.
//!
//! It is GTK-free: the same session drives the GTK shell, headless tools
//! and tests. All views read it immutably while painting and talk back only
//! through actions, so a view can be docked, tabbed or moved to another
//! window without copying any state.

#![forbid(unsafe_code)]

pub mod automation;
pub mod media;
mod meters;
pub mod record;
pub mod render;
mod selection;

pub use meters::{METER_FLOOR_DB, MeterChannel, MeterDisplay};
pub use selection::{SelectMode, Selection};

pub use automation::{AutomationParam, ParamKind};
use faderframe_audio::{
    AudioBackend, AudioError, AudioStream, StreamConfig, StreamInfo, StreamStatus,
};
use faderframe_audio_files::PeakCache;
pub use faderframe_automation::{AutomationMode, AutomationTarget};
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId, builtin};
use faderframe_engine::{
    EngineConfig, EngineController, EngineError, EngineProcessor, Source, SourceMap, StreamPlan,
};
use faderframe_project::file::{self, FileError};
use faderframe_project::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, Command, EditError, History, Impact,
    MidiClip, MidiNote, MusicalRange, PluginRef, PluginSlot, Project, SendTap, SourceSpec, Track,
    TrackColor, TrackKind,
};
use faderframe_realtime::{Epoch, MetricsSnapshot};
use faderframe_timeline::{GridDivision, MusicalTime};
use faderframe_transport::{TransportCommand, TransportSnapshot};
use faderframe_workspace::{
    DockAreaId, LayoutError, ViewId, WindowGeometry, WindowId, WorkspaceSet,
};
pub use media::{ImportJob, ImportTarget};
pub use record::{LiveTake, LoopRecordMode, RecordMode, RecordSettings, RecordedTake};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Edit(#[from] EditError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SessionError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    pub at: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransportAction {
    Play,
    /// Stop; when already stopped, return to the start.
    Stop,
    TogglePlay,
    Locate(MusicalTime),
    ReturnToStart,
    ToggleLoop,
    SetLoop(Option<MusicalRange>),
    /// Record mode on/off: capture armed tracks while playing (punching in
    /// on the fly when already playing).
    ToggleRecord,
    /// Restrict recording to the punch range (defaults to the loop range).
    TogglePunch,
    SetPunch(Option<MusicalRange>),
    /// Move the playhead by whole bars.
    NudgeBars(i32),
}

/// Layout changes requested by views or menus.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceAction {
    ShowView(ViewId),
    Detach(ViewId),
    Attach(ViewId),
    CloseWindow(WindowId),
    ToggleArea(DockAreaId),
    Switch(usize),
    ResetActive,
}

/// Everything an editor view can ask the session to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// An undoable project edit.
    Edit(Command),
    /// Start grouping subsequent edits into one undo step (e.g. a drag).
    BeginGesture(String),
    EndGesture,
    Undo,
    Redo,
    Transport(TransportAction),
    Workspace(WorkspaceAction),
    SelectTracks {
        tracks: Vec<TrackId>,
        mode: SelectMode,
    },
    SelectClips {
        clips: Vec<ClipId>,
        mode: SelectMode,
    },
    SelectNotes {
        notes: Vec<NoteId>,
        mode: SelectMode,
    },
    ClearSelection,
    /// Open a MIDI clip in the piano roll (and show the piano roll).
    OpenClipEditor(ClipId),
    ResetClipIndicators,
    AddTrack(TrackKind),
    /// Add a track with an explicit format (e.g. a stereo audio track).
    AddTrackWithLayout(TrackKind, faderframe_core::ChannelLayout),
    RemoveSelectedTracks,
    /// Toggle record arm on the selected audio/MIDI tracks.
    ToggleArmSelected,
    DeleteSelection,
    SplitSelectedAtPlayhead,
    /// Insert a plugin into a track's insert chain (the session allocates ids).
    InsertPlugin {
        track: TrackId,
        index: usize,
        plugin: PluginRef,
    },
    AddSend {
        track: TrackId,
        target: TrackId,
        level_db: f32,
        tap: SendTap,
    },
    /// Create an empty MIDI clip and open it in the piano roll.
    CreateMidiClip {
        track: TrackId,
        start: MusicalTime,
        length: MusicalTime,
    },
    /// Add a note to a MIDI clip (start relative to the clip) and select it.
    AddNote {
        clip: ClipId,
        start: MusicalTime,
        length: MusicalTime,
        key: u8,
        velocity: u8,
    },
    /// Import audio files (decoded in the background; one undo step).
    ImportFiles {
        files: Vec<PathBuf>,
        /// Put the first file on this audio track (others get new tracks).
        track: Option<TrackId>,
        at: MusicalTime,
    },
    /// Change how recording behaves (modes, metronome, pre-roll, latency).
    SetRecordSettings(RecordSettings),
    /// Replace a take folder by plain clips of its comp.
    FlattenTakes(ClipId),
    /// Show or hide a take folder's take lanes (editor state, not undoable).
    ToggleTakeLanes(ClipId),
    /// Show the automation lane of a parameter (created if needed).
    ShowAutomation {
        track: TrackId,
        target: AutomationTarget,
    },
    HideAutomationLane(faderframe_core::AutomationLaneId),
    /// The track header's automation button: show/hide its lanes.
    ToggleTrackAutomation(TrackId),
    SetAutomationMode {
        track: TrackId,
        lane: faderframe_core::AutomationLaneId,
        mode: AutomationMode,
    },
    /// Save a track's settings into the track preset library.
    SaveTrackPreset {
        track: TrackId,
    },
    /// Add a new track from a track preset file.
    AddTrackFromPreset {
        path: PathBuf,
    },
    /// Give an existing track a preset's settings (one undo step).
    ApplyTrackPreset {
        track: TrackId,
        path: PathBuf,
    },
    /// Arranger track height of one track, or of all (`None`); not undoable,
    /// saved with the layout.
    SetTrackHeight {
        track: Option<TrackId>,
        height: f32,
    },
    SetGrid(GridDivision),
    ToggleSnap,
    ToggleFollowPlayhead,
}

/// Editing preferences shared by the arranger and the piano roll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorSettings {
    pub grid: GridDivision,
    pub snap: bool,
    pub follow_playhead: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            grid: GridDivision::Beat,
            snap: true,
            follow_playhead: true,
        }
    }
}

impl EditorSettings {
    /// Snap `pos` to the grid if snapping is on.
    pub fn snap(
        &self,
        pos: MusicalTime,
        meter: &faderframe_timeline::TimeSignatureMap,
    ) -> MusicalTime {
        if self.snap {
            faderframe_timeline::snap_nearest(pos, self.grid, meter)
        } else {
            pos
        }
    }

    /// Grid step at `pos`.
    pub fn step(
        &self,
        pos: MusicalTime,
        meter: &faderframe_timeline::TimeSignatureMap,
    ) -> MusicalTime {
        self.grid.step(meter.signature_at(pos))
    }
}

/// Folder next to the project file that imported and recorded media go to.
pub const MEDIA_FOLDER: &str = "Audio";

/// Audio startup preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioPreferences {
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
}

/// A recording in progress (or whose writer is finishing).
struct ActiveRecording {
    writer: record::RecordWriter,
    from: i64,
    to: i64,
    tracks: Vec<TrackId>,
    /// Input + output latency compensation in frames.
    latency: i64,
    /// The transport has reported recording at least once.
    seen: bool,
}

/// What the arranger draws while recording.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingView {
    pub tracks: Vec<TrackId>,
    /// Input + output latency compensation (frames) applied to the takes.
    pub latency: i64,
    /// Record window in engine samples (`to` is `i64::MAX` without punch).
    pub from: i64,
    pub to: i64,
}

struct ActiveAudio {
    stream: Box<dyn AudioStream>,
    backend: &'static str,
}

/// See the crate documentation.
pub struct Session {
    project: Project,
    history: History,
    workspace: WorkspaceSet,
    engine: EngineController,
    /// Engine processor waiting to be handed to an audio backend.
    pending: Option<EngineProcessor>,
    engine_config: EngineConfig,
    audio: Option<ActiveAudio>,
    sources: SourceMap,
    peaks: HashMap<AudioSourceId, Arc<PeakCache>>,
    pub selection: Selection,
    pub editor: EditorSettings,
    editor_clip: Option<ClipId>,
    meters: HashMap<TrackId, MeterDisplay>,
    transport: TransportSnapshot,
    metrics: MetricsSnapshot,
    last_metrics: Instant,
    path: Option<PathBuf>,
    saved_revision: u64,
    revision: u64,
    layout_revision: u64,
    notices: VecDeque<Notice>,
    // Media. Declared after `audio` so the stream (the page reader) is
    // dropped before the disk loader frees pages.
    epoch: Arc<Epoch>,
    loader: media::DiskLoader,
    /// Where imported media is written.
    media_dir: PathBuf,
    /// `media_dir` is a scratch folder of a never-saved project.
    unsaved_media: bool,
    /// Media files moved on save (old → new absolute path).
    media_moves: HashMap<PathBuf, PathBuf>,
    missing: HashSet<AudioSourceId>,
    imports: Vec<ImportJob>,
    peak_jobs: Vec<media::PeakJob>,
    pub record: RecordSettings,
    recording: Option<ActiveRecording>,
    finishing: Vec<ActiveRecording>,
    /// Take folders whose take lanes are shown.
    open_takes: HashSet<ClipId>,
    preset_dir: PathBuf,
    presets: Vec<PresetEntry>,
    automation_writer: automation::AutomationWriter,
}

/// A track preset in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetEntry {
    pub name: String,
    pub path: PathBuf,
}

impl Session {
    /// A session for `project` with a not-yet-started engine.
    pub fn new(
        project: Project,
        workspace: Option<WorkspaceSet>,
        config: EngineConfig,
    ) -> Result<Self> {
        let epoch = Epoch::new();
        let (engine, processor) = faderframe_engine::create_with_epoch(config, Arc::clone(&epoch));
        let loader = media::DiskLoader::start(
            Arc::new(StreamPlan::default()),
            engine.shared(),
            config.sample_rate,
        );
        let mut s = Self {
            project,
            history: History::default(),
            workspace: workspace.unwrap_or_default(),
            engine,
            pending: Some(processor),
            engine_config: config,
            audio: None,
            sources: SourceMap::new(),
            peaks: HashMap::new(),
            selection: Selection::default(),
            editor: EditorSettings::default(),
            editor_clip: None,
            meters: HashMap::new(),
            transport: TransportSnapshot::default(),
            metrics: MetricsSnapshot::default(),
            last_metrics: Instant::now(),
            path: None,
            saved_revision: 0,
            revision: 0,
            layout_revision: 0,
            notices: VecDeque::new(),
            epoch,
            loader,
            media_dir: media::new_unsaved_media_dir(),
            unsaved_media: true,
            media_moves: HashMap::new(),
            missing: HashSet::new(),
            imports: Vec::new(),
            peak_jobs: Vec::new(),
            record: RecordSettings::default(),
            recording: None,
            finishing: Vec::new(),
            open_takes: HashSet::new(),
            preset_dir: media::data_dir().join("track-presets"),
            presets: Vec::new(),
            automation_writer: Default::default(),
        };
        s.rescan_track_presets();
        s.render_sources();
        s.engine.sync(&s.project, &s.sources, Impact::Graph)?;
        s.update_loader();
        s.editor_clip = s.first_midi_clip();
        Ok(s)
    }

    /// The bundled demo session.
    pub fn demo(config: EngineConfig) -> Result<Self> {
        Self::new(
            faderframe_project::demo::demo_project(config.sample_rate),
            None,
            config,
        )
    }

    // --- read access --------------------------------------------------------

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn workspace(&self) -> &WorkspaceSet {
        &self.workspace
    }

    /// Direct layout access for the shell (divider ratios, tab switches,
    /// window sizes — not undoable, no engine impact).
    pub fn workspace_mut(&mut self) -> &mut WorkspaceSet {
        &mut self.workspace
    }

    pub fn engine(&self) -> &EngineController {
        &self.engine
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    /// Engine frames per project frame (clip offsets are project frames).
    pub fn frame_ratio(&self) -> f64 {
        self.engine.sample_rate() as f64 / self.project.sample_rate.max(1) as f64
    }

    pub fn peaks(&self, source: AudioSourceId) -> Option<&Arc<PeakCache>> {
        self.peaks.get(&source)
    }

    /// Sample rate of a source's frames (and its peak cache): the file's
    /// own rate for streamed media, the engine rate for generated material.
    pub fn peak_rate(&self, source: AudioSourceId) -> f64 {
        match self.sources.get(&source) {
            Some(Source::Stream(s)) => s.sample_rate() as f64,
            _ => self.engine.sample_rate() as f64,
        }
    }

    /// The source's media file could not be opened (clips play silence).
    pub fn is_source_missing(&self, source: AudioSourceId) -> bool {
        self.missing.contains(&source)
    }

    pub fn missing_sources(&self) -> usize {
        self.missing.len()
    }

    /// Imports still running.
    pub fn imports(&self) -> &[ImportJob] {
        &self.imports
    }

    pub fn cancel_imports(&self) {
        for j in &self.imports {
            j.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Folder imported media is written to.
    pub fn media_dir(&self) -> &Path {
        &self.media_dir
    }

    /// Disk streaming state: resident page memory, reads that missed.
    pub fn streaming_stats(&self) -> (usize, u64) {
        let misses = self
            .sources
            .values()
            .filter_map(|s| match s {
                Source::Stream(s) => Some(s.misses()),
                Source::Memory(_) => None,
            })
            .sum();
        (self.loader.resident_bytes(), misses)
    }

    pub fn meter(&self, track: TrackId) -> MeterDisplay {
        self.meters.get(&track).copied().unwrap_or_default()
    }

    pub fn transport(&self) -> TransportSnapshot {
        self.transport
    }

    pub fn playhead(&self) -> MusicalTime {
        self.engine
            .samples_to_musical(&self.project, self.transport.position)
    }

    pub fn metrics(&self) -> &MetricsSnapshot {
        &self.metrics
    }

    pub fn editor_clip(&self) -> Option<ClipId> {
        self.editor_clip.filter(|c| self.project.clip(*c).is_some())
    }

    /// Bumped on every model change (views may cache against it).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bumped when the dock layout must be rebuilt by the shell.
    pub fn layout_revision(&self) -> u64 {
        self.layout_revision
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.history.revision() != self.saved_revision
    }

    pub fn title(&self) -> String {
        format!(
            "{}{} — FaderFrame",
            if self.is_dirty() { "• " } else { "" },
            self.project.name
        )
    }

    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.audio.as_ref().map(|a| a.stream.info())
    }

    pub fn stream_status(&self) -> Option<StreamStatus> {
        self.audio.as_ref().map(|a| a.stream.status())
    }

    pub fn backend_name(&self) -> Option<&'static str> {
        self.audio.as_ref().map(|a| a.backend)
    }

    pub fn notices(&self) -> impl Iterator<Item = &Notice> {
        self.notices.iter()
    }

    pub fn latest_notice(&self) -> Option<&Notice> {
        self.notices.back()
    }

    pub fn notify(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        match level {
            NoticeLevel::Error => tracing::error!("{text}"),
            NoticeLevel::Warning => tracing::warn!("{text}"),
            NoticeLevel::Info => tracing::info!("{text}"),
        }
        self.notices.push_back(Notice {
            level,
            text,
            at: Instant::now(),
        });
        while self.notices.len() > 50 {
            self.notices.pop_front();
        }
    }

    /// Anything animating (playback, meters falling)?
    pub fn is_animating(&self) -> bool {
        self.transport.playing
            || self.meters.values().any(MeterDisplay::is_active)
            || !self.imports.is_empty()
            || !self.peak_jobs.is_empty()
            || self.recording.is_some()
            || !self.finishing.is_empty()
    }

    /// The recording in progress, if any (record mode is on).
    pub fn recording_view(&self) -> Option<RecordingView> {
        self.recording.as_ref().map(|r| RecordingView {
            tracks: r.tracks.clone(),
            latency: r.latency,
            from: r.from,
            to: r.to,
        })
    }

    /// Arranger height of a track (`None`: the theme default).
    pub fn track_height(&self, track: TrackId) -> Option<f32> {
        self.workspace.track_height(track)
    }

    /// Are the take lanes of this take folder shown?
    pub fn takes_open(&self, clip: ClipId) -> bool {
        self.open_takes.contains(&clip)
    }

    /// Takes still being finalised after recording stopped.
    pub fn is_finishing_recording(&self) -> bool {
        !self.finishing.is_empty()
    }

    // --- audio ---------------------------------------------------------------

    fn recreate_engine(&mut self) -> Result<()> {
        let position = self.transport.position;
        self.engine_config.sample_rate = self.engine.sample_rate();
        let (engine, processor) =
            faderframe_engine::create_with_epoch(self.engine_config, Arc::clone(&self.epoch));
        self.engine = engine;
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
        self.engine.transport(TransportCommand::Locate(position))?;
        self.pending = Some(processor);
        self.update_loader();
        Ok(())
    }

    fn set_engine_rate(&mut self, rate: u32) -> Result<()> {
        if rate == self.engine.sample_rate() {
            return Ok(());
        }
        self.engine.set_sample_rate(rate);
        self.engine_config.sample_rate = rate;
        self.render_sources();
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
        self.update_loader();
        self.notify(NoticeLevel::Info, format!("engine now runs at {rate} Hz"));
        Ok(())
    }

    /// Open an audio stream on the first backend that works.
    pub fn start_audio(
        &mut self,
        backends: Vec<Box<dyn AudioBackend>>,
        prefs: &AudioPreferences,
    ) -> Result<StreamInfo> {
        self.stop_audio();
        let mut last_err: Option<SessionError> = None;
        for mut backend in backends {
            if !backend.is_available() {
                last_err = Some(SessionError::Audio(AudioError::BackendUnavailable(
                    format!("{} is not available", backend.display_name()),
                )));
                continue;
            }
            if self.pending.is_none() {
                self.recreate_engine()?;
            }
            let Some(processor) = self.pending.take() else {
                continue;
            };
            let config = StreamConfig {
                sample_rate: prefs.sample_rate,
                buffer_size: prefs.buffer_size,
                ..StreamConfig::default()
            };
            match backend.open_stream(config, Box::new(processor)) {
                Ok(stream) => {
                    let info = stream.info();
                    self.audio = Some(ActiveAudio {
                        stream,
                        backend: backend.id(),
                    });
                    self.set_engine_rate(info.sample_rate)?;
                    self.notify(NoticeLevel::Info, format!("audio: {info}"));
                    return Ok(info);
                }
                Err(e) => {
                    self.notify(
                        NoticeLevel::Warning,
                        format!("{}: {e}", backend.display_name()),
                    );
                    last_err = Some(e.into());
                }
            }
        }
        Err(last_err.unwrap_or_else(|| SessionError::Other("no audio backend available".into())))
    }

    /// While no stream runs, the processor is owned here; drain its queues
    /// with a zero-frame call so control-side commands never pile up and
    /// state (transport position, loop range) stays current.
    fn pump_idle(&mut self) {
        if let Some(p) = &mut self.pending {
            let mut none = faderframe_audio::OwnedBuffers::new(0, 0, 0);
            none.set_frames(0);
            p.process_device(&mut none);
            self.engine.collect_garbage();
        }
    }

    /// Release all voices and tails (panic button).
    pub fn engine_reset_processors(&mut self) -> Result<()> {
        self.engine.reset_processors()?;
        self.pump_idle();
        Ok(())
    }

    /// Start an offline render of the current project state.
    pub fn render(
        &self,
        settings: render::RenderSettings,
    ) -> std::result::Result<render::RenderJob, render::RenderError> {
        let mut project = self.project.clone();
        let dir = self.project_dir();
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path, .. } = &mut s.spec {
                *path = self.resolve_media(path, dir.as_deref());
            }
        }
        render::start(project, settings)
    }

    /// Close the stream (the engine processor goes with it).
    pub fn stop_audio(&mut self) {
        self.audio = None;
    }

    pub fn request_buffer_size(&mut self, frames: u32) -> Result<()> {
        match &mut self.audio {
            Some(a) => Ok(a.stream.request_buffer_size(frames)?),
            None => Err(SessionError::Other("no audio stream is running".into())),
        }
    }

    // --- periodic --------------------------------------------------------------

    /// Poll the engine (call once per UI frame). `dt` is in seconds.
    pub fn tick(&mut self, dt: f32) {
        self.poll_jobs();
        self.pump_idle();
        self.poll_recording();
        self.engine.collect_garbage();
        let was_playing = self.transport.playing;
        self.transport = self.engine.transport_snapshot();
        if was_playing && !self.transport.playing {
            // Stopped (by the user, the end of a bounce, a dropped stream):
            // Latch/Write automation ends here.
            self.automation_play_stopped();
        }
        for t in &self.project.tracks {
            if let Some(m) = self.engine.take_meter(t.id) {
                self.meters.entry(t.id).or_default().update(&m, dt);
            }
        }
        if self.last_metrics.elapsed().as_millis() >= 250 {
            self.metrics = self.engine.metrics();
            self.last_metrics = Instant::now();
        }
        let status = self.stream_status();
        if let Some(status) = status {
            if status.shut_down {
                self.audio = None;
                self.notify(NoticeLevel::Error, "the audio server shut down the stream");
                if let Err(e) = self.recreate_engine() {
                    self.notify(NoticeLevel::Error, e.to_string());
                }
            } else if status.sample_rate != 0
                && status.sample_rate != self.engine.sample_rate()
                && let Err(e) = self.set_engine_rate(status.sample_rate)
            {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
    }

    // --- project lifecycle ---------------------------------------------------

    /// Re-render generated sources at the engine rate and (re)open media.
    /// Already opened streams are kept (they play at any engine rate).
    fn render_sources(&mut self) {
        let mut sources =
            faderframe_engine::render_generated_sources(&self.project, self.engine.sample_rate());
        for (id, data) in &sources {
            if let Source::Memory(d) = data {
                self.peaks.insert(*id, Arc::new(PeakCache::build(d)));
            }
        }
        for (id, s) in self.sources.drain() {
            if matches!(s, Source::Stream(_)) {
                sources.insert(id, s);
            }
        }
        self.sources = sources;
        self.open_media();
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    fn resolve_media(&self, stored: &Path, dir: Option<&Path>) -> PathBuf {
        let p = media::resolve(stored, dir);
        self.media_moves.get(&p).cloned().unwrap_or(p)
    }

    /// Bring opened media in line with the project's sources: forget
    /// removed ones, open new ones, find peaks.
    fn open_media(&mut self) {
        let live = &self.project.sources;
        self.sources.retain(|id, _| live.contains_key(id));
        self.peaks.retain(|id, _| live.contains_key(id));
        self.missing.retain(|id| live.contains_key(id));
        // Re-point sources at media moved by a save (e.g. after redo of an
        // import made before the first save).
        let dir = self.project_dir();
        if !self.media_moves.is_empty() {
            let moves: Vec<(AudioSourceId, PathBuf)> = live
                .values()
                .filter_map(|s| match &s.spec {
                    SourceSpec::File { path, .. } => {
                        self.media_moves.get(path).map(|new| (s.id, new.clone()))
                    }
                    SourceSpec::Generated { .. } => None,
                })
                .collect();
            for (id, path) in moves {
                if let Some(SourceSpec::File { path: p, .. }) =
                    self.project.sources.get_mut(&id).map(|s| &mut s.spec)
                {
                    *p = path;
                }
            }
        }
        let failed = media::open_file_sources(&self.project, dir.as_deref(), &mut self.sources);
        for (id, path, e) in failed {
            if self.missing.insert(id) {
                self.notify(
                    NoticeLevel::Warning,
                    format!("audio file offline: {} ({e})", path.display()),
                );
            }
        }
        let sources = &self.sources;
        self.missing.retain(|id| !sources.contains_key(id));
        let mut wanted = Vec::new();
        for (id, s) in &self.sources {
            if let Source::Stream(st) = s
                && !self.peaks.contains_key(id)
                && !self.peak_jobs.iter().any(|j| j.source == *id)
            {
                match media::load_peaks(st.path()) {
                    Some(p) => {
                        self.peaks.insert(*id, Arc::new(p));
                    }
                    None => wanted.push((*id, st.path().to_path_buf())),
                }
            }
        }
        for (id, path) in wanted {
            self.peak_jobs.push(media::PeakJob::spawn(id, path));
        }
    }

    /// Tell the disk loader about the current plan, engine and loop.
    fn update_loader(&self) {
        let loop_range = self
            .project
            .loop_range
            .filter(|_| self.project.loop_enabled)
            .map(|r| {
                (
                    self.engine.musical_to_samples(&self.project, r.start),
                    self.engine.musical_to_samples(&self.project, r.end),
                )
            });
        self.loader.update(
            self.engine.stream_plan(),
            self.engine.shared(),
            loop_range,
            self.engine.sample_rate(),
        );
    }

    fn poll_jobs(&mut self) {
        let mut i = 0;
        while i < self.peak_jobs.len() {
            if self.peak_jobs[i].is_finished() {
                let job = self.peak_jobs.swap_remove(i);
                let id = job.source;
                if let Some(p) = job.join() {
                    self.peaks.insert(id, Arc::new(p));
                    self.revision += 1;
                }
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.imports.len() {
            if self.imports[i].is_finished() {
                let job = self.imports.remove(i);
                if let Err(e) = self.finish_import(job) {
                    self.notify(NoticeLevel::Error, format!("import failed: {e}"));
                }
            } else {
                i += 1;
            }
        }
    }

    /// Start importing `files` in the background.
    pub fn import_audio(&mut self, files: Vec<PathBuf>, target: ImportTarget) {
        let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
        if files.is_empty() {
            self.notify(NoticeLevel::Warning, "nothing to import");
            return;
        }
        let n = files.len();
        let job = ImportJob::spawn(
            files,
            self.media_dir.clone(),
            self.project.sample_rate,
            target,
        );
        self.imports.push(job);
        self.notify(
            NoticeLevel::Info,
            format!("importing {n} file{}…", if n == 1 { "" } else { "s" }),
        );
    }

    /// Block until all imports are done and applied (tests, scripting).
    pub fn wait_for_imports(&mut self) {
        while !self.imports.is_empty() {
            let job = self.imports.remove(0);
            if let Err(e) = self.finish_import(job) {
                self.notify(NoticeLevel::Error, format!("import failed: {e}"));
            }
        }
    }

    fn finish_import(&mut self, job: ImportJob) -> Result<()> {
        let target = job.target.clone();
        let results = job.join();
        let mut commands = Vec::new();
        let mut clips = Vec::new();
        let mut failures = Vec::new();
        let mut reuse = target.track.filter(|t| {
            self.project
                .track(*t)
                .is_some_and(|t| t.kind == TrackKind::Audio)
        });
        let p = &mut self.project;
        let mut index = reuse
            .and_then(|t| p.track_index(t))
            .or_else(|| p.tracks.iter().rposition(|t| t.kind.has_clips()))
            .map_or(0, |i| i + 1);
        let mut colour = p.tracks.len();
        for r in results {
            let audio = match r {
                Ok(a) => a,
                Err((path, e)) => {
                    failures.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let source = AudioSource {
                id: p.ids.allocate(),
                name: audio.name.clone(),
                spec: SourceSpec::File {
                    path: audio.path.clone(),
                    channels: audio.channels as u16,
                    frames: audio.frames as i64,
                    sample_rate: audio.sample_rate,
                },
            };
            let length = source.frames(p.sample_rate);
            let track = match reuse.take() {
                Some(t) => t,
                None => {
                    let id: TrackId = p.ids.allocate();
                    let layout = if audio.channels == 1 {
                        faderframe_core::ChannelLayout::Mono
                    } else {
                        faderframe_core::ChannelLayout::Stereo
                    };
                    let t = Track::new(
                        id,
                        TrackKind::Audio,
                        audio.name.clone(),
                        TrackColor::palette(colour),
                    )
                    .with_layout(layout);
                    colour += 1;
                    commands.push(Command::AddTrack {
                        track: Box::new(t),
                        index,
                    });
                    index += 1;
                    id
                }
            };
            let clip = Clip {
                id: p.ids.allocate(),
                track,
                name: audio.name.clone(),
                color: None,
                start: target.at,
                muted: false,
                content: ClipContent::Audio(AudioClip {
                    source: source.id,
                    source_offset: 0,
                    length,
                    gain_db: 0.0,
                    fades: Default::default(),
                    stretch: Default::default(),
                    reversed: false,
                }),
            };
            clips.push(clip.id);
            match media::open_stream(&audio.path) {
                Ok(s) => {
                    self.sources.insert(source.id, Source::Stream(s));
                }
                Err(e) => failures.push(format!("{}: {e}", audio.path.display())),
            }
            self.peaks.insert(source.id, Arc::new(audio.peaks));
            commands.push(Command::AddSource {
                source: Box::new(source),
            });
            commands.push(Command::AddClip {
                clip: Box::new(clip),
            });
        }
        let imported = clips.len();
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: if imported == 1 {
                    "Import Audio File".into()
                } else {
                    "Import Audio Files".into()
                },
                commands,
            })?;
            self.selection.select_clips(&clips, SelectMode::Replace);
        }
        for f in &failures {
            self.notify(NoticeLevel::Error, format!("import: {f}"));
        }
        if imported > 0 {
            self.notify(
                NoticeLevel::Info,
                format!(
                    "imported {imported} file{}",
                    if imported == 1 { "" } else { "s" }
                ),
            );
        }
        Ok(())
    }

    fn first_midi_clip(&self) -> Option<ClipId> {
        self.project
            .clips
            .values()
            .find(|c| c.as_midi().is_some())
            .map(|c| c.id)
    }

    fn replace_project(&mut self, project: Project, workspace: Option<WorkspaceSet>) -> Result<()> {
        self.project = project;
        if let Some(ws) = workspace {
            self.workspace = ws;
        }
        self.history.clear();
        self.saved_revision = self.history.revision();
        self.selection.clear();
        self.meters.clear();
        self.sources.clear();
        self.peaks.clear();
        self.missing.clear();
        self.media_moves.clear();
        self.render_sources();
        self.editor_clip = self.first_midi_clip();
        self.engine.transport(TransportCommand::Stop)?;
        self.engine.transport(TransportCommand::Locate(0))?;
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
        self.update_loader();
        self.revision += 1;
        self.layout_revision += 1;
        Ok(())
    }

    pub fn new_project(&mut self, demo: bool) -> Result<()> {
        let rate = self.engine.sample_rate();
        let project = if demo {
            faderframe_project::demo::demo_project(rate)
        } else {
            let mut p = Project::new("Untitled", rate);
            let id: TrackId = p.ids.allocate();
            let mut t = Track::new(id, TrackKind::Audio, "Audio 1", TrackColor::palette(0));
            t.input = faderframe_project::InputRouting::Hardware { first_channel: 0 };
            p.tracks.insert(0, t);
            p
        };
        self.discard_unsaved_media();
        self.path = None;
        self.media_dir = media::new_unsaved_media_dir();
        self.unsaved_media = true;
        self.replace_project(project, None)
    }

    /// Delete the scratch media of a never-saved project.
    fn discard_unsaved_media(&mut self) {
        if self.unsaved_media
            && self.media_dir.starts_with(media::data_dir())
            && self.media_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&self.media_dir)
        {
            tracing::warn!("could not remove {}: {e}", self.media_dir.display());
        }
    }

    pub fn open(&mut self, path: &Path) -> Result<Vec<String>> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let loaded = file::load(&path)?;
        let notes = loaded.notes.clone();
        let mut project = loaded.project;
        let dir = path.parent().map(Path::to_path_buf);
        // In memory, media paths are absolute (stable across save-as and in
        // undo history); they are stored relative to the project file.
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::resolve(p, dir.as_deref());
            }
        }
        self.discard_unsaved_media();
        self.path = Some(path.clone());
        self.media_dir = dir.unwrap_or_default().join(MEDIA_FOLDER);
        self.unsaved_media = false;
        self.replace_project(project, loaded.workspace)?;
        for n in &notes {
            self.notify(NoticeLevel::Warning, n.clone());
        }
        self.notify(NoticeLevel::Info, format!("opened {}", path.display()));
        Ok(notes)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let mut path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if path.extension().is_none() {
            path.set_extension(file::FILE_EXTENSION);
        }
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let project_media = dir.join(MEDIA_FOLDER);
        if self.unsaved_media {
            self.consolidate_media(&project_media);
        }
        let mut stored = self.project.clone();
        for s in stored.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::to_stored(p, Some(&dir));
            }
        }
        file::save(&path, &stored, Some(&self.workspace))?;
        self.media_dir = project_media;
        self.unsaved_media = false;
        self.path = Some(path.clone());
        self.saved_revision = self.history.revision();
        self.notify(NoticeLevel::Info, format!("saved {}", path.display()));
        Ok(())
    }

    /// Move the scratch media of a never-saved project into `to`.
    fn consolidate_media(&mut self, to: &Path) {
        let Ok(entries) = std::fs::read_dir(&self.media_dir) else {
            return;
        };
        for e in entries.flatten() {
            let from = e.path();
            if from.extension().is_none_or(|x| x != "wav") {
                continue;
            }
            match media::relocate(&from, to, false) {
                Ok(moved) => {
                    // Across file systems the move is a copy; the scratch
                    // original goes (open streams keep their handle).
                    let _ = std::fs::remove_file(&from);
                    let _ =
                        std::fs::remove_file(faderframe_audio_files::import::peaks_path_for(&from));
                    self.media_moves.insert(from, moved);
                }
                Err(err) => self.notify(
                    NoticeLevel::Error,
                    format!("could not move {} into the project: {err}", from.display()),
                ),
            }
        }
        for s in self.project.sources.values_mut() {
            if let SourceSpec::File { path, .. } = &mut s.spec
                && let Some(new) = self.media_moves.get(path.as_path())
            {
                *path = new.clone();
            }
        }
        let _ = std::fs::remove_dir_all(&self.media_dir);
    }

    pub fn save(&mut self) -> Result<()> {
        match self.path.clone() {
            Some(p) => self.save_as(&p),
            None => Err(SessionError::Other(
                "the project has no file name yet".into(),
            )),
        }
    }

    // --- actions -----------------------------------------------------------------

    fn sync(&mut self, impact: Impact) -> Result<()> {
        if impact >= Impact::Timeline {
            self.open_media();
        }
        self.engine.sync(&self.project, &self.sources, impact)?;
        if impact >= Impact::Timeline {
            self.update_loader();
        }
        self.pump_idle();
        self.revision += 1;
        if self
            .editor_clip
            .is_some_and(|c| self.project.clip(c).is_none())
        {
            self.editor_clip = None;
        }
        let live = &self.project;
        self.selection.tracks.retain(|t| live.track(*t).is_some());
        self.selection.clips.retain(|c| live.clip(*c).is_some());
        self.open_takes
            .retain(|c| live.clip(*c).is_some_and(|c| c.as_takes().is_some()));
        Ok(())
    }

    /// Apply an undoable edit.
    pub fn edit(&mut self, cmd: Command) -> Result<()> {
        self.capture_automation(&cmd);
        let impact = self.history.apply(&mut self.project, cmd)?;
        self.sync(impact)
    }

    pub fn dispatch(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Edit(cmd) => self.edit(cmd)?,
            Action::BeginGesture(label) => self.history.begin(label),
            Action::EndGesture => {
                self.history.end();
                self.automation_gesture_ended();
                self.revision += 1;
            }
            Action::Undo => {
                if let Some(r) = self.history.undo(&mut self.project)? {
                    self.sync(r.impact)?;
                    self.notify(NoticeLevel::Info, format!("undo: {}", r.label));
                }
            }
            Action::Redo => {
                if let Some(r) = self.history.redo(&mut self.project)? {
                    self.sync(r.impact)?;
                    self.notify(NoticeLevel::Info, format!("redo: {}", r.label));
                }
            }
            Action::Transport(t) => {
                self.transport_action(t)?;
                self.pump_idle();
                // Locates must not wait for the loader's next poll.
                self.loader.wake();
            }
            Action::Workspace(w) => self.workspace_action(w)?,
            Action::SelectTracks { tracks, mode } => {
                self.selection.select_tracks(&tracks, mode);
                self.revision += 1;
            }
            Action::SelectClips { clips, mode } => {
                self.selection.select_clips(&clips, mode);
                self.revision += 1;
            }
            Action::SelectNotes { notes, mode } => {
                self.selection.select_notes(&notes, mode);
                self.revision += 1;
            }
            Action::ClearSelection => {
                self.selection.clear();
                self.revision += 1;
            }
            Action::OpenClipEditor(clip) => {
                if self
                    .project
                    .clip(clip)
                    .is_some_and(|c| c.as_midi().is_some())
                {
                    self.editor_clip = Some(clip);
                    self.selection.notes.clear();
                    self.workspace_action(WorkspaceAction::ShowView(ViewId::piano_roll()))?;
                    self.revision += 1;
                }
            }
            Action::ResetClipIndicators => {
                for m in self.meters.values_mut() {
                    m.reset_clip();
                }
            }
            Action::AddTrack(kind) => {
                self.add_track(kind)?;
            }
            Action::AddTrackWithLayout(kind, layout) => {
                self.add_track_with_layout(kind, Some(layout))?;
            }
            Action::RemoveSelectedTracks => {
                let tracks: Vec<TrackId> = self
                    .selection
                    .tracks
                    .iter()
                    .copied()
                    .filter(|t| {
                        self.project
                            .track(*t)
                            .is_some_and(|t| t.kind != TrackKind::Master)
                    })
                    .collect();
                if !tracks.is_empty() {
                    self.edit(Command::Batch {
                        label: if tracks.len() == 1 {
                            "Remove Track".into()
                        } else {
                            "Remove Tracks".into()
                        },
                        commands: tracks
                            .into_iter()
                            .map(|track| Command::RemoveTrack { track })
                            .collect(),
                    })?;
                }
            }
            Action::DeleteSelection => self.delete_selection()?,
            Action::SplitSelectedAtPlayhead => self.split_at_playhead()?,
            Action::InsertPlugin {
                track,
                index,
                plugin,
            } => {
                let slot = PluginSlot {
                    id: self.project.ids.allocate(),
                    plugin,
                    bypass: false,
                    parameters: Vec::new(),
                    state: None,
                };
                self.edit(Command::InsertPlugin { track, index, slot })?;
            }
            Action::AddSend {
                track,
                target,
                level_db,
                tap,
            } => {
                let send = AuxSend {
                    id: self.project.ids.allocate(),
                    target,
                    level_db,
                    tap,
                    enabled: true,
                };
                self.edit(Command::AddSend {
                    track,
                    send,
                    index: None,
                })?;
            }
            Action::CreateMidiClip {
                track,
                start,
                length,
            } => {
                let id: ClipId = self.project.ids.allocate();
                let clip = Clip {
                    id,
                    track,
                    name: "MIDI".into(),
                    color: None,
                    start: start.max(MusicalTime::ZERO),
                    muted: false,
                    content: ClipContent::Midi(MidiClip {
                        length: length.max(MusicalTime(1)),
                        notes: Vec::new(),
                    }),
                };
                self.edit(Command::AddClip {
                    clip: Box::new(clip),
                })?;
                self.selection.select_clips(&[id], SelectMode::Replace);
                self.dispatch(Action::OpenClipEditor(id))?;
            }
            Action::AddNote {
                clip,
                start,
                length,
                key,
                velocity,
            } => {
                let id: NoteId = self.project.ids.allocate();
                let note = MidiNote {
                    id,
                    start,
                    length,
                    key,
                    velocity,
                    channel: 0,
                };
                self.edit(Command::AddNote { clip, note })?;
                self.selection.select_notes(&[id], SelectMode::Replace);
            }
            Action::SetRecordSettings(r) => {
                self.record = r;
                self.engine.metronome().set_mode(r.metronome);
                self.revision += 1;
            }
            Action::FlattenTakes(clip) => self.flatten_takes(clip)?,
            Action::ToggleArmSelected => {
                let targets: Vec<(TrackId, bool)> = self
                    .selection
                    .tracks
                    .iter()
                    .filter_map(|t| self.project.track(*t))
                    .filter(|t| t.kind.has_clips())
                    .map(|t| (t.id, t.record_arm))
                    .collect();
                let on = targets.iter().any(|(_, armed)| !armed);
                if !targets.is_empty() {
                    self.edit(Command::Batch {
                        label: "Toggle Record Arm".into(),
                        commands: targets
                            .into_iter()
                            .map(|(track, _)| Command::SetTrackRecordArm { track, on })
                            .collect(),
                    })?;
                }
            }
            Action::SaveTrackPreset { track } => self.save_track_preset(track)?,
            Action::AddTrackFromPreset { path } => {
                self.add_track_from_preset(&path)?;
            }
            Action::ApplyTrackPreset { track, path } => self.apply_track_preset(track, &path)?,
            Action::SetTrackHeight { track, height } => {
                self.workspace.set_track_height(track, height);
                self.revision += 1;
            }
            Action::ShowAutomation { track, target } => {
                self.show_automation(track, target)?;
            }
            Action::HideAutomationLane(lane) => self.hide_automation(lane),
            Action::ToggleTrackAutomation(track) => self.toggle_track_automation(track)?,
            Action::SetAutomationMode { track, lane, mode } => {
                self.set_lane_mode(track, lane, mode)?
            }
            Action::ToggleTakeLanes(clip) => {
                if !self.open_takes.remove(&clip) {
                    self.open_takes.insert(clip);
                }
                self.revision += 1;
            }
            Action::ImportFiles { files, track, at } => {
                self.import_audio(files, ImportTarget { track, at });
            }
            Action::SetGrid(grid) => {
                self.editor.grid = grid;
                self.revision += 1;
            }
            Action::ToggleSnap => {
                self.editor.snap = !self.editor.snap;
                self.revision += 1;
            }
            Action::ToggleFollowPlayhead => {
                self.editor.follow_playhead = !self.editor.follow_playhead;
                self.revision += 1;
            }
        }
        Ok(())
    }

    fn transport_action(&mut self, action: TransportAction) -> Result<()> {
        let to_samples = |s: &Self, pos: MusicalTime| s.engine.musical_to_samples(&s.project, pos);
        match action {
            TransportAction::Play => self.play()?,
            TransportAction::Stop => {
                if self.transport.playing {
                    self.engine.transport(TransportCommand::Stop)?;
                    self.stop_recording()?;
                    self.automation_play_stopped();
                } else if self.recording.is_some() {
                    self.stop_recording()?;
                } else {
                    self.engine.transport(TransportCommand::Locate(0))?;
                }
            }
            TransportAction::TogglePlay => {
                if self.transport.playing {
                    self.engine.transport(TransportCommand::Stop)?;
                    self.stop_recording()?;
                    self.automation_play_stopped();
                } else {
                    self.play()?;
                }
            }
            TransportAction::Locate(pos) => {
                let s = to_samples(self, pos.max(MusicalTime::ZERO));
                self.engine.transport(TransportCommand::Locate(s))?;
                // Show the new position immediately, even while stopped.
                self.transport.position = s;
            }
            TransportAction::ReturnToStart => {
                self.engine.transport(TransportCommand::Locate(0))?;
                self.transport.position = 0;
            }
            TransportAction::ToggleLoop => {
                let range = self.project.loop_range.or_else(|| {
                    MusicalRange::new(MusicalTime::ZERO, self.project.timeline.meter.bar_start(4))
                });
                let enabled = !self.project.loop_enabled;
                self.edit(Command::SetLoop { range, enabled })?;
            }
            TransportAction::SetLoop(range) => {
                let enabled = range.is_some()
                    && (self.project.loop_enabled || self.project.loop_range.is_none());
                self.edit(Command::SetLoop { range, enabled })?;
            }
            TransportAction::ToggleRecord => {
                if self.recording.is_some() {
                    self.stop_recording()?;
                } else {
                    let from = self.transport.position;
                    self.start_recording(from)?;
                }
            }
            TransportAction::TogglePunch => {
                let range = self.project.punch_range.or(self.project.loop_range);
                let enabled = !self.project.punch_enabled;
                self.edit(Command::SetPunch { range, enabled })?;
            }
            TransportAction::SetPunch(range) => {
                let enabled = range.is_some() && self.project.punch_enabled;
                self.edit(Command::SetPunch { range, enabled })?;
            }
            TransportAction::NudgeBars(n) => {
                let meter = &self.project.timeline.meter;
                let bar = meter.bar_at(self.playhead()) + n;
                let pos = meter.bar_start(bar.max(0));
                self.transport_action(TransportAction::Locate(pos))?;
            }
        }
        Ok(())
    }

    fn workspace_action(&mut self, action: WorkspaceAction) -> Result<()> {
        let layout = self.workspace.active_layout_mut();
        match action {
            WorkspaceAction::ShowView(v) => {
                if layout.is_showing(&v) {
                    return Ok(());
                }
                layout.activate(&v)?;
            }
            WorkspaceAction::Detach(v) => {
                layout.detach(&v, WindowGeometry::default())?;
            }
            WorkspaceAction::Attach(v) => layout.attach(&v, None)?,
            WorkspaceAction::CloseWindow(id) => {
                layout.close_window(id)?;
            }
            WorkspaceAction::ToggleArea(area) => {
                layout.toggle_area(&area)?;
            }
            WorkspaceAction::Switch(i) => {
                self.workspace.switch_to(i);
            }
            WorkspaceAction::ResetActive => self.workspace.reset_active(),
        }
        self.layout_revision += 1;
        Ok(())
    }

    // --- track presets -------------------------------------------------------------

    /// Presets in the library folder, sorted by name.
    pub fn track_presets(&self) -> &[PresetEntry] {
        &self.presets
    }

    pub fn track_preset_dir(&self) -> &Path {
        &self.preset_dir
    }

    /// Use another library folder (tests, portable setups).
    pub fn set_track_preset_dir(&mut self, dir: PathBuf) {
        self.preset_dir = dir;
        self.rescan_track_presets();
    }

    pub fn rescan_track_presets(&mut self) {
        let ext = faderframe_project::preset::PRESET_EXTENSION;
        let mut list: Vec<PresetEntry> = std::fs::read_dir(&self.preset_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == ext))
            .map(|path| PresetEntry {
                name: path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().to_string()),
                path,
            })
            .collect();
        list.sort_by_key(|e| e.name.to_lowercase());
        self.presets = list;
        self.revision += 1;
    }

    /// Save `track` as a preset file at `path`.
    pub fn export_track_preset(&self, track: TrackId, path: &Path) -> Result<()> {
        let t = self
            .project
            .track(track)
            .ok_or(EditError::UnknownTrack(track))?;
        let preset = faderframe_project::TrackPreset::capture(&self.project, t);
        faderframe_project::preset::save(path, &preset)
            .map_err(|e| SessionError::Other(e.to_string()))
    }

    fn save_track_preset(&mut self, track: TrackId) -> Result<()> {
        let name = self
            .project
            .track(track)
            .ok_or(EditError::UnknownTrack(track))?
            .name
            .clone();
        std::fs::create_dir_all(&self.preset_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.preset_dir.display())))?;
        let path = faderframe_audio_files::import::unique_path(
            &self.preset_dir,
            &name,
            faderframe_project::preset::PRESET_EXTENSION,
        );
        self.export_track_preset(track, &path)?;
        self.rescan_track_presets();
        self.notify(NoticeLevel::Info, format!("saved track preset '{name}'"));
        Ok(())
    }

    fn load_preset(&self, path: &Path) -> Result<faderframe_project::TrackPreset> {
        faderframe_project::preset::load(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))
    }

    /// Add a track built from the preset at `path`; returns its id.
    pub fn add_track_from_preset(&mut self, path: &Path) -> Result<TrackId> {
        let preset = self.load_preset(path)?;
        if preset.kind == TrackKind::Master {
            return Err(EditError::Invalid(
                "a master preset can only be applied to the master".into(),
            )
            .into());
        }
        let index = self.insertion_index(preset.kind);
        let (track, notes) = preset.instantiate(&mut self.project, None);
        let id = track.id;
        self.edit(Command::AddTrack {
            track: Box::new(track),
            index,
        })?;
        for n in notes {
            self.notify(
                NoticeLevel::Warning,
                format!("preset '{}': {n}", preset.name),
            );
        }
        self.selection.select_tracks(&[id], SelectMode::Replace);
        Ok(id)
    }

    fn apply_track_preset(&mut self, track: TrackId, path: &Path) -> Result<()> {
        let preset = self.load_preset(path)?;
        let (commands, notes) = preset
            .apply_commands(&mut self.project, track)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.edit(Command::Batch {
            label: format!("Apply Track Preset '{}'", preset.name),
            commands,
        })?;
        for n in notes {
            self.notify(
                NoticeLevel::Warning,
                format!("preset '{}': {n}", preset.name),
            );
        }
        Ok(())
    }

    /// Where a new track of `kind` goes: content tracks after the last
    /// selected/content track, buses and auxes before the master.
    fn insertion_index(&self, kind: TrackKind) -> usize {
        let p = &self.project;
        if kind.is_summing() {
            p.tracks
                .iter()
                .position(|t| t.kind == TrackKind::Master)
                .unwrap_or(p.tracks.len())
        } else {
            let after = self
                .selection
                .tracks
                .iter()
                .filter_map(|t| p.track_index(*t))
                .max()
                .or_else(|| p.tracks.iter().rposition(|t| t.kind.has_clips()));
            after.map_or(0, |i| i + 1)
        }
    }

    /// Create a track of `kind` with sensible defaults; returns its id.
    pub fn add_track(&mut self, kind: TrackKind) -> Result<TrackId> {
        self.add_track_with_layout(kind, None)
    }

    /// Like [`Session::add_track`] with an explicit channel layout.
    pub fn add_track_with_layout(
        &mut self,
        kind: TrackKind,
        layout: Option<faderframe_core::ChannelLayout>,
    ) -> Result<TrackId> {
        if kind == TrackKind::Master {
            return Err(EditError::Invalid("a project has exactly one master".into()).into());
        }
        let p = &mut self.project;
        let n = p.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let id: TrackId = p.ids.allocate();
        let color = TrackColor::palette(p.tracks.len());
        let mut track = Track::new(id, kind, format!("{} {n}", kind.label()), color);
        if let Some(layout) = layout {
            track.layout = layout;
        }
        if kind == TrackKind::Audio {
            // Like a console channel: input 1 (or 1–2 for stereo) by default.
            track.input = faderframe_project::InputRouting::Hardware { first_channel: 0 };
        }
        if kind == TrackKind::Instrument {
            track.instrument = Some(PluginSlot {
                id: p.ids.allocate(),
                plugin: PluginRef::builtin(builtin::SYNTH, "FaderFrame Synth"),
                bypass: false,
                parameters: Vec::new(),
                state: None,
            });
        }
        // Content tracks go after the last selected/content track; buses and
        // auxes go just before the master.
        let index = if kind.is_summing() {
            p.tracks
                .iter()
                .position(|t| t.kind == TrackKind::Master)
                .unwrap_or(p.tracks.len())
        } else {
            let after = self
                .selection
                .tracks
                .iter()
                .filter_map(|t| p.track_index(*t))
                .max()
                .or_else(|| p.tracks.iter().rposition(|t| t.kind.has_clips()));
            after.map_or(0, |i| i + 1)
        };
        self.edit(Command::AddTrack {
            track: Box::new(track),
            index,
        })?;
        self.selection.select_tracks(&[id], SelectMode::Replace);
        Ok(id)
    }

    fn delete_selection(&mut self) -> Result<()> {
        if let Some(clip) = self.editor_clip()
            && !self.selection.notes.is_empty()
        {
            let notes: Vec<NoteId> = self.selection.notes.iter().copied().collect();
            self.selection.notes.clear();
            return self.edit(Command::Batch {
                label: "Delete Notes".into(),
                commands: notes
                    .into_iter()
                    .map(|note| Command::RemoveNote { clip, note })
                    .collect(),
            });
        }
        let clips: Vec<ClipId> = self.selection.clips.iter().copied().collect();
        if clips.is_empty() {
            return Ok(());
        }
        self.selection.clips.clear();
        self.edit(Command::Batch {
            label: if clips.len() == 1 {
                "Delete Clip".into()
            } else {
                "Delete Clips".into()
            },
            commands: clips
                .into_iter()
                .map(|clip| Command::RemoveClip { clip })
                .collect(),
        })
    }

    fn split_at_playhead(&mut self) -> Result<()> {
        let at = self.playhead();
        let targets: Vec<ClipId> = self
            .selection
            .clips
            .iter()
            .copied()
            .filter(|c| {
                self.project.clip(*c).is_some_and(|clip| {
                    clip.start < at
                        && clip.end(&self.project.timeline, self.project.sample_rate) > at
                })
            })
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        let mut commands = Vec::new();
        for clip in targets {
            let new_clip: ClipId = self.project.ids.allocate();
            commands.push(Command::SplitClip { clip, at, new_clip });
        }
        self.edit(Command::Batch {
            label: "Split Clips".into(),
            commands,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel_imports();
        // Scratch media of a project that was never saved is discarded with
        // it (the shell asks before closing unsaved work).
        self.discard_unsaved_media();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_audio::dummy::DummyBackend;

    fn config() -> EngineConfig {
        EngineConfig::default()
    }

    #[test]
    fn demo_session_starts_on_dummy_backend_at_requested_rate() {
        let mut s = Session::demo(config()).unwrap();
        let info = s
            .start_audio(
                vec![Box::new(DummyBackend::default())],
                &AudioPreferences {
                    sample_rate: Some(96_000),
                    buffer_size: Some(128),
                },
            )
            .unwrap();
        assert_eq!(info.sample_rate, 96_000);
        assert_eq!(s.sample_rate(), 96_000, "engine follows the stream rate");
        s.dispatch(Action::Transport(TransportAction::Play))
            .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while s.transport().position < 9_600 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            s.tick(0.005);
        }
        assert!(s.transport().playing);
        assert!(s.transport().position >= 9_600);
        s.stop_audio();
        // A second start recreates the engine transparently.
        s.start_audio(
            vec![Box::new(DummyBackend::default())],
            &AudioPreferences::default(),
        )
        .unwrap();
        assert_eq!(s.sample_rate(), 48_000);
    }

    #[test]
    fn edits_undo_and_selection_follow_the_project() {
        let mut s = Session::demo(config()).unwrap();
        let before = s.project().tracks.len();
        let id = s.add_track(TrackKind::Instrument).unwrap();
        assert_eq!(s.project().tracks.len(), before + 1);
        assert!(s.project().track(id).unwrap().instrument.is_some());
        assert_eq!(s.selection.primary_track(), Some(id));
        assert!(s.is_dirty());
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(s.project().tracks.len(), before);
        assert!(s.selection.tracks.is_empty(), "selection pruned after undo");
        s.dispatch(Action::Redo).unwrap();
        // Redo restores the track, not the (UI-only) selection.
        s.dispatch(Action::SelectTracks {
            tracks: vec![id],
            mode: SelectMode::Replace,
        })
        .unwrap();
        s.dispatch(Action::RemoveSelectedTracks).unwrap();
        assert_eq!(s.project().tracks.len(), before);
    }

    #[test]
    fn opening_a_midi_clip_shows_the_piano_roll() {
        let mut s = Session::demo(config()).unwrap();
        let clip = s
            .project()
            .clips
            .values()
            .find(|c| c.as_midi().is_some())
            .unwrap()
            .id;
        // Start from a layout where the mixer tab is active.
        assert!(
            !s.workspace()
                .active_layout()
                .is_showing(&ViewId::piano_roll())
        );
        let rev = s.layout_revision();
        s.dispatch(Action::OpenClipEditor(clip)).unwrap();
        assert_eq!(s.editor_clip(), Some(clip));
        assert!(
            s.workspace()
                .active_layout()
                .is_showing(&ViewId::piano_roll())
        );
        assert!(s.layout_revision() > rev);
    }

    #[test]
    fn delete_and_split_selected_clips() {
        let mut s = Session::demo(config()).unwrap();
        let drums = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Drums")
            .unwrap()
            .id;
        let clip = s.project().track(drums).unwrap().clips[0];
        s.dispatch(Action::SelectClips {
            clips: vec![clip],
            mode: SelectMode::Replace,
        })
        .unwrap();
        let bar2 = s.project().timeline.meter.bar_start(2);
        s.dispatch(Action::Transport(TransportAction::Locate(bar2)))
            .unwrap();
        s.dispatch(Action::SplitSelectedAtPlayhead).unwrap();
        assert_eq!(s.project().track(drums).unwrap().clips.len(), 2);
        s.dispatch(Action::SelectClips {
            clips: s.project().track(drums).unwrap().clips.clone(),
            mode: SelectMode::Replace,
        })
        .unwrap();
        s.dispatch(Action::DeleteSelection).unwrap();
        assert!(s.project().track(drums).unwrap().clips.is_empty());
        s.dispatch(Action::Undo).unwrap();
        assert_eq!(s.project().track(drums).unwrap().clips.len(), 2);
    }

    #[test]
    fn save_and_open_round_trip() {
        let mut s = Session::demo(config()).unwrap();
        s.dispatch(Action::Workspace(WorkspaceAction::Detach(ViewId::mixer())))
            .unwrap();
        let dir = std::env::temp_dir().join(format!("ff-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song");
        s.save_as(&path).unwrap();
        assert!(!s.is_dirty());
        let saved = s.path().unwrap().to_path_buf();
        assert_eq!(saved.extension().unwrap(), "ffproj");
        let mut t = Session::new(Project::new("x", 48_000), None, config()).unwrap();
        t.open(&saved).unwrap();
        assert_eq!(t.project(), s.project());
        assert!(
            !t.workspace().active_layout().floating.is_empty(),
            "detached mixer restored"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
