//! Persistent multitrack project model.
//!
//! * [`Project`] — tracks (audio, instrument, MIDI, bus, aux, master),
//!   clips, audio sources, tempo/meter timeline, markers, loop range.
//! * [`Command`] — every edit, with validation, inverse and engine
//!   [`Impact`]; [`History`] turns inverses into undo/redo with gesture
//!   coalescing.
//! * [`file`] — versioned JSON project files with migrations.
//!
//! Nothing here references the engine, the GUI toolkit or plugin runtime
//! objects; those live in the session/engine/UI crates and observe the
//! project through commands.

#![forbid(unsafe_code)]

pub mod album;
pub mod arrange;
mod clip;
pub mod demo;
mod edit;
mod expression;
pub mod file;
pub mod harmony;
mod history;
pub mod midi_ops;
mod midimap;
pub mod preset;
mod project;
mod takes;
mod track;
mod warp;

pub use clip::{
    AudioClip, Clip, ClipContent, ClipFades, ControllerLane, ControllerPoint, FadeShape, MidiClip,
    MidiController, MidiNote, StretchSettings, SysexEvent, bend_factor,
};
pub use edit::{CoalesceKey, Command, EditError, Impact, MAX_LEVEL_DB, RemovedTrack};
pub use expression::{ExpressionKind, ExpressionPoint, MpeConfig, NoteExpression};
pub use harmony::{ChordEvent, KeyChange};
pub use history::{History, Replayed};
pub use midimap::{
    MappingMode, MappingTarget, MidiControl, MidiMapping, MidiSource, TransportControl,
};
pub use preset::{PresetError, TrackPreset};
pub use project::{AudioSource, Marker, MusicalRange, Project, Section, SourceSpec};
pub use takes::{CompPiece, CompSegment, DEFAULT_COMP_CROSSFADE, Take, TakeFolder};
pub use track::{
    AuxSend, Freeze, GroupLink, InputRouting, MidiOutputRouting, MonitorMode, OutputRouting,
    PluginFormat, PluginRef, PluginSlot, SavedParameter, SendTap, Track, TrackColor, TrackGroup,
    TrackKind, midi_port_display,
};
pub use warp::{Warp, WarpAlgorithm, WarpMarker};
