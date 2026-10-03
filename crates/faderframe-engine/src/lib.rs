//! The FaderFrame audio engine.
//!
//! ```text
//!  CONTROL WORLD (GUI thread)                     REALTIME WORLD (audio thread)
//!  ─────────────────────────                      ─────────────────────────────
//!  Project ──build──► GraphBuilder ──compile──►   CompiledGraph   (swap, state adoption)
//!  Project ──────────► TimelineSnapshot ───────►  clip/MIDI players read it
//!  Project ──────────► ParamTable (atomics) ───►  strips/sends read it
//!  TransportCommand ─────── rtrb queue ────────►  TransportState
//!  meters/metrics ◄──────── atomics ───────────   MeterBank, CallbackMetrics
//!  drop old objects ◄────── garbage queue ─────   retired graphs/snapshots
//! ```
//!
//! The realtime side never allocates, frees, locks or performs I/O.
//! [`EngineProcessor`] is the [`faderframe_audio::AudioCallback`] handed to
//! a backend; [`offline::OfflineRenderer`] drives the same code path for
//! bounces, tests and benchmarks.

#![forbid(unsafe_code)]

mod build;
mod context;
mod engine;
pub mod nodes;
pub mod offline;
mod plugins;
mod slots;
mod snapshot;

pub use build::{BuiltGraph, build_graph};
pub use context::EngineContext;
pub use engine::{
    EngineConfig, EngineController, EngineProcessor, EngineShared, TrackMeter, create,
};
pub use plugins::{ActivatedPlugin, PluginHost};
pub use slots::{SlotRegistry, SlotsExhausted, StripSlots};
pub use snapshot::{
    AudioRegion, Lane, MidiRegion, SourceMap, TimelineSnapshot, render_generated_sources,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Graph(#[from] faderframe_audio_graph::GraphError),
    #[error(transparent)]
    Slots(#[from] SlotsExhausted),
    #[error("engine command queue is full (audio thread not running?)")]
    QueueFull,
}
