//! Edit commands.
//!
//! Every mutation an editor can make is a [`Command`]. Applying a command
//! validates it, mutates the [`Project`] and returns the exact inverse
//! command, which is what the undo history stores. Commands also report
//! their [`Impact`] so the session knows how much engine state must be
//! re-synchronised (parameter values, timeline content, or the whole graph).

use crate::{
    AudioSource, AuxSend, Clip, ClipContent, ClipFades, InputRouting, Marker, MidiNote,
    MonitorMode, MusicalRange, OutputRouting, PluginSlot, Project, SendTap, Track, TrackColor,
    TrackKind,
};
use faderframe_automation::AutomationLane;
use faderframe_core::ChannelLayout;
use faderframe_core::gain::SILENCE_DB;
use faderframe_core::{
    AudioSourceId, AutomationLaneId, ClipId, MarkerId, NoteId, ParameterId, PluginInstanceId,
    SendId, TrackId,
};
use faderframe_timeline::{MusicalTime, Timeline};

/// Upper limit for fader and send levels.
pub const MAX_LEVEL_DB: f32 = 12.0;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("unknown track {0}")]
    UnknownTrack(TrackId),
    #[error("unknown clip {0}")]
    UnknownClip(ClipId),
    #[error("unknown send {0}")]
    UnknownSend(SendId),
    #[error("unknown plugin {0}")]
    UnknownPlugin(PluginInstanceId),
    #[error("unknown note {0}")]
    UnknownNote(NoteId),
    #[error("unknown marker {0}")]
    UnknownMarker(MarkerId),
    #[error("unknown audio source {0}")]
    UnknownSource(AudioSourceId),
    #[error("audio source {0} is still used by clips")]
    SourceInUse(AudioSourceId),
    #[error("the master track cannot be removed")]
    CannotRemoveMaster,
    #[error("invalid routing: {0}")]
    InvalidRouting(String),
    #[error("routing would create a feedback loop")]
    FeedbackLoop,
    #[error("invalid edit: {0}")]
    Invalid(String),
}

/// How much of the engine must be updated after a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact {
    /// Display-only data (names, colours, track order, markers).
    None,
    /// Continuous values (fader, pan, mute/solo, send levels).
    Params,
    /// Timeline content (clips, notes, tempo, loop).
    Timeline,
    /// Routing/graph structure (tracks, plugins, sends, I/O).
    Graph,
}

/// Commands with the same key inside one undo transaction collapse into a
/// single undo step (e.g. one fader drag).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CoalesceKey {
    TrackVolume(TrackId),
    TrackPan(TrackId),
    TrackSurround(TrackId),
    SendLevel(TrackId, SendId),
    Clip(ClipId),
    Note(ClipId, NoteId),
    Tempo,
    Loop,
    Punch,
    Automation(TrackId, AutomationLaneId),
    PluginParameter(PluginInstanceId, ParameterId),
    Marker(MarkerId),
    Section(faderframe_core::SectionId),
    Modulators(TrackId),
    ChainMix(PluginInstanceId, usize),
}

