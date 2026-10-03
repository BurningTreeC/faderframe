/// A point in logical (scale-independent) pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn offset(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy)
    }

    pub fn distance(self, other: Point) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// Axis-aligned rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn from_size(size: Size) -> Self {
        Self::new(0.0, 0.0, size.w, size.h)
    }

    pub fn from_points(a: Point, b: Point) -> Self {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Self::new(x, y, (a.x - b.x).abs(), (a.y - b.y).abs())
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn intersection(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    pub fn inset(&self, d: f32) -> Rect {
        self.inset_xy(d, d)
    }

    pub fn inset_xy(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Split off the top `h` pixels: `(top, rest)`.
    pub fn split_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// Split off the bottom `h` pixels: `(rest, bottom)`.
    pub fn split_bottom(&self, h: f32) -> (Rect, Rect) {
        let h = h.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, self.h - h),
            Rect::new(self.x, self.bottom() - h, self.w, h),
        )
    }

    /// Split off the left `w` pixels: `(left, rest)`.
    pub fn split_left(&self, w: f32) -> (Rect, Rect) {
        let w = w.clamp(0.0, self.w);
        (
            Rect::new(self.x, self.y, w, self.h),
            Rect::new(self.x + w, self.y, self.w - w, self.h),
        )
    }

    /// Split off the right `w` pixels: `(rest, right)`.
    pub fn split_right(&self, w: f32) -> (Rect, Rect) {
        let w = w.clamp(0.0, self.w);
        (
            Rect::new(self.x, self.y, self.w - w, self.h),
            Rect::new(self.right() - w, self.y, w, self.h),
        )
    }

    /// Take `h` from the top, shrinking `self`.
    pub fn take_top(&mut self, h: f32) -> Rect {
        let (top, rest) = self.split_top(h);
        *self = rest;
        top
    }

    /// Take `h` from the bottom, shrinking `self`.
    pub fn take_bottom(&mut self, h: f32) -> Rect {
        let (rest, bottom) = self.split_bottom(h);
        *self = rest;
        bottom
    }

    pub fn take_left(&mut self, w: f32) -> Rect {
        let (left, rest) = self.split_left(w);
        *self = rest;
        left
    }

    pub fn take_right(&mut self, w: f32) -> Rect {
        let (rest, right) = self.split_right(w);
        *self = rest;
        right
    }

    /// Centred sub-rectangle of the given size.
    pub fn centered(&self, w: f32, h: f32) -> Rect {
        Rect::new(
            self.x + (self.w - w) * 0.5,
            self.y + (self.h - h) * 0.5,
            w,
            h,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_and_hit_testing() {
        let mut r = Rect::new(10.0, 10.0, 100.0, 50.0);
        let top = r.take_top(20.0);
        assert_eq!(top, Rect::new(10.0, 10.0, 100.0, 20.0));
        assert_eq!(r, Rect::new(10.0, 30.0, 100.0, 30.0));
        assert!(r.contains(Point::new(10.0, 30.0)));
        assert!(!r.contains(Point::new(110.0, 30.0)));
        let i = Rect::new(0.0, 0.0, 50.0, 50.0).intersection(&Rect::new(25.0, 25.0, 50.0, 50.0));
        assert_eq!(i, Rect::new(25.0, 25.0, 25.0, 25.0));
        assert!(!Rect::new(0.0, 0.0, 10.0, 10.0).intersects(&Rect::new(10.0, 0.0, 5.0, 5.0)));
    }
}
