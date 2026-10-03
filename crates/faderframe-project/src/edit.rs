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
    SendLevel(TrackId, SendId),
    Clip(ClipId),
    Note(ClipId, NoteId),
    Tempo,
    Loop,
    Punch,
    Automation(TrackId, AutomationLaneId),
    PluginParameter(PluginInstanceId, ParameterId),
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
    SetInstrument {
        track: TrackId,
        slot: Option<PluginSlot>,
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
    /// Add a controller mapping at `index` in the mapping list.
    AddMidiMapping {
        index: usize,
        mapping: crate::MidiMapping,
    },
    RemoveMidiMapping {
        mapping: faderframe_core::MidiMappingId,
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
            SetTrackMute { .. } => "Toggle Mute".into(),
            SetTrackSolo { .. } => "Toggle Solo".into(),
            SetTrackRecordArm { .. } => "Toggle Record Arm".into(),
            SetTrackMonitor { .. } => "Change Monitoring".into(),
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
            SetInstrument { .. } => "Change Instrument".into(),
            AddTrack { .. } | RestoreTrack(_) => "Add Track".into(),
            RemoveTrack { .. } => "Remove Track".into(),
            MoveTrack { .. } => "Move Track".into(),
            AddAutomationLane { .. } => "Add Automation Lane".into(),
            RemoveAutomationLane { .. } => "Remove Automation Lane".into(),
            SetAutomationLane { .. } => "Edit Automation".into(),
            AddSource { .. } => "Add Audio".into(),
            RemoveSource { .. } => "Remove Audio".into(),
            AddClip { .. } => "Add Clip".into(),
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
            SetLoop { .. } => "Change Loop".into(),
            SetPunch { .. } => "Change Punch Range".into(),
            AddMarker { .. } => "Add Marker".into(),
            RemoveMarker { .. } => "Remove Marker".into(),
            AddMidiMapping { .. } => "MIDI Learn".into(),
            RemoveMidiMapping { .. } => "Remove MIDI Mapping".into(),
            RenameProject { .. } => "Rename Project".into(),
            Batch { label, .. } => label.clone(),
        }
    }

    pub fn coalesce_key(&self) -> Option<CoalesceKey> {
        use Command::*;
        Some(match self {
            SetTrackVolume { track, .. } => CoalesceKey::TrackVolume(*track),
            SetTrackPan { track, .. } => CoalesceKey::TrackPan(*track),
            SetSendLevel { track, send, .. } => CoalesceKey::SendLevel(*track, *send),
            MoveClip { clip, .. } | SetClipContent { clip, .. } => CoalesceKey::Clip(*clip),
            UpdateNote { clip, note } => CoalesceKey::Note(*clip, note.id),
            SetTempo { .. } => CoalesceKey::Tempo,
            SetLoop { .. } => CoalesceKey::Loop,
            SetPunch { .. } => CoalesceKey::Punch,
            SetAutomationLane { track, lane } => CoalesceKey::Automation(*track, lane.id),
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
            | SetTrackMute { .. }
            | SetTrackSolo { .. }
            | SetTrackPhaseInvert { .. }
            | SetSendLevel { .. }
            | SetPluginParameter { .. } => Impact::Params,
            RenameTrack { .. }
            | SetTrackColor { .. }
            | MoveTrack { .. }
            | AddMarker { .. }
            | RemoveMarker { .. }
            | AddMidiMapping { .. }
            | RemoveMidiMapping { .. }
            | SetPunch { .. }
            | RenameProject { .. }
            | RenameClip { .. } => Impact::None,
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
            | SetLoop { .. } => Impact::Timeline,
            SetTrackRecordArm { .. }
            | SetTrackMonitor { .. }
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
            | SetInstrument { .. }
            | AddTrack { .. }
            | RemoveTrack { .. }
            | RestoreTrack(_) => Impact::Graph,
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
                let slot = track_mut(p, track)?
                    .plugin_mut(plugin)
                    .ok_or(EditError::UnknownPlugin(plugin))?;
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
                let slot = track_mut(p, track)?
                    .plugin_mut(plugin)
                    .ok_or(EditError::UnknownPlugin(plugin))?;
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
                RestoreTrack(Box::new(RemovedTrack {
                    track: removed,
                    index,
                    clips,
                    rerouted,
                    removed_sends,
                }))
            }
            RestoreTrack(r) => {
                let RemovedTrack {
                    track,
                    index,
                    clips,
                    rerouted,
                    removed_sends,
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
            SetTimeline { timeline } => {
                let old = std::mem::replace(&mut p.timeline, *timeline);
                SetTimeline {
                    timeline: Box::new(old),
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
    let end = original.end(&p.timeline, p.sample_rate);
    if at <= original.start || at >= end {
        return Err(EditError::Invalid("split point is outside the clip".into()));
    }
    let mut left = original.clone();
    let mut right = original.clone();
    right.id = new_clip;
    right.start = at;
    match (&mut left.content, &mut right.content) {
        (ClipContent::Audio(l), ClipContent::Audio(r)) => {
            let sr = p.sample_rate as f64;
            let offset = p.timeline.to_samples(at, sr) - p.timeline.to_samples(original.start, sr);
            if offset <= 0 || offset >= l.length {
                return Err(EditError::Invalid("split point is outside the clip".into()));
            }
            r.source_offset = l.source_offset + offset;
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
            let sr = p.sample_rate as f64;
            let offset = p.timeline.to_samples(at, sr) - p.timeline.to_samples(original.start, sr);
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
        }
        _ => return Err(EditError::Invalid("inconsistent clip content".into())),
    }
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