/// State needed to undo a track removal.
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedTrack {
    pub track: Track,
    pub index: usize,
    pub clips: Vec<Clip>,
    /// Tracks whose output pointed at the removed track (old routing).
    pub rerouted: Vec<(TrackId, OutputRouting)>,
    /// Sends that targeted the removed track: (owner, index, send).
    pub removed_sends: Vec<(TrackId, usize, AuxSend)>,
    /// Its launcher clips, by scene.
    pub slots: Vec<(faderframe_core::SceneId, Clip)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    // --- track parameters ------------------------------------------------
    SetTrackVolume {
        track: TrackId,
        db: f32,
    },
    SetTrackPan {
        track: TrackId,
        pan: f32,
    },
    /// Where the track sits in a surround bed (clamped).
    SetTrackSurround {
        track: TrackId,
        pan: faderframe_core::SurroundPan,
    },
    SetTrackMute {
        track: TrackId,
        on: bool,
    },
    SetTrackSolo {
        track: TrackId,
        on: bool,
    },
    SetTrackRecordArm {
        track: TrackId,
        on: bool,
    },
    SetTrackMonitor {
        track: TrackId,
        mode: MonitorMode,
    },
    /// Play a MIDI track on an external MIDI device (or stop doing so).
    SetTrackMidiOutput {
        track: TrackId,
        output: Option<crate::MidiOutputRouting>,
    },
    /// MPE on (a zone) or off for a track's instrument / MIDI output.
    SetTrackMpe {
        track: TrackId,
        mpe: Option<crate::MpeConfig>,
    },
    SetTrackPhaseInvert {
        track: TrackId,
        on: bool,
    },
    RenameTrack {
        track: TrackId,
        name: String,
    },
    SetTrackColor {
        track: TrackId,
        color: TrackColor,
    },

    // --- routing -----------------------------------------------------------
    SetTrackOutput {
        track: TrackId,
        output: OutputRouting,
    },
    SetTrackInput {
        track: TrackId,
        input: InputRouting,
    },
    /// Mono/stereo format of a track (what it records and processes).
    SetTrackLayout {
        track: TrackId,
        layout: ChannelLayout,
    },
    AddSend {
        track: TrackId,
        send: AuxSend,
        index: Option<usize>,
    },
    RemoveSend {
        track: TrackId,
        send: SendId,
    },
    SetSendLevel {
        track: TrackId,
        send: SendId,
        db: f32,
    },
    SetSendTap {
        track: TrackId,
        send: SendId,
        tap: SendTap,
    },
    SetSendEnabled {
        track: TrackId,
        send: SendId,
        enabled: bool,
    },

    // --- plugins -----------------------------------------------------------
    InsertPlugin {
        track: TrackId,
        index: usize,
        slot: PluginSlot,
    },
    RemovePlugin {
        track: TrackId,
        plugin: PluginInstanceId,
    },
    SetPluginBypass {
        track: TrackId,
        plugin: PluginInstanceId,
        bypass: bool,
    },
    /// A parameter value stored in the slot (plain units); `None` drops the
    /// explicit value, leaving the plugin's own (state) value.
    SetPluginParameter {
        track: TrackId,
        plugin: PluginInstanceId,
        parameter: ParameterId,
        value: Option<f64>,
    },
    SetPreamp {
        track: TrackId,
        slot: Option<PluginSlot>,
    },
    SetInstrument {
        track: TrackId,
        slot: Option<PluginSlot>,
    },
    /// Feed `source`'s signal (after its inserts, before its fader, mute
    /// and solo) to a plugin's sidechain input (`None`: none).
    SetPluginSidechain {
        track: TrackId,
        plugin: PluginInstanceId,
        source: Option<TrackId>,
    },
    /// Add a track group (members join with `SetTrackGroup`).
    AddGroup {
        group: Box<crate::TrackGroup>,
    },
    /// Remove a group definition (members are released first by the
    /// session).
    RemoveGroup {
        group: faderframe_core::GroupId,
    },
    /// Replace a group's name, colour, active state or links.
    UpdateGroup {
        group: Box<crate::TrackGroup>,
    },
    SetTrackGroup {
        track: TrackId,
        group: Option<faderframe_core::GroupId>,
    },
    /// Assign a track to a VCA fader (`None`: none).
    SetTrackVca {
        track: TrackId,
        vca: Option<TrackId>,
    },
    /// A container's chains, all at once (devices added, removed or moved,
    /// chains added, removed or renamed); `None` takes the entry away.
    SetContainer {
        track: TrackId,
        container: PluginInstanceId,
        chains: Option<Vec<crate::container::Chain>>,
    },
    /// A container chain's level, pan, mute and solo (a drag coalesces).
    SetChainMix {
        track: TrackId,
        container: PluginInstanceId,
        chain: usize,
        gain_db: f32,
        pan: f32,
        mute: bool,
        solo: bool,
    },
    /// A track's modulators, all at once (a knob's drag coalesces).
    SetModulators {
        track: TrackId,
        modulators: Vec<crate::modulation::Modulator>,
    },
    /// Put a track into a folder track (`None`: out of any).
    SetTrackFolder {
        track: TrackId,
        folder: Option<TrackId>,
    },
    /// Freeze (`Some`) or unfreeze a track.
    SetTrackFreeze {
        track: TrackId,
        freeze: Option<crate::Freeze>,
    },
    /// Replace a plugin's saved state and explicit parameter values (a
    /// preset); the running plugin follows.
    SetPluginState {
        track: TrackId,
        plugin: PluginInstanceId,
        state: Option<String>,
        parameters: Vec<crate::SavedParameter>,
    },

    /// Enable analogue crosstalk for adjacent mixer channels.
    SetCrosstalk {
        enabled: bool,
    },

    // --- track structure -------------------------------------------------
    AddTrack {
        track: Box<Track>,
        index: usize,
    },
    RemoveTrack {
        track: TrackId,
    },
    RestoreTrack(Box<RemovedTrack>),
    MoveTrack {
        track: TrackId,
        index: usize,
    },

    // --- automation --------------------------------------------------------------
    AddAutomationLane {
        track: TrackId,
        lane: Box<AutomationLane>,
    },
    RemoveAutomationLane {
        track: TrackId,
        lane: AutomationLaneId,
    },
    /// Replace a lane (points, mode, target) by id; coalesces while dragging.
    SetAutomationLane {
        track: TrackId,
        lane: Box<AutomationLane>,
    },

    // --- media -----------------------------------------------------------------
    /// Register an audio source (e.g. an imported file).
    AddSource {
        source: Box<AudioSource>,
    },
    /// Unregister an unused audio source (the media file stays on disk).
    RemoveSource {
        source: AudioSourceId,
    },

    // --- clips ---------------------------------------------------------------
    AddClip {
        clip: Box<Clip>,
    },
    RemoveClip {
        clip: ClipId,
    },
    MoveClip {
        clip: ClipId,
        track: TrackId,
        start: MusicalTime,
    },
    /// Replace a clip's start and content (trim, fades, gain, bulk note edits).
    SetClipContent {
        clip: ClipId,
        start: MusicalTime,
        content: Box<ClipContent>,
    },
    RenameClip {
        clip: ClipId,
        name: String,
    },
    SetClipMuted {
        clip: ClipId,
        muted: bool,
    },
    SplitClip {
        clip: ClipId,
        at: MusicalTime,
        new_clip: ClipId,
    },
    /// Link a clip to others sharing its content (`None`: its own again).
    SetClipLink {
        clip: ClipId,
        link: Option<faderframe_core::ClipLinkId>,
    },

    // --- MIDI notes ----------------------------------------------------------
    AddNote {
        clip: ClipId,
        note: MidiNote,
    },
    RemoveNote {
        clip: ClipId,
        note: NoteId,
    },
    UpdateNote {
        clip: ClipId,
        note: MidiNote,
    },

    // --- timeline ------------------------------------------------------------
    SetTempo {
        bpm: f64,
    },
    SetTimeline {
        timeline: Box<Timeline>,
    },
    /// Replace the key changes (normalised: sorted, no repeats).
    SetKeys {
        keys: Vec<crate::KeyChange>,
    },
    /// Replace the chord track (normalised: sorted, no overlaps).
    SetChords {
        chords: Vec<crate::ChordEvent>,
    },
    /// Replace the lyrics (normalised: sorted, no empty lines).
    SetLyrics {
        lyrics: Vec<crate::lyrics::LyricLine>,
    },
    /// Put a clip in a launcher slot (`None`: empty it); the clip it held
    /// goes.
    SetLauncherSlot {
        track: TrackId,
        scene: faderframe_core::SceneId,
        clip: Option<Box<Clip>>,
    },
    /// The launcher's scenes (their slots stay keyed by id).
    SetScenes {
        scenes: Vec<crate::launcher::Scene>,
    },
    SetLaunchQuantize {
        quantize: crate::launcher::LaunchQuantize,
    },
    /// A slot's follow action (`None`: none).
    SetFollowAction {
        track: TrackId,
        scene: faderframe_core::SceneId,
        follow: Option<crate::launcher::FollowAction>,
    },
    /// How slot recordings go: their length in bars (0: until ended) and
    /// bars counted in from stop.
    SetLaunchRecording {
        bars: u16,
        count_in: u8,
    },
    /// A slot's launch settings (`None`: the defaults).
    SetClipLaunch {
        track: TrackId,
        scene: faderframe_core::SceneId,
        launch: Option<crate::launcher::ClipLaunch>,
    },
    /// Replace everything that lives in time (clips, automation, tempo and
    /// meter, markers, sections, loop and punch) at once: section moves,
    /// copies and deletes (see [`crate::arrange`]).
    SetArrangement {
        arrangement: Box<crate::arrange::Arrangement>,
    },
    /// Replace the album (songs and delivery settings).
    SetAlbum {
        album: Box<crate::album::Album>,
    },
    /// Replace an album song's inserts (its own plugin chain; the engine
    /// hosts them, and runs the monitored song's on the master output).
    SetSongInserts {
        song: faderframe_core::SongId,
        inserts: Vec<PluginSlot>,
    },
    /// Set (`Some`) or remove (`None`) the time signature change at `bar`.
    SetTimeSignature {
        bar: i32,
        signature: Option<faderframe_timeline::TimeSignature>,
    },
    SetLoop {
        range: Option<MusicalRange>,
        enabled: bool,
    },
    SetPunch {
        range: Option<MusicalRange>,
        enabled: bool,
    },
    AddMarker {
        marker: Marker,
    },
    RemoveMarker {
        marker: MarkerId,
    },
    /// Move or rename a marker.
    UpdateMarker {
        marker: Marker,
    },
    AddSection {
        section: crate::Section,
    },
    RemoveSection {
        section: faderframe_core::SectionId,
    },
    /// Move, resize, rename or recolour a section.
    UpdateSection {
        section: crate::Section,
    },
    /// Add a controller mapping at `index` in the mapping list.
    AddMidiMapping {
        index: usize,
        mapping: crate::MidiMapping,
    },
    RemoveMidiMapping {
        mapping: faderframe_core::MidiMappingId,
    },
    /// Replace a mapping (same id), e.g. its mode.
    UpdateMidiMapping {
        mapping: crate::MidiMapping,
    },
    RenameProject {
        name: String,
    },

    /// Several commands applied atomically (all or nothing).
    Batch {
        label: String,
        commands: Vec<Command>,
    },
}

fn track_mut(p: &mut Project, id: TrackId) -> Result<&mut Track, EditError> {
    p.track_mut(id).ok_or(EditError::UnknownTrack(id))
}

/// A plugin slot of `track` — or one of an album song's inserts (those
/// commands name a track, e.g. the master, that does not own them).
fn plugin_slot_mut(
    p: &mut Project,
    track: TrackId,
    plugin: PluginInstanceId,
) -> Result<&mut PluginSlot, EditError> {
    if p.album.insert(plugin).is_some() {
        return p
            .album
            .insert_mut(plugin)
            .ok_or(EditError::UnknownPlugin(plugin));
    }
    track_mut(p, track)?
        .plugin_mut(plugin)
        .ok_or(EditError::UnknownPlugin(plugin))
}

fn clip_mut(p: &mut Project, id: ClipId) -> Result<&mut Clip, EditError> {
    p.clip_mut(id).ok_or(EditError::UnknownClip(id))
}

fn clamp_level(db: f32) -> f32 {
    if db.is_nan() {
        0.0
    } else {
        db.clamp(SILENCE_DB, MAX_LEVEL_DB)
    }
}

fn check_clip_fits(p: &Project, track: TrackId, content: &ClipContent) -> Result<(), EditError> {
    let t = p.track(track).ok_or(EditError::UnknownTrack(track))?;
    let ok = match content {
        ClipContent::Audio(_) | ClipContent::Takes(_) => t.kind == TrackKind::Audio,
        ClipContent::Midi(_) => matches!(t.kind, TrackKind::Instrument | TrackKind::Midi),
    };
    if ok {
        Ok(())
    } else {
        Err(EditError::Invalid(format!(
            "a {} clip cannot be placed on {} track '{}'",
            if content.is_audio() { "audio" } else { "MIDI" },
            t.kind.label(),
            t.name
        )))
    }
}

/// Every referenced source must exist; take folders are normalised.
fn check_sources(p: &Project, content: &mut ClipContent) -> Result<(), EditError> {
    if let Some(s) = content
        .sources()
        .into_iter()
        .find(|s| !p.sources.contains_key(s))
    {
        return Err(EditError::UnknownSource(s));
    }
    if let ClipContent::Takes(f) = content {
        f.normalize();
    }
    Ok(())
}

