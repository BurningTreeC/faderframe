//! Clip envelopes: volume and pan that belong to an audio clip.
//!
//! An envelope's points are in clip-relative output frames at the project
//! rate (frame 0 = the clip's first played frame), so the envelope moves
//! with the clip and stays with its audio: a start trim shifts the points
//! by what it took off (points left outside the clip are kept, so a trim
//! back brings them again), a split cuts it in two, a time-stretch trim
//! scales it. Segments have the shapes of track automation
//! ([`CurveShape`]); volume is in dB (at or below [`FLOOR_DB`]: silent),
//! pan −1…1 (a balance: the far side fades out, the near one stays).
//! The engine applies it as the clip plays, sample by sample, after the
//! clip's gain and fades (and after clip effects and spectral edits, which
//! are rendered into the audio it plays).

use faderframe_automation::{CurveShape, interpolate};
use serde::{Deserialize, Serialize};

/// Volume at or below this is silence.
pub const FLOOR_DB: f64 = -96.0;
/// The loudest a volume point may be.
pub const MAX_DB: f64 = 24.0;

/// The parameters a clip envelope can drive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeLane {
    #[default]
    Volume,
    Pan,
}

impl EnvelopeLane {
    pub const ALL: [EnvelopeLane; 2] = [EnvelopeLane::Volume, EnvelopeLane::Pan];

    pub fn label(self) -> &'static str {
        match self {
            EnvelopeLane::Volume => "Volume",
            EnvelopeLane::Pan => "Pan",
        }
    }

    /// Lowest and highest value.
    pub fn range(self) -> (f64, f64) {
        match self {
            EnvelopeLane::Volume => (FLOOR_DB, MAX_DB),
            EnvelopeLane::Pan => (-1.0, 1.0),
        }
    }

    /// The value with no envelope (0 dB, centre).
    pub fn neutral(self) -> f64 {
        0.0
    }

    /// `v` within the lane's range (NaN: neutral).
    pub fn clamp(self, v: f64) -> f64 {
        let (lo, hi) = self.range();
        if v.is_nan() {
            self.neutral()
        } else {
            v.clamp(lo, hi)
        }
    }

    /// A value as the views show it.
    pub fn value_text(self, v: f64) -> String {
        match self {
            EnvelopeLane::Volume if v <= FLOOR_DB => "−∞ dB".into(),
            EnvelopeLane::Volume => format!("{v:+.1} dB"),
            EnvelopeLane::Pan if v.abs() < 0.005 => "C".into(),
            EnvelopeLane::Pan if v < 0.0 => format!("L{:.0}", -v * 100.0),
            EnvelopeLane::Pan => format!("R{:.0}", v * 100.0),
        }
    }

    /// Height on a lane (0 bottom … 1 top) of a value: volume on a fader
    /// law (the dBs near 0 get the room), pan linear.
    pub fn to_unit(self, v: f64) -> f64 {
        match self {
            EnvelopeLane::Volume => {
                if v <= FLOOR_DB {
                    0.0
                } else {
                    // 0 dB at 3/4 of the height, +24 at the top, a
                    // quartic-root law below.
                    let g = 10f64.powf(v / 20.0);
                    let top = 10f64.powf(MAX_DB / 20.0);
                    if g >= 1.0 {
                        0.75 + 0.25 * (v / MAX_DB)
                    } else {
                        let _ = top;
                        0.75 * g.powf(0.25)
                    }
                }
            }
            EnvelopeLane::Pan => (v + 1.0) / 2.0,
        }
        .clamp(0.0, 1.0)
    }

    /// The value at a height (inverse of [`Self::to_unit`]).
    pub fn from_unit(self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            EnvelopeLane::Volume => {
                if u >= 0.75 {
                    (u - 0.75) / 0.25 * MAX_DB
                } else if u <= 0.0 {
                    FLOOR_DB
                } else {
                    let g = (u / 0.75).powi(4);
                    (20.0 * g.log10()).max(FLOOR_DB)
                }
            }
            EnvelopeLane::Pan => u * 2.0 - 1.0,
        }
    }
}

/// A breakpoint: clip-relative output frame (project rate), value, and
/// the shape of the segment to the next point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvelopePoint {
    pub at: i64,
    pub value: f64,
    #[serde(default)]
    pub shape: CurveShape,
}

impl EnvelopePoint {
    pub fn new(at: i64, value: f64) -> Self {
        Self {
            at,
            value,
            shape: CurveShape::Linear,
        }
    }
}

