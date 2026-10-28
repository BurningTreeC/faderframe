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

pub mod adr;
pub mod album;
pub mod arrange;
mod clip;
pub mod clip_fx;
pub mod compare;
pub mod conform;
pub mod console;
pub mod container;
pub mod cue;
pub mod demo;
mod edit;
mod expression;
pub mod file;
pub mod harmony;
mod history;
pub mod launcher;
pub mod lyrics;
pub mod midi_ops;
pub mod midi_tools;
mod midimap;
pub mod modulation;
pub mod pitch;
pub mod preset;
mod project;
pub mod reference;
pub mod ripple;
pub mod scratch;
pub mod setlist;
pub mod spectral;
mod takes;
pub mod template;
pub mod time_lock;
mod track;
pub mod video;
mod warp;

pub use clip::{
    AudioClip, Clip, ClipContent, ClipFades, ControllerLane, ControllerPoint, FadeShape, MidiClip,
    MidiController, MidiNote, StretchSettings, SysexEvent, bend_factor,
};
pub use cue::{CueMix, CueSend};
pub use edit::{CoalesceKey, Command, EditError, Impact, MAX_LEVEL_DB, RemovedTrack};
pub use expression::{ExpressionKind, ExpressionPoint, MpeConfig, NoteExpression};
pub use harmony::{ChordEvent, KeyChange};
pub use history::{History, Replayed};
pub use midimap::{
    MappingMode, MappingTarget, MidiControl, MidiMapping, MidiSource, ShowControl, TransportControl,
};
pub use preset::{PresetError, TrackPreset};
pub use project::{AudioSource, Marker, MusicalRange, PlayStart, Project, Section, SourceSpec};
pub use takes::{COMP_DECLICK, CompPiece, CompSegment, DEFAULT_COMP_CROSSFADE, Take, TakeFolder};
pub use track::{
    AuxSend, BinauralRender, Freeze, GroupLink, InputRouting, MAX_TRACK_DELAY_MS,
    MidiOutputRouting, MonitorMode, OutputRouting, PluginFormat, PluginRef, PluginSlot,
    SavedParameter, SendTap, TimeBase, Track, TrackColor, TrackGroup, TrackKind, device_name,
    midi_port_display,
};
pub use warp::{Warp, WarpAlgorithm, WarpMarker};
