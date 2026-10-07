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
pub mod midi;
pub mod modulators;
pub mod notes;
pub mod performance;
pub mod pitch;
pub use performance::{Load, PerformanceReport, PluginPerformance, TrackPerformance};
pub mod adm;
pub mod album;
mod album_master;
mod aliases;
pub mod analysis;
pub mod capture;
pub mod clip_fx;
pub mod containers;
pub mod control;
mod control_extra;
pub mod ddp;
pub mod delivery;
pub mod detect;
pub mod editing;
mod folders;
mod freeze;
pub mod groove;
mod groups;
pub mod iamf;
pub mod lanes;
pub mod launcher;
pub mod listening;
pub mod outputs;
mod programs;
mod redraw;
pub mod samples;
pub mod sampling;
mod sandbox;
pub mod speech;
pub mod to_midi;
pub mod vinyl;
pub use groups::GroupMenuEntry;
mod midifile;
pub mod presets;
pub mod record;
pub mod render;
mod selection;
pub mod sync;
mod sysex;
pub mod templates;
mod transients;
pub mod versions;
pub mod video;
/// A plugin state as projects store it (and back).
pub use faderframe_engine::{
    decode_state as decode_plugin_state, encode_state as encode_plugin_state,
};
pub use midifile::is_midi_file;
pub mod warping;
pub use editing::{
    ClipEdge, CounterUnit, EditFlag, EditMode, EditRange, EditTool, GridMode, NudgeTarget,
    NudgeValue, ZoomRequest, parse_position,
};
pub use faderframe_workspace::{DEFAULT_INSERT_SLOTS, INSERT_SLOTS_RANGE, STRIP_WIDTH_RANGE};
pub use sync::{MANUAL_SPEED_RANGE, MtcRate, SyncSettings, SyncSource, SyncStatus, Timecode};

pub use meters::{METER_FLOOR_DB, MeterChannel, MeterDisplay};
pub use selection::{SelectMode, Selection};

pub use automation::{AutomationParam, ParamKind};
use faderframe_audio::{
    AudioBackend, AudioError, AudioStream, StreamConfig, StreamInfo, StreamStatus,
};
use faderframe_audio_files::PeakCache;
pub use faderframe_automation::{AutomationMode, AutomationTarget};
use faderframe_core::{AudioSourceId, ClipId, NoteId, TrackId};
use faderframe_engine::{
    EngineConfig, EngineController, EngineError, EngineProcessor, Source, SourceMap, StreamPlan,
};
use faderframe_project::file::{self, FileError};
use faderframe_project::midi_ops::QuantizeSettings;
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
pub use midi::{
    KEYBOARD_PORT, LiveNote, MidiOutputStatus, MidiPortStatus, MidiPreferences, StepInput,
};
pub use notes::{KeyFold, NoteLength, NoteOp, PianoRollSettings, ToolPreview};
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
    /// Scrubbing: play a short snippet from here (the playhead stays).
    Scrub(MusicalTime),
    ReturnToStart,
    ToggleLoop,
    SetLoop(Option<MusicalRange>),
    /// The record button: stopped — record the armed tracks and play;
    /// playing — punch in here; recording — punch out (keeps playing).
    /// With nothing armed it only warns.
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
    /// Show or hide the master strip at the window's right edge (this
    /// workspace).
    ToggleMasterPanel,
}

/// Everything an editor view can ask the session to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// An undoable project edit.
    Edit(Command),
    /// Start grouping subsequent edits into one undo step (e.g. a drag).
    BeginGesture(String),
    EndGesture,
    /// Drop the open gesture, undoing its edits (a drag taken elsewhere).
    CancelGesture,
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
    SetPreamp {
        track: TrackId,
        model: Option<usize>,
    },
    InsertPlugin {
        track: TrackId,
        index: usize,
        plugin: PluginRef,
    },
    /// Move an insert to slot `index` of `to` (the same track reorders);
    /// the plugin keeps running with its state.
    MovePlugin {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        to: TrackId,
        index: usize,
    },
    /// Pencil at sample level: replace `samples` of the clip's audio from
    /// source frame `start` (one channel or all), non-destructively.
    RedrawAudio {
        clip: ClipId,
        channel: Option<usize>,
        start: i64,
        samples: Vec<f32>,
    },
    /// Open the colour chooser for a track (the selected tracks follow)
    /// or a section.
    PickColor(ColorTarget),
    /// A marker at this position.
    AddMarker(MusicalTime),
    /// A section of the arrangement over this range.
    AddSection {
        start: MusicalTime,
        end: MusicalTime,
    },
    /// Move a section with everything in it (clips, automation, tempo,
    /// signatures, markers) so that it starts at `to`, or insert a copy
    /// there (`copy`). The material in between closes up or makes room.
    MoveSection {
        section: faderframe_core::SectionId,
        to: MusicalTime,
        copy: bool,
    },
    /// A copy of the section with its content right after it.
    DuplicateSection(faderframe_core::SectionId),
    /// Swap a section (with content) with the one before or after it.
    SwapSection {
        section: faderframe_core::SectionId,
        later: bool,
    },
    /// Remove a section and everything in it; what follows moves up.
    DeleteSectionContent(faderframe_core::SectionId),
    /// A tempo change at this position (keeping the tempo there).
    AddTempoPoint(MusicalTime),
    /// Move tempo point `index` and set its tempo.
    SetTempoPoint {
        index: usize,
        position: MusicalTime,
        bpm: f64,
    },
    RemoveTempoPoint(usize),
    /// Ramp from tempo point `index` to the next (or step).
    SetTempoRamp {
        index: usize,
        ramp: bool,
    },
    ShowGlobalLane(lanes::GlobalLane, bool),
    /// Fill the chord track from the selected MIDI clips (or all of them).
    DetectChords,
    /// Set the project's key from the selected MIDI clips (or all of them).
    DetectKey,
    /// Analyse this track in the Tools view (`None`: the master).
    SetAnalysisSource(Option<TrackId>),
    /// Start a new loudness measurement.
    ResetAnalysis,
    SetLoudnessTarget(f32),
    SetLevelScale(analysis::LevelScale),
    /// Album songs, settings, analysis and export.
    Album(album::AlbumAction),
    /// The DDP player (`session::ddp`).
    Ddp(ddp::DdpAction),
    /// Start a plugin again from its slot (after a crash, or to move it
    /// into or out of a sandbox).
    ReloadPlugin(faderframe_core::PluginInstanceId),
    /// Give a built-in sampler `files` from `slot` on (one each; copied
    /// into the project, decoded in the background; no files: clear the
    /// slot). One undo step once loaded.
    LoadDeviceSamples {
        plugin: faderframe_core::PluginInstanceId,
        slot: usize,
        files: Vec<PathBuf>,
    },
    /// Every plugin of the project.
    ReloadAllPlugins,
    /// Render an audio track from `start` to `end` (its clips as they
    /// play, without its plugins) and give it to a sampler or a file.
    MakeSample {
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
        target: sampling::SampleTarget,
    },
    /// Turn what the live tracks were played last into clips (recording
    /// or not).
    CaptureMidi,
    /// Show an audio clip in the pitch editor (finding its notes first if
    /// it has none).
    OpenPitchEditor(ClipId),
    /// Select the track and show the surround panner.
    ShowSurroundPanner(TrackId),
    /// Read an object-based master (ADM BWF) into the project.
    ImportAdm(std::path::PathBuf),
    /// The mono check (listening only).
    SetMonoCheck(bool),
    /// Listen on headphones (binaural, with a room) or speakers (`None`).
    SetHeadphones(Option<faderframe_binaural::Room>),
    /// Listen through a head (a built-in id, or `sofa:<path>`).
    SetHead(String),
    /// Even out the headphones with a correction file (`None`: none).
    SetHeadphoneCorrection(Option<std::path::PathBuf>),
    /// Download the speech model (Whisper) once.
    DownloadSpeechModel,
    /// Transcribe an audio clip's words into the lyrics.
    Transcribe(ClipId),
    /// Write the lyrics as LRC and SRT next to the project.
    ExportLyrics,
    /// Change (`Some`) or remove (`None`) a lyric line's words.
    EditLyric {
        index: usize,
        text: Option<String>,
    },
    /// Show an audio clip's effects in their editor.
    OpenClipEffects(ClipId),
    /// Edit an audio clip's effects (rendered once the chain rests).
    ClipEffects {
        clip: ClipId,
        op: clip_fx::ClipFxOp,
    },
    /// An audio clip's notes on a new instrument track (melody, harmony
    /// or drums; after listening to it).
    ConvertToMidi {
        clip: ClipId,
        how: to_midi::ToMidi,
    },
    /// Set the project's tempo or key from a clip, or warp it to the
    /// tempo (after analysing it).
    FromClip {
        clip: ClipId,
        what: detect::FromClip,
    },
    /// Find the notes of audio clips for pitch editing.
    DetectPitch {
        clips: Vec<ClipId>,
    },
    /// Edit the notes of a clip's pitch edit.
    EditPitch {
        clip: ClipId,
        op: pitch::PitchOp,
    },
    /// Launch clips and scenes, edit the clip launcher.
    Launcher(launcher::LauncherOp),
    /// Undo or redo until `n` steps are done (0: as opened); the history
    /// view's click.
    HistoryTo(usize),
    /// A new modulator on a track (its id allocated here).
    AddModulator {
        track: TrackId,
        source: faderframe_project::modulation::ModSource,
    },
    RemoveModulator {
        track: TrackId,
        modulator: faderframe_core::ModulatorId,
    },
    /// Replace a modulator (its settings, routes, depths), found by id.
    SetModulator {
        track: TrackId,
        modulator: faderframe_project::modulation::Modulator,
    },
    /// Map the next parameter touched on the track's devices to the
    /// modulator (`None`: stop mapping).
    LearnModulation(Option<(TrackId, faderframe_core::ModulatorId)>),
    /// Containers: a chain more, one renamed or removed, a device into a
    /// chain or out of one.
    AddChain {
        track: TrackId,
        container: faderframe_core::PluginInstanceId,
    },
    RenameChain {
        track: TrackId,
        container: faderframe_core::PluginInstanceId,
        chain: usize,
        name: String,
    },
    RemoveChain {
        track: TrackId,
        container: faderframe_core::PluginInstanceId,
        chain: usize,
    },
    InsertIntoChain {
        track: TrackId,
        container: faderframe_core::PluginInstanceId,
        chain: usize,
        index: usize,
        plugin: PluginRef,
    },
    RemoveFromChain {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
    },
    /// The keys a chain's devices get (`low..=high`).
    SetChainKeys {
        track: TrackId,
        container: faderframe_core::PluginInstanceId,
        chain: usize,
        low: u8,
        high: u8,
    },
    /// Keep the project as it is now as a named version.
    SaveVersion {
        name: String,
    },
    /// Ask for a name, then save a version.
    PromptSaveVersion,
    /// Show the project's versions (compare, restore).
    ShowVersions,
    /// Go back to a saved version (the project as it is is kept as one).
    RestoreVersion(PathBuf),
    /// Picture: import, clip edits, offset, export, sync test.
    Video(video::VideoOp),
    /// Keep the project's set-up (everything but its content) as the
    /// template `name`, replacing one of that name.
    SaveTemplate {
        name: String,
    },
    /// Ask for a name, then save a template.
    PromptSaveTemplate,
    /// Show the templates (new project from one, delete, default).
    ShowTemplates,
    /// Delete a template.
    DeleteTemplate(PathBuf),
    /// An alias of each clip (sharing its content) right after it.
    DuplicateAsAlias(Vec<ClipId>),
    /// The clips are their own again (no longer aliases).
    MakeClipsUnique(Vec<ClipId>),
    /// A new folder track holding these tracks.
    NewFolder {
        tracks: Vec<TrackId>,
    },
    /// Put tracks into a folder (`None`: out of theirs, a level up).
    MoveToFolder {
        tracks: Vec<TrackId>,
        folder: Option<TrackId>,
    },
    /// Open or close a folder (not undoable, kept with the layout).
    ToggleFolder(TrackId),
    /// Sum a folder's tracks in a new bus inside it.
    SumFolder(TrackId),
    /// Ask where to save such a sample, then make it.
    PromptSaveSample {
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
    },
    /// Switch a plugin to one of its own programs (one undo step).
    SelectPluginProgram {
        plugin: faderframe_core::PluginInstanceId,
        index: usize,
    },
    SetResetOnPlay(bool),
    /// Group the selected tracks.
    GroupSelectedTracks,
    DeleteGroup(faderframe_core::GroupId),
    /// Turn a group's linking on or off.
    SetGroupActive {
        group: faderframe_core::GroupId,
        active: bool,
    },
    /// Change what a group links.
    SetGroupLink {
        group: faderframe_core::GroupId,
        link: faderframe_project::GroupLink,
    },
    RenameGroup {
        group: faderframe_core::GroupId,
        name: String,
    },
    /// Ask (in the shell) for a group's new name.
    PromptRenameGroup(faderframe_core::GroupId),
    /// Assign the selected tracks to a VCA.
    AssignSelectedToVca(TrackId),
    /// Render a track and play the result instead of its clips, instrument
    /// and inserts (unloading its plugins).
    FreezeTrack(TrackId),
    UnfreezeTrack(TrackId),
    /// Render a track (after its inserts) onto a new audio track and mute
    /// the original.
    BounceTrack(TrackId),
    /// Ask (in the shell) for a preset name for the plugin.
    PromptSavePluginPreset(faderframe_core::PluginInstanceId),
    /// Save a plugin's current settings as a user preset named `name`.
    SavePluginPreset {
        plugin: faderframe_core::PluginInstanceId,
        name: String,
    },
    /// Load a preset file into a plugin (one undo step).
    LoadPluginPreset {
        plugin: faderframe_core::PluginInstanceId,
        path: PathBuf,
    },
    /// Copy an insert, with its current settings, to slot `index` of `to`.
    CopyPlugin {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        to: TrackId,
        index: usize,
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
    /// Import a Standard MIDI File as new instrument tracks at `at` (with
    /// `tempo`: and its tempo map and time signatures).
    ImportMidiFile {
        path: PathBuf,
        at: MusicalTime,
        tempo: bool,
    },
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
    /// One mode for several lanes (one undo step).
    SetAutomationModes {
        lanes: Vec<(TrackId, faderframe_core::AutomationLaneId)>,
        mode: AutomationMode,
    },
    /// Clear performance peaks, history and callback statistics.
    ResetPerformance,
    // --- piano roll (ids allocated by the session) ---
    AddNotes {
        clip: ClipId,
        notes: Vec<MidiNote>,
    },
    /// Apply the MIDI Tools panel's tool to these notes (all of the clip's
    /// without any; generators: the bars they span).
    ApplyMidiTool {
        clip: ClipId,
        notes: Vec<NoteId>,
    },
    /// A chord of the piano roll's chord kind on `key`.
    AddChord {
        clip: ClipId,
        start: MusicalTime,
        length: MusicalTime,
        key: u8,
        velocity: u8,
    },
    /// Copies moved by `offset` (`None`: right after them) and `keys`.
    DuplicateNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
        offset: Option<MusicalTime>,
        keys: i32,
    },
    SplitNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
        at: MusicalTime,
    },
    RemoveNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
    },
    /// Apply an operation to notes (all notes of the clip when empty).
    NoteOperation {
        clip: ClipId,
        notes: Vec<NoteId>,
        op: NoteOp,
    },
    CopyNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
    },
    CutNotes {
        clip: ClipId,
        notes: Vec<NoteId>,
    },
    PasteNotes {
        clip: ClipId,
        at: MusicalTime,
    },
    SetControllerPoints {
        clip: ClipId,
        controller: faderframe_project::MidiController,
        channel: u8,
        from: MusicalTime,
        to: MusicalTime,
        points: Vec<faderframe_project::ControllerPoint>,
    },
    SetMidiClipLength {
        clip: ClipId,
        length: MusicalTime,
    },
    /// Add SysEx messages to a MIDI clip (clip-relative time).
    AddSysex {
        clip: ClipId,
        at: MusicalTime,
        messages: Vec<Vec<u8>>,
    },
    RemoveSysex {
        clip: ClipId,
        index: usize,
    },
    /// Ask the shell for a `.syx` file to add to a clip at `at`.
    RequestSysexImport {
        clip: ClipId,
        at: MusicalTime,
    },
    /// Send SysEx to a MIDI output now (port key).
    SendSysex {
        output: String,
        messages: Vec<Vec<u8>>,
    },
    /// Replace a note's expression curve in `from..to` (note-relative).
    SetNoteExpression {
        clip: ClipId,
        note: NoteId,
        kind: faderframe_project::ExpressionKind,
        from: MusicalTime,
        to: MusicalTime,
        points: Vec<faderframe_project::ExpressionPoint>,
    },
    /// Play a note on a track's instrument (until `AuditionOff`).
    Audition {
        track: TrackId,
        key: u8,
        velocity: u8,
        channel: u8,
    },
    AuditionOff,
    SetStepInput(Option<midi::StepInput>),
    SetPianoRoll(PianoRollSettings),
    /// Strength, swing and note ends of Quantize.
    SetQuantize(QuantizeSettings),
    SetHumanize(groove::HumanizeSettings),
    /// Quantize whole clips: audio by its transients, MIDI by its notes
    /// (one undo step).
    QuantizeClips(Vec<ClipId>),
    /// Humanize whole clips: transients and notes in time, note velocities.
    HumanizeClips(Vec<ClipId>),
    /// Map the next control moved on a MIDI device to this target.
    MidiLearn(faderframe_project::MappingTarget),
    CancelMidiLearn,
    /// Ask the shell to open the plugin browser for a track.
    OpenPluginBrowser {
        track: TrackId,
        target: PluginTarget,
    },
    /// A new instrument track, and the plugin browser to choose its
    /// instrument.
    AddInstrumentTrack,
    /// A built-in device editor's own setting (its analyser, its display):
    /// kept for the session, not part of the project or the undo history.
    SetDeviceView {
        plugin: faderframe_core::PluginInstanceId,
        values: Vec<(String, f64)>,
    },
    /// Ask the shell to show a plugin's editor: its own GUI, or (`generic`,
    /// or when it has none) the generic parameter window.
    OpenPluginEditor {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        generic: bool,
    },
    /// Choose (or remove) the instrument of an instrument track.
    SetInstrumentPlugin {
        track: TrackId,
        plugin: Option<PluginRef>,
    },
    /// Ask (in the shell) for names to save the tracks' settings under.
    PromptSaveTrackPreset {
        tracks: Vec<TrackId>,
    },
    /// Tracks for a plugin's extra output buses (`buses`: those, else all
    /// that have none), routed like its track, in a folder under it.
    CreateOutputTracks {
        plugin: faderframe_core::PluginInstanceId,
        buses: Option<Vec<u16>>,
    },
    /// Ask (in the shell) whether to delete a user preset (a plugin's or
    /// a track preset).
    PromptDeletePreset {
        path: PathBuf,
    },
    /// Delete a user preset file (never a factory one).
    DeletePreset {
        path: PathBuf,
    },
    /// Save a track's settings into the track preset library, as `name`
    /// (`None`: the track's). A preset of that name is replaced when
    /// `replace`, else both are kept (the new one numbered).
    SaveTrackPreset {
        track: TrackId,
        name: Option<String>,
        replace: bool,
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
    /// Width of the arranger's track header column (saved with the layout).
    SetHeaderWidth(f32),
    /// Mixer strip width of one track, or of all (`None`); `width: None`
    /// goes back to the default. Not undoable, saved with the layout.
    SetStripWidth {
        track: Option<TrackId>,
        width: Option<f32>,
    },
    /// Insert slots per mixer strip (saved with the layout).
    SetMixerInsertSlots(u16),
    /// Remember where a plugin editor window is (saved with the layout).
    SetPluginWindowPosition {
        plugin: faderframe_core::PluginInstanceId,
        x: i32,
        y: i32,
    },
    SetGrid(GridDivision),
    ToggleSnap,
    SetEditMode(EditMode),
    SetGridMode(GridMode),
    SetEditTool(EditTool),
    SetNudge(NudgeValue),
    SetEditFlag(EditFlag, bool),
    SetCounterUnit(CounterUnit),
    SetTransientSensitivity(f32),
    /// Ask the arranger to zoom.
    Zoom(ZoomRequest),
    /// The edit selection range (`None`: none); tracks via `SelectTracks`.
    SetEditRange(Option<EditRange>),
    /// Split at the selection's edges (or the selected clips at the
    /// playhead).
    Separate,
    TrimToSelection,
    /// Delete the selection range on the selected tracks.
    ClearRange,
    CopyRange,
    CutRange,
    /// Paste the copied range at the playhead.
    PasteRange,
    /// Copies right after the selection (1 = duplicate).
    RepeatRange(u32),
    InsertSilence,
    Nudge {
        forward: bool,
        target: NudgeTarget,
    },
    TrimClip {
        clip: ClipId,
        edge: ClipEdge,
        to: MusicalTime,
    },
    /// Fade lengths in project frames.
    SetClipFades {
        clip: ClipId,
        fade_in: i64,
        fade_out: i64,
    },
    SetClipGain {
        clip: ClipId,
        db: f32,
    },
    SpotClip {
        clip: ClipId,
        start: MusicalTime,
    },
    ShuffleClip {
        clip: ClipId,
        track: TrackId,
        at: MusicalTime,
    },
    /// To the next (previous) clip boundary or transient; `extend` grows
    /// the selection.
    TabTo {
        forward: bool,
        extend: bool,
    },
    /// Move clips by `by` (ticks) and `tracks` lanes from where the gesture
    /// started (all of them, or none, change track).
    MoveClips {
        clips: Vec<ClipId>,
        by: i64,
        tracks: i32,
    },
    /// Move one edge of clips by `by` ticks (`stretch`: time-compress or
    /// expand instead of trimming).
    TrimClips {
        clips: Vec<ClipId>,
        edge: ClipEdge,
        by: i64,
        stretch: bool,
    },
    /// Time-compress/expand a clip by moving an edge to `to`.
    StretchClip {
        clip: ClipId,
        edge: ClipEdge,
        to: MusicalTime,
    },
    /// Add `delta_db` to the clips' gain (from the gesture start).
    ClipGain {
        clips: Vec<ClipId>,
        delta_db: f32,
    },
    /// Set the clips' gain.
    SetClipsGain {
        clips: Vec<ClipId>,
        db: f32,
    },
    /// Fade length (project frames), shape or drawn bend (percent) of the
    /// clips' fade-ins or fade-outs.
    SetFade {
        clips: Vec<ClipId>,
        edge: ClipEdge,
        length: Option<i64>,
        shape: Option<faderframe_project::FadeShape>,
        bend: Option<i16>,
    },
    SetClipsMuted {
        clips: Vec<ClipId>,
        muted: bool,
    },
    /// Pin source frame `source` of an audio clip at clip-relative output
    /// frame `to` (adds a warp marker).
    WarpTo {
        clip: ClipId,
        source: i64,
        to: i64,
        drag: warping::WarpDrag,
    },
    RemoveWarpMarker {
        clip: ClipId,
        source: i64,
    },
    QuantizeWarp(Vec<ClipId>),
    ClearWarp(Vec<ClipId>),
    SetWarpAlgorithm {
        clips: Vec<ClipId>,
        algorithm: faderframe_project::WarpAlgorithm,
    },
    SeparateAtTransients(Vec<ClipId>),
    ToggleFollowPlayhead,
}