fn check_output(p: &Project, track: TrackId, output: OutputRouting) -> Result<(), EditError> {
    let src = p.track(track).ok_or(EditError::UnknownTrack(track))?;
    let target = match output {
        OutputRouting::Track { track: t } => t,
        OutputRouting::Master => {
            if src.kind == TrackKind::Master {
                return Err(EditError::InvalidRouting(
                    "the master cannot feed itself".into(),
                ));
            }
            p.master_id()
                .ok_or_else(|| EditError::InvalidRouting("project has no master".into()))?
        }
        OutputRouting::Hardware { .. } | OutputRouting::None => return Ok(()),
    };
    let dst = p.track(target).ok_or(EditError::UnknownTrack(target))?;
    if src.kind == TrackKind::Master {
        return Err(EditError::InvalidRouting(
            "the master can only feed hardware outputs".into(),
        ));
    }
    let compatible = if src.kind == TrackKind::Midi {
        dst.kind == TrackKind::Instrument
    } else {
        dst.kind.is_summing()
    };
    if !compatible {
        return Err(EditError::InvalidRouting(format!(
            "'{}' ({}) cannot feed '{}' ({})",
            src.name,
            src.kind.label(),
            dst.name,
            dst.kind.label()
        )));
    }
    if p.would_cycle(track, target) {
        return Err(EditError::FeedbackLoop);
    }
    Ok(())
}

fn check_send_target(p: &Project, track: TrackId, target: TrackId) -> Result<(), EditError> {
    let src = p.track(track).ok_or(EditError::UnknownTrack(track))?;
    let dst = p.track(target).ok_or(EditError::UnknownTrack(target))?;
    if !src.kind.has_audio() || src.kind == TrackKind::Master {
        return Err(EditError::InvalidRouting(format!(
            "'{}' cannot have sends",
            src.name
        )));
    }
    if !matches!(dst.kind, TrackKind::Bus | TrackKind::Aux) {
        return Err(EditError::InvalidRouting(format!(
            "sends must target a bus or aux, not '{}'",
            dst.name
        )));
    }
    if p.would_cycle(track, target) {
        return Err(EditError::FeedbackLoop);
    }
    Ok(())
}

impl Command {
    /// Human-readable label for undo/redo menus.
    pub fn label(&self) -> String {
        use Command::*;
        match self {
            SetTrackVolume { .. } => "Change Volume".into(),
            SetTrackPan { .. } => "Change Pan".into(),
            SetTrackSurround { .. } => "Change Surround Pan".into(),
            SetTrackMute { .. } => "Toggle Mute".into(),
            SetTrackSolo { .. } => "Toggle Solo".into(),
            SetTrackRecordArm { .. } => "Toggle Record Arm".into(),
            SetTrackMonitor { .. } => "Change Monitoring".into(),
            SetTrackMidiOutput { .. } => "Change MIDI Output".into(),
            SetTrackMpe { .. } => "Change MPE".into(),
            SetTrackPhaseInvert { .. } => "Toggle Phase Invert".into(),
            RenameTrack { .. } => "Rename Track".into(),
            SetTrackColor { .. } => "Change Track Colour".into(),
            SetTrackOutput { .. } => "Change Output".into(),
            SetTrackInput { .. } => "Change Input".into(),
            SetTrackLayout { .. } => "Change Track Format".into(),
            AddSend { .. } => "Add Send".into(),
            RemoveSend { .. } => "Remove Send".into(),
            SetSendLevel { .. } => "Change Send Level".into(),
            SetSendTap { .. } => "Change Send Position".into(),
            SetSendEnabled { .. } => "Toggle Send".into(),
            InsertPlugin { .. } => "Insert Plugin".into(),
            RemovePlugin { .. } => "Remove Plugin".into(),
            SetPluginBypass { .. } => "Toggle Bypass".into(),
            SetPluginParameter { .. } => "Change Plugin Parameter".into(),
            SetPluginState { .. } => "Load Preset".into(),
            SetPluginSidechain { .. } => "Set Sidechain".into(),
            AddGroup { .. } => "Add Group".into(),
            RemoveGroup { .. } => "Delete Group".into(),
            UpdateGroup { .. } => "Change Group".into(),
            SetTrackGroup { group: Some(_), .. } => "Add to Group".into(),
            SetTrackGroup { group: None, .. } => "Remove from Group".into(),
            SetTrackVca { .. } => "Assign VCA".into(),
            SetModulators { .. } => "Change Modulators".into(),
            SetContainer { .. } => "Change Container".into(),
            SetChainMix { .. } => "Chain Mix".into(),
            SetTrackFolder {
                folder: Some(_), ..
            } => "Move to Folder".into(),
            SetTrackFolder { folder: None, .. } => "Move out of Folder".into(),
            SetTrackFreeze {
                freeze: Some(_), ..
            } => "Freeze Track".into(),
            SetTrackFreeze { freeze: None, .. } => "Unfreeze Track".into(),
            SetPreamp { .. } => "Change Preamp".into(),
            SetInstrument { .. } => "Change Instrument".into(),
            AddTrack { .. } | RestoreTrack(_) => "Add Track".into(),
            RemoveTrack { .. } => "Remove Track".into(),
            MoveTrack { .. } => "Move Track".into(),
            SetCrosstalk { .. } => "Analogue Crosstalk".into(),
            AddAutomationLane { .. } => "Add Automation Lane".into(),
            RemoveAutomationLane { .. } => "Remove Automation Lane".into(),
            SetAutomationLane { .. } => "Edit Automation".into(),
            AddSource { .. } => "Add Audio".into(),
            RemoveSource { .. } => "Remove Audio".into(),
            AddClip { .. } => "Add Clip".into(),
            SetClipLink { link: Some(_), .. } => "Link Clips".into(),
            SetClipLink { link: None, .. } => "Make Clip Unique".into(),
            RemoveClip { .. } => "Delete Clip".into(),
            MoveClip { .. } => "Move Clip".into(),
            SetClipContent { .. } => "Edit Clip".into(),
            RenameClip { .. } => "Rename Clip".into(),
            SetClipMuted { .. } => "Toggle Clip Mute".into(),
            SplitClip { .. } => "Split Clip".into(),
            AddNote { .. } => "Add Note".into(),
            RemoveNote { .. } => "Delete Note".into(),
            UpdateNote { .. } => "Edit Note".into(),
            SetTempo { .. } => "Change Tempo".into(),
            SetTimeline { .. } => "Change Tempo Map".into(),
            SetKeys { .. } => "Change Key".into(),
            SetChords { .. } => "Edit Chords".into(),
            SetLyrics { .. } => "Edit Lyrics".into(),
            SetLauncherSlot { .. } => "Launcher Clip".into(),
            SetScenes { .. } => "Scenes".into(),
            SetLaunchQuantize { .. } => "Launch Quantize".into(),
            SetFollowAction { .. } => "Follow Action".into(),
            SetClipLaunch { .. } => "Clip Launch".into(),
            SetLaunchRecording { .. } => "Slot Recording".into(),
            SetArrangement { .. } => "Rearrange".into(),
            SetAlbum { .. } => "Edit Album".into(),
            SetSongInserts { .. } => "Change Song Inserts".into(),
            SetTimeSignature { .. } => "Change Time Signature".into(),
            SetLoop { .. } => "Change Loop".into(),
            SetPunch { .. } => "Change Punch Range".into(),
            AddMarker { .. } => "Add Marker".into(),
            RemoveMarker { .. } => "Remove Marker".into(),
            UpdateMarker { .. } => "Edit Marker".into(),
            AddSection { .. } => "Add Section".into(),
            RemoveSection { .. } => "Delete Section".into(),
            UpdateSection { .. } => "Edit Section".into(),
            AddMidiMapping { .. } => "MIDI Learn".into(),
            RemoveMidiMapping { .. } => "Remove MIDI Mapping".into(),
            UpdateMidiMapping { .. } => "Change MIDI Mapping".into(),
            RenameProject { .. } => "Rename Project".into(),
            Batch { label, .. } => label.clone(),
        }
    }

    pub fn coalesce_key(&self) -> Option<CoalesceKey> {
        use Command::*;
        Some(match self {
            SetTrackVolume { track, .. } => CoalesceKey::TrackVolume(*track),
            SetTrackPan { track, .. } => CoalesceKey::TrackPan(*track),
            SetTrackSurround { track, .. } => CoalesceKey::TrackSurround(*track),
            SetSendLevel { track, send, .. } => CoalesceKey::SendLevel(*track, *send),
            MoveClip { clip, .. } | SetClipContent { clip, .. } => CoalesceKey::Clip(*clip),
            UpdateNote { clip, note } => CoalesceKey::Note(*clip, note.id),
            SetTempo { .. } => CoalesceKey::Tempo,
            SetLoop { .. } => CoalesceKey::Loop,
            SetPunch { .. } => CoalesceKey::Punch,
            UpdateMarker { marker } => CoalesceKey::Marker(marker.id),
            UpdateSection { section } => CoalesceKey::Section(section.id),
            SetAutomationLane { track, lane } => CoalesceKey::Automation(*track, lane.id),
            SetModulators { track, .. } => CoalesceKey::Modulators(*track),
            SetChainMix {
                container, chain, ..
            } => CoalesceKey::ChainMix(*container, *chain),
            SetPluginParameter {
                plugin, parameter, ..
            } => CoalesceKey::PluginParameter(*plugin, *parameter),
            _ => return None,
        })
    }

