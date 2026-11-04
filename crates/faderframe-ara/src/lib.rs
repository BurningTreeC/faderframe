//! ARA 2 hosting: plug-ins such as Melodyne, SpectraLayers or VocAlign that
//! read the host's audio at will and edit it as a whole, through VST3 or
//! CLAP as companion API.
//!
//! * [`ffi`] — the C API, translated by hand (struct packing as the header
//!   declares it, layouts checked by tests).
//! * [`Factory`] — a plug-in's static ARA factory (from a VST3 module's
//!   `ARA::IMainFactory`/`IPlugInEntryPoint` or a CLAP entry's ARA factory);
//!   initialised once per process.
//! * [`Document`] — one ARA document (one per project and factory): the
//!   host's interfaces (audio readers over [`Samples`], archives, the
//!   musical context's content, model update and playback requests), and
//!   the document controller the plug-in made for it. [`Document::sync`]
//!   brings the plug-in's model graph to a [`Model`] — the musical context,
//!   region sequences (tracks), audio sources (media files), audio
//!   modifications and playback regions (clips) — in one edit cycle, and
//!   assigns playback regions to the plug-in instances bound to it
//!   ([`Renderer`]).
//! * [`Extension`] — what binding a companion plug-in instance to the
//!   document's controller hands back (playback renderer, editor renderer,
//!   editor view).
//!
//! Everything here runs on the control (main) thread, except the audio
//! readers, which plug-ins use from their own (non-realtime) threads.
//! `unsafe` is allowed in this crate for the C API only; every block says
//! why it holds.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod content;
mod document;
mod extension;
mod factory;
pub mod ffi;
mod host;
mod model;

#[cfg(any(test, feature = "mock"))]
pub mod mock;
#[cfg(test)]
mod tests;

pub use content::{BarSignature, Chord, Context, KeySignature, Note, TempoEntry};
pub use document::{
    Analysis, AnalysisState, Archive, ControllerRef, Document, PlaybackRequest, SyncReport, Updates,
};
pub use extension::{Extension, Roles};
pub use factory::Factory;
pub use model::{Assignment, Model, Modification, Region, Renderer, Sequence, Source};

/// Sample data of an audio source, read by the plug-in's audio readers.
///
/// Called from the plug-in's own threads (never its realtime render
/// thread), one thread per reader at a time but several readers at once.
pub trait Samples: Send + Sync {
    /// Fill `out` (one slice per channel, all the same length) with the
    /// frames from `start` on; frames outside the source are silence.
    /// `false` on an I/O error (the plug-in then gets silence).
    fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool;
}

/// What went wrong talking to an ARA plug-in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AraError {
    #[error("not an ARA factory: {0}")]
    InvalidFactory(String),
    #[error("{plugin} supports ARA {lowest}…{highest}; FaderFrame needs ARA 2.0 or later")]
    Generation {
        plugin: String,
        lowest: i32,
        highest: i32,
    },
    #[error("{0}: the plug-in did not create a document controller")]
    NoController(String),
    #[error("{0}: the plug-in's document controller lacks a required function")]
    Incomplete(String),
    #[error("the archive was made by another plug-in version ({0})")]
    IncompatibleArchive(String),
    #[error("{0}: storing the document failed")]
    Store(String),
    #[error("{0}")]
    Other(String),
}

/// How many times plug-ins have reported a programming error on either
/// side of the API (ARA's assert function) in this process. Tests check
/// that the host causes none.
pub fn assert_count() -> usize {
    host::ASSERTS.load(std::sync::atomic::Ordering::Relaxed)
}
