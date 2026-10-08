use crate::{Color, Image, Paint, Path, Point, Rect, TextStyle};

/// Drawing backend used by every custom view.
///
/// Coordinates are logical pixels; the backend maps them to device pixels
/// (HiDPI scale is handled by the toolkit, never by views). Implemented by
/// the GTK snapshot painter today; a wgpu painter for very dense surfaces
/// can implement the same trait later.
pub trait Painter {
    fn fill_rect(&mut self, rect: Rect, paint: &Paint);
    fn fill_rounded(&mut self, rect: Rect, radius: f32, paint: &Paint);
    fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, color: Color);
    fn fill_path(&mut self, path: &Path, color: Color);
    fn stroke_path(&mut self, path: &Path, width: f32, color: Color);
    /// Fill a path with a gradient (or a solid paint).
    fn fill_path_paint(&mut self, path: &Path, paint: &Paint);

    /// Draw the `src` part of `image` (its pixels) into `dst`, its colour
    /// scaled by `brightness` (1: as it is; alpha untouched).
    fn image(&mut self, image: &Image, src: Rect, dst: Rect, brightness: f32);

    /// Draw run-time pixels (a video frame) scaled into `dst`. Backends
    /// without it draw nothing.
    fn pixels(&mut self, _pixels: &crate::Pixels<'_>, _dst: Rect) {}

    /// Draw a picture in video memory scaled into `dst`; whether it could
    /// (backends that cannot take it draw nothing and say so).
    fn external(&mut self, _image: &crate::External<'_>, _dst: Rect) -> bool {
        false
    }

    /// Draw what follows translated by `(dx, dy)` and then scaled by
    /// `scale` (a view laid out in its own coordinates), until
    /// [`Painter::pop_transform`].
    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32);
    fn pop_transform(&mut self);

    /// Soft drop shadow outside a rounded rectangle.
    fn shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32);
    /// Shadow cast inside a rounded rectangle (recessed look).
    fn inset_shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32);

    /// Draw single-line text aligned within `rect` (ellipsised if too long).
    fn text(&mut self, text: &str, rect: Rect, style: &TextStyle);
    fn text_width(&mut self, text: &str, style: &TextStyle) -> f32;

    fn push_clip(&mut self, rect: Rect);
    fn pop_clip(&mut self);

    /// Device pixels per logical pixel (for hairline snapping).
    fn scale_factor(&self) -> f32 {
        1.0
    }

    // --- convenience ------------------------------------------------------

    fn fill(&mut self, rect: Rect, color: Color) {
        self.fill_rect(rect, &Paint::Solid(color));
    }

    fn line(&mut self, a: Point, b: Point, width: f32, color: Color) {
        let mut p = Path::new();
        p.move_to(a).line_to(b);
        self.stroke_path(&p, width, color);
    }

    /// Crisp 1-device-pixel horizontal line.
    fn hline(&mut self, x0: f32, x1: f32, y: f32, color: Color) {
        let px = 1.0 / self.scale_factor().max(1.0);
        self.fill(Rect::new(x0, y, x1 - x0, px), color);
    }

    /// Crisp 1-device-pixel vertical line.
    fn vline(&mut self, x: f32, y0: f32, y1: f32, color: Color) {
        let px = 1.0 / self.scale_factor().max(1.0);
        self.fill(Rect::new(x, y0, px, y1 - y0), color);
    }

    fn circle(&mut self, center: Point, radius: f32, color: Color) {
        self.fill_path(&Path::circle(center, radius), color);
    }
}

/// One recorded drawing operation (for tests and diagnostics).
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    Rect(Rect),
    Rounded(Rect),
    Path,
    Shadow(Rect),
    Text(String, Rect),
    Clip(Rect),
    PopClip,
    Image(&'static str, Rect),
    Transform(f32, f32, f32),
    PopTransform,
}

/// A painter that records operations instead of drawing. Text width is
/// approximated as `0.55 × size` per character.
#[derive(Debug, Default)]
pub struct RecordingPainter {
    pub ops: Vec<DrawOp>,
    clips: usize,
}

impl RecordingPainter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn texts(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|o| match o {
                DrawOp::Text(t, _) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn balanced_clips(&self) -> bool {
        self.clips == 0
    }
}

impl Painter for RecordingPainter {
    fn fill_rect(&mut self, rect: Rect, _paint: &Paint) {
        self.ops.push(DrawOp::Rect(rect));
    }
    fn fill_rounded(&mut self, rect: Rect, _radius: f32, _paint: &Paint) {
        self.ops.push(DrawOp::Rounded(rect));
    }
    fn stroke_rounded(&mut self, rect: Rect, _radius: f32, _width: f32, _color: Color) {
        self.ops.push(DrawOp::Rounded(rect));
    }
    fn fill_path(&mut self, _path: &Path, _color: Color) {
        self.ops.push(DrawOp::Path);
    }
    fn stroke_path(&mut self, _path: &Path, _width: f32, _color: Color) {
        self.ops.push(DrawOp::Path);
    }
    fn fill_path_paint(&mut self, _path: &Path, _paint: &Paint) {
        self.ops.push(DrawOp::Path);
    }
    fn image(&mut self, image: &Image, _src: Rect, dst: Rect, _brightness: f32) {
        self.ops.push(DrawOp::Image(image.key, dst));
    }
    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
        self.clips += 1;
        self.ops.push(DrawOp::Transform(dx, dy, scale));
    }
    fn pop_transform(&mut self) {
        self.clips = self.clips.saturating_sub(1);
        self.ops.push(DrawOp::PopTransform);
    }
    fn shadow(&mut self, rect: Rect, _r: f32, _c: Color, _dx: f32, _dy: f32, _blur: f32) {
        self.ops.push(DrawOp::Shadow(rect));
    }
    fn inset_shadow(&mut self, rect: Rect, _r: f32, _c: Color, _dx: f32, _dy: f32, _blur: f32) {
        self.ops.push(DrawOp::Shadow(rect));
    }
    fn text(&mut self, text: &str, rect: Rect, _style: &TextStyle) {
        self.ops.push(DrawOp::Text(text.to_string(), rect));
    }
    fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        text.chars().count() as f32 * style.size * 0.55
    }
    fn push_clip(&mut self, rect: Rect) {
        self.clips += 1;
        self.ops.push(DrawOp::Clip(rect));
    }
    fn pop_clip(&mut self) {
        self.clips = self.clips.saturating_sub(1);
        self.ops.push(DrawOp::PopClip);
    }
}
