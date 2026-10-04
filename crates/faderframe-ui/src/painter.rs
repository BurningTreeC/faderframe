//! [`Painter`] implementation on top of `gtk::Snapshot`.
//!
//! Every primitive becomes a GSK render node (colour, gradient, shadow,
//! fill/stroke path, text layout), which GTK renders with its GPU renderer
//! at the surface's (possibly fractional) scale. Coordinates are logical
//! pixels, so HiDPI is handled entirely by GTK.

use faderframe_ui_canvas::{
    Align, Color, FontFamily, FontWeight, Image, Paint, Painter, Path, PathCmd, Rect, TextStyle,
};
use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, pango};
use std::cell::RefCell;
use std::collections::HashMap;

fn rgba(c: Color) -> gdk::RGBA {
    gdk::RGBA::new(c.r, c.g, c.b, c.a)
}

fn grect(r: Rect) -> graphene::Rect {
    graphene::Rect::new(r.x, r.y, r.w.max(0.0), r.h.max(0.0))
}

fn rounded(r: Rect, radius: f32) -> gsk::RoundedRect {
    let radius = radius.min(r.w * 0.5).min(r.h * 0.5).max(0.0);
    gsk::RoundedRect::from_rect(grect(r), radius)
}

fn stops(s: &[(f32, Color)]) -> Vec<gsk::ColorStop> {
    s.iter()
        .map(|&(o, c)| gsk::ColorStop::new(o, rgba(c)))
        .collect()
}

fn gsk_path(path: &Path) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    for cmd in path.commands() {
        match *cmd {
            PathCmd::MoveTo(p) => b.move_to(p.x, p.y),
            PathCmd::LineTo(p) => b.line_to(p.x, p.y),
            PathCmd::CubicTo(c1, c2, p) => b.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y),
            PathCmd::Close => b.close(),
        }
    }
    b.to_path()
}

thread_local! {
    /// Decoded images by key (they live as long as the program).
    static TEXTURES: RefCell<HashMap<&'static str, Option<gdk::Texture>>> =
        RefCell::new(HashMap::new());
}

fn texture(image: &Image) -> Option<gdk::Texture> {
    TEXTURES.with(|t| {
        t.borrow_mut()
            .entry(image.key)
            .or_insert_with(|| {
                gdk::Texture::from_bytes(&gtk::glib::Bytes::from_static(image.png))
                    .map_err(|e| tracing::warn!("image {}: {e}", image.key))
                    .ok()
            })
            .clone()
    })
}

/// Cache key of a shaped text layout.
#[derive(Clone, PartialEq, Eq, Hash)]
struct TextKey {
    text: String,
    size: u32,
    family: u8,
    weight: u8,
    tracking: u32,
    width: i32,
    align: u8,
}

/// Shaped pango layouts reused across frames (most labels never change).
#[derive(Default)]
pub struct TextCache {
    layouts: HashMap<TextKey, (pango::Layout, u64)>,
    frame: u64,
}

impl TextCache {
    const MAX: usize = 4096;

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        if self.layouts.len() > Self::MAX {
            let keep_from = self.frame.saturating_sub(2);
            self.layouts.retain(|_, (_, used)| *used >= keep_from);
        }
    }

    pub fn clear(&mut self) {
        self.layouts.clear();
    }
}

fn font_description(style: &TextStyle) -> pango::FontDescription {
    let mut fd = pango::FontDescription::new();
    match style.family {
        FontFamily::Sans => {
            fd.set_family("Inter, Noto Sans, Fira Sans, Cantarell, DejaVu Sans, Sans")
        }
        FontFamily::Condensed => {
            fd.set_family(
                "Fira Sans Condensed, Roboto Condensed, Noto Sans, DejaVu Sans Condensed, Sans",
            );
            fd.set_stretch(pango::Stretch::SemiCondensed);
        }
        FontFamily::Mono => {
            fd.set_family("JetBrains Mono, Fira Mono, Noto Sans Mono, DejaVu Sans Mono, Monospace")
        }
    }
    fd.set_weight(match style.weight {
        FontWeight::Normal => pango::Weight::Normal,
        FontWeight::Medium => pango::Weight::Medium,
        FontWeight::Bold => pango::Weight::Bold,
    });
    fd.set_absolute_size(style.size as f64 * pango::SCALE as f64);
    fd
}

