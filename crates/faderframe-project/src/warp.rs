//! Elastic audio: warping a clip's audio in time.
//!
//! A warped audio clip plays its source through a piecewise-linear time
//! map. The clip start (output frame 0 ↔ `source_offset`) and end (output
//! `length` ↔ `source_offset + source_length`) are fixed anchors; warp
//! markers in between pin a source frame to an output frame. Between two
//! neighbouring anchors the audio is stretched or squeezed uniformly, so
//! moving one marker changes only the two segments next to it — the
//! audio outside them stays where it is.
//!
//! Output frames are clip-relative project frames; source frames are the
//! source's (project-rate) frames.

use serde::{Deserialize, Serialize};

/// How warped audio is rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarpAlgorithm {
    /// Full-band time stretching for any material (keeps pitch).
    #[default]
    Polyphonic,
    /// Favours transients (drums, percussive loops; keeps pitch).
    Rhythmic,
    /// Speed changes like tape: pitch follows.
    Varispeed,
}

impl WarpAlgorithm {
    pub const ALL: [WarpAlgorithm; 3] = [Self::Polyphonic, Self::Rhythmic, Self::Varispeed];

    pub fn label(self) -> &'static str {
        match self {
            Self::Polyphonic => "Polyphonic",
            Self::Rhythmic => "Rhythmic",
            Self::Varispeed => "Varispeed",
        }
    }
}

/// A pinned point: output frame `at` plays source frame `source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WarpMarker {
    pub at: i64,
    pub source: i64,
}

/// The time map of a warped clip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warp {
    /// Source frames between the clip's start and end anchors.
    pub source_length: i64,
    /// Strictly increasing in both `at` (inside `0..length`) and `source`.
    #[serde(default)]
    pub markers: Vec<WarpMarker>,
    #[serde(default)]
    pub algorithm: WarpAlgorithm,
}

impl Warp {
    /// A uniform stretch of `source_length` source frames.
    pub fn uniform(source_length: i64) -> Self {
        Self {
            source_length: source_length.max(1),
            markers: Vec::new(),
            algorithm: WarpAlgorithm::default(),
        }
    }

    /// All anchors: start, markers, end.
    pub fn points(&self, source_offset: i64, length: i64) -> Vec<WarpMarker> {
        let mut out = Vec::with_capacity(self.markers.len() + 2);
        out.push(WarpMarker {
            at: 0,
            source: source_offset,
        });
        out.extend(
            self.markers
                .iter()
                .filter(|m| m.at > 0 && m.at < length)
                .copied(),
        );
        out.push(WarpMarker {
            at: length.max(1),
            source: source_offset + self.source_length,
        });
        out
    }

    /// The source frame (fractional) played at output frame `out`.
    pub fn source_of(&self, source_offset: i64, length: i64, out: f64) -> f64 {
        Self::source_at_points(&self.points(source_offset, length), out)
    }

    /// [`Self::source_of`] over anchors from [`Self::points`] (for many
    /// lookups; extrapolates linearly outside them).
    pub fn source_at_points(pts: &[WarpMarker], out: f64) -> f64 {
        if pts.len() < 2 {
            return pts
                .first()
                .map_or(out, |p| p.source as f64 + out - p.at as f64);
        }
        let i = pts
            .partition_point(|p| (p.at as f64) <= out)
            .saturating_sub(1)
            .min(pts.len() - 2);
        let (a, b) = (pts[i], pts[i + 1]);
        let span = (b.at - a.at).max(1) as f64;
        a.source as f64 + (out - a.at as f64) * (b.source - a.source) as f64 / span
    }

    /// The output frame where source frame `src` plays.
    pub fn output_of(&self, source_offset: i64, length: i64, src: i64) -> i64 {
        let pts = self.points(source_offset, length);
        let i = pts
            .iter()
            .rposition(|p| p.source <= src)
            .unwrap_or(0)
            .min(pts.len() - 2);
        let (a, b) = (pts[i], pts[i + 1]);
        let span = (b.source - a.source).max(1) as f64;
        a.at + ((src - a.source) as f64 * (b.at - a.at) as f64 / span).round() as i64
    }

    /// Source frames of the clip.
    pub fn source_range(&self, source_offset: i64) -> std::ops::Range<i64> {
        source_offset..source_offset + self.source_length
    }