/// A clip's envelopes (`AudioClip::envelope`; `None` there when empty).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipEnvelope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume: Vec<EnvelopePoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pan: Vec<EnvelopePoint>,
}

impl ClipEnvelope {
    pub fn lane(&self, lane: EnvelopeLane) -> &[EnvelopePoint] {
        match lane {
            EnvelopeLane::Volume => &self.volume,
            EnvelopeLane::Pan => &self.pan,
        }
    }

    pub fn lane_mut(&mut self, lane: EnvelopeLane) -> &mut Vec<EnvelopePoint> {
        match lane {
            EnvelopeLane::Volume => &mut self.volume,
            EnvelopeLane::Pan => &mut self.pan,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.volume.is_empty() && self.pan.is_empty()
    }

    /// As an `AudioClip::envelope` (`None` when empty).
    pub fn boxed(self) -> Option<Box<ClipEnvelope>> {
        (!self.is_empty()).then(|| Box::new(self))
    }

    /// The envelope with `lane` replaced by `points` (sorted, clamped).
    pub fn with_lane(
        envelope: Option<&ClipEnvelope>,
        lane: EnvelopeLane,
        points: Vec<EnvelopePoint>,
    ) -> Option<Box<ClipEnvelope>> {
        let mut e = envelope.cloned().unwrap_or_default();
        *e.lane_mut(lane) = sanitize(lane, points);
        e.boxed()
    }

    /// Points sorted (stable: equal times keep their order, a jump) and
    /// values clamped to their lanes.
    pub fn sanitized(mut self) -> Self {
        for lane in EnvelopeLane::ALL {
            let points = std::mem::take(self.lane_mut(lane));
            *self.lane_mut(lane) = sanitize(lane, points);
        }
        self
    }

    /// Value of `lane` at clip frame `at` (`None` without points). Before
    /// the first point the first value holds, after the last the last.
    pub fn value_at(&self, lane: EnvelopeLane, at: f64) -> Option<f64> {
        value_at(self.lane(lane), at)
    }

    /// Linear gain of the volume lane at `at` (1 without points).
    pub fn gain_at(&self, at: f64) -> f32 {
        self.value_at(EnvelopeLane::Volume, at).map_or(1.0, db_gain)
    }

    /// Every point moved by `by` frames (a start trim by `d`: `-d`).
    pub fn shift(&mut self, by: i64) {
        for p in self.volume.iter_mut().chain(self.pan.iter_mut()) {
            p.at += by;
        }
    }

    /// Times scaled by `factor` (a time-stretch trim).
    pub fn scale(&mut self, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        for p in self.volume.iter_mut().chain(self.pan.iter_mut()) {
            p.at = (p.at as f64 * factor).round() as i64;
        }
    }

    /// The part between clip frames `from` and `to`, moved to start at 0
    /// (a split's piece): the points inside, and where the curve went on
    /// beyond an edge, a point there holding its value.
    pub fn slice(&self, from: i64, to: i64) -> ClipEnvelope {
        let mut out = ClipEnvelope::default();
        for lane in EnvelopeLane::ALL {
            let points = self.lane(lane);
            if points.is_empty() {
                continue;
            }
            let mut kept: Vec<EnvelopePoint> = Vec::new();
            if points.first().is_some_and(|p| p.at < from)
                && let Some(v) = value_at(points, from as f64)
            {
                kept.push(EnvelopePoint {
                    at: 0,
                    value: v,
                    shape: shape_at(points, from),
                });
            }
            kept.extend(
                points
                    .iter()
                    .filter(|p| p.at >= from && p.at <= to)
                    .map(|p| EnvelopePoint {
                        at: p.at - from,
                        ..*p
                    }),
            );
            if points.last().is_some_and(|p| p.at > to)
                && let Some(v) = value_at(points, to as f64)
                && kept.last().is_none_or(|p| p.at < to - from)
            {
                kept.push(EnvelopePoint::new(to - from, v));
            }
            *out.lane_mut(lane) = kept;
        }
        out
    }
}

/// Points sorted by time (stable) with values clamped and NaNs dropped.
pub fn sanitize(lane: EnvelopeLane, mut points: Vec<EnvelopePoint>) -> Vec<EnvelopePoint> {
    points.retain(|p| !p.value.is_nan());
    for p in &mut points {
        p.value = lane.clamp(p.value);
    }
    points.sort_by_key(|p| p.at);
    points
}

/// Linear gain of a volume in dB (0 at or below [`FLOOR_DB`]).
#[inline]
pub fn db_gain(db: f64) -> f32 {
    if db <= FLOOR_DB {
        0.0
    } else {
        10f64.powf(db.min(MAX_DB) / 20.0) as f32
    }
}

/// Balance gains (left, right) of a pan position: the far side fades out,
/// the near one stays at unity (a mono clip on a stereo track pans
/// linearly with the centre at 0 dB).
#[inline]
pub fn balance(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0) as f32;
    if p >= 0.0 {
        (1.0 - p, 1.0)
    } else {
        (1.0, 1.0 + p)
    }
}

