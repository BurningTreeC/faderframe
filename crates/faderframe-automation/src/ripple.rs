//! Ripple edits of a curve: everything at or after a cut moves by an
//! offset, the curve keeping its exact shape on both sides.
//!
//! Automation is its own entity, not part of the clips: a ripple moves a
//! track's clips and its automation by the same offset at the same cut,
//! so they stay in sync. [`AutomationCurve::ripple`] is the curve's half.
//!
//! * **Opening time** (`delta > 0`): the points at or after `cut` move
//!   later. A point exactly on the cut is duplicated — one copy stays on
//!   the cut, the original moves to `cut + delta`; a segment the cut
//!   crosses gets two new points with the value it has there, one staying
//!   and one moving. The opened gap holds the value at the cut: the
//!   point's there (of several points on the cut — a jump — the first
//!   one's: the jump belongs to what follows and moves with it), else the
//!   cut segment's.
//! * **Taking time out** (`delta < 0`): `[cut + delta, cut)` goes; what
//!   follows moves up. The curve keeps the value it reached at
//!   `cut + delta` and jumps there to the value it had at `cut`; both cut
//!   segments end and start with the values they had.
//! * **Shapes**: linear, step and exponential segments cut in two are the
//!   same shape again (a piece of an exponential is an exponential; an
//!   exponential between values of different sign is a straight line and
//!   its pieces become linear). A piece of a smooth (S-shaped) segment is
//!   not an S, so it is redrawn as straight pieces within
//!   [`SMOOTH_TOLERANCE`] of the original.
//! * **Cost**: the cut is found by binary search, the moving part is
//!   detached with `split_off` and shifted in place: O(log n + k) for k
//!   points after the cut, nothing before it is visited. (Shifting is
//!   O(k) for any sorted array; a lazily offset tree would make it
//!   O(log n), but every reader of the curve and the engine's sample
//!   conversion would then pay for it.)
//!
//! The curve is plain owned data edited on the control thread through
//! `&mut self`; the audio thread never reads it (it gets a converted
//! [`crate::SampleLane`] in the next timeline snapshot), so there is
//! nothing shared to race on and no lock.

use crate::{AutomationCurve, AutomationPoint, CurveShape, interpolate};
use faderframe_timeline::MusicalTime;

/// How far the straight pieces redrawing a cut smooth segment may stray
/// from it, as a share of the segment's rise.
pub const SMOOTH_TOLERANCE: f64 = 1e-3;

/// The value of the segment `a → b` at `t` (`a.time ≤ t ≤ b.time`).
fn segment_value(a: &AutomationPoint, b: &AutomationPoint, t: MusicalTime) -> f64 {
    let span = (b.time - a.time).ticks();
    if span <= 0 {
        return b.value;
    }
    let f = (t - a.time).ticks() as f64 / span as f64;
    interpolate(a.value, b.value, f, a.shape)
}

/// The shape a piece of the segment `a → b` keeps: its own, except an
/// exponential between values of different sign, which draws a straight
/// line (and its pieces might share a sign).
fn piece_shape(a: &AutomationPoint, b: &AutomationPoint) -> CurveShape {
    match a.shape {
        CurveShape::Exponential
            if !((a.value > 0.0 && b.value > 0.0) || (a.value < 0.0 && b.value < 0.0)) =>
        {
            CurveShape::Linear
        }
        shape => shape,
    }
}

/// Straight pieces redrawing the smooth segment `a → b` over `(from, to)`
/// (the ends left out): points strictly inside, moved by `by`.
fn smooth_points(
    a: &AutomationPoint,
    b: &AutomationPoint,
    from: MusicalTime,
    to: MusicalTime,
    by: MusicalTime,
) -> Vec<AutomationPoint> {
    let span = (b.time - a.time).ticks();
    let part = (to - from).ticks();
    if span <= 0 || part <= 1 {
        return Vec::new();
    }
    // Linear interpolation of smoothstep strays at most 0.75·h² of the
    // rise (|s''| ≤ 6, error ≤ h²·max|s''|/8) for pieces h of the whole.
    let whole = (0.75 / SMOOTH_TOLERANCE).sqrt().ceil();
    let pieces = ((whole * part as f64 / span as f64).ceil() as i64).clamp(1, part);
    let mut out = Vec::with_capacity(pieces as usize);
    let mut last = from;
    for j in 1..pieces {
        let t = MusicalTime(from.ticks() + part * j / pieces);
        if t > last && t < to {
            out.push(AutomationPoint {
                time: t + by,
                value: segment_value(a, b, t),
                shape: CurveShape::Linear,
            });
            last = t;
        }
    }
    out
}