pub struct SnapshotPainter<'a> {
    snapshot: &'a gtk::Snapshot,
    widget: &'a gtk::Widget,
    cache: &'a RefCell<TextCache>,
    scale: f32,
}

impl<'a> SnapshotPainter<'a> {
    pub fn new(
        snapshot: &'a gtk::Snapshot,
        widget: &'a gtk::Widget,
        cache: &'a RefCell<TextCache>,
    ) -> Self {
        let scale = widget
            .native()
            .and_then(|n| n.surface())
            .map_or(widget.scale_factor() as f64, |s| s.scale()) as f32;
        Self {
            snapshot,
            widget,
            cache,
            scale,
        }
    }

    fn layout(&self, text: &str, width: f32, style: &TextStyle) -> pango::Layout {
        let key = TextKey {
            text: text.to_string(),
            size: style.size.to_bits(),
            family: style.family as u8,
            weight: style.weight as u8,
            tracking: style.tracking.to_bits(),
            width: width.round() as i32,
            align: style.align as u8,
        };
        let mut cache = self.cache.borrow_mut();
        let frame = cache.frame;
        if let Some((layout, used)) = cache.layouts.get_mut(&key) {
            *used = frame;
            return layout.clone();
        }
        let layout = self.widget.create_pango_layout(Some(text));
        layout.set_font_description(Some(&font_description(style)));
        if style.tracking > 0.0 {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_letter_spacing(
                (style.tracking * pango::SCALE as f32) as i32,
            ));
            layout.set_attributes(Some(&attrs));
        }
        if width > 0.0 {
            layout.set_width((width * pango::SCALE as f32) as i32);
            layout.set_ellipsize(pango::EllipsizeMode::End);
        }
        layout.set_single_paragraph_mode(true);
        layout.set_alignment(match style.align {
            Align::Start => pango::Alignment::Left,
            Align::Center => pango::Alignment::Center,
            Align::End => pango::Alignment::Right,
        });
        cache.layouts.insert(key, (layout.clone(), frame));
        layout
    }

    fn fill_paint(&self, rect: Rect, paint: &Paint) {
        let s = self.snapshot;
        match paint {
            Paint::Solid(c) => s.append_color(&rgba(*c), &grect(rect)),
            Paint::Linear {
                start,
                end,
                stops: st,
            } => s.append_linear_gradient(
                &grect(rect),
                &graphene::Point::new(start.x, start.y),
                &graphene::Point::new(end.x, end.y),
                &stops(st),
            ),
            Paint::Radial {
                center,
                radius,
                stops: st,
            } => s.append_radial_gradient(
                &grect(rect),
                &graphene::Point::new(center.x, center.y),
                *radius,
                *radius,
                0.0,
                1.0,
                &stops(st),
            ),
        }
    }
}

