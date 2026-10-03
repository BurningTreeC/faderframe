//! Foundational types shared by every DAW subsystem.
//!
//! This crate is deliberately tiny and dependency-light: it must be usable from
//! realtime code, the project model, the engine and the UI alike. Nothing in
//! here may depend on GTK, an audio backend or a plugin format.

#![forbid(unsafe_code)]

pub mod builtin;
pub mod channel;
pub mod gain;
pub mod id;
pub mod pan;

pub use channel::ChannelLayout;
pub use gain::{Decibels, FaderLaw, db_to_gain, gain_to_db};
pub use id::{
    AudioSourceId, AutomationLaneId, ClipId, IdAllocator, MarkerId, MidiMappingId, NoteId,
    ParameterId, PluginInstanceId, SendId, TrackId,
};
pub use pan::PanLaw;