/// A point that holds its value (a stretch with no change of its own).
fn held(time: MusicalTime, value: f64) -> AutomationPoint {
    AutomationPoint {
        time,
        value,
        shape: CurveShape::Linear,
    }
}

impl AutomationCurve {
    /// The value an approach from the left meets at `time`: the first of
    /// several points there ([`Self::value_at`] gives the last, where the
    /// curve goes on from); `None` for an empty curve.
    pub fn value_before(&self, time: MusicalTime) -> Option<f64> {
        let first = self.points.first()?;
        if time <= first.time {
            return Some(first.value);
        }
        // The last point before `time`, and the one after it.
        let i = self.points.partition_point(|p| p.time < time) - 1;
        let a = &self.points[i];
        Some(match self.points.get(i + 1) {
            Some(b) => segment_value(a, b, time),
            None => a.value,
        })
    }

    /// Ripple the curve: everything at or after `cut` moves by `delta`
    /// (see the module docs) — later for a positive one, opening a gap that
    /// holds the value reached there; earlier for a negative one, taking
    /// `[cut + delta, cut)` out (never before zero: the offset is cut short
    /// there). Returns whether anything changed.
    pub fn ripple(&mut self, cut: MusicalTime, delta: MusicalTime) -> bool {
        if delta > MusicalTime::ZERO {
            self.open(cut, delta)
        } else if delta < MusicalTime::ZERO {
            let from = (cut + delta).max(MusicalTime::ZERO);
            from < cut && self.close(from, cut)
        } else {
            false
        }
    }

    /// Open `len` at `at`.
    fn open(&mut self, at: MusicalTime, len: MusicalTime) -> bool {
        // The first point at or after the cut: from there on all move.
        let i = self.points.partition_point(|p| p.time < at);
        if i == self.points.len() {
            // Nothing after the cut: the last value holds as it did.
            return false;
        }
        let mut tail = self.points.split_off(i);
        let next = tail[0];
        for p in &mut tail {
            p.time += len;
        }
        // Without a point before the cut the first value holds before it,
        // over the gap too: nothing to add.
        if let Some(&prev) = self.points.last() {
            if next.time == at {
                // On a point: a copy stays (the first there — the value
                // reached from the left), the original moves.
                self.points.push(held(at, next.value));
            } else {
                // Across a segment: its value at the cut stays and moves.
                let v = segment_value(&prev, &next, at);
                let shape = piece_shape(&prev, &next);
                if shape == CurveShape::Smooth {
                    let left = smooth_points(&prev, &next, prev.time, at, MusicalTime::ZERO);
                    if let Some(p) = self.points.last_mut() {
                        p.shape = CurveShape::Linear;
                    }
                    self.points.extend(left);
                    self.points.push(held(at, v));
                    self.points.push(held(at + len, v));
                    self.points
                        .extend(smooth_points(&prev, &next, at, next.time, len));
                } else {
                    if let Some(p) = self.points.last_mut() {
                        p.shape = shape;
                    }
                    self.points.push(held(at, v));
                    self.points.push(AutomationPoint {
                        time: at + len,
                        value: v,
                        shape,
                    });
                }
            }
        }
        self.points.append(&mut tail);
        true
    }