    /// Keep the markers valid for a clip of `length`: inside it, strictly
    /// increasing in both coordinates, inside the source range.
    pub fn normalize(&mut self, source_offset: i64, length: i64) {
        let end = source_offset + self.source_length;
        self.markers.sort_by_key(|m| m.at);
        let mut kept: Vec<WarpMarker> = Vec::with_capacity(self.markers.len());
        let mut last = WarpMarker {
            at: 0,
            source: source_offset,
        };
        for m in &self.markers {
            if m.at > last.at && m.source > last.source && m.at < length && m.source < end {
                kept.push(*m);
                last = *m;
            }
        }
        self.markers = kept;
    }

    /// The part from output frame `from` on (for splitting and trimming
    /// the start): returns the new source offset and map.
    pub fn after(&self, source_offset: i64, length: i64, from: i64) -> (i64, Warp) {
        let offset = self.source_of(source_offset, length, from as f64).round() as i64;
        let end = source_offset + self.source_length;
        let mut w = Warp {
            source_length: (end - offset).max(1),
            markers: self
                .markers
                .iter()
                .filter(|m| m.at > from)
                .map(|m| WarpMarker {
                    at: m.at - from,
                    source: m.source,
                })
                .collect(),
            algorithm: self.algorithm,
        };
        w.normalize(offset, length - from);
        (offset, w)
    }

    /// The part before output frame `to` (for splitting and trimming the
    /// end).
    pub fn before(&self, source_offset: i64, length: i64, to: i64) -> Warp {
        let end = self.source_of(source_offset, length, to as f64).round() as i64;
        let mut w = Warp {
            source_length: (end - source_offset).max(1),
            markers: self.markers.iter().filter(|m| m.at < to).copied().collect(),
            algorithm: self.algorithm,
        };
        w.normalize(source_offset, to);
        w
    }

    /// Stretch the whole clip to `new_length` (markers keep their relative
    /// positions).
    pub fn scaled(&self, length: i64, new_length: i64) -> Warp {
        let f = new_length as f64 / length.max(1) as f64;
        Warp {
            source_length: self.source_length,
            markers: self
                .markers
                .iter()
                .map(|m| WarpMarker {
                    at: (m.at as f64 * f).round() as i64,
                    source: m.source,
                })
                .collect(),
            algorithm: self.algorithm,
        }
    }

    /// Is this the identity (no stretching anywhere)?
    pub fn is_identity(&self, source_offset: i64, length: i64) -> bool {
        self.source_length == length
            && self
                .markers
                .iter()
                .all(|m| m.source - source_offset == m.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_stretch_only_their_neighbourhood() {
        // 1000 source frames from 500 played over 1000 output frames, a
        // marker pulling source 750 to output 500 (first half squeezed,
        // second half stretched).
        let w = Warp {
            source_length: 1000,
            markers: vec![WarpMarker {
                at: 500,
                source: 750,
            }],
            algorithm: WarpAlgorithm::default(),
        };
        assert_eq!(w.source_of(500, 1000, 0.0), 500.0);
        assert_eq!(w.source_of(500, 1000, 250.0), 625.0);
        assert_eq!(w.source_of(500, 1000, 500.0), 750.0);
        assert_eq!(w.source_of(500, 1000, 1000.0), 1500.0);
        assert_eq!(w.output_of(500, 1000, 1125), 750);
        // Split at 250: both halves keep the exact mapping.
        let left = w.before(500, 1000, 250);
        assert_eq!(left.source_length, 125);
        let (offset, right) = w.after(500, 1000, 250);
        assert_eq!(offset, 625);
        assert_eq!(right.source_of(offset, 750, 250.0), 750.0);
        assert_eq!(right.source_of(offset, 750, 750.0), 1500.0);
        // Uniform stretch keeps relative positions.
        let s = w.scaled(1000, 2000);
        assert_eq!(s.markers[0].at, 1000);
        // Invalid markers are dropped.
        let mut bad = w.clone();
        bad.markers.push(WarpMarker {
            at: 600,
            source: 700,
        });
        bad.normalize(500, 1000);
        assert_eq!(bad.markers.len(), 1);
        assert!(Warp::uniform(10).is_identity(0, 10));
    }
}