/// Editing preferences shared by the arranger and the piano roll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorSettings {
    pub grid: GridDivision,
    /// Grid mode is on (kept in step with `edit_mode`).
    pub snap: bool,
    pub follow_playhead: bool,
    /// Scale, chords, note length, audition … of the piano roll.
    pub piano: PianoRollSettings,
    pub edit_mode: EditMode,
    pub grid_mode: GridMode,
    pub tool: EditTool,
    pub nudge: NudgeValue,
    pub tab_to_transients: bool,
    pub link_timeline: bool,
    pub insertion_follows_playback: bool,
    pub show_transients: bool,
    pub warp: bool,
    pub show_edit_toolbar: bool,
    pub counter_unit: CounterUnit,
    /// Transient detection sensitivity, 0–1 (more transients when higher).
    pub transient_sensitivity: f32,
    /// The latest zoom request and its sequence number (views apply each
    /// request once).
    pub zoom_request: (u64, ZoomRequest),
    /// Lanes under the arranger's ruler.
    pub lanes: lanes::GlobalLanes,
    /// How Quantize moves notes and transients (its grid is the edit
    /// grid's).
    pub quantize: QuantizeSettings,
    /// How far Humanize moves notes and transients.
    pub humanize: groove::HumanizeSettings,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            grid: GridDivision::Beat,
            snap: true,
            follow_playhead: true,
            piano: PianoRollSettings::default(),
            edit_mode: EditMode::Grid,
            grid_mode: GridMode::Absolute,
            tool: EditTool::Smart,
            nudge: NudgeValue::default(),
            tab_to_transients: false,
            link_timeline: true,
            insertion_follows_playback: true,
            show_transients: false,
            warp: false,
            show_edit_toolbar: false,
            counter_unit: CounterUnit::BarsBeats,
            transient_sensitivity: 0.5,
            zoom_request: (0, ZoomRequest::Fit),
            lanes: lanes::GlobalLanes::default(),
            quantize: QuantizeSettings::default(),
            humanize: groove::HumanizeSettings::default(),
        }
    }
}

impl EditorSettings {
    /// The quantize settings with the edit grid.
    pub fn quantize_settings(&self) -> QuantizeSettings {
        QuantizeSettings {
            grid: self.grid,
            ..self.quantize
        }
    }

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
    /// Threads processing the graph, the audio thread included (`None`:
    /// one per core; 1: the audio thread alone).
    pub threads: Option<u16>,
}

/// A recording in progress (or whose writer is finishing).
struct ActiveRecording {
    /// Audio takes (none when only MIDI tracks are armed).
    writer: Option<record::RecordWriter>,
    /// MIDI takes.
    midi: Option<midi::MidiTake>,
    from: i64,
    to: i64,
    tracks: Vec<TrackId>,
    /// Input + output latency compensation in frames.
    latency: i64,
    /// The transport has reported recording at least once.
    seen: bool,
    /// Recording into a clip-launcher slot (one track) instead of the
    /// arrangement.
    slot: Option<launcher::SlotRecording>,
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
    /// A position shown before the engine got there (a locate it applies
    /// later, e.g. once render-ahead has primed it): the target, the
    /// engine's jumps when it was asked, and when.
    shown_position: Option<(i64, u32, Instant)>,
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
    /// DSP worker threads shared by this session's engines (created when
    /// audio starts).
    pool: Option<Arc<faderframe_realtime::WorkerPool>>,
    /// Following an external MIDI clock / MTC.
    sync: sync::SyncState,
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
    sample_jobs: Vec<samples::SampleJob>,
    pub record: RecordSettings,
    recording: Option<ActiveRecording>,
    finishing: Vec<ActiveRecording>,
    /// Take folders whose take lanes are shown.
    open_takes: HashSet<ClipId>,
    preset_dir: PathBuf,
    presets: Vec<PresetEntry>,
    automation_writer: automation::AutomationWriter,
    ui_requests: Vec<UiRequest>,
    /// Device editors' own settings ([`Action::SetDeviceView`]).
    device_views: HashMap<(faderframe_core::PluginInstanceId, String), f64>,
    /// Notes copied in the piano roll (relative to the earliest).
    note_clipboard: Vec<MidiNote>,
    range_clipboard: editing::RangeClipboard,
    /// Where playback last started (for "insertion follows playback" off).
    play_started_at: Option<i64>,
    transients: transients::TransientCache,
    pitch: pitch::PitchCache,
    clip_analyses: detect::ClipAnalyses,
    conversions: to_midi::Conversions,
    clip_fx: clip_fx::ClipFxState,
    speech: speech::SpeechState,
    launcher: launcher::LauncherState,
    control: control::ControlState,
    /// The audio clip the pitch editor shows.
    pitch_clip: Option<ClipId>,
    /// Track renders for freezing and bouncing.
    bounces: Vec<freeze::PendingBounce>,
    /// The mono check is on (listening only).
    mono_check: bool,
    /// The headphone correction's file.
    correction_path: Option<std::path::PathBuf>,
    /// ADM BWF files being read.
    adm_imports: Vec<adm::ImportJob>,
    samplings: Vec<sampling::PendingSample>,
    video: video::VideoState,
    /// What the live tracks were played, for Capture MIDI.
    capture: capture::CaptureBuffer,
    /// Album analyses and the running album job.
    album_state: album::AlbumState,
    ddp_state: ddp::DdpState,
    /// Plugin failures noticed, sandboxed plugins' unsaved state.
    plugin_care: sandbox::PluginCare,
    /// Render tracks nobody plays live this far ahead (`None`: off).
    render_ahead: Option<std::time::Duration>,
    /// Render buses ahead too (see [`Session::set_render_ahead_buses`]).
    render_ahead_buses: bool,
    /// Tracks with a plugin editor open.
    edited_tracks: std::collections::HashSet<TrackId>,
    /// Selected programs whose new state is not recorded yet.
    pending_programs: Vec<programs::PendingProgram>,
    /// The Tools view's meters.
    analysis: analysis::AnalysisState,
    /// Where tracks following a multi-track fader/pan/send move started
    /// (for the running gesture).
    follow_base: HashMap<groups::FollowKey, f32>,
    /// The edit being applied comes from the user (`Action::Edit`): other
    /// selected tracks follow it (mapped controllers and automation don't
    /// move the selection).
    user_edit: bool,
    /// A modulator mapping (see [`modulators`]).
    mod_learn: Option<modulators::ModLearn>,
    /// Clips as the running gesture first saw them (drags recompute from
    /// these).
    gesture_base: HashMap<ClipId, faderframe_project::Clip>,
    /// Expression of the clipboard's notes (by their original ids).
    note_clipboard_expressions: Vec<faderframe_project::NoteExpression>,
    perf: performance::PerformanceMonitor,
    midi: midi::MidiState,
}