    /// Take `[a, b)` out.
    fn close(&mut self, a: MusicalTime, b: MusicalTime) -> bool {
        let ia = self.points.partition_point(|p| p.time < a);
        if ia == self.points.len() {
            // Nothing at or after `a`: nothing moves, nothing goes.
            return false;
        }
        let ib = self.points.partition_point(|p| p.time < b);
        let len = b - a;
        // What the curve reached at `a` and what it goes on with at `b`,
        // from the curve as it is.
        let left = self.value_before(a);
        // The segment across `b` (from the last point before it), when `b`
        // is not on a point: its piece after `b` starts the moved part.
        let across_b = (ib > 0 && self.points.get(ib).is_none_or(|p| p.time > b))
            .then(|| (self.points[ib - 1], self.points.get(ib).copied()));
        // The segment across `a`, when `a` is not on a point.
        let across_a =
            (ia > 0 && self.points[ia].time > a).then(|| (self.points[ia - 1], self.points[ia]));
        let mut tail = self.points.split_off(ib);
        let removed = self.points.split_off(ia);
        for p in &mut tail {
            p.time -= len;
        }
        // The piece before `a` keeps its shape.
        if let Some((prev, next)) = across_a {
            let shape = piece_shape(&prev, &next);
            if shape == CurveShape::Smooth {
                if let Some(p) = self.points.last_mut() {
                    p.shape = CurveShape::Linear;
                }
                self.points
                    .extend(smooth_points(&prev, &next, prev.time, a, MusicalTime::ZERO));
            } else if let Some(p) = self.points.last_mut() {
                p.shape = shape;
            }
        }
        // The value reached at `a` stays there: after what comes before
        // it, or where points inside the cut went (the first value used to
        // hold before them).
        let reached = left
            .filter(|_| ia > 0 || (!removed.is_empty() && a > MusicalTime::ZERO))
            .map(|v| held(a, v));
        // The piece after `b`, from `a` on.
        let mut after: Vec<AutomationPoint> = Vec::new();
        if let Some((prev, next)) = across_b {
            match next {
                Some(next) => {
                    let v = segment_value(&prev, &next, b);
                    let shape = piece_shape(&prev, &next);
                    if shape == CurveShape::Smooth {
                        after.push(held(a, v));
                        after.extend(smooth_points(
                            &prev,
                            &next,
                            b,
                            next.time,
                            MusicalTime(-len.ticks()),
                        ));
                    } else {
                        after.push(AutomationPoint {
                            time: a,
                            value: v,
                            shape,
                        });
                    }
                }
                // Past the last point: its value held on.
                None => after.push(held(a, prev.value)),
            }
        }
        // A value reached that the curve goes on with anyway is no jump.
        let goes_on = after
            .first()
            .or(tail.first())
            .filter(|p| p.time == a)
            .map(|p| p.value);
        if let Some(p) = reached.filter(|p| Some(p.value) != goes_on) {
            self.points.push(p);
        }
        self.points.append(&mut after);
        self.points.append(&mut tail);
        true
    }
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

    fn lin(t: f64, v: f64) -> AutomationPoint {
        pt(t, v, CurveShape::Linear)
    }

