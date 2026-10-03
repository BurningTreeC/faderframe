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
    fn exponential_falls_back_on_sign_change() {
        assert_eq!(interpolate(-1.0, 1.0, 0.5, CurveShape::Exponential), 0.0);
        assert_eq!(interpolate(2.0, 8.0, 0.5, CurveShape::Exponential), 4.0);
    }
}
