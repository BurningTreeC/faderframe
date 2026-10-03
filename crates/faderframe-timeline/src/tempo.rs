use crate::time::{MusicalTime, TICKS_PER_QUARTER};
use serde::{Deserialize, Serialize};

/// Shape of the tempo between a point and the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TempoCurve {
    /// Tempo holds until the next point (step change).
    #[default]
    Constant,
    /// Tempo changes linearly (per quarter note) towards the next point.
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    pub position: MusicalTime,
    /// Quarter notes per minute.
    pub bpm: f64,
    #[serde(default)]
    pub curve: TempoCurve,
}

/// Persisted form of a tempo map (the cache is rebuilt on load).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TempoMapData {
    points: Vec<TempoPoint>,
}

/// Piecewise tempo map with constant and linearly ramped segments.
///
/// Invariants: at least one point, the first point is at position 0, points
/// are strictly ascending by position, every bpm is within
/// [`TempoMap::MIN_BPM`, `TempoMap::MAX_BPM`]. `seconds[i]` caches the
/// absolute time at which point `i` starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TempoMapData", into = "TempoMapData")]
pub struct TempoMap {
    points: Vec<TempoPoint>,
    seconds: Vec<f64>,
}

impl From<TempoMapData> for TempoMap {
    fn from(data: TempoMapData) -> Self {
        let mut map = TempoMap {
            points: data.points,
            seconds: Vec::new(),
        };
        map.normalise();
        map
    }
}

impl From<TempoMap> for TempoMapData {
    fn from(map: TempoMap) -> Self {
        TempoMapData { points: map.points }
    }
}

impl TempoMap {
    pub const MIN_BPM: f64 = 1.0;
    pub const MAX_BPM: f64 = 999.0;

    /// A constant-tempo map.
    pub fn new(bpm: f64) -> Self {
        let mut map = Self {
            points: vec![TempoPoint {
                position: MusicalTime::ZERO,
                bpm,
                curve: TempoCurve::Constant,
            }],
            seconds: Vec::new(),
        };
        map.normalise();
        map
    }

    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Insert or replace the point at `point.position`.
    pub fn set_point(&mut self, point: TempoPoint) {
        match self
            .points
            .binary_search_by(|p| p.position.cmp(&point.position))
        {
            Ok(i) => self.points[i] = point,
            Err(i) => self.points.insert(i, point),
        }
        self.normalise();
    }

    /// Remove the point at index `i` (the first point cannot be removed).
    pub fn remove_point(&mut self, i: usize) -> Option<TempoPoint> {
        if i == 0 || i >= self.points.len() {
            return None;
        }
        let p = self.points.remove(i);
        self.normalise();
        Some(p)
    }

    /// Change the initial tempo.
    pub fn set_initial_bpm(&mut self, bpm: f64) {
        self.points[0].bpm = bpm;
        self.normalise();
    }

    fn normalise(&mut self) {
        if self.points.is_empty() {
            self.points.push(TempoPoint {
                position: MusicalTime::ZERO,
                bpm: 120.0,
                curve: TempoCurve::Constant,
            });
        }
        self.points.sort_by_key(|p| p.position);
        self.points.dedup_by_key(|p| p.position);
        self.points[0].position = MusicalTime::ZERO;
        for p in &mut self.points {
            if !p.bpm.is_finite() {
                p.bpm = 120.0;
            }
            p.bpm = p.bpm.clamp(Self::MIN_BPM, Self::MAX_BPM);
        }
        self.seconds.clear();
        let mut t = 0.0;
        for i in 0..self.points.len() {
            self.seconds.push(t);
            if i + 1 < self.points.len() {
                let len = self.points[i + 1].position - self.points[i].position;
                t += self.segment_seconds(i, len.quarters());
            }
        }
    }

    /// Tempo at the start of segment `i` and its slope in bpm per quarter.
    #[inline]
    fn segment_shape(&self, i: usize) -> (f64, f64) {
        let p = &self.points[i];
        match (p.curve, self.points.get(i + 1)) {
            (TempoCurve::Linear, Some(next)) => {
                let len = (next.position - p.position).quarters();
                if len > 0.0 {
                    (p.bpm, (next.bpm - p.bpm) / len)
                } else {
                    (p.bpm, 0.0)
                }
            }
            _ => (p.bpm, 0.0),
        }
    }

    /// Seconds elapsed `beats` quarter notes into segment `i`.
    #[inline]
    fn segment_seconds(&self, i: usize, beats: f64) -> f64 {
        let (bpm0, k) = self.segment_shape(i);
        if k.abs() < 1e-12 {
            beats * 60.0 / bpm0
        } else {
            // ∫ 60 / (bpm0 + k·b) db = 60/k · ln((bpm0 + k·b) / bpm0)
            60.0 / k * ((bpm0 + k * beats) / bpm0).ln()
        }
    }

    /// Quarter notes elapsed after `secs` seconds into segment `i`.
    #[inline]
    fn segment_beats(&self, i: usize, secs: f64) -> f64 {
        let (bpm0, k) = self.segment_shape(i);
        if k.abs() < 1e-12 {
            secs * bpm0 / 60.0
        } else {
            bpm0 * ((k * secs / 60.0).exp() - 1.0) / k
        }
    }

    #[inline]
    fn segment_index_for_position(&self, pos: MusicalTime) -> usize {
        self.points
            .partition_point(|p| p.position <= pos)
            .saturating_sub(1)
    }

    #[inline]
    fn segment_index_for_seconds(&self, secs: f64) -> usize {
        self.seconds
            .partition_point(|&t| t <= secs)
            .saturating_sub(1)
    }