    pub fn impact(&self) -> Impact {
        use Command::*;
        match self {
            SetTrackVolume { .. }
            | SetTrackPan { .. }
            | SetTrackSurround { .. }
            | SetTrackMute { .. }
            | SetTrackSolo { .. }
            | SetTrackPhaseInvert { .. }
            | SetSendLevel { .. }
            | SetPluginParameter { .. }
            | SetPluginState { .. } => Impact::Params,
            RenameTrack { .. }
            | SetTrackColor { .. }
            | AddMarker { .. }
            | RemoveMarker { .. }
            | UpdateMarker { .. }
            | AddSection { .. }
            | RemoveSection { .. }
            | UpdateSection { .. }
            | AddMidiMapping { .. }
            | RemoveMidiMapping { .. }
            | UpdateMidiMapping { .. }
            | SetPunch { .. }
            | RenameProject { .. }
            | RenameClip { .. }
            | AddGroup { .. }
            | RemoveGroup { .. }
            | UpdateGroup { .. }
            | SetTrackGroup { .. }
            | SetClipLink { .. }
            | SetAlbum { .. }
            | SetLyrics { .. }
            | SetScenes { .. }
            | SetLaunchQuantize { .. }
            | SetClipLaunch { .. }
            | SetLaunchRecording { .. } => Impact::None,
            AddSource { .. }
            | RemoveSource { .. }
            | AddAutomationLane { .. }
            | RemoveAutomationLane { .. }
            | SetAutomationLane { .. }
            | AddClip { .. }
            | RemoveClip { .. }
            | MoveClip { .. }
            | SetClipContent { .. }
            | SetClipMuted { .. }
            | SplitClip { .. }
            | AddNote { .. }
            | RemoveNote { .. }
            | UpdateNote { .. }
            | SetTempo { .. }
            | SetTimeline { .. }
            | SetKeys { .. }
            | SetChords { .. }
            | SetLauncherSlot { .. }
            | SetFollowAction { .. }
            | SetArrangement { .. }
            | SetTimeSignature { .. }
            | SetLoop { .. } => Impact::Timeline,
            SetCrosstalk { .. }
            | MoveTrack { .. }
            | SetTrackRecordArm { .. }
            | SetTrackMonitor { .. }
            | SetTrackMidiOutput { .. }
            | SetTrackMpe { .. }
            | SetTrackOutput { .. }
            | SetTrackInput { .. }
            | SetTrackLayout { .. }
            | AddSend { .. }
            | RemoveSend { .. }
            | SetSendTap { .. }
            | SetSendEnabled { .. }
            | InsertPlugin { .. }
            | RemovePlugin { .. }
            | SetPluginBypass { .. }
            | SetSongInserts { .. }
            | SetPreamp { .. }
            | SetInstrument { .. }
            | AddTrack { .. }
            | RemoveTrack { .. }
            | RestoreTrack(_)
            | SetTrackFreeze { .. }
            | SetPluginSidechain { .. } => Impact::Graph,
            // VCA automation travels with the timeline snapshot.
            SetTrackVca { .. } => Impact::Timeline,
            // Folder mutes and solos reach the tracks inside.
            SetTrackFolder { .. } => Impact::Params,
            // The engine's modulation table (the graph when a track gains
            // or loses its modulators: escalated by the controller).
            SetModulators { .. } => Impact::Params,
            // Chains are graph structure; their mix is parameter slots.
            SetContainer { .. } => Impact::Graph,
            SetChainMix { .. } => Impact::Params,
            Batch { commands, .. } => commands
                .iter()
                .map(Command::impact)
                .max()
                .unwrap_or(Impact::None),
        }
    }