    fn near(a: Option<f64>, b: f64, tol: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() <= tol)
    }

    /// The curve rippled at `cut` by `delta` (quarters).
    fn rippled(c: &AutomationCurve, cut: f64, delta: f64) -> AutomationCurve {
        let mut r = c.clone();
        r.ripple(q(cut), MusicalTime::from_quarters(delta));
        r
    }

    /// Opening: before the cut and after the gap the curve is what it
    /// was before and after the cut, every shape; the gap holds.
    #[test]
    fn opening_keeps_both_sides_of_every_shape() {
        for shape in [
            CurveShape::Linear,
            CurveShape::Step,
            CurveShape::Smooth,
            CurveShape::Exponential,
        ] {
            let c = AutomationCurve::from_points(vec![
                pt(0.0, 1.0, shape),
                pt(8.0, 9.0, CurveShape::Linear),
                lin(12.0, 3.0),
            ]);
            let r = rippled(&c, 3.0, 4.0);
            let tol = 9.0 * SMOOTH_TOLERANCE * 1.01;
            for i in 0..=30 {
                let t = i as f64 / 10.0;
                assert!(
                    near(r.value_at(q(t)), c.value_at(q(t)).unwrap(), tol),
                    "{shape:?} before {t}"
                );
            }
            let at_cut = c.value_at(q(3.0)).unwrap();
            for t in [3.0, 4.5, 6.99] {
                assert!(
                    near(r.value_at(q(t)), at_cut, 1e-9),
                    "{shape:?} gap holds at {t}"
                );
            }
            for i in 0..=90 {
                let t = 3.0 + i as f64 / 10.0;
                assert!(
                    near(r.value_at(q(t + 4.0)), c.value_at(q(t)).unwrap(), tol),
                    "{shape:?} after the gap at {t}"
                );
            }
        }
    }

    /// A point on the cut is duplicated: one stays, one moves.
    #[test]
    fn a_point_on_the_cut_is_duplicated() {
        let c = AutomationCurve::from_points(vec![lin(0.0, 0.0), lin(4.0, 4.0), lin(8.0, 0.0)]);
        let r = rippled(&c, 4.0, 2.0);
        let times: Vec<f64> = r.points().iter().map(|p| p.time.quarters()).collect();
        assert_eq!(times, vec![0.0, 4.0, 6.0, 10.0]);
        assert_eq!(r.points()[1].value, 4.0);
        assert_eq!(r.points()[2].value, 4.0);
        assert!(near(r.value_at(q(5.0)), 4.0, 1e-12));
    }

    /// A segment crossing the cut gets two points at its value there.
    #[test]
    fn a_cut_segment_gets_two_points_at_its_value() {
        let c = AutomationCurve::from_points(vec![lin(0.0, 0.0), lin(4.0, 4.0)]);
        let r = rippled(&c, 1.0, 2.0);
        let pts: Vec<(f64, f64)> = r
            .points()
            .iter()
            .map(|p| (p.time.quarters(), p.value))
            .collect();
        assert_eq!(pts, vec![(0.0, 0.0), (1.0, 1.0), (3.0, 1.0), (6.0, 4.0)]);
    }

    /// A jump on the cut moves with what follows; the gap holds the value
    /// reached from the left.
    #[test]
    fn a_jump_on_the_cut_moves_with_what_follows() {
        let c = AutomationCurve::from_points(vec![
            lin(0.0, 0.0),
            lin(4.0, 4.0),
            lin(4.0, -10.0),
            lin(8.0, -10.0),
        ]);
        let r = rippled(&c, 4.0, 2.0);
        assert!(near(r.value_at(q(3.0)), 3.0, 1e-12));
        assert!(
            near(r.value_at(q(5.0)), 4.0, 1e-12),
            "the gap holds the left value"
        );
        assert_eq!(r.value_at(q(6.0)), Some(-10.0), "the jump moved");
        assert!(near(r.value_before(q(6.0)), 4.0, 1e-12));
    }

    /// Nothing changes where there is nothing to move, and nothing is added
    /// where only the first value held before the cut.
    #[test]
    fn edges_add_nothing_that_changes_nothing() {
        let c = AutomationCurve::from_points(vec![lin(2.0, 1.0), lin(4.0, 5.0)]);
        // After the last point.
        let mut r = c.clone();
        assert!(!r.ripple(q(6.0), q(1.0)));
        assert_eq!(r, c);
        assert!(!r.ripple(q(6.0), MusicalTime::from_quarters(-1.0)));
        assert_eq!(r, c);
        // Before the first point: only shifted.
        let r = rippled(&c, 1.0, 2.0);
        assert_eq!(r.points().len(), 2);
        assert_eq!(r.points()[0].time, q(4.0));
        // On the first point: no copy needed (its value held before it).
        let r = rippled(&c, 2.0, 2.0);
        assert_eq!(r.points().len(), 2);
        assert_eq!(r.points()[0].time, q(4.0));
        // An empty curve.
        let mut e = AutomationCurve::new();
        assert!(!e.ripple(q(1.0), q(1.0)));
        assert!(e.is_empty());
    }

    /// Taking time out: the value reached at its start stays, the curve
    /// goes on from its end, every shape.
    #[test]
    fn closing_keeps_both_sides_of_every_shape() {
        for shape in [
            CurveShape::Linear,
            CurveShape::Step,
            CurveShape::Smooth,
            CurveShape::Exponential,
        ] {
            let c = AutomationCurve::from_points(vec![
                pt(0.0, 1.0, shape),
                pt(8.0, 9.0, shape),
                lin(12.0, 3.0),
            ]);
            // Out: [3, 6) of the first segment.
            let r = rippled(&c, 6.0, -3.0);
            let tol = 9.0 * SMOOTH_TOLERANCE * 1.01;
            for i in 0..30 {
                let t = i as f64 / 10.0;
                assert!(
                    near(r.value_at(q(t)), c.value_at(q(t)).unwrap(), tol),
                    "{shape:?} before {t}"
                );
            }
            assert!(near(
                r.value_before(q(3.0)),
                c.value_at(q(3.0)).unwrap(),
                tol
            ));
            for i in 0..=90 {
                let t = 6.0 + i as f64 / 10.0;
                assert!(
                    near(r.value_at(q(t - 3.0)), c.value_at(q(t)).unwrap(), tol),
                    "{shape:?} after the cut at {t}"
                );
            }
        }
    }

    /// Points inside the cut go; across two segments the seam jumps from
    /// one value to the other.
    #[test]
    fn closing_across_points_jumps_at_the_seam() {
        let c = AutomationCurve::from_points(vec![lin(0.0, 0.0), lin(4.0, 4.0), lin(8.0, 0.0)]);
        let r = rippled(&c, 6.0, -4.0);
        // [2, 6) out: 2 reached, 2 goes on (6 → 2 on the way down).
        let pts: Vec<(f64, f64)> = r
            .points()
            .iter()
            .map(|p| (p.time.quarters(), p.value))
            .collect();
        assert_eq!(
            pts,
            vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)],
            "no jump: the values meet"
        );
        let r = rippled(&c, 7.0, -4.0);
        // [3, 7) out: 3 reached, 1 goes on.
        assert!(near(r.value_before(q(3.0)), 3.0, 1e-12));
        assert!(near(r.value_at(q(3.0)), 1.0, 1e-12));
        assert!(near(r.value_at(q(4.0)), 0.0, 1e-12));
    }

    /// Cuts that start or end on points, at the curve's ends.
    #[test]
    fn closing_on_points_and_past_the_ends() {
        let c = AutomationCurve::from_points(vec![lin(2.0, 2.0), lin(4.0, 6.0), lin(6.0, 0.0)]);
        // [2, 4) out exactly between points: the first value goes, the
        // second comes forward; before it the first value held, so the
        // seam keeps 2 there.
        let r = rippled(&c, 4.0, -2.0);
        assert_eq!(
            r.value_at(q(0.0)),
            Some(2.0),
            "what held before still holds"
        );
        assert_eq!(r.value_at(q(2.0)), Some(6.0));
        assert_eq!(r.value_at(q(4.0)), Some(0.0));
        // Past the last point: the curve ends on the last value inside.
        let r = rippled(&c, 9.0, -4.0);
        assert!(near(r.value_before(q(5.0)), 3.0, 1e-12));
        assert_eq!(r.value_at(q(5.0)), Some(0.0));
        assert_eq!(r.value_at(q(20.0)), Some(0.0));
        // Before zero: cut short there.
        let r = rippled(&c, 3.0, -10.0);
        assert!(near(r.value_at(q(0.0)), 4.0, 1e-12));
        assert_eq!(r.points().last().map(|p| p.time), Some(q(3.0)));
    }

    /// Opening and closing the same time gives the curve back (as values).
    #[test]
    fn opening_then_closing_is_the_curve_again() {
        let c = AutomationCurve::from_points(vec![
            pt(0.0, -6.0, CurveShape::Exponential),
            pt(5.0, -1.0, CurveShape::Smooth),
            pt(9.0, 4.0, CurveShape::Step),
            lin(11.0, 1.0),
        ]);
        for cut in [1.0, 5.0, 7.5, 10.0] {
            let mut r = c.clone();
            r.ripple(q(cut), q(3.0));
            r.ripple(q(cut + 3.0), MusicalTime::from_quarters(-3.0));
            for i in 0..=140 {
                let t = i as f64 / 10.0;
                assert!(
                    near(
                        r.value_at(q(t)),
                        c.value_at(q(t)).unwrap(),
                        5.0 * SMOOTH_TOLERANCE * 1.01
                    ),
                    "cut {cut}, at {t}"
                );
            }
        }
    }

    /// An exponential across a sign change is a straight line: its pieces
    /// stay straight even where they share a sign.
    #[test]
    fn a_straight_exponential_stays_straight() {
        let c = AutomationCurve::from_points(vec![
            pt(0.0, -1.0, CurveShape::Exponential),
            lin(4.0, 3.0),
        ]);
        let r = rippled(&c, 2.0, 2.0);
        assert!(
            near(r.value_at(q(5.0)), 2.0, 1e-12),
            "straight after the gap"
        );
        let r = rippled(&c, 3.0, -2.0);
        assert!(
            near(r.value_at(q(1.5)), 2.5, 1e-12),
            "straight after the seam"
        );
    }

    #[test]
    fn the_value_before_a_jump_is_its_first_point() {
        let c = AutomationCurve::from_points(vec![lin(0.0, 0.0), lin(2.0, 2.0), lin(2.0, 7.0)]);
        assert!(near(c.value_before(q(1.0)), 1.0, 1e-12));
        assert_eq!(c.value_before(q(2.0)), Some(2.0));
        assert_eq!(c.value_at(q(2.0)), Some(7.0));
        assert_eq!(c.value_before(q(-1.0)), Some(0.0));
        assert_eq!(AutomationCurve::new().value_before(q(1.0)), None);
    }
}
