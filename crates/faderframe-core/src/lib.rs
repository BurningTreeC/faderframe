//! Foundational types shared by every DAW subsystem.
//!
//! This crate is deliberately tiny and dependency-light: it must be usable from
//! realtime code, the project model, the engine and the UI alike. Nothing in
//! here may depend on GTK, an audio backend or a plugin format.

#![forbid(unsafe_code)]

pub mod ambisonics;
pub mod builtin;
pub mod channel;
pub mod gain;
pub mod id;
pub mod pan;
pub mod paths;
pub mod role;
pub mod spherical;
pub mod surround;
pub mod timecode;
pub mod vbap;

pub use ambisonics::AmbiOrder;
pub use channel::ChannelLayout;
pub use gain::{Decibels, FaderLaw, db_to_gain, gain_to_db};
pub use id::IdLease;
pub use id::{
    AdrCueId, AudioSourceId, AutomationLaneId, ClipId, ClipLinkId, CueId, GroupId, IdAllocator,
    MarkerId, MidiMappingId, MixSceneId, ModulatorId, NoteId, ParameterId, PluginInstanceId,
    ReferenceId, SceneId, ScratchPadId, SectionId, SendId, SongId, TrackId, VideoClipId,
    VideoSourceId, VideoTrackId,
};
pub use id::{ArticulationMapId, DrumMapId};
pub use id::{MusicLayerId, MusicStateId, ScoreParamId, StingerId, TransitionId};
pub use pan::{DualPan, PanLaw, PanMode};
pub use role::{RoleFamily, TrackRole};
pub use surround::{SurroundFormat, SurroundPan, SurroundParam};