    /// Apply to `project`; on success returns the inverse command.
    /// On error the project is unchanged.
    pub fn apply(self, p: &mut Project) -> Result<Command, EditError> {
        use Command::*;
        Ok(match self {
            SetTrackVolume { track, db } => {
                let t = track_mut(p, track)?;
                let old = std::mem::replace(&mut t.volume_db, clamp_level(db));
                SetTrackVolume { track, db: old }
            }
            SetTrackPan { track, pan } => {
                let t = track_mut(p, track)?;
                let pan = if pan.is_nan() {
                    0.0
                } else {
                    pan.clamp(-1.0, 1.0)
                };
                let old = std::mem::replace(&mut t.pan, pan);
                SetTrackPan { track, pan: old }
            }
            SetTrackSurround { track, pan } => {
                let t = track_mut(p, track)?;
                let pan = if [pan.x, pan.y, pan.z, pan.spread, pan.width, pan.lfe_db]
                    .iter()
                    .any(|v| v.is_nan())
                {
                    faderframe_core::SurroundPan::default()
                } else {
                    pan.clamped()
                };
                let old = std::mem::replace(&mut t.surround, pan);
                SetTrackSurround { track, pan: old }
            }
            SetTrackMute { track, on } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.mute, on);
                SetTrackMute { track, on: old }
            }
            SetTrackSolo { track, on } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.solo, on);
                SetTrackSolo { track, on: old }
            }
            SetTrackRecordArm { track, on } => {
                let t = track_mut(p, track)?;
                if on
                    && !matches!(
                        t.kind,
                        TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
                    )
                {
                    return Err(EditError::Invalid(format!(
                        "'{}' cannot be record-armed",
                        t.name
                    )));
                }
                let old = std::mem::replace(&mut t.record_arm, on);
                SetTrackRecordArm { track, on: old }
            }
            SetTrackMidiOutput { track, output } => {
                let t = track_mut(p, track)?;
                let old = std::mem::replace(&mut t.midi_output, output);
                SetTrackMidiOutput { track, output: old }
            }
            SetTrackMpe { track, mpe } => {
                let mpe = mpe.map(|m| crate::MpeConfig {
                    members: m.members.clamp(1, 15),
                    bend_range: m.bend_range.clamp(1, 96),
                });
                let old = std::mem::replace(&mut track_mut(p, track)?.mpe, mpe);
                SetTrackMpe { track, mpe: old }
            }
            SetTrackMonitor { track, mode } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.monitor, mode);
                SetTrackMonitor { track, mode: old }
            }
            SetTrackPhaseInvert { track, on } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.phase_invert, on);
                SetTrackPhaseInvert { track, on: old }
            }
            RenameTrack { track, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(EditError::Invalid("track names cannot be empty".into()));
                }
                let old = std::mem::replace(&mut track_mut(p, track)?.name, name);
                RenameTrack { track, name: old }
            }
            SetTrackColor { track, color } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.color, color);
                SetTrackColor { track, color: old }
            }
            SetTrackOutput { track, output } => {
                check_output(p, track, output)?;
                let old = std::mem::replace(&mut track_mut(p, track)?.output, output);
                SetTrackOutput { track, output: old }
            }
            SetTrackInput { track, input } => {
                let old = std::mem::replace(&mut track_mut(p, track)?.input, input);
                SetTrackInput { track, input: old }
            }
            SetTrackLayout { track, layout } => {
                let t = track_mut(p, track)?;
                if t.kind == TrackKind::Master && layout.channel_count() < 2 {
                    return Err(EditError::Invalid("the master must be stereo".into()));
                }
                let old = std::mem::replace(&mut t.layout, layout);
                SetTrackLayout { track, layout: old }
            }
            AddSend {
                track,
                mut send,
                index,
            } => {
                check_send_target(p, track, send.target)?;
                send.level_db = clamp_level(send.level_db);
                let t = track_mut(p, track)?;
                if t.sends.iter().any(|s| s.id == send.id) {
                    return Err(EditError::Invalid(format!("duplicate send id {}", send.id)));
                }
                let id = send.id;
                let i = index.unwrap_or(t.sends.len()).min(t.sends.len());
                t.sends.insert(i, send);
                RemoveSend { track, send: id }
            }
            RemoveSend { track, send } => {
                let t = track_mut(p, track)?;
                let i = t
                    .sends
                    .iter()
                    .position(|s| s.id == send)
                    .ok_or(EditError::UnknownSend(send))?;
                let removed = t.sends.remove(i);
                AddSend {
                    track,
                    send: removed,
                    index: Some(i),
                }
            }
            SetSendLevel { track, send, db } => {
                let s = track_mut(p, track)?
                    .send_mut(send)
                    .ok_or(EditError::UnknownSend(send))?;
                let old = std::mem::replace(&mut s.level_db, clamp_level(db));
                SetSendLevel {
                    track,
                    send,
                    db: old,
                }
            }
            SetSendTap { track, send, tap } => {
                let s = track_mut(p, track)?
                    .send_mut(send)
                    .ok_or(EditError::UnknownSend(send))?;
                let old = std::mem::replace(&mut s.tap, tap);
                SetSendTap {
                    track,
                    send,
                    tap: old,
                }
            }
            SetSendEnabled {
                track,
                send,
                enabled,
            } => {
                let target = p
                    .track(track)
                    .ok_or(EditError::UnknownTrack(track))?
                    .send(send)
                    .ok_or(EditError::UnknownSend(send))?
                    .target;
                if enabled && p.would_cycle(track, target) {
                    return Err(EditError::FeedbackLoop);
                }
                let s = track_mut(p, track)?
                    .send_mut(send)
                    .ok_or(EditError::UnknownSend(send))?;
                let old = std::mem::replace(&mut s.enabled, enabled);
                SetSendEnabled {
                    track,
                    send,
                    enabled: old,
                }
            }
            InsertPlugin { track, index, slot } => {
                let t = track_mut(p, track)?;
                let id = slot.id;
                let i = index.min(t.inserts.len());
                t.inserts.insert(i, slot);
                RemovePlugin { track, plugin: id }
            }
            RemovePlugin { track, plugin } => {
                let t = track_mut(p, track)?;
                let i = t
                    .inserts
                    .iter()
                    .position(|s| s.id == plugin)
                    .ok_or(EditError::UnknownPlugin(plugin))?;
                let slot = t.inserts.remove(i);
                InsertPlugin {
                    track,
                    index: i,
                    slot,
                }
            }
            SetPluginBypass {
                track,
                plugin,
                bypass,
            } => {
                let slot = plugin_slot_mut(p, track, plugin)?;
                let old = std::mem::replace(&mut slot.bypass, bypass);
                SetPluginBypass {
                    track,
                    plugin,
                    bypass: old,
                }
            }
            SetPluginParameter {
                track,
                plugin,
                parameter,
                value,
            } => {
                let slot = plugin_slot_mut(p, track, plugin)?;
                let i = slot.parameters.iter().position(|q| q.id == parameter);
                let old = i.map(|i| slot.parameters[i].value);
                match (i, value) {
                    (Some(i), Some(v)) => slot.parameters[i].value = v,
                    (None, Some(v)) => slot.parameters.push(crate::SavedParameter {
                        id: parameter,
                        value: v,
                    }),
                    (Some(i), None) => {
                        slot.parameters.remove(i);
                    }
                    (None, None) => {}
                }
                SetPluginParameter {
                    track,
                    plugin,
                    parameter,
                    value: old,
                }
            }
            SetPluginSidechain {
                track,
                plugin,
                source,
            } => {
                if let Some(src) = source {
                    let ok = p.track(src).is_some_and(|t| t.kind.has_audio());
                    if !ok {
                        return Err(EditError::Invalid(
                            "the sidechain source has no audio".into(),
                        ));
                    }
                    if p.would_cycle(src, track) {
                        return Err(EditError::Invalid(
                            "that sidechain would create a feedback loop".into(),
                        ));
                    }
                }
                let slot = track_mut(p, track)?
                    .plugin_mut(plugin)
                    .ok_or(EditError::UnknownPlugin(plugin))?;
                let old = std::mem::replace(&mut slot.sidechain, source);
                SetPluginSidechain {
                    track,
                    plugin,
                    source: old,
                }
            }
            AddGroup { group } => {
                if p.group(group.id).is_some() {
                    return Err(EditError::Invalid(format!("group {} exists", group.id)));
                }
                let id = group.id;
                p.groups.push(*group);
                RemoveGroup { group: id }
            }
            RemoveGroup { group } => {
                let i = p
                    .groups
                    .iter()
                    .position(|g| g.id == group)
                    .ok_or_else(|| EditError::Invalid(format!("unknown group {group}")))?;
                AddGroup {
                    group: Box::new(p.groups.remove(i)),
                }
            }
            UpdateGroup { group } => {
                let g = p
                    .groups
                    .iter_mut()
                    .find(|g| g.id == group.id)
                    .ok_or_else(|| EditError::Invalid(format!("unknown group {}", group.id)))?;
                UpdateGroup {
                    group: Box::new(std::mem::replace(g, *group)),
                }
            }
            SetTrackGroup { track, group } => {
                if let Some(g) = group
                    && p.group(g).is_none()
                {
                    return Err(EditError::Invalid(format!("unknown group {g}")));
                }
                let old = std::mem::replace(&mut track_mut(p, track)?.group, group);
                SetTrackGroup { track, group: old }
            }
            SetTrackVca { track, vca } => {
                if let Some(v) = vca {
                    let ok = p.track(v).is_some_and(|t| t.kind == TrackKind::Vca);
                    if !ok {
                        return Err(EditError::Invalid("that track is not a VCA".into()));
                    }
                    // The VCA must not (through its own VCAs) be scaled by
                    // this track.
                    let mut next = Some(v);
                    let mut steps = 0;
                    while let Some(id) = next {
                        if id == track || steps > p.tracks.len() {
                            return Err(EditError::Invalid("a VCA cannot control itself".into()));
                        }
                        next = p.track(id).and_then(|t| t.vca);
                        steps += 1;
                    }
                }
                let t = track_mut(p, track)?;
                if t.kind == TrackKind::Master && vca.is_some() {
                    return Err(EditError::Invalid("the master cannot follow a VCA".into()));
                }
                let old = std::mem::replace(&mut t.vca, vca);
                SetTrackVca { track, vca: old }
            }
            SetContainer {
                track,
                container,
                chains,
            } => {
                if chains
                    .as_ref()
                    .is_some_and(|c| c.len() > crate::container::MAX_CHAINS)
                {
                    return Err(EditError::Invalid(format!(
                        "a container has at most {} chains",
                        crate::container::MAX_CHAINS
                    )));
                }
                let t = track_mut(p, track)?;
                let old = match chains {
                    Some(c) => t.containers.insert(container, c),
                    None => t.containers.remove(&container),
                };
                SetContainer {
                    track,
                    container,
                    chains: old,
                }
            }
            SetChainMix {
                track,
                container,
                chain,
                gain_db,
                pan,
                mute,
                solo,
            } => {
                let t = track_mut(p, track)?;
                let c = t
                    .containers
                    .get_mut(&container)
                    .and_then(|c| c.get_mut(chain))
                    .ok_or(EditError::UnknownPlugin(container))?;
                let old = SetChainMix {
                    track,
                    container,
                    chain,
                    gain_db: c.gain_db,
                    pan: c.pan,
                    mute: c.mute,
                    solo: c.solo,
                };
                c.gain_db = clamp_level(gain_db);
                c.pan = pan.clamp(-1.0, 1.0);
                c.mute = mute;
                c.solo = solo;
                old
            }
            SetModulators { track, modulators } => {
                if modulators.len() > crate::modulation::MAX_MODULATORS {
                    return Err(EditError::Invalid(format!(
                        "a track has at most {} modulators",
                        crate::modulation::MAX_MODULATORS
                    )));
                }
                let t = track_mut(p, track)?;
                let old = std::mem::replace(&mut t.modulators, modulators);
                SetModulators {
                    track,
                    modulators: old,
                }
            }
            SetTrackFolder { track, folder } => {
                if let Some(f) = folder {
                    let ok = p.track(f).is_some_and(|t| t.kind == TrackKind::Folder);
                    if !ok {
                        return Err(EditError::Invalid("that track is not a folder".into()));
                    }
                    // Not into itself or a folder inside it.
                    let mut next = Some(f);
                    let mut steps = 0;
                    while let Some(id) = next {
                        if id == track || steps > p.tracks.len() {
                            return Err(EditError::Invalid(
                                "a folder cannot go inside itself".into(),
                            ));
                        }
                        next = p.track(id).and_then(|t| t.folder);
                        steps += 1;
                    }
                }
                let t = track_mut(p, track)?;
                if t.kind == TrackKind::Master && folder.is_some() {
                    return Err(EditError::Invalid(
                        "the master cannot go into a folder".into(),
                    ));
                }
                let old = std::mem::replace(&mut t.folder, folder);
                SetTrackFolder { track, folder: old }
            }
            SetTrackFreeze { track, freeze } => {
                let t = track_mut(p, track)?;
                let old = std::mem::replace(&mut t.freeze, freeze);
                SetTrackFreeze { track, freeze: old }
            }
            SetPluginState {
                track,
                plugin,
                state,
                parameters,
            } => {
                let slot = plugin_slot_mut(p, track, plugin)?;
                let old_state = std::mem::replace(&mut slot.state, state);
                let old_params = std::mem::replace(&mut slot.parameters, parameters);
                SetPluginState {
                    track,
                    plugin,
                    state: old_state,
                    parameters: old_params,
                }
            }
            SetPreamp { track, slot } => {
                let t = track_mut(p, track)?;
                if let Some(s) = &slot
                    && (!t.kind.has_audio()
                        || s.plugin.format != crate::PluginFormat::Builtin
                        || faderframe_core::builtin::preamp_index(&s.plugin.id).is_none())
                {
                    return Err(EditError::Invalid("Invalid microphone preamp slot".into()));
                }
                let old = std::mem::replace(&mut t.preamp, slot);
                SetPreamp { track, slot: old }
            }
            SetInstrument { track, slot } => {
                let t = track_mut(p, track)?;
                if slot.is_some() && t.kind != TrackKind::Instrument {
                    return Err(EditError::Invalid(format!(
                        "'{}' is not an instrument track",
                        t.name
                    )));
                }
                let old = std::mem::replace(&mut t.instrument, slot);
                SetInstrument { track, slot: old }
            }
            AddTrack { track, index } => {
                if p.track(track.id).is_some() {
                    return Err(EditError::Invalid(format!(
                        "duplicate track id {}",
                        track.id
                    )));
                }
                if track.kind == TrackKind::Master && p.master().is_some() {
                    return Err(EditError::Invalid(
                        "a project has exactly one master".into(),
                    ));
                }
                let id = track.id;
                let mut track = *track;
                track.clips.clear();
                let i = index.min(p.tracks.len());
                p.tracks.insert(i, track);
                RemoveTrack { track: id }
            }
            RemoveTrack { track } => {
                let index = p.track_index(track).ok_or(EditError::UnknownTrack(track))?;
                if p.tracks[index].kind == TrackKind::Master {
                    return Err(EditError::CannotRemoveMaster);
                }
                let removed = p.tracks.remove(index);
                let clips = removed
                    .clips
                    .iter()
                    .filter_map(|c| p.clips.remove(c))
                    .collect();
                let mut rerouted = Vec::new();
                let mut removed_sends = Vec::new();
                for t in &mut p.tracks {
                    if t.output == (OutputRouting::Track { track }) {
                        rerouted.push((t.id, t.output));
                        t.output = OutputRouting::Master;
                    }
                    for i in (0..t.sends.len()).rev() {
                        if t.sends[i].target == track {
                            let s = t.sends.remove(i);
                            removed_sends.push((t.id, i, s));
                        }
                    }
                }
                removed_sends.reverse();
                let keys: Vec<crate::launcher::SlotKey> = p
                    .launcher
                    .slots
                    .keys()
                    .filter(|k| k.track == track)
                    .copied()
                    .collect();
                let slots = keys
                    .into_iter()
                    .filter_map(|k| {
                        let id = p.launcher.slots.remove(&k)?;
                        Some((k.scene, p.clips.remove(&id)?))
                    })
                    .collect();
                RestoreTrack(Box::new(RemovedTrack {
                    track: removed,
                    index,
                    clips,
                    rerouted,
                    removed_sends,
                    slots,
                }))
            }
            RestoreTrack(r) => {
                let RemovedTrack {
                    track,
                    index,
                    clips,
                    rerouted,
                    removed_sends,
                    slots,
                } = *r;
                if p.track(track.id).is_some() {
                    return Err(EditError::Invalid(format!(
                        "duplicate track id {}",
                        track.id
                    )));
                }
                let id = track.id;
                let i = index.min(p.tracks.len());
                p.tracks.insert(i, track);
                for c in clips {
                    p.clips.insert(c.id, c);
                }
                for (scene, c) in slots {
                    p.launcher
                        .slots
                        .insert(crate::launcher::SlotKey { track: id, scene }, c.id);
                    p.clips.insert(c.id, c);
                }
                for (t, out) in rerouted {
                    if let Some(t) = p.track_mut(t) {
                        t.output = out;
                    }
                }
                // Ascending indices restore the original order.
                let mut sends = removed_sends;
                sends.sort_by_key(|(_, i, _)| *i);
                for (t, i, s) in sends {
                    if let Some(t) = p.track_mut(t) {
                        let i = i.min(t.sends.len());
                        t.sends.insert(i, s);
                    }
                }
                RemoveTrack { track: id }
            }
            SetCrosstalk { enabled } => {
                let old = std::mem::replace(&mut p.crosstalk, enabled);
                SetCrosstalk { enabled: old }
            }
            MoveTrack { track, index } => {
                let from = p.track_index(track).ok_or(EditError::UnknownTrack(track))?;
                let t = p.tracks.remove(from);
                let to = index.min(p.tracks.len());
                p.tracks.insert(to, t);
                MoveTrack { track, index: from }
            }
            AddAutomationLane { track, lane } => {
                let t = track_mut(p, track)?;
                if t.automation
                    .lanes
                    .iter()
                    .any(|l| l.id == lane.id || l.target == lane.target)
                {
                    return Err(EditError::Invalid(
                        "the track already has this automation lane".into(),
                    ));
                }
                let id = lane.id;
                t.automation.lanes.push(*lane);
                RemoveAutomationLane { track, lane: id }
            }
            RemoveAutomationLane { track, lane } => {
                let t = track_mut(p, track)?;
                let i = t
                    .automation
                    .lanes
                    .iter()
                    .position(|l| l.id == lane)
                    .ok_or_else(|| EditError::Invalid(format!("unknown automation lane {lane}")))?;
                let old = t.automation.lanes.remove(i);
                AddAutomationLane {
                    track,
                    lane: Box::new(old),
                }
            }
            SetAutomationLane { track, lane } => {
                let t = track_mut(p, track)?;
                let slot = t
                    .automation
                    .lanes
                    .iter_mut()
                    .find(|l| l.id == lane.id)
                    .ok_or_else(|| {
                        EditError::Invalid(format!("unknown automation lane {}", lane.id))
                    })?;
                let old = std::mem::replace(slot, *lane);
                SetAutomationLane {
                    track,
                    lane: Box::new(old),
                }
            }
            AddSource { source } => {
                if p.sources.contains_key(&source.id) {
                    return Err(EditError::Invalid(format!(
                        "duplicate source id {}",
                        source.id
                    )));
                }
                let id = source.id;
                p.sources.insert(id, *source);
                RemoveSource { source: id }
            }
            RemoveSource { source } => {
                if !p.sources.contains_key(&source) {
                    return Err(EditError::UnknownSource(source));
                }
                if p.clips
                    .values()
                    .any(|c| c.content.sources().contains(&source))
                {
                    return Err(EditError::SourceInUse(source));
                }
                let s = p
                    .sources
                    .remove(&source)
                    .ok_or(EditError::UnknownSource(source))?;
                AddSource {
                    source: Box::new(s),
                }
            }
            AddClip { mut clip } => {
                check_clip_fits(p, clip.track, &clip.content)?;
                check_sources(p, &mut clip.content)?;
                if p.clips.contains_key(&clip.id) {
                    return Err(EditError::Invalid(format!("duplicate clip id {}", clip.id)));
                }
                let id = clip.id;
                let track = clip.track;
                p.clips.insert(id, *clip);
                track_mut(p, track)?.clips.push(id);
                RemoveClip { clip: id }
            }
            RemoveClip { clip } => {
                let c = p.clips.remove(&clip).ok_or(EditError::UnknownClip(clip))?;
                if let Some(t) = p.track_mut(c.track) {
                    t.clips.retain(|x| *x != clip);
                }
                AddClip { clip: Box::new(c) }
            }
            SetClipLink { clip, link } => {
                if !p.clips.contains_key(&clip) {
                    return Err(EditError::Invalid(format!("no clip {clip}")));
                }
                let old = match link {
                    Some(l) => p.clip_links.insert(clip, l),
                    None => p.clip_links.remove(&clip),
                };
                SetClipLink { clip, link: old }
            }
            MoveClip { clip, track, start } => {
                let content = p
                    .clip(clip)
                    .ok_or(EditError::UnknownClip(clip))?
                    .content
                    .clone();
                check_clip_fits(p, track, &content)?;
                let c = clip_mut(p, clip)?;
                let old_track = std::mem::replace(&mut c.track, track);
                let old_start = std::mem::replace(&mut c.start, start);
                if old_track != track {
                    if let Some(t) = p.track_mut(old_track) {
                        t.clips.retain(|x| *x != clip);
                    }
                    track_mut(p, track)?.clips.push(clip);
                }
                MoveClip {
                    clip,
                    track: old_track,
                    start: old_start,
                }
            }
            SetClipContent {
                clip,
                start,
                content,
            } => {
                let track = p.clip(clip).ok_or(EditError::UnknownClip(clip))?.track;
                check_clip_fits(p, track, &content)?;
                let mut content = content;
                check_sources(p, &mut content)?;
                let c = clip_mut(p, clip)?;
                let old_start = std::mem::replace(&mut c.start, start);
                let old = std::mem::replace(&mut c.content, *content);
                SetClipContent {
                    clip,
                    start: old_start,
                    content: Box::new(old),
                }
            }
            RenameClip { clip, name } => {
                let old = std::mem::replace(&mut clip_mut(p, clip)?.name, name);
                RenameClip { clip, name: old }
            }
            SetClipMuted { clip, muted } => {
                let old = std::mem::replace(&mut clip_mut(p, clip)?.muted, muted);
                SetClipMuted { clip, muted: old }
            }
            SplitClip { clip, at, new_clip } => split_clip(p, clip, at, new_clip)?,
            AddNote { clip, note } => {
                let m = clip_mut(p, clip)?
                    .as_midi_mut()
                    .ok_or_else(|| EditError::Invalid("not a MIDI clip".into()))?;
                if m.note(note.id).is_some() {
                    return Err(EditError::Invalid(format!("duplicate note id {}", note.id)));
                }
                let id = note.id;
                m.notes.push(sanitise_note(note));
                m.sort_notes();
                RemoveNote { clip, note: id }
            }
            RemoveNote { clip, note } => {
                let m = clip_mut(p, clip)?
                    .as_midi_mut()
                    .ok_or_else(|| EditError::Invalid("not a MIDI clip".into()))?;
                let i = m
                    .notes
                    .iter()
                    .position(|n| n.id == note)
                    .ok_or(EditError::UnknownNote(note))?;
                let n = m.notes.remove(i);
                AddNote { clip, note: n }
            }
            UpdateNote { clip, note } => {
                let m = clip_mut(p, clip)?
                    .as_midi_mut()
                    .ok_or_else(|| EditError::Invalid("not a MIDI clip".into()))?;
                let n = m.note_mut(note.id).ok_or(EditError::UnknownNote(note.id))?;
                let old = std::mem::replace(n, sanitise_note(note));
                m.sort_notes();
                UpdateNote { clip, note: old }
            }
            SetTempo { bpm } => {
                if !bpm.is_finite() {
                    return Err(EditError::Invalid("tempo must be finite".into()));
                }
                let old = p.timeline.tempo.points()[0].bpm;
                p.timeline.tempo.set_initial_bpm(bpm);
                SetTempo { bpm: old }
            }
            SetTimeSignature { bar, signature } => {
                let bar = bar.max(0);
                let meter = &mut p.timeline.meter;
                let old = meter
                    .changes()
                    .iter()
                    .find(|c| c.bar == bar)
                    .map(|c| c.signature);
                match signature {
                    Some(signature) => {
                        let signature = faderframe_timeline::TimeSignature::new(
                            signature.numerator,
                            signature.denominator,
                        )
                        .ok_or_else(|| EditError::Invalid("invalid time signature".into()))?;
                        meter.set_change(faderframe_timeline::MeterChange { bar, signature });
                    }
                    None if bar == 0 => {
                        return Err(EditError::Invalid(
                            "the first time signature cannot be removed".into(),
                        ));
                    }
                    None => meter.remove_change(bar),
                }
                SetTimeSignature {
                    bar,
                    signature: old,
                }
            }
            SetTimeline { timeline } => {
                let old = std::mem::replace(&mut p.timeline, *timeline);
                SetTimeline {
                    timeline: Box::new(old),
                }
            }
            SetKeys { mut keys } => {
                crate::harmony::normalize_keys(&mut keys);
                SetKeys {
                    keys: std::mem::replace(&mut p.keys, keys),
                }
            }
            SetChords { mut chords } => {
                crate::harmony::normalize_chords(&mut chords);
                SetChords {
                    chords: std::mem::replace(&mut p.chords, chords),
                }
            }
            SetLyrics { mut lyrics } => {
                crate::lyrics::normalize(&mut lyrics);
                SetLyrics {
                    lyrics: std::mem::replace(&mut p.lyrics, lyrics),
                }
            }
            SetLauncherSlot { track, scene, clip } => {
                track_mut(p, track)?;
                let key = crate::launcher::SlotKey { track, scene };
                if let Some(c) = &clip {
                    if c.track != track {
                        return Err(EditError::Invalid("a slot's clip is on its track".into()));
                    }
                    if p.clips.contains_key(&c.id) && p.launcher.slots.get(&key) != Some(&c.id) {
                        return Err(EditError::Invalid(format!("clip {} exists", c.id)));
                    }
                    check_clip_fits(p, track, &c.content)?;
                }
                let old = p
                    .launcher
                    .slots
                    .remove(&key)
                    .and_then(|id| p.clips.remove(&id))
                    .map(Box::new);
                if let Some(c) = clip {
                    p.launcher.slots.insert(key, c.id);
                    p.clips.insert(c.id, *c);
                }
                SetLauncherSlot {
                    track,
                    scene,
                    clip: old,
                }
            }
            SetScenes { scenes } => SetScenes {
                scenes: std::mem::replace(&mut p.launcher.scenes, scenes),
            },
            SetLaunchQuantize { quantize } => SetLaunchQuantize {
                quantize: std::mem::replace(&mut p.launcher.quantize, quantize),
            },
            SetFollowAction {
                track,
                scene,
                follow,
            } => {
                let key = crate::launcher::SlotKey { track, scene };
                let old = match follow {
                    Some(f) => p.launcher.follow.insert(key, f),
                    None => p.launcher.follow.remove(&key),
                };
                SetFollowAction {
                    track,
                    scene,
                    follow: old,
                }
            }
            SetLaunchRecording { bars, count_in } => SetLaunchRecording {
                bars: std::mem::replace(&mut p.launcher.record_bars, bars),
                count_in: std::mem::replace(&mut p.launcher.count_in, count_in),
            },
            SetClipLaunch {
                track,
                scene,
                launch,
            } => {
                let key = crate::launcher::SlotKey { track, scene };
                let old = match launch.filter(|l| !l.is_default()) {
                    Some(l) => p.launcher.launch.insert(key, l),
                    None => p.launcher.launch.remove(&key),
                };
                SetClipLaunch {
                    track,
                    scene,
                    launch: old,
                }
            }
            SetArrangement { arrangement } => SetArrangement {
                arrangement: Box::new(arrangement.swap_into(p)),
            },
            SetAlbum { album } => SetAlbum {
                album: Box::new(std::mem::replace(&mut p.album, *album)),
            },
            SetSongInserts { song, inserts } => {
                let s = p
                    .album
                    .songs
                    .iter_mut()
                    .find(|s| s.id == song)
                    .ok_or_else(|| EditError::Invalid("no such song".into()))?;
                SetSongInserts {
                    song,
                    inserts: std::mem::replace(&mut s.inserts, inserts),
                }
            }
            SetLoop { range, enabled } => {
                let old_range = std::mem::replace(&mut p.loop_range, range);
                let old_enabled =
                    std::mem::replace(&mut p.loop_enabled, enabled && range.is_some());
                SetLoop {
                    range: old_range,
                    enabled: old_enabled,
                }
            }
            SetPunch { range, enabled } => {
                let old_range = std::mem::replace(&mut p.punch_range, range);
                let old_enabled =
                    std::mem::replace(&mut p.punch_enabled, enabled && range.is_some());
                SetPunch {
                    range: old_range,
                    enabled: old_enabled,
                }
            }
            AddMarker { marker } => {
                let id = marker.id;
                p.markers.push(marker);
                p.markers.sort_by_key(|m| m.position);
                RemoveMarker { marker: id }
            }
            RemoveMarker { marker } => {
                let i = p
                    .markers
                    .iter()
                    .position(|m| m.id == marker)
                    .ok_or(EditError::UnknownMarker(marker))?;
                AddMarker {
                    marker: p.markers.remove(i),
                }
            }
            UpdateMarker { marker } => {
                let m = p
                    .markers
                    .iter_mut()
                    .find(|m| m.id == marker.id)
                    .ok_or(EditError::UnknownMarker(marker.id))?;
                let old = std::mem::replace(m, marker);
                p.markers.sort_by_key(|m| m.position);
                UpdateMarker { marker: old }
            }
            AddSection { section } => {
                if section.end <= section.start {
                    return Err(EditError::Invalid("a section needs a length".into()));
                }
                let id = section.id;
                p.sections.push(section);
                p.sections.sort_by_key(|s| s.start);
                RemoveSection { section: id }
            }
            RemoveSection { section } => {
                let i = p
                    .sections
                    .iter()
                    .position(|s| s.id == section)
                    .ok_or_else(|| EditError::Invalid(format!("unknown section {section}")))?;
                AddSection {
                    section: p.sections.remove(i),
                }
            }
            UpdateSection { section } => {
                if section.end <= section.start {
                    return Err(EditError::Invalid("a section needs a length".into()));
                }
                let s = p
                    .sections
                    .iter_mut()
                    .find(|s| s.id == section.id)
                    .ok_or_else(|| EditError::Invalid(format!("unknown section {}", section.id)))?;
                let old = std::mem::replace(s, section);
                p.sections.sort_by_key(|s| s.start);
                UpdateSection { section: old }
            }
            AddMidiMapping { index, mapping } => {
                if p.midi_mappings.iter().any(|m| m.id == mapping.id) {
                    return Err(EditError::Invalid(format!(
                        "duplicate MIDI mapping {}",
                        mapping.id
                    )));
                }
                let id = mapping.id;
                p.midi_mappings
                    .insert(index.min(p.midi_mappings.len()), mapping);
                RemoveMidiMapping { mapping: id }
            }
            UpdateMidiMapping { mapping } => {
                let m = p
                    .midi_mappings
                    .iter_mut()
                    .find(|m| m.id == mapping.id)
                    .ok_or_else(|| {
                        EditError::Invalid(format!("unknown MIDI mapping {}", mapping.id))
                    })?;
                UpdateMidiMapping {
                    mapping: std::mem::replace(m, mapping),
                }
            }
            RemoveMidiMapping { mapping } => {
                let index = p
                    .midi_mappings
                    .iter()
                    .position(|m| m.id == mapping)
                    .ok_or_else(|| EditError::Invalid(format!("unknown MIDI mapping {mapping}")))?;
                AddMidiMapping {
                    index,
                    mapping: p.midi_mappings.remove(index),
                }
            }
            RenameProject { name } => {
                let old = std::mem::replace(&mut p.name, name);
                RenameProject { name: old }
            }
            Batch { label, commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                for cmd in commands {
                    match cmd.apply(p) {
                        Ok(inv) => inverses.push(inv),
                        Err(e) => {
                            // Roll back what was applied.
                            for inv in inverses.into_iter().rev() {
                                let _ = inv.apply(p);
                            }
                            return Err(e);
                        }
                    }
                }
                inverses.reverse();
                Batch {
                    label,
                    commands: inverses,
                }
            }
        })
    }
}

