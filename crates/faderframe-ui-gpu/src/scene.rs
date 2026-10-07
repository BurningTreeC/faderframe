//! [`Painter`] calls onto a vello scene.

use crate::text::TextSystem;
use faderframe_ui_canvas::{Color, Image, Paint, Painter, Path, PathCmd, Rect, TextStyle};
use std::collections::HashMap;
use std::sync::Arc;
use vello::kurbo::{self, Affine, BezPath, RoundedRect, Shape, Stroke};
use vello::peniko::{
    self, BlendMode, Blob, Brush, Compose, Fill, Gradient, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat, ImageQuality, Mix,
};

pub(crate) fn color(c: Color) -> peniko::Color {
    peniko::Color::new([c.r, c.g, c.b, c.a])
}

fn krect(r: Rect) -> kurbo::Rect {
    kurbo::Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w.max(0.0)),
        f64::from(r.y + r.h.max(0.0)),
    )
}

fn rounded(r: Rect, radius: f32) -> RoundedRect {
    let radius = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
    RoundedRect::from_rect(krect(r), f64::from(radius))
}

fn bez(path: &Path) -> BezPath {
    let p = |p: faderframe_ui_canvas::Point| kurbo::Point::new(f64::from(p.x), f64::from(p.y));
    let mut b = BezPath::new();
    for cmd in path.commands() {
        match *cmd {
            PathCmd::MoveTo(a) => b.move_to(p(a)),
            PathCmd::LineTo(a) => b.line_to(p(a)),
            PathCmd::CubicTo(c1, c2, a) => b.curve_to(p(c1), p(c2), p(a)),
            PathCmd::Close => b.close_path(),
        }
    }
    b
}

fn brush(paint: &Paint) -> Brush {
    let stops = |s: &[(f32, Color)]| -> Vec<peniko::ColorStop> {
        s.iter()
            .map(|&(o, c)| peniko::ColorStop::from((o, color(c))))
            .collect()
    };
    match paint {
        Paint::Solid(c) => Brush::Solid(color(*c)),
        Paint::Linear {
            start,
            end,
            stops: s,
        } => Brush::Gradient(
            Gradient::new_linear(
                (f64::from(start.x), f64::from(start.y)),
                (f64::from(end.x), f64::from(end.y)),
            )
            .with_stops(stops(s).as_slice()),
        ),
        Paint::Radial {
            center,
            radius,
            stops: s,
        } => Brush::Gradient(
            Gradient::new_radial((f64::from(center.x), f64::from(center.y)), *radius)
                .with_stops(stops(s).as_slice()),
        ),
    }
}

/// The part of an image a call draws: its key, its pixel region, its
/// brightness (in 64 steps).
type PieceKey = (&'static str, [u32; 4], u8);

/// A decoded image: width, height, straight RGBA8.
type Decoded = (u32, u32, Arc<Vec<u8>>);

/// What `src` covers of an image: its pixels, its corner in the decoded
/// image, and decoded pixels per declared pixel.
struct Piece {
    data: ImageData,
    corner: (u32, u32),
    density: (f32, f32),
}

/// Decoded images, and the pieces drawn of them (a filmstrip's frame,
/// dimmed): vello keeps images in an atlas, which a tall filmstrip would
/// not fit.
#[derive(Default)]
pub(crate) struct ImageCache {
    decoded: HashMap<&'static str, Option<Decoded>>,
    pieces: HashMap<PieceKey, Option<ImageData>>,
}

impl ImageCache {
    const MAX_PIECES: usize = 1024;

    /// The part of `image` that `src` (in its declared pixels) covers, at
    /// `brightness`.
    fn get(&mut self, image: &Image, src: Rect, brightness: f32) -> Option<Piece> {
        let (w, h, rgba) = self
            .decoded
            .entry(image.key)
            .or_insert_with(|| {
                decode_png(image.png)
                    .map_err(|e| tracing::warn!("image {}: {e}", image.key))
                    .ok()
                    .filter(|(w, h, px)| px.len() == *w as usize * *h as usize * 4)
                    .map(|(w, h, px)| (w, h, Arc::new(px)))
            })
            .clone()?;
        // The region in the decoded pixels (the image may be stored at
        // another size than it declares).
        let (fx, fy) = (
            w as f32 / image.width as f32,
            h as f32 / image.height as f32,
        );
        let x0 = ((src.x * fx).floor().max(0.0) as u32).min(w);
        let y0 = ((src.y * fy).floor().max(0.0) as u32).min(h);
        let x1 = (((src.x + src.w) * fx).ceil() as u32).clamp(x0, w);
        let y1 = (((src.y + src.h) * fy).ceil() as u32).clamp(y0, h);
        let region = [x0, y0, x1 - x0, y1 - y0];
        if region[2] == 0 || region[3] == 0 {
            return None;
        }
        let level = (brightness.clamp(0.0, 1.0) * 64.0).round() as u8;
        if self.pieces.len() > Self::MAX_PIECES {
            self.pieces.clear();
        }
        let piece = self
            .pieces
            .entry((image.key, region, level))
            .or_insert_with(|| {
                let b = f32::from(level) / 64.0;
                let mut out = Vec::with_capacity(region[2] as usize * region[3] as usize * 4);
                for y in y0..y1 {
                    let row = &rgba[(y as usize * w as usize + x0 as usize) * 4
                        ..(y as usize * w as usize + x1 as usize) * 4];
                    if level < 64 {
                        for px in row.as_chunks::<4>().0 {
                            out.extend(px[..3].iter().map(|&c| (f32::from(c) * b).round() as u8));
                            out.push(px[3]);
                        }
                    } else {
                        out.extend_from_slice(row);
                    }
                }
                Some(ImageData {
                    data: Blob::new(Arc::new(out)),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: region[2],
                    height: region[3],
                })
            })
            .clone()?;
        Some(Piece {
            data: piece,
            corner: (x0, y0),
            density: (fx, fy),
        })
    }
}

/// Width, height and straight RGBA8 pixels of a PNG.
fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), png::DecodingError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => Vec::new(),
    };
    Ok((info.width, info.height, rgba))
}

