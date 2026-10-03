use crate::{Color, Point, Rect};
use std::f32::consts::FRAC_PI_2;

/// How a shape is filled.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear {
        start: Point,
        end: Point,
        stops: Vec<(f32, Color)>,
    },
    Radial {
        center: Point,
        radius: f32,
        stops: Vec<(f32, Color)>,
    },
}

impl From<Color> for Paint {
    fn from(c: Color) -> Self {
        Paint::Solid(c)
    }
}

impl Paint {
    /// Vertical gradient across `rect`.
    pub fn vertical(rect: Rect, top: Color, bottom: Color) -> Self {
        Paint::Linear {
            start: Point::new(rect.x, rect.y),
            end: Point::new(rect.x, rect.bottom()),
            stops: vec![(0.0, top), (1.0, bottom)],
        }
    }

    /// Horizontal gradient across `rect`.
    pub fn horizontal(rect: Rect, left: Color, right: Color) -> Self {
        Paint::Linear {
            start: Point::new(rect.x, rect.y),
            end: Point::new(rect.right(), rect.y),
            stops: vec![(0.0, left), (1.0, right)],
        }
    }

    pub fn vertical_stops(rect: Rect, stops: Vec<(f32, Color)>) -> Self {
        Paint::Linear {
            start: Point::new(rect.x, rect.y),
            end: Point::new(rect.x, rect.bottom()),
            stops,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    MoveTo(Point),
    LineTo(Point),
    CubicTo(Point, Point, Point),
    Close,
}

/// A vector path built from lines and cubic Béziers. Arcs are converted to
/// cubics here, so painter backends only need move/line/cubic/close.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    cmds: Vec<PathCmd>,
    current: Point,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn commands(&self) -> &[PathCmd] {
        &self.cmds
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    pub fn move_to(&mut self, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::MoveTo(p));
        self.current = p;
        self
    }

    pub fn line_to(&mut self, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::LineTo(p));
        self.current = p;
        self
    }

    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) -> &mut Self {
        self.cmds.push(PathCmd::CubicTo(c1, c2, p));
        self.current = p;
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.cmds.push(PathCmd::Close);
        self
    }

    /// Circular arc around `center` from angle `a0` to `a1` (radians,
    /// clockwise on screen since y points down). Starts a new sub-path
    /// unless `connect` is true.
    pub fn arc(
        &mut self,
        center: Point,
        radius: f32,
        a0: f32,
        a1: f32,
        connect: bool,
    ) -> &mut Self {
        let point = |a: f32| Point::new(center.x + radius * a.cos(), center.y + radius * a.sin());
        let start = point(a0);
        if connect && !self.cmds.is_empty() {
            self.line_to(start);
        } else {
            self.move_to(start);
        }
        let total = a1 - a0;
        let segments = ((total.abs() / FRAC_PI_2).ceil() as usize).max(1);
        let step = total / segments as f32;
        // Cubic approximation constant for an arc of angle `step`.
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a = a0;
        for _ in 0..segments {
            let b = a + step;
            let (pa, pb) = (point(a), point(b));
            let c1 = Point::new(pa.x - k * radius * a.sin(), pa.y + k * radius * a.cos());
            let c2 = Point::new(pb.x + k * radius * b.sin(), pb.y - k * radius * b.cos());
            self.cubic_to(c1, c2, pb);
            a = b;
        }
        self
    }

    pub fn circle(center: Point, radius: f32) -> Self {
        let mut p = Path::new();
        p.arc(center, radius, 0.0, std::f32::consts::TAU, false)
            .close();
        p
    }

    pub fn rounded_rect(r: Rect, radius: f32) -> Self {
        let rad = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
        let mut p = Path::new();
        if rad <= 0.0 {
            p.move_to(Point::new(r.x, r.y))
                .line_to(Point::new(r.right(), r.y))
                .line_to(Point::new(r.right(), r.bottom()))
                .line_to(Point::new(r.x, r.bottom()))
                .close();
            return p;
        }
        use std::f32::consts::PI;
        p.arc(Point::new(r.x + rad, r.y + rad), rad, PI, PI * 1.5, false);
        p.arc(
            Point::new(r.right() - rad, r.y + rad),
            rad,
            PI * 1.5,
            PI * 2.0,
            true,
        );
        p.arc(
            Point::new(r.right() - rad, r.bottom() - rad),
            rad,
            0.0,
            FRAC_PI_2,
            true,
        );
        p.arc(
            Point::new(r.x + rad, r.bottom() - rad),
            rad,
            FRAC_PI_2,
            PI,
            true,
        );
        p.close();
        p
    }

    pub fn polyline(points: &[Point]) -> Self {
        let mut p = Path::new();
        if let Some((first, rest)) = points.split_first() {
            p.move_to(*first);
            for q in rest {
                p.line_to(*q);
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_endpoints_are_exact() {
        let mut p = Path::new();
        p.arc(Point::new(0.0, 0.0), 10.0, 0.0, std::f32::consts::PI, false);
        let PathCmd::CubicTo(_, _, end) = *p.commands().last().unwrap() else {
            panic!()
        };
        assert!((end.x + 10.0).abs() < 1e-4 && end.y.abs() < 1e-4);
        assert_eq!(p.commands().len(), 3); // move + 2 quarter arcs
        let rr = Path::rounded_rect(Rect::new(0.0, 0.0, 20.0, 10.0), 4.0);
        assert!(matches!(rr.commands().last(), Some(PathCmd::Close)));
    }
}
