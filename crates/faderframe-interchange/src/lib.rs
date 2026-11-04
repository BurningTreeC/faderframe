//! Projects and scores to and from other programs.
//!
//! * [`dawproject`]: DAWproject 1.0 (Bitwig's open exchange format, MIT):
//!   a zip of `project.xml`, `metadata.xml`, media and plug-in states. The
//!   document model mirrors the schema ([`dawproject::Project`]); it is
//!   written in the schema's element order and read back leniently
//!   (unknown elements are noted, never fatal). Plug-in states travel in
//!   their native formats ([`presets`]: `.clap-preset`, `.vstpreset`).
//! * [`musicxml`]: MusicXML scores read into notes in quarters (ties
//!   joined, tuplets and voices placed, dynamics as velocities,
//!   articulations as lengths and accents), the tempo, metre and key,
//!   lyrics and chord symbols; partwise and timewise documents, plain
//!   `.musicxml`/`.xml` and compressed `.mxl`.
//! * [`zip`]: the containers both use (stored and deflated entries,
//!   ZIP64), and [`xml`]: the writer.
//!
//! Everything here is pure: the session maps it onto a project.

#![forbid(unsafe_code)]

pub mod dawproject;
pub mod musicxml;
pub mod presets;
pub mod xml;
pub mod zip;

/// A failure reading or writing an interchange file.
#[derive(Debug, thiserror::Error)]
pub enum InterchangeError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("not a zip archive: {0}")]
    Zip(String),
    #[error("XML: {0}")]
    Xml(String),
    #[error("{0}")]
    Format(String),
}

impl InterchangeError {
    pub(crate) fn io(path: impl std::fmt::Display, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_string(),
            source,
        }
    }
}

pub type Result<T, E = InterchangeError> = std::result::Result<T, E>;
