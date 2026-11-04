//! Lead sheets: a melody with its chords and words — or several tracks'
//! lines, a staff each, one below the other, or a part as played (chords
//! on one stem, a piano's hands on a grand staff) — written out as
//! MusicXML and engraved on pages. [`heard`] turns what a polyphonic
//! transcription heard into clean lines (the melody picked out), and
//! [`harmony`] names the chords the rest plays. [`notate`] writes MIDI
//! notes for the score editor: every note, in two voices where lines
//! sound against each other, each event knowing the notes it came from.

#![forbid(unsafe_code)]

pub mod assets;
pub mod build;
pub mod engrave;
pub mod glyph;
pub mod harmony;
pub mod heard;
pub mod musicxml;
pub mod notate;
pub mod pdf;
pub mod score;
pub mod tab;

pub use build::{Bar, ChordAt, Grid, Input, Line, Note, Staff, StaffTab, build, hands};
pub use score::{LeadSheet, Part};
pub use tab::{TUNINGS, Tuning};
