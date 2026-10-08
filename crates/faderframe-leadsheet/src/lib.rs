//! Lead sheets: a melody with its chords and words, written out as
//! MusicXML and engraved on pages.

#![forbid(unsafe_code)]

pub mod assets;
pub mod build;
pub mod engrave;
pub mod glyph;
pub mod musicxml;
pub mod pdf;
pub mod score;

pub use build::{Bar, ChordAt, Grid, Input, Line, Note, build};
pub use score::LeadSheet;