/// Draws [`Painter`] calls into a vello scene (logical pixels, `scale`
/// device pixels each).
pub struct ScenePainter<'a> {
    scene: &'a mut vello::Scene,
    text: &'a mut TextSystem,
    images: &'a mut ImageCache,
    scale: f32,
    transforms: Vec<Affine>,
    /// Layers pushed by `push_clip` and not popped yet.
    clips: usize,
}

impl<'a> ScenePainter<'a> {
    pub(crate) fn new(
        scene: &'a mut vello::Scene,
        text: &'a mut TextSystem,
        images: &'a mut ImageCache,
        scale: f32,
    ) -> Self {
        Self {
            scene,
            text,
            images,
            scale,
            transforms: vec![Affine::scale(f64::from(scale))],
            clips: 0,
        }
    }

    fn t(&self) -> Affine {
        self.transforms.last().copied().unwrap_or(Affine::IDENTITY)
    }

    /// Close layers a view left open (an unbalanced clip must not leak into
    /// the next frame's encoding).
    pub(crate) fn finish(&mut self) {
        while self.clips > 0 {
            self.scene.pop_layer();
            self.clips -= 1;
        }
    }

    fn fill_shape(&mut self, shape: &impl Shape, paint: &Paint) {
        let t = self.t();
        self.scene
            .fill(Fill::NonZero, t, &brush(paint), None, shape);
    }
}