impl Painter for SnapshotPainter<'_> {
    fn fill_rect(&mut self, rect: Rect, paint: &Paint) {
        if !rect.is_empty() {
            self.fill_paint(rect, paint);
        }
    }

    fn fill_rounded(&mut self, rect: Rect, radius: f32, paint: &Paint) {
        if rect.is_empty() {
            return;
        }
        if radius <= 0.0 {
            return self.fill_paint(rect, paint);
        }
        self.snapshot.push_rounded_clip(&rounded(rect, radius));
        self.fill_paint(rect, paint);
        self.snapshot.pop();
    }

    fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        if rect.is_empty() {
            return;
        }
        let c = rgba(color);
        self.snapshot
            .append_border(&rounded(rect, radius), &[width; 4], &[c, c, c, c]);
    }

    fn fill_path(&mut self, path: &Path, color: Color) {
        if !path.is_empty() {
            self.snapshot
                .append_fill(&gsk_path(path), gsk::FillRule::Winding, &rgba(color));
        }
    }

    fn stroke_path(&mut self, path: &Path, width: f32, color: Color) {
        if path.is_empty() {
            return;
        }
        let stroke = gsk::Stroke::new(width);
        stroke.set_line_cap(gsk::LineCap::Round);
        stroke.set_line_join(gsk::LineJoin::Round);
        self.snapshot
            .append_stroke(&gsk_path(path), &stroke, &rgba(color));
    }

    fn fill_path_paint(&mut self, path: &Path, paint: &Paint) {
        if path.is_empty() {
            return;
        }
        if let Paint::Solid(c) = paint {
            return self.fill_path(path, *c);
        }
        let p = gsk_path(path);
        let Some(bounds) = p.bounds() else {
            return;
        };
        self.snapshot.push_fill(&p, gsk::FillRule::Winding);
        let r = Rect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height());
        self.fill_paint(r, paint);
        self.snapshot.pop();
    }

    fn image(&mut self, image: &Image, src: Rect, dst: Rect, brightness: f32) {
        if dst.is_empty() || src.is_empty() {
            return;
        }
        let Some(tex) = texture(image) else {
            return;
        };
        // The whole image placed so that `src` lands on `dst`.
        let (sx, sy) = (dst.w / src.w, dst.h / src.h);
        let full = Rect::new(
            dst.x - src.x * sx,
            dst.y - src.y * sy,
            image.width as f32 * sx,
            image.height as f32 * sy,
        );
        let s = self.snapshot;
        s.push_clip(&grect(dst));
        let dim = brightness < 0.999;
        if dim {
            let b = brightness.clamp(0.0, 1.0);
            let m = graphene::Matrix::from_float([
                b, 0.0, 0.0, 0.0, 0.0, b, 0.0, 0.0, 0.0, 0.0, b, 0.0, 0.0, 0.0, 0.0, 1.0,
            ]);
            s.push_color_matrix(&m, &graphene::Vec4::new(0.0, 0.0, 0.0, 0.0));
        }
        s.append_scaled_texture(&tex, gsk::ScalingFilter::Trilinear, &grect(full));
        if dim {
            s.pop();
        }
        s.pop();
    }

    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(dx, dy));
        self.snapshot.scale(scale, scale);
    }

    fn pop_transform(&mut self) {
        self.snapshot.restore();
    }

    fn shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32) {
        if !rect.is_empty() {
            self.snapshot.append_outset_shadow(
                &rounded(rect, radius),
                &rgba(color),
                dx,
                dy,
                0.0,
                blur,
            );
        }
    }

    fn inset_shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32) {
        if !rect.is_empty() {
            self.snapshot.append_inset_shadow(
                &rounded(rect, radius),
                &rgba(color),
                dx,
                dy,
                0.0,
                blur,
            );
        }
    }

    fn text(&mut self, text: &str, rect: Rect, style: &TextStyle) {
        if text.is_empty() || rect.w <= 1.0 {
            return;
        }
        let layout = self.layout(text, rect.w, style);
        let (_, h) = layout.pixel_size();
        let y = match style.valign {
            Align::Start => rect.y,
            Align::Center => rect.y + (rect.h - h as f32) * 0.5,
            Align::End => rect.bottom() - h as f32,
        };
        self.snapshot.save();
        self.snapshot
            .translate(&graphene::Point::new(rect.x, y.round()));
        self.snapshot.append_layout(&layout, &rgba(style.color));
        self.snapshot.restore();
    }

    fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.layout(text, 0.0, style).pixel_size().0 as f32
    }

    fn push_clip(&mut self, rect: Rect) {
        self.snapshot.push_clip(&grect(rect));
    }

    fn pop_clip(&mut self) {
        self.snapshot.pop();
    }

    fn scale_factor(&self) -> f32 {
        self.scale
    }
}
