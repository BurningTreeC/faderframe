//! Automation data model and interpolation.
//!
//! Automation is part of the persistent project model. Curves are stored in
//! musical time with *plain* parameter values (dB for volume, -1..1 for pan,
//! the plugin's own range for plugin parameters). The engine evaluates them
//! per block (or per sub-block for sample accuracy) and turns them into
//! [`ParameterEvent`]s carrying a frame offset.

#![forbid(unsafe_code)]

use faderframe_core::{AutomationLaneId, ParameterId, PluginInstanceId, SendId};
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// Shape of the segment from a point to the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveShape {
    /// Hold the value until the next point.
    Step,
    #[default]
    Linear,
    /// S-shaped (smoothstep) ease-in/ease-out.
    Smooth,
    /// Exponential (constant ratio per unit time); falls back to linear if
    /// the endpoints do not share a strictly positive sign.
    Exponential,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub time: MusicalTime,
    pub value: f64,
    #[serde(default)]
    pub shape: CurveShape,
}

/// A time-ordered list of points. Points may share a time to express
/// instantaneous jumps; insertion order then decides which comes first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationCurve {
    points: Vec<AutomationPoint>,
}

impl AutomationCurve {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_points(mut points: Vec<AutomationPoint>) -> Self {
        points.sort_by_key(|p| p.time);
        Self { points }
    }

