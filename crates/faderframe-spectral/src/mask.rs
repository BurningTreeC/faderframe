//! Where an edit applies: a weight in 0…1 for each bin of a frame, from
//! its shape, with soft edges (the feather) outside it. Frequencies are
//! measured in octaves (log2 Hz), times in source frames.

use faderframe_project::spectral::{SpectralEdit, SpectralShape};

/// 1 inside, falling to 0 over one unit outside (a raised cosine).
fn soft(x: f64) -> f32 {
    if x <= 0.0 {
        1.0
    } else if x >= 1.0 {
        0.0
    } else {
        (0.5 * (1.0 + (std::f64::consts::PI * x).cos())) as f32
    }
}

fn octaves(hz: f64) -> f64 {
    hz.max(1e-3).log2()
}

/// A shape in (frames, octaves).
enum Prepared {
    Rect {
        start: f64,
        end: f64,
        /// `-inf`: no lower edge.
        low: f64,
        high: f64,
    },
    Polygon(Vec<(f64, f64)>),
    Brush {
        points: Vec<(f64, f64)>,
        /// Radius in frames and octaves.
        rt: f64,
        rf: f64,
    },
}

/// An edit's region, ready to weigh bins.
pub struct Mask {
    shape: Prepared,
    /// Feather in frames and octaves.
    ft: f64,
    ff: f64,
    /// Frames the region (with its feather) covers.
    pub from: f64,
    pub to: f64,
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    (x * x + y * y).sqrt()
}

impl Mask {
    pub fn new(edit: &SpectralEdit, rate: f64) -> Self {
        let ft = f64::from(edit.feather_ms.max(0.0)) * rate / 1000.0;
        let ff = f64::from(edit.feather_st.max(0.0)) / 12.0;
        let point = |&(t, hz): &(i64, f32)| (t as f64, octaves(f64::from(hz)));
        let (shape, reach) = match &edit.shape {
            SpectralShape::Rect {
                start,
                end,
                low,
                high,
            } => {
                let (lo, hi) = (low.min(*high), low.max(*high));
                (
                    Prepared::Rect {
                        start: (*start.min(end)) as f64,
                        end: (*start.max(end)) as f64,
                        low: if lo <= 1.0 {
                            f64::NEG_INFINITY
                        } else {
                            octaves(f64::from(lo))
                        },
                        high: octaves(f64::from(hi)),
                    },
                    0.0,
                )
            }
            SpectralShape::Lasso { points } => {
                (Prepared::Polygon(points.iter().map(point).collect()), 0.0)
            }
            SpectralShape::Brush {
                points,
                radius_ms,
                radius_st,
            } => {
                let rt = (f64::from(*radius_ms) * rate / 1000.0).max(1.0);
                (
                    Prepared::Brush {
                        points: points.iter().map(point).collect(),
                        rt,
                        rf: (f64::from(*radius_st) / 12.0).max(1e-3),
                    },
                    rt,
                )
            }
        };
        let (a, b) = edit.shape.frames();
        Self {
            shape,
            ft,
            ff,
            from: a as f64 - reach - ft,
            to: b as f64 + reach + ft,
        }
    }

    /// Does the region reach frame `t`?
    pub fn covers(&self, t: f64) -> bool {
        t >= self.from && t <= self.to
    }

    /// The weight of each bin (`bins[k]` = its frequency in octaves) of the
    /// frame centred on source frame `t`.
    pub fn weights(&self, t: f64, bins: &[f64], out: &mut [f32]) {
        out.fill(0.0);
        if !self.covers(t) {
            return;
        }
        match &self.shape {
            Prepared::Rect {
                start,
                end,
                low,
                high,
            } => {
                let dt = (start - t).max(t - end).max(0.0);
                let wt = if self.ft > 0.0 {
                    soft(dt / self.ft)
                } else if dt > 0.0 {
                    0.0
                } else {
                    1.0
                };
                if wt == 0.0 {
                    return;
                }
                for (w, &f) in out.iter_mut().zip(bins) {
                    let df = (low - f).max(f - high).max(0.0);
                    let wf = if self.ff > 0.0 {
                        soft(df / self.ff)
                    } else if df > 0.0 {
                        0.0
                    } else {
                        1.0
                    };
                    *w = wt * wf;
                }
            }
            Prepared::Polygon(points) => self.polygon(points, t, bins, out),
            Prepared::Brush { points, rt, rf } => self.brush(points, *rt, *rf, t, bins, out),
        }
    }

