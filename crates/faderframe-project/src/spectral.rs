//! Spectral edits: regions of an audio clip's spectrum (time × frequency)
//! made quieter, removed, brought down to the sound around them, or healed
//! from it — a cough, a squeaking chair, a phone, a click, a dropout.
//!
//! The edits are kept with the clip in its source's own frames and hertz;
//! a processed copy of the source (same length and frames, so warps, pitch
//! edits and trims carry over) is what the clip plays, and the unedited
//! source stays in [`SpectralEdits::original`]. With clip effects, the
//! edits apply before them (to the effects' original audio).

use faderframe_core::AudioSourceId;
use serde::{Deserialize, Serialize};

/// Where an edit applies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SpectralShape {
    /// Source frames `start..end`, `low..high` Hz (a time range: the whole
    /// band; a band: the whole clip).
    Rect {
        start: i64,
        end: i64,
        low: f32,
        high: f32,
    },
    /// A closed outline of (source frame, Hz) points.
    Lasso { points: Vec<(i64, f32)> },
    /// A brush stroke through (source frame, Hz) points, `radius_ms` wide
    /// in time and `radius_st` semitones in frequency each side.
    Brush {
        points: Vec<(i64, f32)>,
        radius_ms: f32,
        radius_st: f32,
    },
}

impl SpectralShape {
    /// The source frames it covers (without the feather).
    pub fn frames(&self) -> (i64, i64) {
        match self {
            SpectralShape::Rect { start, end, .. } => (*start.min(end), *start.max(end)),
            SpectralShape::Lasso { points } | SpectralShape::Brush { points, .. } => points
                .iter()
                .fold((i64::MAX, i64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0))),
        }
    }

    /// The band it covers (without the feather).
    pub fn band(&self) -> (f32, f32) {
        let hz = |points: &[(i64, f32)]| {
            points.iter().fold((f32::INFINITY, 0.0f32), |(lo, hi), p| {
                (lo.min(p.1), hi.max(p.1))
            })
        };
        match self {
            SpectralShape::Rect { low, high, .. } => (low.min(*high), low.max(*high)),
            SpectralShape::Lasso { points } => hz(points),
            SpectralShape::Brush {
                points, radius_st, ..
            } => {
                let (lo, hi) = hz(points);
                let k = 2f32.powf(radius_st / 12.0);
                (lo / k, hi * k)
            }
        }
    }

    /// Moved by `frames` and `semitones`.
    pub fn moved(&self, frames: i64, semitones: f32) -> SpectralShape {
        let k = 2f32.powf(semitones / 12.0);
        let shift = |points: &[(i64, f32)]| -> Vec<(i64, f32)> {
            points.iter().map(|(t, f)| (t + frames, f * k)).collect()
        };
        match self {
            SpectralShape::Rect {
                start,
                end,
                low,
                high,
            } => SpectralShape::Rect {
                start: start + frames,
                end: end + frames,
                low: low * k,
                high: high * k,
            },
            SpectralShape::Lasso { points } => SpectralShape::Lasso {
                points: shift(points),
            },
            SpectralShape::Brush {
                points,
                radius_ms,
                radius_st,
            } => SpectralShape::Brush {
                points: shift(points),
                radius_ms: *radius_ms,
                radius_st: *radius_st,
            },
        }
    }
}

/// What an edit does.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SpectralOp {
    /// Louder or quieter by `db`.
    Gain { db: f32 },
    /// Silenced.
    Remove,
    /// Brought down to the level of the sound around it in time, only
    /// where it stands out (coughs, clicks, squeaks); quieter parts stay.
    Attenuate,
    /// Replaced by the sound around it in time (dropouts, longer noises).
    Heal,
}

impl SpectralOp {
    pub fn label(self) -> String {
        match self {
            SpectralOp::Gain { db } => format!("Gain {db:+.1} dB").replace('-', "−"),
            SpectralOp::Remove => "Remove".into(),
            SpectralOp::Attenuate => "Attenuate".into(),
            SpectralOp::Heal => "Heal".into(),
        }
    }
}

/// One edit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpectralEdit {
    pub shape: SpectralShape,
    pub op: SpectralOp,
    /// Soft edges outside the shape: in time (ms) and frequency
    /// (semitones).
    #[serde(default = "default_feather_ms")]
    pub feather_ms: f32,
    #[serde(default = "default_feather_st")]
    pub feather_st: f32,
    /// Only this channel (`None`: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u16>,
}

fn default_feather_ms() -> f32 {
    10.0
}

fn default_feather_st() -> f32 {
    1.0
}

impl SpectralEdit {
    pub fn new(shape: SpectralShape, op: SpectralOp) -> Self {
        Self {
            shape,
            op,
            feather_ms: default_feather_ms(),
            feather_st: default_feather_st(),
            channel: None,
        }
    }
}

/// A clip's spectral edits and its audio before them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpectralEdits {
    /// The unedited source.
    pub original: AudioSourceId,
    pub edits: Vec<SpectralEdit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_know_their_extent() {
        let r = SpectralShape::Rect {
            start: 900,
            end: 100,
            low: 2000.0,
            high: 500.0,
        };
        assert_eq!(r.frames(), (100, 900));
        assert_eq!(r.band(), (500.0, 2000.0));
        let b = SpectralShape::Brush {
            points: vec![(10, 1000.0), (50, 2000.0)],
            radius_ms: 5.0,
            radius_st: 12.0,
        };
        assert_eq!(b.frames(), (10, 50));
        let (lo, hi) = b.band();
        assert!((lo - 500.0).abs() < 1e-3 && (hi - 4000.0).abs() < 1e-2);
        let m = r.moved(10, 12.0);
        assert_eq!(m.frames(), (110, 910));
        assert_eq!(m.band(), (1000.0, 4000.0));
        let edit = SpectralEdit::new(r, SpectralOp::Gain { db: -6.0 });
        let json = serde_json::to_string(&edit).unwrap();
        let back: SpectralEdit = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edit);
        assert_eq!(SpectralOp::Gain { db: -6.0 }.label(), "Gain −6.0 dB");
    }
}