    pub fn points(&self) -> &[AutomationPoint] {
        &self.points
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Insert after any existing points at the same time; returns the index.
    pub fn insert(&mut self, point: AutomationPoint) -> usize {
        let i = self.points.partition_point(|p| p.time <= point.time);
        self.points.insert(i, point);
        i
    }

    pub fn remove(&mut self, index: usize) -> Option<AutomationPoint> {
        (index < self.points.len()).then(|| self.points.remove(index))
    }

    /// Replace point `index` (re-sorting if its time moved); returns its new
    /// index.
    pub fn update(&mut self, index: usize, point: AutomationPoint) -> Option<usize> {
        self.remove(index)?;
        Some(self.insert(point))
    }

    /// Replace everything in `[from, to]` by `points` (written automation),
    /// keeping the curve continuous at both ends: the old value is pinned
    /// just outside the written range.
    pub fn replace_range(
        &mut self,
        from: MusicalTime,
        to: MusicalTime,
        points: &[AutomationPoint],
    ) {
        let before = self.value_at(from);
        let after = self.value_at(to);
        let had_after = self.points.iter().any(|p| p.time > to);
        let had_before = self.points.iter().any(|p| p.time < from);
        self.points.retain(|p| p.time < from || p.time > to);
        if had_before && let Some(v) = before {
            self.insert(AutomationPoint {
                time: from,
                value: v,
                shape: CurveShape::Linear,
            });
        }
        for p in points {
            self.insert(*p);
        }
        if had_after && let Some(v) = after {
            self.insert(AutomationPoint {
                time: to,
                value: v,
                shape: CurveShape::Linear,
            });
        }
    }

    /// Indices of points inside `[from, to]`.
    pub fn range(&self, from: MusicalTime, to: MusicalTime) -> std::ops::Range<usize> {
        let a = self.points.partition_point(|p| p.time < from);
        let b = self.points.partition_point(|p| p.time <= to);
        a..b.max(a)
    }

    /// Value at `time`, or `None` for an empty curve. Before the first point
    /// the first value holds; after the last point the last value holds.
    pub fn value_at(&self, time: MusicalTime) -> Option<f64> {
        let first = self.points.first()?;
        if time < first.time {
            return Some(first.value);
        }
        // Index of the last point at or before `time`.
        let i = self.points.partition_point(|p| p.time <= time) - 1;
        let a = &self.points[i];
        let Some(b) = self.points.get(i + 1) else {
            return Some(a.value);
        };
        let span = (b.time - a.time).ticks();
        if span <= 0 {
            return Some(b.value);
        }
        let t = (time - a.time).ticks() as f64 / span as f64;
        Some(interpolate(a.value, b.value, t, a.shape))
    }
}

/// Drop points that a straight line between their neighbours reproduces
/// within `tolerance` (Ramer–Douglas–Peucker over time and value), keeping
/// the first and last point. Used to thin written automation.
pub fn thin(points: &[AutomationPoint], tolerance: f64) -> Vec<AutomationPoint> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (&points[a], &points[b]);
        let span = (pb.time - pa.time).ticks() as f64;
        let mut worst = (0.0, a);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let t = if span > 0.0 {
                (p.time - pa.time).ticks() as f64 / span
            } else {
                0.0
            };
            let line = pa.value + (pb.value - pa.value) * t;
            let d = (p.value - line).abs();
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// A curve converted to absolute engine samples, for the audio thread.
/// Evaluation is allocation-free (binary search + interpolation).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleLane {
    points: Vec<SamplePoint>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplePoint {
    pub at: i64,
    pub value: f64,
    pub shape: CurveShape,
}

impl SampleLane {
    /// Convert `curve` with `to_samples` (musical time → engine sample).
    pub fn from_curve(curve: &AutomationCurve, to_samples: impl Fn(MusicalTime) -> i64) -> Self {
        Self {
            points: curve
                .points()
                .iter()
                .map(|p| SamplePoint {
                    at: to_samples(p.time),
                    value: p.value,
                    shape: p.shape,
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn points(&self) -> &[SamplePoint] {
        &self.points
    }

    /// Value at engine sample `s` (`None` for an empty lane).
    #[inline]
    pub fn value_at(&self, s: i64) -> Option<f64> {
        let first = self.points.first()?;
        if s < first.at {
            return Some(first.value);
        }
        let i = self.points.partition_point(|p| p.at <= s) - 1;
        let a = &self.points[i];
        let Some(b) = self.points.get(i + 1) else {
            return Some(a.value);
        };
        let span = b.at - a.at;
        if span <= 0 {
            return Some(b.value);
        }
        Some(interpolate(
            a.value,
            b.value,
            (s - a.at) as f64 / span as f64,
            a.shape,
        ))
    }

    /// First point strictly after `s` and before `end` (for placing
    /// sample-accurate events at breakpoints).
    #[inline]
    pub fn next_point_in(&self, s: i64, end: i64) -> Option<i64> {
        let i = self.points.partition_point(|p| p.at <= s);
        self.points.get(i).map(|p| p.at).filter(|&at| at < end)
    }
}

/// Interpolate between `a` and `b` at fraction `t` (0..=1) with `shape`.
pub fn interpolate(a: f64, b: f64, t: f64, shape: CurveShape) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match shape {
        CurveShape::Step => a,
        CurveShape::Linear => a + (b - a) * t,
        CurveShape::Smooth => {
            let s = t * t * (3.0 - 2.0 * t);
            a + (b - a) * s
        }
        CurveShape::Exponential => {
            if (a > 0.0 && b > 0.0) || (a < 0.0 && b < 0.0) {
                a * (b / a).powf(t)
            } else {
                a + (b - a) * t
            }
        }
    }
}

/// How a lane interacts with live control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    /// Lane is ignored.
    Off,
    /// Lane drives the parameter.
    #[default]
    Read,
    /// Writes while the control is touched, returns to the curve on release.
    Touch,
    /// Writes from first touch until playback stops.
    Latch,
    /// Overwrites everything while playing.
    Write,
}

/// What a lane controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTarget {
    TrackVolume,
    TrackPan,
    TrackMute,
    SendLevel(SendId),
    PluginParameter {
        plugin: PluginInstanceId,
        parameter: ParameterId,
    },
    PluginBypass(PluginInstanceId),
    /// Where the track sits in the surround bed it feeds.
    Surround(faderframe_core::SurroundParam),
}

impl AutomationTarget {
    pub fn label(&self) -> String {
        match self {
            AutomationTarget::TrackVolume => "Volume".into(),
            AutomationTarget::TrackPan => "Pan".into(),
            AutomationTarget::TrackMute => "Mute".into(),
            AutomationTarget::SendLevel(s) => format!("Send {s}"),
            AutomationTarget::PluginParameter { plugin, parameter } => {
                format!("{plugin} param {}", parameter.0)
            }
            AutomationTarget::PluginBypass(p) => format!("{p} bypass"),
            AutomationTarget::Surround(p) => p.name().into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub id: AutomationLaneId,
    pub target: AutomationTarget,
    pub curve: AutomationCurve,
    #[serde(default)]
    pub mode: AutomationMode,
    #[serde(default)]
    pub visible: bool,
}

/// All automation lanes of one track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationSet {
    pub lanes: Vec<AutomationLane>,
}

impl AutomationSet {
    pub fn lane(&self, target: AutomationTarget) -> Option<&AutomationLane> {
        self.lanes.iter().find(|l| l.target == target)
    }

    pub fn lane_mut(&mut self, target: AutomationTarget) -> Option<&mut AutomationLane> {
        self.lanes.iter_mut().find(|l| l.target == target)
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }
}

/// A sample-accurate parameter change inside a processing block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParameterEvent {
    pub parameter: ParameterId,
    pub value: f32,
    pub sample_offset: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: f64) -> MusicalTime {
        MusicalTime::from_quarters(n)
    }

    fn pt(t: f64, v: f64, shape: CurveShape) -> AutomationPoint {
        AutomationPoint {
            time: q(t),
            value: v,
            shape,
        }
    }