    /// Tempo (bpm) in effect at `pos`.
    pub fn bpm_at(&self, pos: MusicalTime) -> f64 {
        let i = self.segment_index_for_position(pos);
        let (bpm0, k) = self.segment_shape(i);
        let beats = (pos - self.points[i].position).quarters().max(0.0);
        bpm0 + k * beats
    }

    /// Absolute time in seconds of a musical position.
    ///
    /// Negative positions extrapolate the first tempo backwards.
    pub fn musical_to_seconds(&self, pos: MusicalTime) -> f64 {
        let i = self.segment_index_for_position(pos);
        let beats = (pos - self.points[i].position).quarters();
        if beats < 0.0 {
            return beats * 60.0 / self.points[0].bpm;
        }
        self.seconds[i] + self.segment_seconds(i, beats)
    }

    /// Fractional quarter-note position at an absolute time.
    pub fn seconds_to_quarters(&self, secs: f64) -> f64 {
        if secs < 0.0 {
            return secs * self.points[0].bpm / 60.0;
        }
        let i = self.segment_index_for_seconds(secs);
        self.points[i].position.quarters() + self.segment_beats(i, secs - self.seconds[i])
    }

    /// Musical position (nearest tick) at an absolute time.
    pub fn seconds_to_musical(&self, secs: f64) -> MusicalTime {
        MusicalTime((self.seconds_to_quarters(secs) * TICKS_PER_QUARTER as f64).round() as i64)
    }

    /// Musical position → nearest absolute sample index.
    pub fn musical_to_samples(&self, pos: MusicalTime, sample_rate: f64) -> i64 {
        (self.musical_to_seconds(pos) * sample_rate).round() as i64
    }

    /// Absolute sample index → nearest musical position.
    pub fn samples_to_musical(&self, samples: i64, sample_rate: f64) -> MusicalTime {
        self.seconds_to_musical(samples as f64 / sample_rate)
    }

    /// Fractional quarter-note position of a sample index (used per block by
    /// the transport; allocation-free).
    pub fn samples_to_quarters(&self, samples: i64, sample_rate: f64) -> f64 {
        self.seconds_to_quarters(samples as f64 / sample_rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    #[test]
    fn constant_tempo_conversions() {
        let map = TempoMap::new(120.0);
        // One quarter at 120 BPM is 0.5 s.
        assert!((map.musical_to_seconds(MusicalTime::QUARTER) - 0.5).abs() < 1e-12);
        assert_eq!(
            map.musical_to_samples(MusicalTime::from_quarters_i(4), SR),
            96_000
        );
        assert_eq!(map.samples_to_musical(24_000, SR), MusicalTime::QUARTER);
        assert!((map.bpm_at(MusicalTime::from_quarters_i(100)) - 120.0).abs() < 1e-12);
    }

    #[test]
    fn step_tempo_change() {
        let mut map = TempoMap::new(120.0);
        map.set_point(TempoPoint {
            position: MusicalTime::from_quarters_i(4),
            bpm: 60.0,
            curve: TempoCurve::Constant,
        });
        // 4 quarters at 120 = 2 s, then 2 quarters at 60 = 2 s.
        let t = map.musical_to_seconds(MusicalTime::from_quarters_i(6));
        assert!((t - 4.0).abs() < 1e-12);
        assert_eq!(map.seconds_to_musical(4.0), MusicalTime::from_quarters_i(6));
        assert_eq!(map.seconds_to_musical(1.0), MusicalTime::from_quarters_i(2));
    }

    #[test]
    fn linear_ramp_matches_numeric_integration() {
        let mut map = TempoMap::new(100.0);
        map.points[0].curve = TempoCurve::Linear;
        map.set_point(TempoPoint {
            position: MusicalTime::from_quarters_i(8),
            bpm: 140.0,
            curve: TempoCurve::Constant,
        });
        // Midway the tempo is 120.
        assert!((map.bpm_at(MusicalTime::from_quarters_i(4)) - 120.0).abs() < 1e-9);
        // Numerically integrate 60/bpm over 8 quarters.
        let steps = 100_000;
        let mut t = 0.0;
        for s in 0..steps {
            let b = (s as f64 + 0.5) * 8.0 / steps as f64;
            t += 60.0 / (100.0 + 5.0 * b) * (8.0 / steps as f64);
        }
        let exact = map.musical_to_seconds(MusicalTime::from_quarters_i(8));
        assert!((exact - t).abs() < 1e-6, "{exact} vs {t}");
        // Round trip inside the ramp.
        for q in [0.5, 1.0, 3.3, 7.9, 12.0] {
            let pos = MusicalTime::from_quarters(q);
            let secs = map.musical_to_seconds(pos);
            assert_eq!(map.seconds_to_musical(secs), pos);
        }
    }

    #[test]
    fn negative_positions_extrapolate() {
        let map = TempoMap::new(120.0);
        assert!((map.musical_to_seconds(MusicalTime::from_quarters_i(-2)) + 1.0).abs() < 1e-12);
        assert_eq!(
            map.seconds_to_musical(-1.0),
            MusicalTime::from_quarters_i(-2)
        );
    }

    #[test]
    fn serde_rebuilds_cache_and_validates() {
        let json = r#"{"points":[{"position":3840000,"bpm":90.0},{"position":0,"bpm":5000.0}]}"#;
        let map: TempoMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.points().len(), 2);
        assert_eq!(map.points()[0].bpm, TempoMap::MAX_BPM);
        assert!(map.musical_to_seconds(MusicalTime::from_quarters_i(5)) > 0.0);
        let back = serde_json::to_string(&map).unwrap();
        let again: TempoMap = serde_json::from_str(&back).unwrap();
        assert_eq!(map, again);
    }
}
