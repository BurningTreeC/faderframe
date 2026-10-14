//! Lead sheets: a melody with its chords and words — or several tracks'
//! lines, a staff each, one below the other — written out as MusicXML and
//! engraved on pages.

#![forbid(unsafe_code)]

pub mod assets;
pub mod build;
pub mod engrave;
pub mod glyph;
pub mod musicxml;
pub mod pdf;
pub mod score;
pub mod tab;

pub use build::{Bar, ChordAt, Grid, Input, Line, Note, Staff, StaffTab, build};
pub use score::{LeadSheet, Part};
pub use tab::{TUNINGS, Tuning};