/// Value of sorted `points` at `at`.
pub fn value_at(points: &[EnvelopePoint], at: f64) -> Option<f64> {
    let first = points.first()?;
    if at < first.at as f64 {
        return Some(first.value);
    }
    let i = points.partition_point(|p| p.at as f64 <= at) - 1;
    let a = &points[i];
    let Some(b) = points.get(i + 1) else {
        return Some(a.value);
    };
    let span = (b.at - a.at) as f64;
    if span <= 0.0 {
        return Some(b.value);
    }
    Some(interpolate(
        a.value,
        b.value,
        (at - a.at as f64) / span,
        a.shape,
    ))
}

/// The shape of the segment running through `at`.
fn shape_at(points: &[EnvelopePoint], at: i64) -> CurveShape {
    let i = points.partition_point(|p| p.at <= at);
    i.checked_sub(1)
        .and_then(|i| points.get(i))
        .map_or(CurveShape::Linear, |p| p.shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(points: &[(i64, f64)]) -> ClipEnvelope {
        ClipEnvelope {
            volume: points
                .iter()
                .map(|&(a, v)| EnvelopePoint::new(a, v))
                .collect(),
            pan: Vec::new(),
        }
    }

    #[test]
    fn values_follow_the_points_and_hold_outside() {
        let e = env(&[(100, -6.0), (200, 0.0)]);
        assert_eq!(e.value_at(EnvelopeLane::Volume, 0.0), Some(-6.0));
        assert_eq!(e.value_at(EnvelopeLane::Volume, 150.0), Some(-3.0));
        assert_eq!(e.value_at(EnvelopeLane::Volume, 500.0), Some(0.0));
        assert_eq!(e.value_at(EnvelopeLane::Pan, 0.0), None);
        assert!((e.gain_at(100.0) - 0.501_187).abs() < 1e-5);
        assert_eq!(db_gain(FLOOR_DB), 0.0);
    }

    #[test]
    fn a_slice_keeps_the_curve_at_its_edges() {
        let e = env(&[(0, 0.0), (100, -10.0), (300, -30.0)]);
        let s = e.slice(50, 200);
        assert_eq!(
            s.volume,
            vec![
                EnvelopePoint::new(0, -5.0),
                EnvelopePoint::new(50, -10.0),
                EnvelopePoint::new(150, -20.0),
            ]
        );
        // Both pieces together play the curve as before.
        let right = e.slice(200, 400);
        for at in [0, 25, 100] {
            assert_eq!(
                right.value_at(EnvelopeLane::Volume, at as f64),
                e.value_at(EnvelopeLane::Volume, (200 + at) as f64)
            );
        }
    }

    #[test]
    fn sanitizing_sorts_and_clamps() {
        let e = ClipEnvelope {
            volume: vec![
                EnvelopePoint::new(10, 99.0),
                EnvelopePoint::new(5, f64::NAN),
            ],
            pan: vec![EnvelopePoint::new(3, -4.0)],
        }
        .sanitized();
        assert_eq!(e.volume, vec![EnvelopePoint::new(10, MAX_DB)]);
        assert_eq!(e.pan, vec![EnvelopePoint::new(3, -1.0)]);
    }

    #[test]
    fn heights_and_values_round_trip() {
        for v in [-60.0, -12.0, -3.0, 0.0, 6.0, 24.0] {
            let u = EnvelopeLane::Volume.to_unit(v);
            assert!((EnvelopeLane::Volume.from_unit(u) - v).abs() < 1e-9, "{v}");
        }
        assert_eq!(
            EnvelopeLane::Pan.from_unit(EnvelopeLane::Pan.to_unit(0.25)),
            0.25
        );
        assert_eq!(balance(0.5), (0.5, 1.0));
        assert_eq!(balance(-1.0), (1.0, 0.0));
    }
}