/// Where a plugin chosen in the browser goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginTarget {
    /// Insert slot (index in the insert chain).
    Insert(usize),
    Instrument,
    /// The end of an album song's inserts (the track is ignored).
    Song(faderframe_core::SongId),
}

/// Things views ask the toolkit shell to show (polled every frame).
#[derive(Clone, Debug, PartialEq)]
pub enum UiRequest {
    PluginBrowser {
        track: TrackId,
        target: PluginTarget,
    },
    PluginEditor {
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        generic: bool,
    },
    /// Pick a `.syx` file and add its messages to `clip` at `at`.
    ImportSysex { clip: ClipId, at: MusicalTime },
    /// Edit the release's details (`None`) or a song's: titles, credits,
    /// UPC/EAN and ISRC.
    AlbumDetails(Option<faderframe_core::SongId>),
    /// Ask for a name and save the plugin's settings as a preset.
    SavePluginPreset {
        plugin: faderframe_core::PluginInstanceId,
    },
    /// Ask for names and save the tracks as track presets.
    SaveTrackPreset { tracks: Vec<TrackId> },
    /// Ask whether to delete the user preset at `path` (named `name`).
    DeletePreset { path: PathBuf, name: String },
    /// Ask for a group's new name.
    RenameGroup(faderframe_core::GroupId),
    /// Pick a colour (track or section).
    PickColor(ColorTarget),
    /// Ask for a version's name, then save it.
    SaveVersion,
    /// The project's versions (compare, restore, save another).
    Versions,
    /// Ask for a template's name, then save it.
    SaveTemplate,
    /// The templates (new project from one, delete, default).
    Templates,
    /// Ask where to save a sample of `track` from `start` to `end`
    /// (suggesting `name`), then make it there.
    SaveSample {
        track: TrackId,
        start: MusicalTime,
        end: MusicalTime,
        name: String,
    },
}

/// What a picked colour is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTarget {
    Track(TrackId),
    Section(faderframe_core::SectionId),
}

/// One parameter of a hosted plugin, for generic editors.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginParameterView {
    pub info: faderframe_plugin_host::ParameterInfo,
    /// Current value (plain units).
    pub value: f64,
    /// Explicitly stored in the project slot.
    pub explicit: bool,
}

/// A plugin offered in insert/instrument menus.
#[derive(Clone, Debug, PartialEq)]
pub struct AvailablePlugin {
    pub plugin: PluginRef,
    pub vendor: String,
    pub version: String,
    pub instrument: bool,
    /// Notes in, notes out, no audio: it plays before the instrument.
    pub midi_effect: bool,
    /// Channels of the main audio ports.
    pub audio_inputs: u16,
    pub audio_outputs: u16,
    pub note_inputs: u16,
}

/// Does this command remove plugin slots (directly or with a track)?
fn removes_plugins(cmd: &Command) -> bool {
    match cmd {
        Command::RemovePlugin { .. }
        | Command::RemoveTrack { .. }
        | Command::SetPreamp { .. }
        | Command::SetInstrument { .. } => true,
        Command::Batch { commands, .. } => commands.iter().any(removes_plugins),
        _ => false,
    }
}