fn sanitise_note(mut n: MidiNote) -> MidiNote {
    n.key = n.key.min(127);
    n.velocity = n.velocity.clamp(1, 127);
    n.channel = n.channel.min(15);
    n.start = n.start.max(MusicalTime::ZERO);
    n.length = n.length.max(MusicalTime(1));
    n
}

fn split_clip(
    p: &mut Project,
    clip: ClipId,
    at: MusicalTime,
    new_clip: ClipId,
) -> Result<Command, EditError> {
    if p.clips.contains_key(&new_clip) {
        return Err(EditError::Invalid(format!("duplicate clip id {new_clip}")));
    }
    let original = p.clip(clip).ok_or(EditError::UnknownClip(clip))?.clone();
    let (left, right) = original.split_at(at, new_clip, &p.timeline, p.sample_rate)?;
    let track = original.track;
    let (left_start, left_content) = (left.start, left.content);
    clip_mut(p, clip)?.content = left_content;
    p.clips.insert(new_clip, right);
    track_mut(p, track)?.clips.push(new_clip);
    Ok(Command::Batch {
        label: "Split Clip".into(),
        commands: vec![
            Command::RemoveClip { clip: new_clip },
            Command::SetClipContent {
                clip,
                start: left_start,
                content: Box::new(original.content),
            },
        ],
    })
}