    fn polygon(&self, points: &[(f64, f64)], t: f64, bins: &[f64], out: &mut [f32]) {
        if points.len() < 3 {
            return;
        }
        // The outline's crossings of this frame's vertical line: inside
        // between pairs.
        let n = points.len();
        let mut crossings: Vec<f64> = Vec::new();
        for i in 0..n {
            let (a, b) = (points[i], points[(i + 1) % n]);
            if (a.0 <= t) != (b.0 <= t) {
                let k = (t - a.0) / (b.0 - a.0);
                crossings.push(a.1 + k * (b.1 - a.1));
            }
        }
        crossings.sort_by(f64::total_cmp);
        let inside = |f: f64| crossings.partition_point(|c| *c < f) % 2 == 1;
        // Soft edges: the distance to the outline, time and frequency
        // scaled by their feathers.
        let (st, sf) = (self.ft.max(1.0), self.ff.max(1e-3));
        let feathered = self.ft > 0.0 || self.ff > 0.0;
        let scaled = |p: (f64, f64)| (p.0 / st, p.1 / sf);
        let near: Vec<((f64, f64), (f64, f64))> = if feathered {
            (0..n)
                .map(|i| (points[i], points[(i + 1) % n]))
                .filter(|(a, b)| a.0.min(b.0) - st <= t && a.0.max(b.0) + st >= t)
                .map(|(a, b)| (scaled(a), scaled(b)))
                .collect()
        } else {
            Vec::new()
        };
        for (w, &f) in out.iter_mut().zip(bins) {
            *w = if inside(f) {
                1.0
            } else if near.is_empty() {
                0.0
            } else {
                let p = scaled((t, f));
                let d = near
                    .iter()
                    .map(|(a, b)| segment_distance(p, *a, *b))
                    .fold(f64::INFINITY, f64::min);
                soft(d)
            };
        }
    }

    fn brush(
        &self,
        points: &[(f64, f64)],
        rt: f64,
        rf: f64,
        t: f64,
        bins: &[f64],
        out: &mut [f32],
    ) {
        if points.is_empty() {
            return;
        }
        // Distances in radii: 1 is the stroke's edge; the feather beyond
        // it in the same units.
        let reach = rt + self.ft;
        let segments: Vec<((f64, f64), (f64, f64))> = if points.len() == 1 {
            vec![(points[0], points[0])]
        } else {
            points.windows(2).map(|w| (w[0], w[1])).collect()
        };
        let near: Vec<((f64, f64), (f64, f64))> = segments
            .into_iter()
            .filter(|(a, b)| a.0.min(b.0) - reach <= t && a.0.max(b.0) + reach >= t)
            .map(|(a, b)| ((a.0 / rt, a.1 / rf), (b.0 / rt, b.1 / rf)))
            .collect();
        if near.is_empty() {
            return;
        }
        let feather = (self.ft / rt).max(self.ff / rf);
        for (w, &f) in out.iter_mut().zip(bins) {
            let p = (t / rt, f / rf);
            let d = near
                .iter()
                .map(|(a, b)| segment_distance(p, *a, *b))
                .fold(f64::INFINITY, f64::min);
            *w = if d <= 1.0 {
                1.0
            } else if feather > 0.0 {
                soft((d - 1.0) / feather)
            } else {
                0.0
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_project::spectral::SpectralOp;

    fn bins() -> Vec<f64> {
        [100.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0]
            .iter()
            .map(|h| octaves(*h))
            .collect()
    }

    fn weigh(edit: &SpectralEdit, t: f64) -> Vec<f32> {
        let m = Mask::new(edit, 48_000.0);
        let mut out = vec![0.0; 6];
        m.weights(t, &bins(), &mut out);
        out
    }

    #[test]
    fn a_rectangle_is_full_inside_and_soft_outside() {
        let mut e = SpectralEdit::new(
            SpectralShape::Rect {
                start: 48_000,
                end: 96_000,
                low: 900.0,
                high: 2200.0,
            },
            SpectralOp::Remove,
        );
        e.feather_ms = 0.0;
        e.feather_st = 0.0;
        assert_eq!(weigh(&e, 60_000.0), [0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
        assert_eq!(weigh(&e, 40_000.0), [0.0; 6]);
        // A semitone of feather: 2000 Hz is in, 4000 Hz well out; 10 ms of
        // feather in time: 5 ms after the end, half way.
        e.feather_st = 12.0;
        e.feather_ms = 10.0;
        let w = weigh(&e, 96_000.0 + 240.0);
        assert!((w[2] - 0.5).abs() < 0.01, "{w:?}");
        assert!(w[4] > 0.0 && w[4] < w[3], "{w:?}");
        assert_eq!(w[0], 0.0);
    }

    #[test]
    fn a_lasso_and_a_brush_cover_their_outline() {
        // A triangle from 1 kHz at frame 0 up to 4 kHz at 10000, back to
        // 1 kHz at 20000; and its base.
        let mut lasso = SpectralEdit::new(
            SpectralShape::Lasso {
                points: vec![(0, 900.0), (10_000, 4500.0), (20_000, 900.0)],
            },
            SpectralOp::Remove,
        );
        lasso.feather_ms = 0.0;
        lasso.feather_st = 0.0;
        let w = weigh(&lasso, 10_000.0);
        assert_eq!(w, [0.0, 0.0, 1.0, 1.0, 1.0, 0.0]);
        let w = weigh(&lasso, 2_000.0);
        assert_eq!(w, [0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        let brush = SpectralEdit::new(
            SpectralShape::Brush {
                points: vec![(0, 1000.0), (48_000, 1000.0)],
                radius_ms: 20.0,
                radius_st: 3.0,
            },
            SpectralOp::Remove,
        );
        let w = weigh(&brush, 24_000.0);
        assert_eq!(w[2], 1.0);
        assert_eq!(w[0], 0.0);
        assert_eq!(w[4], 0.0);
        // The end cap: within the radius past the last point.
        assert_eq!(weigh(&brush, 48_000.0 + 480.0)[2], 1.0);
        assert_eq!(weigh(&brush, 48_000.0 + 2_400.0)[2], 0.0);
    }
}