impl Painter for ScenePainter<'_> {
    fn fill_rect(&mut self, rect: Rect, paint: &Paint) {
        if !rect.is_empty() {
            self.fill_shape(&krect(rect), paint);
        }
    }

    fn fill_rounded(&mut self, rect: Rect, radius: f32, paint: &Paint) {
        if rect.is_empty() {
            return;
        }
        if radius <= 0.0 {
            self.fill_shape(&krect(rect), paint);
        } else {
            self.fill_shape(&rounded(rect, radius), paint);
        }
    }

    fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, c: Color) {
        if rect.is_empty() || width <= 0.0 {
            return;
        }
        // A border inside the rectangle (as GTK draws one): the stroke's
        // centre line half its width in.
        let h = width * 0.5;
        let inner = Rect::new(rect.x + h, rect.y + h, rect.w - width, rect.h - width);
        let t = self.t();
        self.scene.stroke(
            &Stroke::new(f64::from(width)),
            t,
            color(c),
            None,
            &rounded(inner, (radius - h).max(0.0)),
        );
    }

    fn fill_path(&mut self, path: &Path, c: Color) {
        if !path.is_empty() {
            self.fill_shape(&bez(path), &Paint::Solid(c));
        }
    }

    fn stroke_path(&mut self, path: &Path, width: f32, c: Color) {
        if path.is_empty() {
            return;
        }
        let stroke = Stroke::new(f64::from(width))
            .with_caps(kurbo::Cap::Round)
            .with_join(kurbo::Join::Round);
        let t = self.t();
        self.scene.stroke(&stroke, t, color(c), None, &bez(path));
    }

    fn fill_path_paint(&mut self, path: &Path, paint: &Paint) {
        if !path.is_empty() {
            self.fill_shape(&bez(path), paint);
        }
    }

    fn image(&mut self, image: &Image, src: Rect, dst: Rect, brightness: f32) {
        if dst.is_empty() || src.is_empty() {
            return;
        }
        let Some(Piece {
            data,
            corner: (x0, y0),
            density: (fx, fy),
        }) = self.images.get(image, src, brightness)
        else {
            return;
        };
        // Piece pixel (u, v) is the image's declared point
        // ((x0 + u) / fx, (y0 + v) / fy), which maps from `src` onto `dst`;
        // clipped to `dst`.
        let (sx, sy) = (dst.w / src.w, dst.h / src.h);
        let placed = self.t()
            * Affine::translate((f64::from(dst.x), f64::from(dst.y)))
            * Affine::scale_non_uniform(f64::from(sx), f64::from(sy))
            * Affine::translate((-f64::from(src.x), -f64::from(src.y)))
            * Affine::scale_non_uniform(f64::from(1.0 / fx), f64::from(1.0 / fy))
            * Affine::translate((f64::from(x0), f64::from(y0)));
        let t = self.t();
        self.scene.push_clip_layer(Fill::NonZero, t, &krect(dst));
        self.scene.draw_image(
            &ImageBrush::new(data).with_quality(ImageQuality::High),
            placed,
        );
        self.scene.pop_layer();
    }

    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
        let t = self.t()
            * Affine::translate((f64::from(dx), f64::from(dy)))
            * Affine::scale(f64::from(scale));
        self.transforms.push(t);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn shadow(&mut self, rect: Rect, radius: f32, c: Color, dx: f32, dy: f32, blur: f32) {
        if rect.is_empty() {
            return;
        }
        let t = self.t();
        // Outside the shape only (GTK's outset shadow): even-odd between a
        // rectangle that holds the whole shadow and the shape.
        let reach = f64::from(blur * 1.5 + dx.abs().max(dy.abs()) + 2.0);
        let mut outside = BezPath::from_vec(
            krect(rect)
                .inflate(reach, reach)
                .path_elements(0.1)
                .collect(),
        );
        outside.extend(rounded(rect, radius).path_elements(0.1));
        self.scene.push_clip_layer(Fill::EvenOdd, t, &outside);
        let shifted = Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h);
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        self.scene.draw_blurred_rounded_rect(
            t,
            krect(shifted),
            color(c),
            f64::from(r),
            f64::from((blur * 0.5).max(0.01)),
        );
        self.scene.pop_layer();
    }

    fn inset_shadow(&mut self, rect: Rect, radius: f32, c: Color, dx: f32, dy: f32, blur: f32) {
        if rect.is_empty() {
            return;
        }
        let t = self.t();
        let shape = rounded(rect, radius);
        // Inside the shape: its colour, less a blurred copy of the shape
        // moved by the offset (what the light reaches).
        self.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
        self.scene
            .fill(Fill::NonZero, t, color(c), None, &krect(rect));
        let reach = f64::from(blur * 1.5 + 2.0);
        self.scene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::Normal, Compose::DestOut),
            1.0,
            t,
            &krect(rect).inflate(reach, reach),
        );
        let shifted = Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h);
        let r = radius.min(rect.w * 0.5).min(rect.h * 0.5).max(0.0);
        self.scene.draw_blurred_rounded_rect(
            t,
            krect(shifted),
            peniko::Color::BLACK,
            f64::from(r),
            f64::from((blur * 0.5).max(0.01)),
        );
        self.scene.pop_layer();
        self.scene.pop_layer();
    }

    fn text(&mut self, text: &str, rect: Rect, style: &TextStyle) {
        if text.is_empty() || rect.w <= 1.0 {
            return;
        }
        let t = self.t();
        self.text.draw(self.scene, t, self.scale, text, rect, style);
    }

    fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.text.width(text, style)
    }

    fn push_clip(&mut self, rect: Rect) {
        let t = self.t();
        self.scene.push_clip_layer(Fill::NonZero, t, &krect(rect));
        self.clips += 1;
    }

    fn pop_clip(&mut self) {
        if self.clips > 0 {
            self.scene.pop_layer();
            self.clips -= 1;
        }
    }

    fn scale_factor(&self) -> f32 {
        self.scale
    }
}