impl Clip {
    /// The two parts of this clip cut at `at` (the right one gets id
    /// `right_id`). Audio and takes are cut at the sample, MIDI notes go
    /// to the part they start in (cut short at the end of the left part),
    /// controllers carry their value across, expression goes with notes.
    pub fn split_at(
        &self,
        at: MusicalTime,
        right_id: ClipId,
        timeline: &faderframe_timeline::Timeline,
        rate: u32,
    ) -> Result<(Clip, Clip), EditError> {
        let original = self;
        let end = original.end(timeline, rate);
        if at <= original.start || at >= end {
            return Err(EditError::Invalid("split point is outside the clip".into()));
        }
        let mut left = original.clone();
        let mut right = original.clone();
        right.id = right_id;
        right.start = at;
        match (&mut left.content, &mut right.content) {
            (ClipContent::Audio(l), ClipContent::Audio(r)) => {
                let sr = rate as f64;
                let offset = timeline.to_samples(at, sr) - timeline.to_samples(original.start, sr);
                if offset <= 0 || offset >= l.length {
                    return Err(EditError::Invalid("split point is outside the clip".into()));
                }
                match l.warp.take() {
                    Some(w) => {
                        let (src, right_warp) = w.after(l.source_offset, l.length, offset);
                        r.source_offset = src;
                        r.warp = Some(right_warp);
                        l.warp = Some(w.before(l.source_offset, l.length, offset));
                    }
                    None => r.source_offset = l.source_offset + offset,
                }
                r.length = l.length - offset;
                l.length = offset;
                l.fades = ClipFades {
                    fade_out: 0,
                    ..l.fades
                };
                r.fades = ClipFades {
                    fade_in: 0,
                    ..r.fades
                };
            }
            (ClipContent::Takes(l), ClipContent::Takes(r)) => {
                let sr = rate as f64;
                let offset = timeline.to_samples(at, sr) - timeline.to_samples(original.start, sr);
                let (a, b) = l
                    .split(offset)
                    .ok_or_else(|| EditError::Invalid("split point is outside the clip".into()))?;
                *l = a;
                *r = b;
            }
            (ClipContent::Midi(l), ClipContent::Midi(r)) => {
                let rel = at - original.start;
                r.length = l.length - rel;
                l.length = rel;
                r.notes = l
                    .notes
                    .iter()
                    .filter(|n| n.start >= rel)
                    .map(|n| MidiNote {
                        start: n.start - rel,
                        ..*n
                    })
                    .collect();
                l.notes.retain(|n| n.start < rel);
                for n in &mut l.notes {
                    if n.end() > rel {
                        n.length = rel - n.start;
                    }
                }
                // SysEx by time.
                r.sysex = l
                    .sysex
                    .iter()
                    .filter(|e| e.time >= rel)
                    .map(|e| crate::SysexEvent {
                        time: e.time - rel,
                        data: e.data.clone(),
                    })
                    .collect();
                l.sysex.retain(|e| e.time < rel);
                // Expression goes with its note.
                r.expressions = l
                    .expressions
                    .iter()
                    .filter(|e| r.notes.iter().any(|n| n.id == e.note))
                    .cloned()
                    .collect();
                l.prune_expressions();
                // Controller values continue across the cut.
                for (rl, ll) in r.controllers.iter_mut().zip(l.controllers.iter_mut()) {
                    let carried = ll.value_at(rel);
                    rl.points = ll
                        .points
                        .iter()
                        .filter(|p| p.time >= rel)
                        .map(|p| crate::ControllerPoint {
                            time: p.time - rel,
                            value: p.value,
                        })
                        .collect();
                    if let Some(v) = carried
                        && rl.points.first().is_none_or(|p| p.time > MusicalTime::ZERO)
                    {
                        rl.points.insert(
                            0,
                            crate::ControllerPoint {
                                time: MusicalTime::ZERO,
                                value: v,
                            },
                        );
                    }
                    ll.points.retain(|p| p.time < rel);
                }
            }
            _ => return Err(EditError::Invalid("inconsistent clip content".into())),
        }
        Ok((left, right))
    }

    /// The part of this clip between `from` and `to` (a copy with id `id`),
    /// `None` when they do not overlap.
    pub fn slice(
        &self,
        from: MusicalTime,
        to: MusicalTime,
        id: ClipId,
        timeline: &faderframe_timeline::Timeline,
        rate: u32,
    ) -> Option<Clip> {
        let end = self.end(timeline, rate);
        if to <= self.start || from >= end || to <= from {
            return None;
        }
        let mut part = self.clone();
        part.id = id;
        if from > part.start {
            part = part.split_at(from, id, timeline, rate).ok()?.1;
        }
        if to < end {
            part = part.split_at(to, id, timeline, rate).ok()?.0;
        }
        part.id = id;
        Some(part)
    }
}
