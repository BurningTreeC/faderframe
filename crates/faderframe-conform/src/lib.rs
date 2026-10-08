//! Conforming: when the picture editor sends a new cut, the sound follows
//! it. A cut arrives as a CMX3600 EDL ([`edl`]) or an OpenTimelineIO
//! timeline ([`otio`]), both read into a [`CutList`]; [`changes`] finds
//! where each span of the old cut is in the new one (by the source
//! material both show), what was cut out and what is new. Without lists,
//! [`shots`] finds the same by matching the two pictures frame by frame.
//!
//! Pure: no I/O but the text handed in, times in seconds.

pub mod changes;
pub mod edl;
pub mod otio;
pub mod shots;

pub use changes::{Changes, Move, changes};

/// What a cut list says: its events in record order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CutList {
    pub title: Option<String>,
    pub events: Vec<Event>,
}

/// Picture or sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Audio,
}

/// One edit: a span of a source placed on the record (the cut).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: Kind,
    /// Its track among those of its kind (0: V1 or A1).
    pub track: usize,
    /// The source: the reel, or the clip's file or name where the list
    /// names it.
    pub source: String,
    /// The clip's name, when the list gives one.
    pub name: Option<String>,
    /// In and out on the source and on the record (seconds; out not
    /// included).
    pub source_in: f64,
    pub source_out: f64,
    pub record_in: f64,
    pub record_out: f64,
    /// Played at this speed (1: as shot; a motion effect otherwise).
    pub speed: f64,
}

impl Event {
    pub fn length(&self) -> f64 {
        self.record_out - self.record_in
    }
}

impl CutList {
    /// The events of one kind and track, in record order.
    pub fn track(&self, kind: Kind, track: usize) -> Vec<&Event> {
        let mut out: Vec<&Event> = self
            .events
            .iter()
            .filter(|e| e.kind == kind && e.track == track)
            .collect();
        out.sort_by(|a, b| a.record_in.total_cmp(&b.record_in));
        out
    }

    /// Read a list: OpenTimelineIO when it is JSON, else an EDL (whose
    /// timecodes count frames at `rate`).
    pub fn read(
        text: &str,
        rate: faderframe_core::timecode::FrameRate,
    ) -> Result<Self, ConformError> {
        if text.trim_start().starts_with('{') {
            otio::parse(text)
        } else {
            edl::parse(text, rate)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConformError {
    #[error("EDL line {line}: {message}")]
    Edl { line: usize, message: String },
    #[error("OpenTimelineIO: {0}")]
    Otio(String),
}
