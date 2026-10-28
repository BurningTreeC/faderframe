//! What the newer rules measured, kept beside the findings for the view:
//! every subject's dynamics and frequency-dependent width, every pair
//! compared for phase and timing (with or without a finding), and charts
//! for findings (by finding id). Facts, no judgement; compact (a chart's
//! series are at most a value per band or per 100 ms).

use crate::dynamics::{Flattening, TrackDynamics};
use crate::finding::{Subject, TimeSpan, Unit};
use crate::phase::{LagEstimate, PairReason};
use faderframe_core::TrackId;

/// Equal values, `NaN` (no value) equal to itself: results compare equal
/// when they were made the same way.
fn same_values(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x == y || x.to_bits() == y.to_bits())
}

/// One line of a chart (`NaN`: no value there).
#[derive(Clone, Debug)]
pub struct ChartSeries {
    /// What it shows ("Sum now", "Short-term loudness").
    pub label: String,
    /// The track it belongs to (its name is shown before the label).
    pub track: Option<TrackId>,
    pub values: Vec<f64>,
}

impl PartialEq for ChartSeries {
    fn eq(&self, o: &Self) -> bool {
        self.label == o.label && self.track == o.track && same_values(&self.values, &o.values)
    }
}

impl ChartSeries {
    pub fn new(label: impl Into<String>, track: Option<TrackId>, values: Vec<f64>) -> Self {
        Self {
            label: label.into(),
            track,
            values,
        }
    }
}

/// A chart behind a finding.
#[derive(Clone, Debug, PartialEq)]
pub enum Chart {
    /// Values per band (Hz centres, log axis); a reference line at 0 when
    /// `zero`; bands `a..=b` highlighted.
    Bands {
        centres: Vec<f64>,
        series: Vec<ChartSeries>,
        highlight: Option<(usize, usize)>,
        unit: Unit,
        zero: bool,
        caption: String,
    },
    /// Values over time: value `i` at `start + i · step` seconds from the
    /// analysed range's start; stretches highlighted.
    Time {
        start: f64,
        step: f64,
        series: Vec<ChartSeries>,
        highlight: Vec<TimeSpan>,
        unit: Unit,
        caption: String,
    },
    /// Offsets (ms) of paired events, their median marked.
    Offsets {
        values: Vec<f64>,
        median: Option<f64>,
        caption: String,
    },
}

/// A subject's dynamics (see [`crate::dynamics`]); in a processed
/// analysis also its source's and how the attacks changed.
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicsSummary {
    pub subject: Subject,
    pub dynamics: TrackDynamics,
    pub source: Option<TrackDynamics>,
    pub flattening: Option<Flattening>,
}

/// A subject's width per band and per broad region ([`crate::width`];
/// `NaN`: silent there).
#[derive(Clone, Debug)]
pub struct WidthSummary {
    pub subject: Subject,
    pub centres: Vec<f64>,
    /// Side over mid (dB).
    pub width_db: Vec<f64>,
    /// Mono fold-down over the channels' mean (dB, ≤ 0).
    pub mono_db: Vec<f64>,
    /// The same per broad region ([`crate::bands::BROAD_NAMES`]).
    pub broad_width_db: Vec<f64>,
    pub broad_mono_db: Vec<f64>,
}

impl PartialEq for WidthSummary {
    fn eq(&self, o: &Self) -> bool {
        self.subject == o.subject
            && same_values(&self.centres, &o.centres)
            && same_values(&self.width_db, &o.width_db)
            && same_values(&self.mono_db, &o.mono_db)
            && same_values(&self.broad_width_db, &o.broad_width_db)
            && same_values(&self.broad_mono_db, &o.broad_mono_db)
    }
}

/// A pair compared for phase and timing (whether or not it made a
/// finding).
#[derive(Clone, Debug, PartialEq)]
pub struct PairSummary {
    pub a: TrackId,
    pub b: TrackId,
    pub reasons: Vec<PairReason>,
    pub excerpt: TimeSpan,
    /// Seconds both played in the excerpt.
    pub together: f64,
    pub lag: Option<LagEstimate>,
    /// Mean coherence over the bands both are relevant in.
    pub coherence: Option<f64>,
    /// Their sum against the sum of their energies (dB) over those bands:
    /// now, with the measured delay and polarity corrected, in mono.
    pub sum_db: Option<f64>,
    pub corrected_db: Option<f64>,
    pub mono_db: Option<f64>,
    pub onsets_paired: usize,
    pub onset_median_ms: Option<f64>,
    pub onset_spread_ms: Option<f64>,
}

/// Everything above, for one analysis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub dynamics: Vec<DynamicsSummary>,
    pub width: Vec<WidthSummary>,
    pub pairs: Vec<PairSummary>,
    /// Pairs that were candidates but not compared, and why.
    pub skipped_pairs: Vec<(TrackId, TrackId, String)>,
    pub charts: Vec<(u64, Chart)>,
}

impl Details {
    /// The chart of finding `id`.
    pub fn chart(&self, id: u64) -> Option<&Chart> {
        self.charts.iter().find(|(f, _)| *f == id).map(|(_, c)| c)
    }

    /// The dynamics of `subject`.
    pub fn dynamics_of(&self, subject: Subject) -> Option<&DynamicsSummary> {
        self.dynamics.iter().find(|d| d.subject == subject)
    }

    /// The width of `subject`.
    pub fn width_of(&self, subject: Subject) -> Option<&WidthSummary> {
        self.width.iter().find(|d| d.subject == subject)
    }
}
