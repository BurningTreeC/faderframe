//! Timeline mathematics: musical time, tempo maps, time signatures and grids.
//!
//! Positions on the arrangement are stored as [`MusicalTime`] — integer ticks
//! at [`TICKS_PER_QUARTER`] resolution — so that editing operations (snap,
//! quantise, move by a bar) are exact and free of floating-point drift.
//! Conversions to seconds/samples go through a [`TempoMap`] which supports
//! constant tempo segments as well as linear tempo ramps.
//!
//! Everything here is allocation-free once constructed, so the realtime
//! engine may use a `TempoMap` copy for per-block beat positions.

#![forbid(unsafe_code)]

mod grid;
mod meter;
mod tempo;
mod time;

pub use grid::{GridDivision, GridLineKind, for_each_grid_line, snap_floor, snap_nearest};
pub use meter::{Bbt, MeterChange, TimeSignature, TimeSignatureMap};
pub use tempo::{TempoCurve, TempoMap, TempoPoint};
pub use time::{MusicalDuration, MusicalTime, TICKS_PER_QUARTER, format_seconds};

use serde::{Deserialize, Serialize};

/// Tempo map and meter map together: the musical "shape" of a project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub tempo: TempoMap,
    pub meter: TimeSignatureMap,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            tempo: TempoMap::new(120.0),
            meter: TimeSignatureMap::new(TimeSignature::FOUR_FOUR),
        }
    }
}

impl Timeline {
    /// Musical position → absolute sample index at `sample_rate`.
    #[inline]
    pub fn to_samples(&self, pos: MusicalTime, sample_rate: f64) -> i64 {
        self.tempo.musical_to_samples(pos, sample_rate)
    }

    /// Absolute sample index → musical position at `sample_rate`.
    #[inline]
    pub fn to_musical(&self, samples: i64, sample_rate: f64) -> MusicalTime {
        self.tempo.samples_to_musical(samples, sample_rate)
    }

    /// Musical end position of something that starts at `start` and lasts
    /// `frames` samples (e.g. an un-stretched audio clip).
    pub fn end_of_sample_span(
        &self,
        start: MusicalTime,
        frames: i64,
        sample_rate: f64,
    ) -> MusicalTime {
        let s = self.to_samples(start, sample_rate);
        self.to_musical(s + frames, sample_rate)
    }

    /// "bar.beat.tick" display string (1-based bars and beats).
    pub fn format_bbt(&self, pos: MusicalTime) -> String {
        self.meter.to_bbt(pos).to_string()
    }
}
