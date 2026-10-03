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

mod clip;
pub mod demo;
mod edit;
pub mod file;
mod history;
pub mod preset;
mod project;
mod takes;
mod track;

pub use clip::{
    AudioClip, Clip, ClipContent, ClipFades, FadeShape, MidiClip, MidiNote, StretchSettings,
};
pub use edit::{CoalesceKey, Command, EditError, Impact, MAX_LEVEL_DB, RemovedTrack};
pub use history::{History, Replayed};
pub use preset::{PresetError, TrackPreset};
pub use project::{AudioSource, Marker, MusicalRange, Project, SourceSpec};
pub use takes::{CompPiece, CompSegment, DEFAULT_COMP_CROSSFADE, Take, TakeFolder};
pub use track::{
    AuxSend, InputRouting, MonitorMode, OutputRouting, PluginFormat, PluginRef, PluginSlot,
    SavedParameter, SendTap, Track, TrackColor, TrackKind,
};
