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

mod meters;
pub mod render;
mod selection;

pub use meters::{METER_FLOOR_DB, MeterChannel, MeterDisplay};
pub use selection::{SelectMode, Selection};

use faderframe_audio::{
    AudioBackend, AudioError, AudioStream, StreamConfig, StreamInfo, StreamStatus,
};
use faderframe_audio_files::PeakCache;
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId, builtin};
use faderframe_engine::{EngineConfig, EngineController, EngineError, EngineProcessor, SourceMap};
use faderframe_project::file::{self, FileError};
use faderframe_project::{
    AuxSend, Clip, ClipContent, Command, EditError, History, Impact, MidiClip, MidiNote,
    MusicalRange, PluginRef, PluginSlot, Project, SendTap, Track, TrackColor, TrackKind,
};
use faderframe_realtime::MetricsSnapshot;
use faderframe_timeline::{GridDivision, MusicalTime};
use faderframe_transport::{TransportCommand, TransportSnapshot};
use faderframe_workspace::{
    DockAreaId, LayoutError, ViewId, WindowGeometry, WindowId, WorkspaceSet,
};
use std::collections::{HashMap, VecDeque};
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
    ToggleRecord,
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
    RemoveSelectedTracks,
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

/// Audio startup preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioPreferences {
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
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
}

impl Session {
    /// A session for `project` with a not-yet-started engine.
    pub fn new(
        project: Project,
        workspace: Option<WorkspaceSet>,
        config: EngineConfig,
    ) -> Result<Self> {
        let (engine, processor) = faderframe_engine::create(config);
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
        };
        s.render_sources();
        s.engine.sync(&s.project, &s.sources, Impact::Graph)?;
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
        self.transport.playing || self.meters.values().any(MeterDisplay::is_active)
    }

    // --- audio ---------------------------------------------------------------

    fn recreate_engine(&mut self) -> Result<()> {
        let position = self.transport.position;
        self.engine_config.sample_rate = self.engine.sample_rate();
        let (engine, processor) = faderframe_engine::create(self.engine_config);
        self.engine = engine;
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
        self.engine.transport(TransportCommand::Locate(position))?;
        self.pending = Some(processor);
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
        render::start(self.project.clone(), settings)
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
        self.pump_idle();
        self.engine.collect_garbage();
        self.transport = self.engine.transport_snapshot();
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

    fn render_sources(&mut self) {
        self.sources =
            faderframe_engine::render_generated_sources(&self.project, self.engine.sample_rate());
        self.peaks = self
            .sources
            .iter()
            .map(|(id, data)| (*id, Arc::new(PeakCache::build(data))))
            .collect();
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
        self.render_sources();
        self.editor_clip = self.first_midi_clip();
        self.engine.transport(TransportCommand::Stop)?;
        self.engine.transport(TransportCommand::Locate(0))?;
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
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
            p.tracks.insert(
                0,
                Track::new(id, TrackKind::Audio, "Audio 1", TrackColor::palette(0)),
            );
            p
        };
        self.path = None;
        self.replace_project(project, None)
    }

    pub fn open(&mut self, path: &Path) -> Result<Vec<String>> {
        let loaded = file::load(path)?;
        let notes = loaded.notes.clone();
        self.replace_project(loaded.project, loaded.workspace)?;
        self.path = Some(path.to_path_buf());
        for n in &notes {
            self.notify(NoticeLevel::Warning, n.clone());
        }
        self.notify(NoticeLevel::Info, format!("opened {}", path.display()));
        Ok(notes)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let mut path = path.to_path_buf();
        if path.extension().is_none() {
            path.set_extension(file::FILE_EXTENSION);
        }
        file::save(&path, &self.project, Some(&self.workspace))?;
        self.path = Some(path.clone());
        self.saved_revision = self.history.revision();
        self.notify(NoticeLevel::Info, format!("saved {}", path.display()));
        Ok(())
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
        self.engine.sync(&self.project, &self.sources, impact)?;
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
        Ok(())
    }

    /// Apply an undoable edit.
    pub fn edit(&mut self, cmd: Command) -> Result<()> {
        let impact = self.history.apply(&mut self.project, cmd)?;
        self.sync(impact)
    }

    pub fn dispatch(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Edit(cmd) => self.edit(cmd)?,
            Action::BeginGesture(label) => self.history.begin(label),
            Action::EndGesture => {
                self.history.end();
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
            TransportAction::Play => self.engine.transport(TransportCommand::Play)?,
            TransportAction::Stop => {
                if self.transport.playing {
                    self.engine.transport(TransportCommand::Stop)?;
                } else {
                    self.engine.transport(TransportCommand::Locate(0))?;
                }
            }
            TransportAction::TogglePlay => self.engine.transport(TransportCommand::TogglePlay)?,
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
                // Recording to disk arrives with the recording subsystem; the
                // transport flag already reaches processors and monitoring.
                let on = !self.transport.recording;
                self.engine.transport(TransportCommand::SetRecording(on))?;
                if on {
                    self.notify(
                        NoticeLevel::Warning,
                        "record mode: monitoring only — writing takes to disk is not implemented yet",
                    );
                }
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

    /// Create a track of `kind` with sensible defaults; returns its id.
    pub fn add_track(&mut self, kind: TrackKind) -> Result<TrackId> {
        if kind == TrackKind::Master {
            return Err(EditError::Invalid("a project has exactly one master".into()).into());
        }
        let p = &mut self.project;
        let n = p.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let id: TrackId = p.ids.allocate();
        let color = TrackColor::palette(p.tracks.len());
        let mut track = Track::new(id, kind, format!("{} {n}", kind.label()), color);
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
                vec![Box::new(DummyBackend)],
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
        s.start_audio(vec![Box::new(DummyBackend)], &AudioPreferences::default())
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