    #[test]
    fn empty_and_edges() {
        let mut c = AutomationCurve::new();
        assert_eq!(c.value_at(q(1.0)), None);
        c.insert(pt(2.0, -6.0, CurveShape::Linear));
        assert_eq!(c.value_at(q(0.0)), Some(-6.0));
        assert_eq!(c.value_at(q(9.0)), Some(-6.0));
    }

    #[test]
    fn linear_step_smooth_exponential() {
        let c = AutomationCurve::from_points(vec![
            pt(0.0, 0.0, CurveShape::Linear),
            pt(4.0, 1.0, CurveShape::Step),
            pt(8.0, 0.0, CurveShape::Smooth),
            pt(12.0, 1.0, CurveShape::Exponential),
            pt(16.0, 100.0, CurveShape::Linear),
        ]);
        assert!((c.value_at(q(1.0)).unwrap() - 0.25).abs() < 1e-12);
        assert_eq!(c.value_at(q(6.0)), Some(1.0));
        assert_eq!(c.value_at(q(8.0)), Some(0.0));
        // Smoothstep is symmetric around the midpoint and flat at the ends.
        assert!((c.value_at(q(10.0)).unwrap() - 0.5).abs() < 1e-12);
        assert!(c.value_at(q(8.5)).unwrap() < 0.125 * 0.5);
        // Exponential 1 → 100 halfway is 10.
        assert!((c.value_at(q(14.0)).unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn coincident_points_make_a_jump() {
        let mut c = AutomationCurve::new();
        c.insert(pt(0.0, 0.0, CurveShape::Linear));
        c.insert(pt(4.0, 1.0, CurveShape::Linear));
        c.insert(pt(4.0, 5.0, CurveShape::Linear));
        c.insert(pt(8.0, 5.0, CurveShape::Linear));
        assert!((c.value_at(q(3.999)).unwrap() - 1.0).abs() < 1e-3);
        assert_eq!(c.value_at(q(4.0)), Some(5.0));
    }

    #[test]
    fn sample_lane_matches_curve_and_finds_breakpoints() {
        let c = AutomationCurve::from_points(vec![
            pt(0.0, 0.0, CurveShape::Linear),
            pt(1.0, 1.0, CurveShape::Step),
            pt(2.0, 0.5, CurveShape::Linear),
        ]);
        // 1 quarter = 24 000 samples (120 bpm, 48 kHz).
        let l = SampleLane::from_curve(&c, |t| (t.quarters() * 24_000.0) as i64);
        assert_eq!(l.value_at(12_000), Some(0.5));
        assert_eq!(l.value_at(30_000), Some(1.0), "step holds");
        assert_eq!(l.value_at(48_000), Some(0.5));
        assert_eq!(l.next_point_in(0, 30_000), Some(24_000));
        assert_eq!(l.next_point_in(24_000, 40_000), None);
        assert_eq!(SampleLane::default().value_at(5), None);
    }

    #[test]
    fn replace_range_keeps_the_curve_continuous() {
        let mut c = AutomationCurve::from_points(vec![
            pt(0.0, 0.0, CurveShape::Linear),
            pt(8.0, 8.0, CurveShape::Linear),
        ]);
        c.replace_range(
            q(2.0),
            q(4.0),
            &[
                pt(2.5, 10.0, CurveShape::Linear),
                pt(3.5, 10.0, CurveShape::Linear),
            ],
        );
        assert!(
            (c.value_at(q(2.0)).unwrap() - 2.0).abs() < 1e-9,
            "old value pinned at the start"
        );
        assert_eq!(c.value_at(q(3.0)), Some(10.0));
        assert!(
            (c.value_at(q(4.0)).unwrap() - 4.0).abs() < 1e-9,
            "old value restored at the end"
        );
        assert!((c.value_at(q(6.0)).unwrap() - 6.0).abs() < 1e-9);
        assert_eq!(c.range(q(2.0), q(4.0)).len(), 4);
    }

    #[test]
    fn thinning_keeps_shape_within_tolerance() {
        let pts: Vec<_> = (0..=100)
            .map(|i| {
                let x = i as f64 / 10.0;
                pt(x, if x < 5.0 { x } else { 10.0 - x }, CurveShape::Linear)
            })
            .collect();
        let thin = thin(&pts, 0.01);
        assert_eq!(thin.len(), 3, "a triangle needs three points");
        assert_eq!(thin[1].time, q(5.0));
    }

    #[test]
    fn exponential_falls_back_on_sign_change() {
        assert_eq!(interpolate(-1.0, 1.0, 0.5, CurveShape::Exponential), 0.0);
        assert_eq!(interpolate(2.0, 8.0, 0.5, CurveShape::Exponential), 4.0);
    }
}
