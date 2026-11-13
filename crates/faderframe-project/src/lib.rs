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
pub mod ara;
pub mod arrange;
pub mod articulation;
mod clip;
pub mod clip_envelope;
pub mod clip_fx;
pub mod compare;
pub mod conform;
pub mod console;
pub mod container;
pub mod cue;
pub mod demo;
pub mod device_compare;
pub mod drum_map;
mod edit;
mod expression;
pub mod file;
pub mod harmony;
mod history;
pub mod interactive;
pub mod journal;
pub mod launcher;
pub mod lineage;
pub mod lyric_comp;
pub mod lyrics;
pub mod merge;
pub mod midi_ops;
pub mod midi_tools;
mod midimap;
pub mod modulation;
pub mod notation;
pub mod pitch;
pub mod preset;
mod project;
pub mod reference;
pub mod ripple;
pub mod scenes;
pub mod scratch;
pub mod setlist;
pub mod spectral;
mod takes;
pub mod template;
pub mod time_lock;
mod track;
pub mod tuning;
pub mod video;
mod warp;

pub use clip::{
    AudioClip, Clip, ClipContent, ClipFades, ControllerLane, ControllerPoint, FadeShape, MidiClip,
    MidiController, MidiNote, NoteFine, StretchSettings, SysexEvent, bend_factor, steps_text,
    velocity_of_steps, velocity_steps,
};
pub use cue::{CueMix, CueSend};
pub use edit::{CoalesceKey, Command, EditError, Impact, MAX_LEVEL_DB, RemovedTrack};
pub use expression::{ExpressionKind, ExpressionPoint, MpeConfig, NoteExpression};
pub use harmony::{ChordEvent, KeyChange};
pub use history::{History, HistoryStep, Moved, Replayed, TreeRow};
pub use midimap::{
    MappingMode, MappingTarget, MidiControl, MidiMapping, MidiSource, SceneControl, ShowControl,
    TransportControl,
};
pub use preset::{PresetError, TrackPreset};
pub use project::{AudioSource, Marker, MusicalRange, PlayStart, Project, Section, SourceSpec};
pub use takes::{
    AudioRef, COMP_DECLICK, CompPiece, CompSegment, DEFAULT_COMP_CROSSFADE, PlayedPiece, Take,
    TakeFolder, TakeId, TakeRef,
};
pub use track::{
    AuxSend, BinauralRender, Freeze, GroupLink, InputRouting, MAX_TRACK_DELAY_MS,
    MidiOutputRouting, MonitorMode, OutputRouting, PluginFormat, PluginRef, PluginSlot,
    SavedParameter, SendTap, TimeBase, Track, TrackColor, TrackGroup, TrackKind, device_name,
    midi_port_display,
};
pub use warp::{Warp, WarpAlgorithm, WarpMarker};