/// One entry of a track's input menu.
#[derive(Clone, Debug, PartialEq)]
pub struct InputChoice {
    pub label: String,
    pub action: Action,
    pub checked: bool,
    /// Draw a separator before this entry.
    pub group_start: bool,
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
        let (mut engine, processor) =
            faderframe_engine::create_with_epoch(config, Arc::clone(&epoch));
        let (midi, midi_queue) = midi::MidiState::new();
        engine.set_midi_input(midi_queue)?;
        engine.set_midi_output(midi.renew_output_queue())?;
        engine.set_midi_ports(midi.port_map());
        engine.set_midi_output_ports(midi.output_port_map());
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
            shown_position: None,
            metrics: MetricsSnapshot::default(),
            last_metrics: Instant::now(),
            path: None,
            saved_revision: 0,
            revision: 0,
            layout_revision: 0,
            notices: VecDeque::new(),
            epoch,
            pool: None,
            sync: sync::SyncState::default(),
            loader,
            media_dir: media::new_unsaved_media_dir(),
            unsaved_media: true,
            media_moves: HashMap::new(),
            missing: HashSet::new(),
            imports: Vec::new(),
            peak_jobs: Vec::new(),
            sample_jobs: Vec::new(),
            record: RecordSettings::default(),
            recording: None,
            finishing: Vec::new(),
            open_takes: HashSet::new(),
            preset_dir: media::data_dir().join("track-presets"),
            presets: Vec::new(),
            automation_writer: Default::default(),
            ui_requests: Vec::new(),
            device_views: HashMap::new(),
            note_clipboard: Vec::new(),
            range_clipboard: editing::RangeClipboard::default(),
            play_started_at: None,
            transients: transients::TransientCache::default(),
            pitch: pitch::PitchCache::default(),
            clip_analyses: detect::ClipAnalyses::default(),
            conversions: to_midi::Conversions::default(),
            clip_fx: clip_fx::ClipFxState::default(),
            speech: speech::SpeechState::default(),
            launcher: launcher::LauncherState::default(),
            control: control::ControlState::default(),
            pitch_clip: None,
            gesture_base: HashMap::new(),
            bounces: Vec::new(),
            mono_check: false,
            correction_path: None,
            adm_imports: Vec::new(),
            samplings: Vec::new(),
            video: video::VideoState::default(),
            capture: capture::CaptureBuffer::default(),
            album_state: album::AlbumState::default(),
            ddp_state: ddp::DdpState::default(),
            plugin_care: sandbox::PluginCare::default(),
            render_ahead: None,
            render_ahead_buses: false,
            edited_tracks: Default::default(),
            pending_programs: Vec::new(),
            analysis: analysis::AnalysisState::new(config.sample_rate),
            follow_base: HashMap::new(),
            user_edit: false,
            mod_learn: None,
            note_clipboard_expressions: Vec::new(),
            perf: Default::default(),
            midi,
        };
        s.rescan_track_presets();
        s.render_sources();
        s.engine.sync(&s.project, &s.sources, Impact::Graph)?;
        s.update_loader();
        s.apply_analysis_source();
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

    /// The master strip is shown at the window's right edge (the active
    /// workspace's layout).
    pub fn master_panel(&self) -> bool {
        self.workspace.active().layout.master_panel
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

    /// Show `position` as the playhead's now, and until the engine has
    /// applied the locate that moves it there.
    pub(crate) fn show_position(&mut self, position: i64) {
        self.transport.position = position;
        self.shown_position = Some((position, self.transport.jumps, Instant::now()));
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

    /// The audio clip the pitch editor shows.
    pub fn pitch_clip(&self) -> Option<ClipId> {
        self.pitch_clip.filter(|c| {
            self.project
                .clip(*c)
                .is_some_and(|c| c.as_audio().is_some())
        })
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

    /// The undo history: the steps done (oldest first) and those that can
    /// be redone (the next first).
    pub fn history_steps(&self) -> (Vec<String>, Vec<String>) {
        let own = |v: Vec<&str>| v.into_iter().map(String::from).collect();
        (
            own(self.history.undo_labels()),
            own(self.history.redo_labels()),
        )
    }

    /// Undo or redo until `steps` are done, syncing the engine once.
    fn history_to(&mut self, steps: usize) -> Result<()> {
        let mut impact = faderframe_project::Impact::None;
        let mut moved = 0usize;
        loop {
            let done = self.history.undo_labels().len();
            let r = if done > steps {
                self.history.undo(&mut self.project)?
            } else if done < steps {
                self.history.redo(&mut self.project)?
            } else {
                break;
            };
            let Some(r) = r else { break };
            impact = impact.max(r.impact);
            moved += 1;
        }
        if moved > 0 {
            self.sync(impact)?;
            let label = self
                .history
                .undo_labels()
                .last()
                .map_or_else(|| "the start".to_string(), |l| format!("‘{l}’"));
            self.notify(NoticeLevel::Info, format!("history: back to {label}"));
        }
        Ok(())
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
            // A locate the engine has not applied yet: painted until it has.
            || self.shown_position.is_some()
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

    /// Insert slots per mixer strip.
    pub fn mixer_insert_slots(&self) -> usize {
        self.workspace
            .mixer_insert_slots
            .unwrap_or(faderframe_workspace::DEFAULT_INSERT_SLOTS) as usize
    }

    /// Arranger track header width (`None`: the theme default).
    pub fn header_width(&self) -> Option<f32> {
        self.workspace.header_width
    }

    /// Arranger height of a track (`None`: the theme default).
    pub fn track_height(&self, track: TrackId) -> Option<f32> {
        self.workspace.track_height(track)
    }

    /// A mixer strip's width, if not the theme's.
    pub fn strip_width(&self, track: TrackId) -> Option<f32> {
        self.workspace.strip_width(track)
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
        self.capture_plugin_states();
        let position = self.transport.position;
        self.engine_config.sample_rate = self.engine.sample_rate();
        let (engine, processor) =
            faderframe_engine::create_with_epoch(self.engine_config, Arc::clone(&self.epoch));
        self.engine = engine;
        self.engine.set_midi_input(self.midi.renew_queue())?;
        self.engine
            .set_midi_output(self.midi.renew_output_queue())?;
        self.engine.set_midi_ports(self.midi.port_map());
        self.engine
            .set_midi_output_ports(self.midi.output_port_map());
        self.engine.set_midi_live(self.midi.live().clone());
        self.engine.set_render_ahead_buses(self.render_ahead_buses);
        self.engine
            .set_render_ahead(self.render_ahead, render_ahead_threads());
        self.engine.set_edited_tracks(self.edited_tracks.clone());
        self.midi.reset_engine_tables();
        self.engine
            .midi_shared()
            .clock_ports
            .store(self.midi.clock_mask(), std::sync::atomic::Ordering::Relaxed);
        self.engine
            .midi_shared()
            .mtc_ports
            .store(self.midi.mtc_mask(), std::sync::atomic::Ordering::Relaxed);
        {
            let s = &self.sync.settings;
            let rate = s.mtc_out_rate;
            self.engine
                .midi_shared()
                .set_mtc(rate, s.mtc_out_offset.total_frames(rate));
        }
        self.engine
            .sync(&self.project, &self.sources, Impact::Graph)?;
        self.engine.transport(TransportCommand::Locate(position))?;
        self.pending = Some(processor);
        self.update_loader();
        self.apply_analysis_source();
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
            let Some(mut processor) = self.pending.take() else {
                continue;
            };
            processor.set_worker_pool(self.worker_pool(prefs.threads));
            let config = StreamConfig {
                sample_rate: prefs.sample_rate,
                buffer_size: prefs.buffer_size,
                ..StreamConfig::default()
            };
            match backend.open_stream(config, Box::new(processor)) {
                Ok(stream) => {
                    let info = stream.info();
                    // macOS: the workers join the device's audio workgroup,
                    // and so do sandboxed plugins' audio threads.
                    let workgroup = stream.io_workgroup();
                    faderframe_realtime::set_process_workgroup(workgroup.clone());
                    if let Some(pool) = &self.pool {
                        pool.set_workgroup(workgroup);
                    }
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

    /// The worker pool for `threads` processing threads in total (reused
    /// while the count stays the same).
    fn worker_pool(
        &mut self,
        threads: Option<u16>,
    ) -> Option<Arc<faderframe_realtime::WorkerPool>> {
        let workers = match threads {
            Some(t) => (t as usize).saturating_sub(1),
            None => faderframe_realtime::default_worker_count(),
        };
        if workers == 0 {
            self.pool = None;
            return None;
        }
        if self.pool.as_ref().is_none_or(|p| p.threads() != workers) {
            self.pool = Some(Arc::new(faderframe_realtime::WorkerPool::new(
                faderframe_realtime::PoolConfig::new(workers),
            )));
        }
        self.pool.clone()
    }

    /// Render tracks nobody plays live this far ahead of the playhead, on
    /// threads of their own (`None`: everything on the audio thread). See
    /// `faderframe_engine::ahead`.
    pub fn set_render_ahead(&mut self, lookahead: Option<std::time::Duration>) -> Result<()> {
        if lookahead == self.render_ahead {
            return Ok(());
        }
        self.render_ahead = lookahead;
        self.engine
            .set_render_ahead(lookahead, render_ahead_threads());
        self.sync(Impact::Graph)
    }

    pub fn render_ahead(&self) -> Option<std::time::Duration> {
        self.render_ahead
    }

    /// Render buses (auxes, the master's devices) ahead too, when
    /// everything that reaches them can be rendered ahead: their devices
    /// leave the audio thread. They and the strips reaching them run a
    /// device callback and two small blocks ahead (about 10 ms), so moving
    /// those faders stays immediate; automation is exact, meters in time.
    /// The mono check: what is heard summed to both speakers (listening
    /// only: renders stay as mixed).
    pub fn set_mono_check(&mut self, on: bool) -> Result<()> {
        self.engine.set_mono_check(on)?;
        self.mono_check = on;
        self.revision += 1;
        Ok(())
    }

    pub fn mono_check(&self) -> bool {
        self.mono_check
    }

    /// Listen on headphones: the master rendered binaurally with `room`
    /// (`None`: speakers). Listening only.
    pub fn set_headphones(&mut self, room: Option<faderframe_binaural::Room>) -> Result<()> {
        if room == self.engine.binaural() {
            return Ok(());
        }
        self.engine.set_binaural(room);
        self.sync(Impact::Graph)?;
        self.revision += 1;
        Ok(())
    }

    pub fn headphones(&self) -> Option<faderframe_binaural::Room> {
        self.engine.binaural()
    }

    pub fn set_render_ahead_buses(&mut self, on: bool) -> Result<()> {
        if on == self.render_ahead_buses {
            return Ok(());
        }
        self.render_ahead_buses = on;
        self.engine.set_render_ahead_buses(on);
        self.sync(Impact::Graph)
    }

    pub fn render_ahead_buses(&self) -> bool {
        self.render_ahead_buses
    }

    /// The plugins whose editors are open (the UI says so every tick): their
    /// tracks play on the audio thread while rendering ahead, so what is
    /// turned is heard at once.
    pub fn set_plugins_being_edited(
        &mut self,
        plugins: &std::collections::HashSet<faderframe_core::PluginInstanceId>,
    ) {
        if self.render_ahead.is_none() {
            return;
        }
        let tracks: std::collections::HashSet<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| {
                t.inserts
                    .iter()
                    .chain(t.instrument.iter())
                    .chain(t.preamp.iter())
                    .any(|s| plugins.contains(&s.id))
            })
            .map(|t| t.id)
            .collect();
        if tracks == self.edited_tracks {
            return;
        }
        self.edited_tracks = tracks.clone();
        self.engine.set_edited_tracks(tracks);
        if let Err(e) = self.engine.update_params(&self.project) {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    /// Tracks rendered ahead now, and blocks in which their audio was late.
    pub fn render_ahead_status(&self) -> (usize, u64) {
        (self.engine.ahead_tracks().len(), self.engine.ahead_misses())
    }

    /// Tracks whose strips are rendered ahead now (they reach buses
    /// rendered ahead).
    pub fn render_ahead_strips(&self) -> usize {
        self.engine.ahead_strips().len()
    }

    /// Threads processing the graph (the audio thread plus workers).
    pub fn processing_threads(&self) -> usize {
        1 + self.pool.as_ref().map_or(0, |p| p.threads())
    }

    /// Worker threads that could not get the audio thread's realtime
    /// priority (no permission): they still work, but may be preempted.
    pub fn worker_priority_failures(&self) -> u64 {
        self.pool.as_ref().map_or(0, |p| p.priority_failures())
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
        &mut self,
        mut settings: render::RenderSettings,
    ) -> std::result::Result<render::RenderJob, render::RenderError> {
        if matches!(settings.channels, render::RenderChannels::Binaural(_))
            && settings.head.is_none()
        {
            settings.head = Some(self.head().clone());
        }
        render::start(self.render_copy(), settings)
    }

    /// The project as a render needs it: plugin states captured (the render
    /// thread creates its own instances from the slots), media paths
    /// absolute.
    pub(crate) fn render_copy(&mut self) -> Project {
        self.capture_plugin_states();
        let mut project = self.project.clone();
        let dir = self.project_dir();
        for s in project.sources.values_mut() {
            if let SourceSpec::File { path, .. } = &mut s.spec {
                *path = self.resolve_media(path, dir.as_deref());
            }
        }
        project
    }

    /// Close the stream (the engine processor goes with it).
    pub fn stop_audio(&mut self) {
        self.audio = None;
        faderframe_realtime::set_process_workgroup(None);
        if let Some(pool) = &self.pool {
            pool.set_workgroup(None);
        }
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
        self.poll_bounces();
        self.poll_adm_imports();
        self.poll_samples();
        self.poll_video();
        self.poll_album();
        self.poll_ddp();
        self.poll_analysis(dt);
        self.pump_idle();
        self.poll_recording();
        self.engine.collect_garbage();
        let was_playing = self.transport.playing;
        self.transport = self.engine.transport_snapshot();
        // A locate the engine has not applied yet (render-ahead primes the
        // new position first) stays shown, not the position it left.
        if let Some((target, jumps, at)) = self.shown_position {
            let arrived = self.transport.jumps != jumps || self.transport.position == target;
            if arrived || at.elapsed() > std::time::Duration::from_secs(2) {
                self.shown_position = None;
            } else {
                self.transport.position = target;
            }
        }
        if !was_playing && self.transport.playing {
            self.automation_play_requested();
        }
        self.poll_launcher(was_playing);
        self.poll_slot_recording();
        if self.album_state.is_playing() {
            // The project started playing: it has the outputs back.
            if !was_playing && self.transport.playing {
                self.album_stop_playing();
            }
            // The album's playhead moves.
            self.revision += 1;
        }
        if self.ddp_state.is_playing() {
            // As the album: the project takes the outputs back.
            if !was_playing && self.transport.playing {
                self.ddp_stop();
            }
            self.revision += 1;
        }
        let plugin_poll = self.engine.poll_plugins();
        if plugin_poll.restart {
            // Latency or ports changed: rebuild (re-activates the plugin).
            if let Err(e) = self.sync(Impact::Graph) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        } else if plugin_poll.params_changed {
            self.revision += 1;
        }
        self.care_for_plugins();
        self.tick_programs();
        // Moves in plugins' own editors write automation like ours do.
        let edits = self.engine.take_plugin_edits();
        self.plugin_editor_edits(edits);
        if was_playing && !self.transport.playing {
            // Stopped (by the user, the end of a bounce, a dropped stream):
            // Latch/Write automation ends here.
            self.automation_play_stopped();
        }
        // Stopped: tracks kept on the audio thread while playing go back to
        // being rendered ahead.
        if !self.transport.playing
            && self.engine.ahead_wants_rebuild(&self.project)
            && let Err(e) = self.sync(Impact::Graph)
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
        // The device's callbacks changed size: the preamps buffer one, the
        // buses rendered ahead keep one ahead. Its outputs changed: beds
        // fold down to them.
        let beds = || {
            self.project
                .tracks
                .iter()
                .any(|t| matches!(t.layout, faderframe_core::ChannelLayout::Surround(_)))
        };
        if ((self.engine.device_block_changed()
            && (self.render_ahead_buses || self.project.tracks.iter().any(|t| t.preamp.is_some())))
            || (self.engine.device_outputs_changed() && beds()))
            && let Err(e) = self.sync(Impact::Graph)
        {
            self.notify(NoticeLevel::Error, e.to_string());
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
        self.tick_performance();
        self.tick_midi();
        self.tick_mtc_out();
        // A manual varispeed waits for the stream.
        if self.manual_speed().is_some() && !self.engine.varispeed() {
            self.apply_manual_speed();
        }
        self.tick_control();
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
        // Generated sources added by an edit (or restored by undo).
        let rate = self.engine.sample_rate();
        let new_generated: Vec<(AudioSourceId, faderframe_audio_files::GeneratorSpec)> = self
            .project
            .sources
            .values()
            .filter(|s| !self.sources.contains_key(&s.id))
            .filter_map(|s| match &s.spec {
                SourceSpec::Generated { generator } => Some((s.id, generator.clone())),
                SourceSpec::File { .. } => None,
            })
            .collect();
        for (id, generator) in new_generated {
            let data = Arc::new(faderframe_audio_files::generate(&generator, rate));
            self.peaks.insert(id, Arc::new(PeakCache::build(&data)));
            self.sources.insert(id, Source::Memory(data));
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
        self.poll_transients();
        self.poll_pitch();
        self.poll_clip_analyses();
        self.poll_conversions();
        self.poll_clip_fx();
        self.poll_speech();
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
        while i < self.sample_jobs.len() {
            if self.sample_jobs[i].is_finished() {
                let job = self.sample_jobs.swap_remove(i);
                if let Some(loaded) = job.join()
                    && let Err(e) = self.finish_samples(loaded)
                {
                    self.notify(NoticeLevel::Error, format!("loading samples failed: {e}"));
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

    /// Start loading samples into a sampler (or clear one of its slots).
    fn load_device_samples(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        slot: usize,
        files: Vec<PathBuf>,
    ) -> Result<()> {
        let Some((_, owner)) = self.plugin_owner(plugin) else {
            return Err(SessionError::Other("no such plugin".into()));
        };
        if files.is_empty() {
            let mut doc = samples::doc_of(owner);
            doc.set(slot, None);
            return self.apply_samples(plugin, &doc);
        }
        let media = self.media_dir.clone();
        self.sample_jobs
            .push(samples::SampleJob::spawn(plugin, slot, files, media));
        self.revision += 1;
        Ok(())
    }

    /// Whether samples are being loaded for `plugin`.
    pub fn loading_samples(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.sample_jobs.iter().any(|j| j.plugin == plugin)
    }

    fn finish_samples(&mut self, loaded: samples::Loaded) -> Result<()> {
        for n in &loaded.notes {
            self.notify(NoticeLevel::Warning, n.clone());
        }
        // Into the document as it is now (other loads may have finished
        // since this one started).
        let Some((_, owner)) = self.plugin_owner(loaded.plugin) else {
            return Ok(());
        };
        let mut doc = samples::doc_of(owner);
        for (slot, file) in &loaded.assigned {
            doc.set(*slot, Some(file.clone()));
        }
        if loaded.assigned.is_empty() {
            return Ok(());
        }
        // The decoded samples stay in the cache (held here) until the
        // instance has loaded them.
        let held = Arc::clone(&loaded.set);
        let r = self.apply_samples(loaded.plugin, &doc);
        drop(held);
        r
    }

    /// The edit giving a sampler `doc` (its parameters as they are).
    fn apply_samples(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        doc: &faderframe_plugin_host::devices::samples::SampleDoc,
    ) -> Result<()> {
        let Some((track, owner)) = self.plugin_owner(plugin) else {
            return Ok(());
        };
        let parameters = owner.parameters.clone();
        let params = match self.plugin_tap(plugin) {
            Some(tap) => tap.params.save(),
            None => owner
                .state
                .as_deref()
                .and_then(faderframe_engine::decode_state)
                .and_then(|b| {
                    faderframe_plugin_host::devices::samples::unpack(&b).map(|(p, _)| p.to_vec())
                })
                .unwrap_or_default(),
        };
        let state = faderframe_engine::encode_state(
            &faderframe_plugin_host::devices::samples::pack(&params, doc),
        );
        self.edit(Command::Batch {
            label: "Load Samples".into(),
            commands: vec![Command::SetPluginState {
                track,
                plugin,
                state: Some(state),
                parameters,
            }],
        })
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
                    warp: None,
                    pitch: None,
                    effects: None,
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
        self.forget_video();
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
        self.reset_launcher()?;
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
        samples::map_states(&mut project, |p| media::resolve(p, dir.as_deref()));
        for v in project.video.sources.values_mut() {
            v.path = media::resolve(&v.path, dir.as_deref());
        }
        let mut adopted = Vec::new();
        for t in &mut project.tracks {
            adopted.extend(self.adopt_instrument(t));
        }
        self.discard_unsaved_media();
        self.path = Some(path.clone());
        self.media_dir = dir.unwrap_or_default().join(MEDIA_FOLDER);
        self.unsaved_media = false;
        self.replace_project(project, loaded.workspace)?;
        for n in notes.iter().chain(&adopted) {
            self.notify(NoticeLevel::Warning, n.clone());
        }
        self.notify(NoticeLevel::Info, format!("opened {}", path.display()));
        Ok(notes.into_iter().chain(adopted).collect())
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
        self.capture_plugin_states();
        let mut stored = self.project.clone();
        for s in stored.sources.values_mut() {
            if let SourceSpec::File { path: p, .. } = &mut s.spec {
                *p = media::to_stored(p, Some(&dir));
            }
        }
        samples::map_states(&mut stored, |p| media::to_stored(p, Some(&dir)));
        for v in stored.video.sources.values_mut() {
            v.path = media::to_stored(&v.path, Some(&dir));
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
        match samples::move_folder(&self.media_dir, to) {
            Ok(moves) => self.media_moves.extend(moves),
            Err(err) => self.notify(
                NoticeLevel::Error,
                format!("could not move the samples into the project: {err}"),
            ),
        }
        for s in self.project.sources.values_mut() {
            if let SourceSpec::File { path, .. } = &mut s.spec
                && let Some(new) = self.media_moves.get(path.as_path())
            {
                *path = new.clone();
            }
        }
        // Album songs made in the media folder (an imported DDP's tracks).
        remap_album_files(&mut self.project, &self.media_moves);
        if self.remap_sample_paths()
            && let Err(e) = self.sync(Impact::Params)
        {
            self.notify(NoticeLevel::Error, e.to_string());
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

    /// Re-point samplers at samples a save moved (also after undo or redo
    /// brings back a state from before the save). Returns whether any
    /// changed (a `Params` sync makes the engine reload them).
    fn remap_sample_paths(&mut self) -> bool {
        if self.media_moves.is_empty() {
            return false;
        }
        remap_album_files(&mut self.project, &self.media_moves);
        let moves = &self.media_moves;
        samples::map_states(&mut self.project, |p| {
            moves.get(p).cloned().unwrap_or_else(|| p.to_path_buf())
        })
    }

    fn sync(&mut self, mut impact: Impact) -> Result<()> {
        if impact >= Impact::Params {
            self.remap_sample_paths();
        }
        // The monitored album song is gone (removed, undone): stop hearing
        // its inserts.
        if self
            .engine
            .album_monitor()
            .is_some_and(|id| self.project.album.song(id).is_none())
        {
            self.engine.set_album_monitor(None);
            impact = Impact::Graph;
        }
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
        if let Some(name) = self.frozen_target(&cmd) {
            return Err(SessionError::Other(format!(
                "'{name}' is frozen: unfreeze it to edit it"
            )));
        }
        // Linked group members and other selected tracks follow (one undo
        // step).
        let linked = self.group_edits(&cmd);
        self.capture_automation(&cmd);
        for c in &linked {
            self.capture_automation(c);
        }
        let cmd = if linked.is_empty() {
            cmd
        } else {
            Command::Batch {
                label: cmd.label(),
                commands: std::iter::once(cmd).chain(linked).collect(),
            }
        };
        // Removing a folder keeps its tracks.
        let cmd = self.keep_folder_contents(cmd);
        // Splitting an alias makes it its own; content edits reach the
        // others (one undo step).
        let cmd = self.unlink_split_aliases(cmd);
        let aliases = self.aliases_before(&cmd);
        if removes_plugins(&cmd) {
            // Undo restores removed plugins from their slots: keep the
            // slots' state current.
            self.capture_plugin_states();
        }
        if aliases.is_empty() {
            let impact = self.history.apply(&mut self.project, cmd)?;
            return self.sync(impact);
        }
        self.history.begin(cmd.label());
        let impact = match self.history.apply(&mut self.project, cmd) {
            Ok(i) => self.mirror_aliases(aliases).map(|m| m.max(i)),
            Err(e) => Err(e.into()),
        };
        self.history.end();
        self.sync(impact?)
    }

    pub fn dispatch(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Edit(cmd) => {
                // Mapping a modulator: the touched parameter is taken.
                let mapped = match &cmd {
                    Command::SetPluginParameter {
                        plugin, parameter, ..
                    } if self.mod_learn.is_some() => self.map_touched(*plugin, *parameter)?,
                    _ => false,
                };
                if !mapped {
                    self.user_edit = true;
                    let r = self.edit(cmd);
                    self.user_edit = false;
                    r?;
                }
            }
            Action::BeginGesture(label) => self.history.begin(label),
            Action::CancelGesture => {
                let impact = self.history.cancel(&mut self.project)?;
                self.gesture_base.clear();
                self.follow_base.clear();
                self.sync(impact)?;
                self.revision += 1;
            }
            Action::EndGesture => {
                self.mapping_gesture_ended();
                self.history.end();
                self.gesture_base.clear();
                self.follow_base.clear();
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
            Action::HistoryTo(steps) => self.history_to(steps)?,
            Action::AddModulator { track, source } => {
                self.add_modulator(track, source)?;
            }
            Action::RemoveModulator { track, modulator } => {
                self.remove_modulator(track, modulator)?;
            }
            Action::SetModulator { track, modulator } => self.set_modulator(track, modulator)?,
            Action::LearnModulation(learn) => self.learn_modulation(learn),
            Action::AddChain { track, container } => self.add_chain(track, container)?,
            Action::RenameChain {
                track,
                container,
                chain,
                name,
            } => self.rename_chain(track, container, chain, name)?,
            Action::RemoveChain {
                track,
                container,
                chain,
            } => self.remove_chain(track, container, chain)?,
            Action::InsertIntoChain {
                track,
                container,
                chain,
                index,
                plugin,
            } => {
                self.insert_into_chain(track, container, chain, index, plugin)?;
            }
            Action::RemoveFromChain { track, plugin } => self.remove_from_chain(track, plugin)?,
            Action::SetChainKeys {
                track,
                container,
                chain,
                low,
                high,
            } => self.set_chain_keys(track, container, chain, low, high)?,
            Action::Transport(t) => {
                self.transport_action(t)?;
                self.pump_idle();
                // Locates must not wait for the loader's next poll.
                self.loader.wake();
            }
            Action::Workspace(w) => self.workspace_action(w)?,
            Action::SelectTracks { tracks, mode } => {
                let tracks = self.with_group_selection(&tracks);
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
            Action::DeleteSelection => {
                // A time range is cleared; otherwise the selected objects go.
                if !self.clear_range()? {
                    self.delete_selection()?;
                }
            }
            Action::SplitSelectedAtPlayhead => self.split_at_playhead()?,
            Action::SetPreamp { track, model } => {
                let slot = if let Some(model) = model {
                    let &(id, name, _) = faderframe_core::builtin::PREAMPS
                        .get(model)
                        .ok_or_else(|| SessionError::Other("Unknown preamp".into()))?;
                    Some(PluginSlot {
                        id: self.project.ids.allocate(),
                        plugin: PluginRef {
                            format: faderframe_project::PluginFormat::Builtin,
                            id: id.into(),
                            name: name.into(),
                        },
                        bypass: false,
                        parameters: Vec::new(),
                        state: None,
                        sidechain: None,
                    })
                } else {
                    None
                };
                self.edit(Command::SetPreamp { track, slot })?;
            }
            Action::InsertPlugin {
                track,
                index,
                plugin,
            } => {
                if plugin.format == faderframe_project::PluginFormat::Builtin
                    && faderframe_core::builtin::preamp_index(&plugin.id).is_some()
                {
                    return Err(SessionError::Other(
                        "Choose microphone preamps in the dedicated mixer section".into(),
                    ));
                }
                // MIDI effects work on notes: on instrument tracks, before
                // the instrument (after it they would reach nothing).
                let mut index = index;
                let midi_effect = self.is_midi_effect(&plugin);
                if !midi_effect
                    && self
                        .project
                        .track(track)
                        .is_some_and(|t| t.kind == TrackKind::Midi)
                {
                    return Err(SessionError::Other(format!(
                        "a MIDI track has no audio: {} cannot go on it, only MIDI effects can",
                        plugin.name
                    )));
                }
                if midi_effect {
                    let t = self
                        .project
                        .track(track)
                        .ok_or_else(|| SessionError::Other("no track".into()))?;
                    if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
                        return Err(SessionError::Other(format!(
                            "{} is a MIDI effect: it goes on an instrument track, before the instrument",
                            plugin.name
                        )));
                    }
                    if let Some(first) = t
                        .inserts
                        .iter()
                        .position(|s| self.engine.plugin_is_instrument(s.id))
                    {
                        index = index.min(first);
                    }
                }
                let slot = PluginSlot {
                    id: self.project.ids.allocate(),
                    plugin,
                    bypass: false,
                    parameters: Vec::new(),
                    state: None,
                    sidechain: None,
                };
                // A container comes with its first chains.
                if slot.plugin.is_container() {
                    let container = slot.id;
                    self.edit(Command::Batch {
                        label: "Insert Container".into(),
                        commands: vec![
                            Command::InsertPlugin { track, index, slot },
                            Command::SetContainer {
                                track,
                                container,
                                chains: Some(containers::default_chains()),
                            },
                        ],
                    })?;
                } else {
                    self.edit(Command::InsertPlugin { track, index, slot })?;
                }
            }
            Action::MovePlugin {
                track,
                plugin,
                to,
                index,
            } => self.move_plugin(track, plugin, to, index, false)?,
            Action::CopyPlugin {
                track,
                plugin,
                to,
                index,
            } => self.move_plugin(track, plugin, to, index, true)?,
            Action::RedrawAudio {
                clip,
                channel,
                start,
                samples,
            } => self.redraw_audio(clip, channel, start, &samples)?,
            Action::PickColor(target) => self.ui_requests.push(UiRequest::PickColor(target)),
            Action::AddMarker(at) => {
                self.add_marker(at)?;
            }
            Action::AddSection { start, end } => {
                self.add_section(start, end)?;
            }
            Action::MoveSection { section, to, copy } => self.move_section(section, to, copy)?,
            Action::DuplicateSection(section) => self.duplicate_section(section)?,
            Action::SwapSection { section, later } => self.swap_section(section, later)?,
            Action::DeleteSectionContent(section) => self.delete_section_content(section)?,
            Action::AddTempoPoint(at) => self.add_tempo_point(at)?,
            Action::SetTempoPoint {
                index,
                position,
                bpm,
            } => self.set_tempo_point(index, position, bpm)?,
            Action::RemoveTempoPoint(index) => self.remove_tempo_point(index)?,
            Action::SetTempoRamp { index, ramp } => self.set_tempo_ramp(index, ramp)?,
            Action::ShowGlobalLane(lane, on) => {
                self.editor.lanes.set(lane, on);
                self.revision += 1;
            }
            Action::DetectChords => self.detect_chords()?,
            Action::DetectKey => {
                let key = self.detect_key()?;
                self.notify(NoticeLevel::Info, format!("the key is {}", key.name()));
            }
            Action::SetAnalysisSource(track) => {
                self.set_analysis_source(track);
                self.revision += 1;
            }
            Action::ResetAnalysis => self.reset_analysis(),
            Action::SetLoudnessTarget(lufs) => {
                self.update_analysis_settings(|s| s.target_lufs = lufs);
            }
            Action::Album(a) => self.album_action(a)?,
            Action::Ddp(a) => self.ddp_action(a)?,
            Action::ReloadPlugin(plugin) => self.reload_plugins(&[plugin])?,
            Action::LoadDeviceSamples {
                plugin,
                slot,
                files,
            } => self.load_device_samples(plugin, slot, files)?,
            Action::SelectPluginProgram { plugin, index } => {
                self.select_plugin_program(plugin, index)?;
            }
            Action::MakeSample {
                track,
                start,
                end,
                target,
            } => self.make_sample(track, start, end, target)?,
            Action::CaptureMidi => self.capture_midi()?,
            Action::DetectPitch { clips } => self.detect_pitch(&clips)?,
            Action::FromClip { clip, what } => self.from_clip(clip, what)?,
            Action::ConvertToMidi { clip, how } => self.convert_to_midi(clip, how)?,
            Action::OpenClipEffects(clip) => self.open_clip_fx(clip)?,
            Action::DownloadSpeechModel => self.download_speech_model()?,
            Action::Transcribe(clip) => self.transcribe(clip)?,
            Action::ExportLyrics => {
                self.export_lyrics()?;
            }
            Action::EditLyric { index, text } => self.edit_lyric(index, text)?,
            Action::ClipEffects { clip, op } => self.edit_clip_fx(clip, op)?,
            Action::ImportAdm(path) => self.start_adm_import(path),
            Action::SetMonoCheck(on) => self.set_mono_check(on)?,
            Action::SetHeadphones(room) => self.set_headphones(room)?,
            Action::SetHead(id) => self.set_head(&id)?,
            Action::SetHeadphoneCorrection(path) => {
                self.set_headphone_correction(path.as_deref())?
            }
            Action::ShowSurroundPanner(track) => {
                self.dispatch(Action::SelectTracks {
                    tracks: vec![track],
                    mode: SelectMode::Replace,
                })?;
                self.workspace_action(WorkspaceAction::ShowView(ViewId::surround()))?;
            }
            Action::OpenPitchEditor(clip) => {
                let Some(a) = self.project.clip(clip).and_then(|c| c.as_audio()) else {
                    return Ok(());
                };
                let analysed = a.pitch.is_some();
                self.pitch_clip = Some(clip);
                self.workspace_action(WorkspaceAction::ShowView(ViewId::pitch()))?;
                if !analysed {
                    self.detect_pitch(&[clip])?;
                }
                self.revision += 1;
            }
            Action::EditPitch { clip, op } => self.edit_pitch(clip, op)?,
            Action::Launcher(op) => self.launcher_op(op)?,
            Action::SaveVersion { name } => {
                self.save_version(&name)?;
            }
            Action::PromptSaveVersion => {
                if self.project_dir().is_none() {
                    return Err(SessionError::Other(
                        "save the project first: its versions are kept next to it".into(),
                    ));
                }
                self.ui_requests.push(UiRequest::SaveVersion);
            }
            Action::ShowVersions => self.ui_requests.push(UiRequest::Versions),
            Action::SaveTemplate { name } => {
                self.save_template(&name)?;
            }
            Action::PromptSaveTemplate => self.ui_requests.push(UiRequest::SaveTemplate),
            Action::Video(op) => self.video_op(op)?,
            Action::ShowTemplates => self.ui_requests.push(UiRequest::Templates),
            Action::DeleteTemplate(path) => {
                templates::delete_template(&path)?;
                self.notify(NoticeLevel::Info, "deleted the template".to_string());
            }
            Action::RestoreVersion(path) => self.restore_version(&path)?,
            Action::DuplicateAsAlias(clips) => {
                self.duplicate_as_alias(&clips)?;
            }
            Action::MakeClipsUnique(clips) => self.make_unique(&clips)?,
            Action::NewFolder { tracks } => {
                self.new_folder(&tracks)?;
            }
            Action::MoveToFolder { tracks, folder } => self.move_to_folder(&tracks, folder)?,
            Action::ToggleFolder(folder) => self.toggle_folder(folder),
            Action::SumFolder(folder) => {
                self.sum_folder(folder)?;
            }
            Action::PromptSaveSample { track, start, end } => {
                self.prompt_save_sample(track, start, end)?;
            }
            Action::ReloadAllPlugins => {
                let all: Vec<_> = self
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| t.slots())
                    .filter(|s| s.plugin.format != faderframe_project::PluginFormat::Builtin)
                    .map(|s| s.id)
                    .collect();
                self.reload_plugins(&all)?;
            }
            Action::SetLevelScale(scale) => self.update_analysis_settings(|s| s.scale = scale),
            Action::SetResetOnPlay(on) => self.update_analysis_settings(|s| s.reset_on_play = on),
            Action::GroupSelectedTracks => {
                let tracks: Vec<TrackId> = self.selection.tracks.iter().copied().collect();
                self.create_group(&tracks)?;
            }
            Action::DeleteGroup(group) => self.delete_group(group)?,
            Action::PromptRenameGroup(group) => {
                self.ui_requests.push(UiRequest::RenameGroup(group));
            }
            Action::AssignSelectedToVca(vca) => self.assign_selected_to_vca(vca)?,
            Action::SetGroupActive { group, active } => {
                self.update_group(group, |g| g.active = active)?;
            }
            Action::SetGroupLink { group, link } => self.update_group(group, |g| g.link = link)?,
            Action::RenameGroup { group, name } => {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    self.update_group(group, |g| g.name = name)?;
                }
            }
            Action::FreezeTrack(track) => self.start_bounce(track, true)?,
            Action::UnfreezeTrack(track) => self.unfreeze(track)?,
            Action::BounceTrack(track) => self.start_bounce(track, false)?,
            Action::PromptSavePluginPreset(plugin) => {
                self.ui_requests
                    .push(UiRequest::SavePluginPreset { plugin });
            }
            Action::SavePluginPreset { plugin, name } => {
                self.save_plugin_preset(plugin, &name)?;
            }
            Action::LoadPluginPreset { plugin, path } => self.load_plugin_preset(plugin, &path)?,
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
                        controllers: Vec::new(),
                        expressions: Vec::new(),
                        sysex: Vec::new(),
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
                    muted: false,
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
            Action::PromptSaveTrackPreset { tracks } => {
                self.ui_requests.push(UiRequest::SaveTrackPreset { tracks });
            }
            Action::CreateOutputTracks { plugin, buses } => {
                self.create_output_tracks(plugin, buses.as_deref())?;
            }
            Action::PromptDeletePreset { path } => {
                let name = path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                self.ui_requests
                    .push(UiRequest::DeletePreset { path, name });
            }
            Action::DeletePreset { path } => self.delete_preset(&path)?,
            Action::SaveTrackPreset {
                track,
                name,
                replace,
            } => self.save_track_preset(track, name.as_deref(), replace)?,
            Action::OpenPluginBrowser { track, target } => {
                self.ui_requests
                    .push(UiRequest::PluginBrowser { track, target });
                self.revision += 1;
            }
            Action::AddInstrumentTrack => {
                let track = self.add_track(TrackKind::Instrument)?;
                self.ui_requests.push(UiRequest::PluginBrowser {
                    track,
                    target: PluginTarget::Instrument,
                });
                self.revision += 1;
            }
            Action::ResetPerformance => self.reset_performance(),
            Action::MidiLearn(target) => self.start_midi_learn(target),
            Action::AddNotes { clip, notes } => {
                self.add_notes(clip, &notes)?;
            }
            Action::AddChord {
                clip,
                start,
                length,
                key,
                velocity,
            } => {
                self.add_chord(clip, start, length, key, velocity)?;
            }
            Action::ApplyMidiTool { clip, notes } => self.apply_midi_tool(clip, &notes)?,
            Action::DuplicateNotes {
                clip,
                notes,
                offset,
                keys,
            } => {
                self.duplicate_notes(clip, &notes, offset, keys)?;
            }
            Action::SplitNotes { clip, notes, at } => self.split_notes(clip, &notes, at)?,
            Action::RemoveNotes { clip, notes } => self.remove_notes(clip, &notes)?,
            Action::NoteOperation { clip, notes, op } => self.note_operation(clip, &notes, &op)?,
            Action::CopyNotes { clip, notes } => self.copy_notes(clip, &notes)?,
            Action::CutNotes { clip, notes } => self.cut_notes(clip, &notes)?,
            Action::PasteNotes { clip, at } => {
                self.paste_notes(clip, at)?;
            }
            Action::SetControllerPoints {
                clip,
                controller,
                channel,
                from,
                to,
                points,
            } => self.set_controller_points(clip, controller, channel, from, to, &points)?,
            Action::SetMidiClipLength { clip, length } => {
                self.set_midi_clip_length(clip, length)?
            }
            Action::SetNoteExpression {
                clip,
                note,
                kind,
                from,
                to,
                points,
            } => self.set_note_expression(clip, note, kind, from, to, &points)?,
            Action::AddSysex { clip, at, messages } => self.add_sysex(clip, at, messages)?,
            Action::RemoveSysex { clip, index } => self.remove_sysex(clip, index)?,
            Action::SendSysex { output, messages } => self.send_sysex(&output, messages)?,
            Action::RequestSysexImport { clip, at } => {
                self.ui_requests.push(UiRequest::ImportSysex { clip, at });
                self.revision += 1;
            }
            Action::Audition {
                track,
                key,
                velocity,
                channel,
            } => self.audition(track, key, velocity, channel),
            Action::AuditionOff => self.audition_off(),
            Action::SetStepInput(step) => self.set_step_input(step),
            Action::SetPianoRoll(p) => {
                self.editor.piano = p;
                self.revision += 1;
            }
            Action::CancelMidiLearn => self.cancel_midi_learn(),
            Action::SetDeviceView { plugin, values } => {
                for (key, value) in values {
                    self.device_views.insert((plugin, key), value);
                }
                self.revision += 1;
            }
            Action::OpenPluginEditor {
                track,
                plugin,
                generic,
            } => {
                self.ui_requests.push(UiRequest::PluginEditor {
                    track,
                    plugin,
                    generic,
                });
                self.revision += 1;
            }
            Action::SetInstrumentPlugin { track, plugin } => {
                let t = self
                    .project
                    .track(track)
                    .ok_or_else(|| SessionError::Other("no track".into()))?;
                let current = self.instrument_slot(t).map(|s| s.id);
                let index = current
                    .and_then(|id| t.inserts.iter().position(|s| s.id == id))
                    .unwrap_or(0);
                let mut commands = Vec::new();
                if t.instrument.is_some() {
                    commands.push(Command::SetInstrument { track, slot: None });
                } else if let Some(plugin) = current {
                    commands.push(Command::RemovePlugin { track, plugin });
                }
                if let Some(plugin) = plugin {
                    let slot = PluginSlot {
                        id: self.project.ids.allocate(),
                        plugin,
                        bypass: false,
                        parameters: Vec::new(),
                        state: None,
                        sidechain: None,
                    };
                    commands.push(Command::InsertPlugin { track, index, slot });
                }
                if !commands.is_empty() {
                    self.edit(Command::Batch {
                        label: "Choose Instrument".into(),
                        commands,
                    })?;
                }
            }

            Action::AddTrackFromPreset { path } => {
                self.add_track_from_preset(&path)?;
            }
            Action::ApplyTrackPreset { track, path } => self.apply_track_preset(track, &path)?,
            Action::SetMixerInsertSlots(n) => {
                let (lo, hi) = faderframe_workspace::INSERT_SLOTS_RANGE;
                self.workspace.mixer_insert_slots = Some(n.clamp(lo, hi));
                self.revision += 1;
            }
            Action::SetHeaderWidth(w) => {
                let (lo, hi) = faderframe_workspace::HEADER_WIDTH_RANGE;
                self.workspace.header_width = Some(w.clamp(lo, hi));
                self.revision += 1;
            }
            Action::SetTrackHeight { track, height } => {
                self.workspace.set_track_height(track, height);
                self.revision += 1;
            }
            Action::SetStripWidth { track, width } => {
                self.workspace.set_strip_width(track, width);
                self.revision += 1;
            }
            Action::SetPluginWindowPosition { plugin, x, y } => {
                self.workspace.plugin_windows.insert(plugin, (x, y));
            }
            Action::ShowAutomation { track, target } => {
                self.show_automation(track, target)?;
            }
            Action::HideAutomationLane(lane) => self.hide_automation(lane),
            Action::ToggleTrackAutomation(track) => self.toggle_track_automation(track)?,
            Action::SetAutomationModes { lanes, mode } => self.set_lane_modes(&lanes, mode)?,
            Action::SetAutomationMode { track, lane, mode } => {
                self.set_lane_mode(track, lane, mode)?
            }
            Action::ToggleTakeLanes(clip) => {
                if !self.open_takes.remove(&clip) {
                    self.open_takes.insert(clip);
                }
                self.revision += 1;
            }
            Action::ImportMidiFile { path, at, tempo } => {
                self.import_midi_file(&path, at, tempo)?;
            }
            Action::ImportFiles { files, track, at } => {
                // MIDI files become instrument tracks; the rest is audio.
                let (midi, audio): (Vec<PathBuf>, Vec<PathBuf>) =
                    files.into_iter().partition(|f| is_midi_file(f));
                let empty = self.project.clips.is_empty();
                for f in midi {
                    if let Err(e) = self.import_midi_file(&f, at, empty && at == MusicalTime::ZERO)
                    {
                        self.notify(NoticeLevel::Error, e.to_string());
                    }
                }
                if !audio.is_empty() {
                    self.import_audio(audio, ImportTarget { track, at });
                }
            }
            Action::SetGrid(grid) => {
                self.editor.grid = grid;
                self.revision += 1;
            }
            Action::ToggleSnap => {
                let mode = if self.editor.snap {
                    EditMode::Slip
                } else {
                    EditMode::Grid
                };
                self.editor.edit_mode = mode;
                self.editor.snap = mode == EditMode::Grid;
                self.revision += 1;
            }
            Action::SetEditMode(mode) => {
                self.editor.edit_mode = mode;
                self.editor.snap = mode == EditMode::Grid;
                self.revision += 1;
            }
            Action::SetGridMode(mode) => {
                self.editor.grid_mode = mode;
                self.editor.edit_mode = EditMode::Grid;
                self.editor.snap = true;
                self.revision += 1;
            }
            Action::SetEditTool(tool) => {
                self.editor.tool = tool;
                self.revision += 1;
            }
            Action::SetNudge(n) => {
                self.editor.nudge = n;
                self.revision += 1;
            }
            Action::SetEditFlag(flag, on) => {
                let e = &mut self.editor;
                match flag {
                    EditFlag::TabToTransients => e.tab_to_transients = on,
                    EditFlag::LinkTimeline => e.link_timeline = on,
                    EditFlag::InsertionFollowsPlayback => e.insertion_follows_playback = on,
                    EditFlag::FollowPlayhead => e.follow_playhead = on,
                    EditFlag::ShowTransients => e.show_transients = on,
                    EditFlag::Warp => e.warp = on,
                    EditFlag::EditToolbar => e.show_edit_toolbar = on,
                }
                if matches!(
                    flag,
                    EditFlag::ShowTransients | EditFlag::Warp | EditFlag::TabToTransients
                ) && on
                {
                    self.analyse_transients_of_project();
                }
                self.revision += 1;
                if flag == EditFlag::EditToolbar {
                    self.layout_revision += 1;
                }
            }
            Action::SetCounterUnit(u) => {
                self.editor.counter_unit = u;
                self.revision += 1;
            }
            Action::Zoom(z) => {
                self.editor.zoom_request = (self.editor.zoom_request.0 + 1, z);
                self.revision += 1;
            }
            Action::SetTransientSensitivity(v) => {
                self.editor.transient_sensitivity = v.clamp(0.0, 1.0);
                self.revision += 1;
            }
            Action::SetEditRange(range) => self.set_edit_range(range)?,
            Action::Separate => self.separate()?,
            Action::TrimToSelection => self.trim_to_selection()?,
            Action::ClearRange => {
                self.clear_range()?;
            }
            Action::CopyRange => self.copy_range()?,
            Action::CutRange => {
                self.copy_range()?;
                self.clear_range()?;
            }
            Action::PasteRange => self.paste_range()?,
            Action::RepeatRange(n) => self.repeat_range(n)?,
            Action::InsertSilence => self.insert_silence()?,
            Action::Nudge { forward, target } => self.nudge(forward, target)?,
            Action::TrimClip { clip, edge, to } => self.trim_clip(clip, edge, to)?,
            Action::SetClipFades {
                clip,
                fade_in,
                fade_out,
            } => self.set_clip_fades(clip, fade_in, fade_out)?,
            Action::SetClipGain { clip, db } => self.set_clip_gain(clip, db)?,
            Action::SpotClip { clip, start } => self.spot_clip(clip, start)?,
            Action::ShuffleClip { clip, track, at } => self.shuffle_clip(clip, track, at)?,
            Action::TabTo { forward, extend } => self.tab_to(forward, extend)?,
            Action::MoveClips { clips, by, tracks } => self.move_clips(&clips, by, tracks)?,
            Action::TrimClips {
                clips,
                edge,
                by,
                stretch,
            } => self.trim_clips(&clips, edge, by, stretch)?,
            Action::StretchClip { clip, edge, to } => self.stretch_clip(clip, edge, to)?,
            Action::ClipGain { clips, delta_db } => self.clip_gain(&clips, Some(delta_db), None)?,
            Action::SetClipsGain { clips, db } => self.clip_gain(&clips, None, Some(db))?,
            Action::SetFade {
                clips,
                edge,
                length,
                shape,
                bend,
            } => self.set_fade(&clips, edge, length, shape, bend)?,
            Action::SetClipsMuted { clips, muted } => {
                let cmds = clips
                    .into_iter()
                    .map(|clip| Command::SetClipMuted { clip, muted })
                    .collect();
                self.batch(if muted { "Mute Clips" } else { "Unmute Clips" }, cmds)?;
            }
            Action::WarpTo {
                clip,
                source,
                to,
                drag,
            } => self.warp_to(clip, source, to, drag)?,
            Action::RemoveWarpMarker { clip, source } => self.remove_warp_marker(clip, source)?,
            Action::SetQuantize(q) => {
                self.editor.quantize = q;
                self.revision += 1;
            }
            Action::SetHumanize(h) => {
                self.editor.humanize = h;
                self.revision += 1;
            }
            Action::QuantizeClips(clips) => self.quantize_clips(&clips)?,
            Action::HumanizeClips(clips) => self.humanize_clips(&clips)?,
            Action::QuantizeWarp(clips) => {
                for c in clips {
                    self.quantize_warp(c)?;
                }
            }
            Action::ClearWarp(clips) => {
                for c in clips {
                    self.clear_warp(c)?;
                }
            }
            Action::SetWarpAlgorithm { clips, algorithm } => {
                for c in clips {
                    self.set_warp_algorithm(c, algorithm)?;
                }
            }
            Action::SeparateAtTransients(clips) => {
                for c in clips {
                    self.separate_at_transients(c)?;
                }
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
                    self.automation_stop_sent();
                    self.return_to_play_start()?;
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
                    self.automation_stop_sent();
                    self.return_to_play_start()?;
                } else {
                    self.play()?;
                }
            }
            TransportAction::Locate(pos) => {
                let s = to_samples(self, pos.max(MusicalTime::ZERO));
                self.engine.transport(TransportCommand::Locate(s))?;
                // Show the new position immediately, even while stopped.
                self.show_position(s);
            }
            TransportAction::Scrub(pos) => {
                let s = to_samples(self, pos.max(MusicalTime::ZERO));
                // ~70 ms: long enough to hear, short enough to follow the
                // pointer.
                let frames = (self.project.sample_rate as f64 * 0.07) as u32;
                self.engine.transport(TransportCommand::Scrub {
                    position: s,
                    frames,
                })?;
                self.show_position(s);
            }
            TransportAction::ReturnToStart => {
                self.engine.transport(TransportCommand::Locate(0))?;
                self.show_position(0);
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
                // Recording: punch out (playback continues). Playing: punch
                // in here. Stopped: record and play.
                if self.recording.is_some() {
                    self.stop_recording()?;
                } else {
                    let from = self.transport.position;
                    self.start_recording(from)?;
                    if self.recording.is_some() && !self.transport.playing {
                        self.play()?;
                    }
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
                // Layouts saved before a view existed do not know it yet.
                if !layout.views.contains_key(&v)
                    && let Some(kind) = faderframe_workspace::ViewKind::of_default_id(&v)
                {
                    layout.views.insert(v.clone(), kind);
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
            WorkspaceAction::ToggleMasterPanel => layout.master_panel = !layout.master_panel,
        }
        self.layout_revision += 1;
        Ok(())
    }

    // --- plugins ----------------------------------------------------------------------

    /// Requests for the shell (windows to open); clears them.
    pub fn take_ui_requests(&mut self) -> Vec<UiRequest> {
        std::mem::take(&mut self.ui_requests)
    }

    /// Insert a plugin into `track` at `target`.
    pub fn place_plugin(
        &mut self,
        track: TrackId,
        target: PluginTarget,
        plugin: PluginRef,
    ) -> Result<()> {
        let hosted = plugin.format != faderframe_project::PluginFormat::Builtin
            || faderframe_core::builtin::has_editor(&plugin.id);
        let before: Vec<faderframe_core::PluginInstanceId> = self
            .project
            .track(track)
            .map(|t| t.inserts.iter().map(|s| s.id).collect())
            .unwrap_or_default();
        match target {
            PluginTarget::Insert(index) => self.dispatch(Action::InsertPlugin {
                track,
                index,
                plugin,
            })?,
            PluginTarget::Instrument => self.dispatch(Action::SetInstrumentPlugin {
                track,
                plugin: Some(plugin),
            })?,
            // Shows its editor itself.
            PluginTarget::Song(song) => {
                return self.dispatch(Action::Album(album::AlbumAction::AddInsert {
                    song,
                    plugin,
                }));
            }
        }
        // Like most DAWs: a newly placed hosted plugin shows its editor.
        let placed = self.project.track(track).and_then(|t| match target {
            // The slot added (a MIDI effect may have gone before the
            // instrument rather than where it was asked for).
            PluginTarget::Insert(_) => t.inserts.iter().find(|s| !before.contains(&s.id)),
            PluginTarget::Instrument => self.instrument_slot(t),
            PluginTarget::Song(_) => None,
        });
        if hosted && let Some(slot) = placed {
            self.ui_requests.push(UiRequest::PluginEditor {
                track,
                plugin: slot.id,
                generic: false,
            });
        }
        Ok(())
    }

    /// Move (or `copy`, with its current settings) an insert to slot
    /// `index` of track `to`.
    fn move_plugin(
        &mut self,
        track: TrackId,
        plugin: faderframe_core::PluginInstanceId,
        to: TrackId,
        index: usize,
        copy: bool,
    ) -> Result<()> {
        if copy {
            // The slot carries the plugin's current state into the copy.
            self.capture_plugin_states();
        }
        let src = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other(format!("no track {track}")))?;
        let from = src
            .inserts
            .iter()
            .position(|s| s.id == plugin)
            .ok_or_else(|| SessionError::Other("no such insert".into()))?;
        let mut slot = src.inserts[from].clone();
        let dest = self
            .project
            .track(to)
            .ok_or_else(|| SessionError::Other(format!("no track {to}")))?;
        // MIDI effects go on MIDI and instrument tracks (before the
        // instrument); MIDI tracks take nothing else.
        let midi_fx = self.is_midi_effect(&slot.plugin);
        match dest.kind {
            TrackKind::Midi if !midi_fx => {
                return Err(SessionError::Other(format!(
                    "a MIDI track has no audio: {} cannot go on it, only MIDI effects can",
                    slot.plugin.name
                )));
            }
            TrackKind::Midi | TrackKind::Instrument => {}
            _ if midi_fx => {
                return Err(SessionError::Other(format!(
                    "{} is a MIDI effect: it goes on a MIDI track or an instrument track",
                    slot.plugin.name
                )));
            }
            _ if !dest.kind.has_audio() => {
                return Err(SessionError::Other(format!(
                    "'{}' cannot hold plugins",
                    dest.name
                )));
            }
            _ => {}
        }
        // Where the instrument is (with and without the moved plugin), for
        // MIDI effects, which play before it.
        let before_instrument = |keep_moved: bool| {
            dest.inserts
                .iter()
                .filter(|s| keep_moved || s.id != plugin)
                .position(|s| self.engine.plugin_is_instrument(s.id))
                .filter(|_| midi_fx)
                .unwrap_or(usize::MAX)
        };
        let (first_with, first_without) = (before_instrument(true), before_instrument(false));
        let len = dest.inserts.len();
        if copy {
            slot.id = self.project.ids.allocate();
            let index = index.min(len).min(first_with);
            return self.batch(
                "Copy Plugin",
                vec![Command::InsertPlugin {
                    track: to,
                    index,
                    slot,
                }],
            );
        }
        // Within the track the plugin lands in the slot it was dropped on.
        let index = if to == track {
            let target = index.min(len - 1).min(first_without);
            if target == from {
                return Ok(());
            }
            target
        } else {
            index.min(len).min(first_without)
        };
        self.batch(
            "Move Plugin",
            vec![
                Command::RemovePlugin { track, plugin },
                Command::InsertPlugin {
                    track: to,
                    index,
                    slot,
                },
            ],
        )
    }

    /// A built-in plugin's tap: its live parameters (automation included),
    /// the audio going in and out for an analyser, its meters.
    pub fn plugin_tap(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<std::sync::Arc<faderframe_plugin_host::tap::AnalysisTap>> {
        self.engine.plugin_tap(plugin)
    }

    /// An album song's insert and its song.
    pub fn song_insert(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<(&faderframe_project::album::Song, &PluginSlot)> {
        self.project.album.insert(plugin)
    }

    /// A device editor's own setting, if it has been set.
    pub fn device_view(&self, plugin: faderframe_core::PluginInstanceId, key: &str) -> Option<f64> {
        self.device_views.get(&(plugin, key.to_string())).copied()
    }

    /// A plugin's slot and the track its commands name: its own, or the
    /// master for an album song's insert (the commands find it there).
    pub fn plugin_owner(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<(TrackId, &PluginSlot)> {
        if let Some((t, s)) = self.plugin_slot(plugin) {
            return Some((t.id, s));
        }
        let (_, s) = self.project.album.insert(plugin)?;
        Some((self.project.master_id()?, s))
    }

    /// The first instrument plugin in the visible insert chain, with support
    /// for the separate instrument slot stored by older projects.
    pub fn instrument_slot<'a>(
        &self,
        track: &'a faderframe_project::Track,
    ) -> Option<&'a PluginSlot> {
        // The inserts first, then what containers hold.
        track.instrument.as_ref().or_else(|| {
            track
                .inserts
                .iter()
                .find(|s| self.engine.plugin_is_instrument(s.id))
                .or_else(|| {
                    track
                        .slots()
                        .into_iter()
                        .find(|s| self.engine.plugin_is_instrument(s.id))
                })
        })
    }

    /// The slot of a plugin instance and its track.
    pub fn plugin_slot(
        &self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<(&faderframe_project::Track, &PluginSlot)> {
        self.project
            .tracks
            .iter()
            .find_map(|t| t.plugin(plugin).map(|s| (t, s)))
    }

    /// The parameters of a hosted plugin with their current values.
    pub fn plugin_parameter_views(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Vec<PluginParameterView> {
        self.host_song_insert(plugin);
        let infos = self
            .engine
            .plugin_parameters(plugin)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        let explicit: Vec<faderframe_core::ParameterId> = self
            .plugin_slot(plugin)
            .map(|(_, s)| s)
            .or_else(|| self.project.album.insert(plugin).map(|(_, s)| s))
            .map(|s| s.parameters.iter().map(|p| p.id).collect())
            .unwrap_or_default();
        infos
            .into_iter()
            .map(|info| {
                let value = self
                    .engine
                    .plugin_parameter_value(plugin, info.id)
                    .unwrap_or(info.default);
                PluginParameterView {
                    explicit: explicit.contains(&info.id),
                    info,
                    value,
                }
            })
            .collect()
    }

    /// Current value of one plugin parameter (plain units).
    pub fn plugin_parameter_value(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
    ) -> Option<f64> {
        self.engine.plugin_parameter_value(plugin, parameter)
    }

    /// The plugin's own text for a value, if it formats values itself.
    pub fn format_plugin_parameter(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        parameter: faderframe_core::ParameterId,
        value: f64,
    ) -> Option<String> {
        self.engine
            .format_plugin_parameter(plugin, parameter, value)
    }

    /// A hosted plugin's own editor GUI (call only from the UI thread).
    pub fn plugin_editor(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
    ) -> Option<&mut dyn faderframe_plugin_host::PluginEditor> {
        self.host_song_insert(plugin);
        self.engine.plugin_editor(plugin)
    }

    /// An album song's insert that is not hosted yet (its song came back
    /// by undo) gets its instance.
    fn host_song_insert(&mut self, plugin: faderframe_core::PluginInstanceId) {
        if self.engine.plugin_latency(plugin).is_none()
            && let Some((_, slot)) = self.project.album.insert(plugin)
        {
            let slot = slot.clone();
            self.engine.host_plugin(&slot);
        }
    }

    /// File descriptors and timers plugins registered for the UI main loop.
    pub fn plugin_event_sources(
        &self,
    ) -> Vec<(
        faderframe_core::PluginInstanceId,
        faderframe_plugin_host::PluginEventSources,
    )> {
        self.engine.plugin_event_sources()
    }

    pub fn plugin_on_fd(
        &mut self,
        plugin: faderframe_core::PluginInstanceId,
        fd: faderframe_plugin_host::PluginFd,
    ) {
        self.engine.plugin_on_fd(plugin, fd);
    }

    pub fn plugin_on_timer(&mut self, plugin: faderframe_core::PluginInstanceId, timer: u32) {
        self.engine.plugin_on_timer(plugin, timer);
    }

    /// Plugins that can be inserted (effects) or used as instruments.
    /// The per-note expressions the track's instrument accepts (`None`:
    /// no instrument, or it does not say).
    pub fn instrument_note_expressions(
        &self,
        track: TrackId,
    ) -> Option<Vec<faderframe_midi::NoteExpressionKind>> {
        let slot = self.instrument_slot(self.project.track(track)?)?;
        self.engine.plugin_note_expressions(slot.id)
    }

    /// What a "+" (add a track) offers: each kind of track, then a new track
    /// from each saved track preset — `(label, action, starts a group)`.
    pub fn add_track_choices(&self) -> Vec<(String, Action, bool)> {
        let mut out = vec![
            (
                "Audio Track (Mono)".to_string(),
                Action::AddTrack(TrackKind::Audio),
                false,
            ),
            (
                "Audio Track (Stereo)".into(),
                Action::AddTrackWithLayout(
                    TrackKind::Audio,
                    faderframe_core::ChannelLayout::Stereo,
                ),
                false,
            ),
            (
                "Instrument Track…".into(),
                Action::AddInstrumentTrack,
                false,
            ),
            (
                "MIDI Track".into(),
                Action::AddTrack(TrackKind::Midi),
                false,
            ),
            ("Bus".into(), Action::AddTrack(TrackKind::Bus), true),
            (
                "Aux (FX Return)".into(),
                Action::AddTrack(TrackKind::Aux),
                false,
            ),
            ("VCA".into(), Action::AddTrack(TrackKind::Vca), false),
            ("Folder".into(), Action::AddTrack(TrackKind::Folder), true),
        ];
        let selected: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| self.selection.tracks.contains(&t.id) && t.kind != TrackKind::Master)
            .map(|t| t.id)
            .collect();
        if !selected.is_empty() {
            out.push((
                "Folder with the Selected Tracks".into(),
                Action::NewFolder { tracks: selected },
                false,
            ));
        }
        for (i, preset) in self.track_presets().iter().take(12).enumerate() {
            out.push((
                format!("From “{}”", preset.name),
                Action::AddTrackFromPreset {
                    path: preset.path.clone(),
                },
                i == 0,
            ));
        }
        out
    }

    /// Is the plugin of this slot an instrument?
    pub fn engine_plugin_is_instrument(&self, plugin: faderframe_core::PluginInstanceId) -> bool {
        self.engine.plugin_is_instrument(plugin)
    }

    /// Is `plugin` an instrument (by what the plugin says it is, without
    /// an instance)?
    pub fn is_instrument_plugin(&self, plugin: &PluginRef) -> bool {
        self.available_plugins()
            .iter()
            .any(|p| p.instrument && p.plugin.id == plugin.id && p.plugin.format == plugin.format)
    }

    /// An instrument track's instrument kept apart from its inserts (older
    /// projects and track presets) becomes an insert like any other, after
    /// the MIDI effects, where it played. A track whose inserts already
    /// hold an instrument never heard it (an instrument replaces what
    /// comes in): it goes, and the note returned says so.
    pub(crate) fn adopt_instrument(&self, t: &mut Track) -> Option<String> {
        if t.kind != TrackKind::Instrument {
            return None;
        }
        let slot = t.instrument.take()?;
        if t.inserts
            .iter()
            .any(|s| self.is_instrument_plugin(&s.plugin))
        {
            return Some(format!(
                "'{}' had a second instrument from an older version that was never heard ({}): removed",
                t.name,
                slot.plugin.name.trim_start_matches("FaderFrame ")
            ));
        }
        let at = t
            .inserts
            .iter()
            .rposition(|s| self.is_midi_effect(&s.plugin))
            .map_or(0, |i| i + 1);
        t.inserts.insert(at, slot);
        None
    }

    /// Is `plugin` a MIDI effect (notes in, notes out, no audio)?
    pub fn is_midi_effect(&self, plugin: &PluginRef) -> bool {
        self.available_plugins()
            .iter()
            .any(|p| p.midi_effect && p.plugin.id == plugin.id && p.plugin.format == plugin.format)
    }

    pub fn available_plugins(&self) -> Vec<AvailablePlugin> {
        use faderframe_plugin_host::{PluginCategory, PluginFormat as F};
        self.engine
            .available_plugins()
            .into_iter()
            .filter(|d| d.category != PluginCategory::Preamp)
            .map(|d| AvailablePlugin {
                plugin: PluginRef {
                    format: match d.format {
                        F::Builtin => faderframe_project::PluginFormat::Builtin,
                        F::Clap => faderframe_project::PluginFormat::Clap,
                        F::Vst3 => faderframe_project::PluginFormat::Vst3,
                        F::AudioUnit => faderframe_project::PluginFormat::AudioUnit,
                        F::Lv2 => faderframe_project::PluginFormat::Lv2,
                    },
                    id: d.id,
                    name: d.name,
                },
                vendor: d.vendor,
                version: d.version,
                instrument: d.category == PluginCategory::Instrument,
                midi_effect: d.category == PluginCategory::MidiEffect
                    || (d.note_inputs > 0 && d.note_outputs > 0 && d.audio_outputs.is_empty()),
                audio_inputs: d.audio_inputs.first().map_or(0, |p| p.channels),
                audio_outputs: d.audio_outputs.first().map_or(0, |p| p.channels),
                note_inputs: d.note_inputs,
            })
            .collect()
    }

    /// Store every hosted plugin's current state in its project slot (before
    /// saving, rendering, rebuilding the engine or removing plugins).
    /// Bookkeeping, not an edit: it does not touch the undo history.
    pub fn capture_plugin_states(&mut self) {
        let slots: Vec<faderframe_core::PluginInstanceId> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.slots())
            .chain(self.project.album.inserts())
            .filter(|s| s.plugin.format != faderframe_project::PluginFormat::Builtin)
            .map(|s| s.id)
            .collect();
        for id in slots {
            self.capture_plugin_state(id);
        }
    }

    /// Take one plugin's current state (and the values its own editor set)
    /// into its slot.
    pub(crate) fn capture_plugin_state(&mut self, id: faderframe_core::PluginInstanceId) {
        let state = self.engine.plugin_state(id);
        let project = &mut self.project;
        let slots = project.tracks.iter_mut().flat_map(|t| t.slots_mut()).chain(
            project
                .album
                .songs
                .iter_mut()
                .flat_map(|s| s.inserts.iter_mut()),
        );
        for s in slots.filter(|s| s.id == id) {
            if let Some(state) = &state {
                s.state = Some(state.clone());
                self.engine.note_plugin_state(id, state);
            }
            // Explicit values follow what the plugin's own editor did
            // since (they are applied after the state on load).
            for p in &mut s.parameters {
                if let Some(v) = self.engine.plugin_parameter_value(id, p.id) {
                    p.value = v;
                    self.engine.note_plugin_parameter(id, p.id, v);
                }
            }
        }
    }

    // --- inputs -----------------------------------------------------------------------

    /// The Object entry of a track's menu (tracks panned into the master's
    /// bed): delivered as an object rather than mixed into the bed.
    pub fn object_choice(&self, track: TrackId) -> Option<InputChoice> {
        let t = self.project.track(track)?;
        self.project.may_be_object(t).then(|| InputChoice {
            label: "Object (delivered with its place, not mixed into the bed)".into(),
            action: Action::Edit(Command::SetTrackObject {
                track,
                on: !t.object,
            }),
            checked: t.object,
            group_start: false,
        })
    }

    /// How the master is listened to: speakers, headphones (binaural, with
    /// a room), the mono check. Listening only — renders stay as mixed.
    pub fn listen_choices(&self) -> Vec<InputChoice> {
        use faderframe_binaural::Room;
        let now = self.headphones();
        let mut out = vec![InputChoice {
            label: "Speakers".into(),
            action: Action::SetHeadphones(None),
            checked: now.is_none(),
            group_start: false,
        }];
        for room in Room::ALL {
            out.push(InputChoice {
                label: format!("Headphones · {} (binaural)", room.name()),
                action: Action::SetHeadphones(Some(room)),
                checked: now == Some(room),
                group_start: false,
            });
        }
        out.push(InputChoice {
            label: "Mono Check".into(),
            action: Action::SetMonoCheck(!self.mono_check),
            checked: self.mono_check,
            group_start: true,
        });
        out
    }

    /// How a headphone render of a delivered master places the track
    /// (Dolby's binaural render modes, written with ADM masters): the
    /// master (its bed) and object tracks, while the master is a bed.
    pub fn binaural_render_choices(&self, track: TrackId) -> Vec<InputChoice> {
        use faderframe_project::BinauralRender;
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let bed = self
            .project
            .master()
            .is_some_and(|m| matches!(m.layout, faderframe_core::ChannelLayout::Surround(_)));
        if !bed || !(t.kind == TrackKind::Master || self.project.is_object(t)) {
            return Vec::new();
        }
        let mut out = vec![InputChoice {
            label: "Not Set".into(),
            action: Action::Edit(Command::SetTrackBinaural { track, mode: None }),
            checked: t.binaural.is_none(),
            group_start: false,
        }];
        for (i, m) in BinauralRender::ALL.into_iter().enumerate() {
            out.push(InputChoice {
                label: m.name().into(),
                action: Action::Edit(Command::SetTrackBinaural {
                    track,
                    mode: Some(m),
                }),
                checked: t.binaural == Some(m),
                group_start: i == 0,
            });
        }
        out
    }

    /// The channel formats a track can have (audio tracks, buses, auxes and
    /// the master): mono, stereo and each surround bed. Tracks feeding a
    /// bed are panned into it, a bed feeding a smaller format is folded.
    pub fn format_choices(&self, track: TrackId) -> Vec<InputChoice> {
        use faderframe_core::{ChannelLayout, SurroundFormat};
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if !matches!(
            t.kind,
            TrackKind::Audio | TrackKind::Bus | TrackKind::Aux | TrackKind::Master
        ) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut push = |label: String, layout: ChannelLayout, group_start: bool| {
            out.push(InputChoice {
                label,
                action: Action::Edit(Command::SetTrackLayout { track, layout }),
                checked: t.layout == layout,
                group_start,
            });
        };
        push("Mono".into(), ChannelLayout::Mono, false);
        push("Stereo".into(), ChannelLayout::Stereo, false);
        for (i, f) in SurroundFormat::ALL.iter().enumerate() {
            push(
                format!("{} ({} channels)", f.name(), f.channels()),
                ChannelLayout::Surround(*f),
                i == 0,
            );
        }
        out
    }

    /// The input choices of an audio track for menus: no input, every mono
    /// input, every stereo pair. Choosing one also sets the track format
    /// (mono or stereo), which is what gets recorded.
    pub fn input_choices(&self, track: TrackId) -> Vec<InputChoice> {
        use faderframe_core::ChannelLayout;
        use faderframe_project::InputRouting;
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let set = |input: InputRouting, layout: ChannelLayout| {
            let mut commands = Vec::new();
            if layout != t.layout {
                commands.push(Command::SetTrackLayout { track, layout });
            }
            commands.push(Command::SetTrackInput { track, input });
            Action::Edit(Command::Batch {
                label: "Change Input".into(),
                commands,
            })
        };
        let inputs = self.stream_info().map_or(8, |i| i.input_channels).max(2);
        let stereo = t.layout.channel_count() >= 2;
        let mut out = vec![InputChoice {
            label: "No input".into(),
            action: Action::Edit(Command::SetTrackInput {
                track,
                input: InputRouting::None,
            }),
            checked: t.input == InputRouting::None,
            group_start: false,
        }];
        for first in 0..inputs {
            let input = InputRouting::Hardware {
                first_channel: first,
            };
            out.push(InputChoice {
                label: format!("Mono · In {}", first + 1),
                action: set(input.clone(), ChannelLayout::Mono),
                checked: !stereo && t.input == input,
                group_start: first == 0,
            });
        }
        for first in (0..inputs.saturating_sub(1)).step_by(2) {
            let input = InputRouting::Hardware {
                first_channel: first,
            };
            out.push(InputChoice {
                label: format!("Stereo · In {}–{}", first + 1, first + 2),
                action: set(input.clone(), ChannelLayout::Stereo),
                checked: stereo && t.input == input,
                group_start: first == 0,
            });
        }
        // Extra outputs of plugins on other tracks (a multi-output
        // instrument's).
        if t.kind != TrackKind::Master {
            let mut first = true;
            for other in self.project.tracks.iter().filter(|o| o.id != track) {
                for slot in other.inserts.iter().chain(other.instrument.iter()) {
                    if self.project.reaches(track, other.id, None) {
                        continue;
                    }
                    for o in self.plugin_output_buses(slot.id).into_iter().skip(1) {
                        let input = InputRouting::Plugin {
                            plugin: slot.id,
                            bus: o.bus,
                        };
                        let layout =
                            ChannelLayout::from_channel_count(usize::from(o.channels.max(1)));
                        out.push(InputChoice {
                            label: format!("{} · {} · {}", other.name, slot.plugin.name, o.name),
                            action: set(input.clone(), layout),
                            checked: t.input == input,
                            group_start: first,
                        });
                        first = false;
                    }
                }
            }
        }
        out
    }

    /// "In 1", "In 3–4", "No input", "MIDI · All", "MIDI · MPK mini 3 · Ch 10".
    pub fn input_label(&self, t: &Track) -> String {
        match &t.input {
            faderframe_project::InputRouting::None => "No input".into(),
            faderframe_project::InputRouting::Hardware { first_channel } => {
                match t.layout.channel_count() {
                    1 => format!("In {}", first_channel + 1),
                    n => format!("In {}–{}", first_channel + 1, *first_channel as usize + n),
                }
            }
            faderframe_project::InputRouting::Midi { port, channel } => {
                let port = port
                    .as_deref()
                    .map_or("All", faderframe_project::midi_port_display);
                match channel {
                    Some(c) => format!("MIDI · {port} · Ch {}", c + 1),
                    None => format!("MIDI · {port}"),
                }
            }
            faderframe_project::InputRouting::Plugin { plugin, bus } => {
                let Some((_, slot)) = self.plugin_slot(*plugin) else {
                    return "Plugin output (missing)".into();
                };
                let name = self
                    .plugin_output_buses(*plugin)
                    .into_iter()
                    .find(|o| o.bus == *bus)
                    .map_or_else(|| format!("Output {}", bus + 1), |o| o.name);
                format!("{} · {name}", slot.plugin.name)
            }
        }
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

    /// Delete a user preset: a track preset in the library or a plugin
    /// preset FaderFrame saved (the plugin formats' own files stay).
    pub fn delete_preset(&mut self, path: &Path) -> Result<()> {
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
        // Compared as the file system sees them (no `..` way out).
        let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let file = real(path);
        let inside = |dir: &Path| file.parent().is_some_and(|p| p.starts_with(real(dir)));
        let track = inside(&self.preset_dir)
            && ext.as_deref() == Some(faderframe_project::preset::PRESET_EXTENSION);
        let plugin =
            inside(&crate::media::data_dir().join("presets")) && ext.as_deref() == Some("ffpreset");
        if !(track || plugin) {
            return Err(SessionError::Other(format!(
                "{} is not one of your presets",
                path.display()
            )));
        }
        std::fs::remove_file(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let name = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        if track {
            self.rescan_track_presets();
        } else {
            self.revision += 1;
        }
        self.notify(NoticeLevel::Info, format!("deleted preset '{name}'"));
        Ok(())
    }

    /// The library's preset that saving as `name` would meet (the same
    /// file name, ignoring case).
    pub fn existing_track_preset(&self, name: &str) -> Option<&PresetEntry> {
        let want = faderframe_audio_files::import::clean_stem(name).to_lowercase();
        self.presets.iter().find(|e| e.name.to_lowercase() == want)
    }

    fn save_track_preset(
        &mut self,
        track: TrackId,
        name: Option<&str>,
        replace: bool,
    ) -> Result<()> {
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => self
                .project
                .track(track)
                .ok_or(EditError::UnknownTrack(track))?
                .name
                .clone(),
        };
        std::fs::create_dir_all(&self.preset_dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", self.preset_dir.display())))?;
        let ext = faderframe_project::preset::PRESET_EXTENSION;
        let replaced = if replace {
            self.existing_track_preset(&name).map(|e| e.path.clone())
        } else {
            None
        };
        let path = match &replaced {
            Some(old) => {
                // Written under the new spelling of the name.
                let stem = faderframe_audio_files::import::clean_stem(&name);
                let path = self.preset_dir.join(format!("{stem}.{ext}"));
                if *old != path {
                    std::fs::remove_file(old)
                        .map_err(|e| SessionError::Other(format!("{}: {e}", old.display())))?;
                }
                path
            }
            None => faderframe_audio_files::import::unique_path(&self.preset_dir, &name, ext),
        };
        self.export_track_preset(track, &path)?;
        self.rescan_track_presets();
        self.notify(
            NoticeLevel::Info,
            format!(
                "{} track preset '{name}'",
                if replaced.is_some() {
                    "replaced"
                } else {
                    "saved"
                }
            ),
        );
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
        let (mut track, mut notes) = preset.instantiate(&mut self.project, None);
        notes.extend(self.adopt_instrument(&mut track));
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
        let (commands, mut notes) = preset
            .apply_commands(&mut self.project, track)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        // A preset's instrument kept apart: an insert after its MIDI
        // effects (unless its inserts bring an instrument).
        let mut out = Vec::with_capacity(commands.len() + 1);
        let mut inserted: Vec<&PluginSlot> = Vec::new();
        for c in &commands {
            if let Command::InsertPlugin { slot, .. } = c {
                inserted.push(slot);
            }
        }
        let has_instrument = inserted
            .iter()
            .any(|s| self.is_instrument_plugin(&s.plugin));
        let after_fx = inserted
            .iter()
            .rposition(|s| self.is_midi_effect(&s.plugin))
            .map_or(0, |i| i + 1);
        for c in commands {
            match c {
                Command::SetInstrument {
                    track,
                    slot: Some(slot),
                } => {
                    out.push(Command::SetInstrument { track, slot: None });
                    if has_instrument {
                        notes.push(format!(
                            "its second instrument ({}) was never heard: left out",
                            slot.plugin.name
                        ));
                    } else {
                        out.push(Command::InsertPlugin {
                            track,
                            index: after_fx,
                            slot,
                        });
                    }
                }
                c => out.push(c),
            }
        }
        let commands = out;
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
        if matches!(kind, TrackKind::Vca | TrackKind::Folder) {
            track.output = faderframe_project::OutputRouting::None;
            track.layout = faderframe_core::ChannelLayout::Mono;
        }
        if kind == TrackKind::Midi {
            // A MIDI track plays the selected instrument track, if one is.
            track.output = p
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Instrument && self.selection.tracks.contains(&t.id))
                .map_or(faderframe_project::OutputRouting::None, |t| {
                    faderframe_project::OutputRouting::Track { track: t.id }
                });
        }
        // Instrument tracks start without an instrument: one is chosen
        // from the plugins (built-in synth, CLAP, VST3).
        // Content tracks go after the last selected/content track; buses and
        // auxes go just before the master.
        let index = if kind.is_summing() || kind == TrackKind::Vca {
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

    /// "Insertion follows playback" off: stopping returns to where
    /// playback started.
    fn return_to_play_start(&mut self) -> Result<()> {
        if let Some(pos) = self.play_started_at.take()
            && !self.editor.insertion_follows_playback
        {
            self.engine.transport(TransportCommand::Locate(pos))?;
            self.show_position(pos);
        }
        Ok(())
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
        // The album rendered to play it.
        self.album_state.discard_preview();
        self.ddp_state.discard();
        // Scratch media of a project that was never saved is discarded with
        // it (the shell asks before closing unsaved work).
        self.discard_unsaved_media();
    }
}

/// Worker threads of the render-ahead graph besides its own thread.
fn render_ahead_threads() -> usize {
    faderframe_realtime::physical_cores()
        .saturating_sub(1)
        .min(8)
}

/// Point album songs' files at where a save moved them.
fn remap_album_files(project: &mut Project, moves: &HashMap<PathBuf, PathBuf>) {
    for song in &mut project.album.songs {
        if let faderframe_project::album::SongSource::AudioFile(path) = &mut song.source
            && let Some(new) = moves.get(path.as_path())
        {
            *path = new.clone();
        }
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
                    ..Default::default()
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
        assert!(
            s.project().track(id).unwrap().instrument.is_none(),
            "instruments are chosen from the plugins"
        );
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
