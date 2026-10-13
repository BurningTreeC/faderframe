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

/// GSK caches rasterized fills and strokes by path identity, not geometry.
/// Recreating unchanged paths every frame fills its GPU atlases with copies
/// of the same knobs and curves, making periodic atlas collection stall the
/// main loop. Keep the native paths alive across redraws of a canvas.
#[derive(Default)]
pub struct PathCache {
    paths: HashMap<Vec<u32>, (gsk::Path, u64)>,
    frame: u64,
    words: usize,
}

impl PathCache {
    const MAX_PATHS: usize = 2048;
    const MAX_WORDS: usize = 64 * 1024;

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        // Only retain geometry used in the previous two frames. Animated
        // curves must not leave an ever-growing history in the cache.
        let keep_from = self.frame.saturating_sub(2);
        self.paths.retain(|key, (_, used)| {
            if *used < keep_from {
                self.words -= key.len();
                false
            } else {
                true
            }
        });
    }

    fn get(&mut self, path: &Path) -> gsk::Path {
        let mut key = Vec::new();
        for cmd in path.commands() {
            match *cmd {
                PathCmd::MoveTo(p) => key.extend([0, p.x.to_bits(), p.y.to_bits()]),
                PathCmd::LineTo(p) => key.extend([1, p.x.to_bits(), p.y.to_bits()]),
                PathCmd::CubicTo(a, b, p) => key.extend([
                    2,
                    a.x.to_bits(),
                    a.y.to_bits(),
                    b.x.to_bits(),
                    b.y.to_bits(),
                    p.x.to_bits(),
                    p.y.to_bits(),
                ]),
                PathCmd::Close => key.push(3),
            }
        }
        if let Some((native, used)) = self.paths.get_mut(&key) {
            *used = self.frame;
            return native.clone();
        }
        let native = gsk_path(path);
        // Bound both the number and size of retained paths, including a
        // single frame with many tracks or a very detailed waveform.
        if self.paths.len() < Self::MAX_PATHS && self.words + key.len() <= Self::MAX_WORDS {
            self.words += key.len();
            self.paths.insert(key, (native.clone(), self.frame));
        }
        native
    }
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

thread_local! {
    /// The textures of the last few run-time pictures (a video frame is
    /// drawn again on every repaint until the next one).
    static PIXELS: RefCell<std::collections::VecDeque<(u64, gdk::Texture)>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

fn pixel_texture(p: &faderframe_ui_canvas::Pixels<'_>) -> Option<gdk::Texture> {
    PIXELS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, t)) = cache.iter().find(|(k, _)| *k == p.key) {
            return Some(t.clone());
        }
        let stride = p.width as usize * 4;
        if p.rgba.len() < stride * p.height as usize {
            return None;
        }
        let bytes = gtk::glib::Bytes::from(&p.rgba[..stride * p.height as usize]);
        let tex: gdk::Texture = gdk::MemoryTexture::new(
            p.width as i32,
            p.height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            stride,
        )
        .upcast();
        cache.push_front((p.key, tex.clone()));
        cache.truncate(6);
        Some(tex)
    })
}

thread_local! {
    /// The textures of the last few dmabuf pictures.
    static EXTERNAL: RefCell<std::collections::VecDeque<(u64, gdk::Texture)>> =
        const { RefCell::new(std::collections::VecDeque::new()) };
}

/// A texture of a dmabuf frame (imported as it is: no copy), or `None`
/// when this is no frame the painter knows or the display refuses it
/// (then frames come through memory from now on).
fn external_texture(e: &faderframe_ui_canvas::External<'_>) -> Option<gdk::Texture> {
    EXTERNAL.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, t)) = cache.iter().find(|(k, _)| *k == e.key) {
            return Some(t.clone());
        }
        let tex = dmabuf_texture(e)?;
        cache.push_front((e.key, tex.clone()));
        cache.truncate(6);
        Some(tex)
    })
}

#[cfg(target_os = "linux")]
fn dmabuf_texture(e: &faderframe_ui_canvas::External<'_>) -> Option<gdk::Texture> {
    let frame = e
        .handle
        .downcast_ref::<faderframe_video::zero_copy::GpuFrame>()?
        .clone();
    let display = gdk::Display::default()?;
    let mut builder = gdk::DmabufTextureBuilder::new()
        .set_display(&display)
        .set_width(e.width)
        .set_height(e.height)
        .set_fourcc(frame.fourcc)
        .set_modifier(frame.modifier)
        .set_n_planes(frame.planes.len() as u32)
        .set_premultiplied(true);
    for (i, p) in frame.planes.iter().enumerate() {
        builder = builder
            .set_offset(i as u32, p.offset)
            .set_stride(i as u32, p.stride);
    }
    let fds: Vec<_> = frame.planes.iter().map(|p| p.fd).collect();
    // SAFETY: the descriptors belong to the frame's buffer, which the
    // closure keeps (a clone of the frame) until GTK drops it once it no
    // longer uses them.
    let built = unsafe {
        for (i, fd) in fds.into_iter().enumerate() {
            builder = builder.set_fd(i as u32, fd);
        }
        builder.build_with_release_func(move || drop(frame))
    };
    match built {
        Ok(t) => Some(t),
        Err(err) => {
            faderframe_video::zero_copy::failed(&err.to_string());
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn dmabuf_texture(_e: &faderframe_ui_canvas::External<'_>) -> Option<gdk::Texture> {
    None
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

/// A label's text node (none for nothing to draw), its height and the
/// frame it was last drawn in.
type TextNode = (Option<gsk::RenderNode>, i32, u64);

/// Shaped pango layouts reused across frames (most labels never change),
/// and the text nodes made from them: a label drawn again is its node
/// again (under a translation), so neither pango nor GTK lays it out or
/// measures its glyphs anew, and GTK's frame diff sees the same node.
#[derive(Default)]
pub struct TextCache {
    layouts: HashMap<TextKey, (pango::Layout, u64)>,
    nodes: HashMap<(TextKey, [u32; 4]), TextNode>,
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
        if self.nodes.len() > Self::MAX {
            let keep_from = self.frame.saturating_sub(2);
            self.nodes.retain(|_, (_, _, used)| *used >= keep_from);
        }
    }

    pub fn clear(&mut self) {
        self.layouts.clear();
        self.nodes.clear();
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
    paths: &'a RefCell<PathCache>,
    kept: Option<&'a RefCell<KeptCache>>,
    /// Pictures being drawn to keep (innermost last): drawing goes there.
    layers: Vec<Layer>,
    scale: f32,
}

/// A picture being drawn to keep: its own snapshot (in its own
/// coordinates), the key and where it shows (on device pixels).
struct Layer {
    snapshot: gtk::Snapshot,
    key: u64,
    rect: Rect,
}

/// Pictures views keep ([`faderframe_ui_canvas::kept`]): layers drawn
/// once into textures and shown again while their key holds, so a frame
/// that changes around them (the playhead over a waveform) renders a
/// texture there instead of filling paths again with cairo.
#[derive(Default)]
pub struct KeptCache {
    pictures: HashMap<u64, Kept>,
    frame: u64,
    bytes: usize,
    /// Pictures made this frame (each is a render and a download).
    made: usize,
}

struct Kept {
    texture: gdk::Texture,
    /// Device pixels per logical pixel it was made at, and its size.
    scale: u32,
    size: (i32, i32),
    used: u64,
}

impl KeptCache {
    /// Pictures made per frame at most: the rest draw as usual (a zoom
    /// gesture changes every key every frame).
    const PER_FRAME: usize = 6;
    const MAX_BYTES: usize = 192 << 20;
    /// Frames a picture not shown is kept (scrolled away and back).
    const KEEP_FRAMES: u64 = 600;

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.made = 0;
        let frame = self.frame;
        if self.bytes > Self::MAX_BYTES || frame.is_multiple_of(60) {
            let mut bytes = self.bytes;
            self.pictures.retain(|_, k| {
                let keep = frame - k.used <= Self::KEEP_FRAMES;
                if !keep {
                    bytes -= k.texture.width() as usize * k.texture.height() as usize * 4;
                }
                keep
            });
            // Still too many: the longest unseen go.
            if bytes > Self::MAX_BYTES {
                let mut by_age: Vec<(u64, u64)> = self
                    .pictures
                    .iter()
                    .map(|(key, k)| (k.used, *key))
                    .collect();
                by_age.sort_unstable();
                for (_, key) in by_age {
                    if bytes <= Self::MAX_BYTES / 2 {
                        break;
                    }
                    if let Some(k) = self.pictures.remove(&key) {
                        bytes -= k.texture.width() as usize * k.texture.height() as usize * 4;
                    }
                }
            }
            self.bytes = bytes;
        }
    }

    pub fn clear(&mut self) {
        self.pictures.clear();
        self.bytes = 0;
    }
}

/// `rect` on whole device pixels: its corner on the pixel it is in, its
/// size rounded up — the size by the size alone, so pictures of the same
/// size anywhere (meters, tiles) are the same picture.
fn on_device_pixels(rect: Rect, scale: f32) -> (Rect, (i32, i32)) {
    let x0 = (rect.x * scale).floor();
    let y0 = (rect.y * scale).floor();
    let w = (rect.w * scale - 1e-3).ceil().max(0.0);
    let h = (rect.h * scale - 1e-3).ceil().max(0.0);
    (
        Rect::new(x0 / scale, y0 / scale, w / scale, h / scale),
        (w as i32, h as i32),
    )
}

impl<'a> SnapshotPainter<'a> {
    pub fn new(
        snapshot: &'a gtk::Snapshot,
        widget: &'a gtk::Widget,
        cache: &'a RefCell<TextCache>,
        paths: &'a RefCell<PathCache>,
    ) -> Self {
        let scale = widget
            .native()
            .and_then(|n| n.surface())
            .map_or(widget.scale_factor() as f64, |s| s.scale()) as f32;
        Self {
            snapshot,
            widget,
            cache,
            paths,
            kept: None,
            layers: Vec::new(),
            scale,
        }
    }

    /// Keep pictures views ask to keep in `kept`.
    pub fn with_kept(mut self, kept: &'a RefCell<KeptCache>) -> Self {
        self.kept = Some(kept);
        self
    }

    /// Where drawing goes: the picture being kept, or the frame.
    fn snap(&self) -> &gtk::Snapshot {
        self.layers.last().map_or(self.snapshot, |l| &l.snapshot)
    }

    /// Show a kept texture in `rect` (on device pixels: drawn 1:1).
    fn show_texture(&self, texture: &gdk::Texture, rect: Rect) {
        self.snap()
            .append_scaled_texture(texture, gsk::ScalingFilter::Nearest, &grect(rect));
    }

    fn text_key(text: &str, width: f32, style: &TextStyle) -> TextKey {
        TextKey {
            text: text.to_string(),
            size: style.size.to_bits(),
            family: style.family as u8,
            weight: style.weight as u8,
            tracking: style.tracking.to_bits(),
            width: width.round() as i32,
            align: style.align as u8,
        }
    }

    /// The text node of a label at the origin and its height (made once
    /// while it keeps being drawn).
    fn text_node(
        &self,
        text: &str,
        width: f32,
        style: &TextStyle,
    ) -> (Option<gsk::RenderNode>, i32) {
        let c = style.color;
        let key = (
            Self::text_key(text, width, style),
            [c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()],
        );
        {
            let mut cache = self.cache.borrow_mut();
            let frame = cache.frame;
            if let Some((node, h, used)) = cache.nodes.get_mut(&key) {
                *used = frame;
                return (node.clone(), *h);
            }
        }
        let layout = self.layout(text, width, style);
        let (_, h) = layout.pixel_size();
        let snapshot = gtk::Snapshot::new();
        snapshot.append_layout(&layout, &rgba(c));
        let node = snapshot.to_node();
        let mut cache = self.cache.borrow_mut();
        let frame = cache.frame;
        cache.nodes.insert(key, (node.clone(), h, frame));
        (node, h)
    }

    fn layout(&self, text: &str, width: f32, style: &TextStyle) -> pango::Layout {
        let key = Self::text_key(text, width, style);
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
        let s = self.snap();
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
        self.snap().push_rounded_clip(&rounded(rect, radius));
        self.fill_paint(rect, paint);
        self.snap().pop();
    }

    fn stroke_rounded(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        if rect.is_empty() {
            return;
        }
        let c = rgba(color);
        self.snap()
            .append_border(&rounded(rect, radius), &[width; 4], &[c, c, c, c]);
    }

    fn fill_path(&mut self, path: &Path, color: Color) {
        if !path.is_empty() {
            let path = self.paths.borrow_mut().get(path);
            self.snap()
                .append_fill(&path, gsk::FillRule::Winding, &rgba(color));
        }
    }

    fn stroke_path(&mut self, path: &Path, width: f32, color: Color) {
        if path.is_empty() {
            return;
        }
        let stroke = gsk::Stroke::new(width);
        stroke.set_line_cap(gsk::LineCap::Round);
        stroke.set_line_join(gsk::LineJoin::Round);
        let path = self.paths.borrow_mut().get(path);
        self.snap().append_stroke(&path, &stroke, &rgba(color));
    }

    fn fill_path_paint(&mut self, path: &Path, paint: &Paint) {
        if path.is_empty() {
            return;
        }
        if let Paint::Solid(c) = paint {
            return self.fill_path(path, *c);
        }
        let p = self.paths.borrow_mut().get(path);
        let Some(bounds) = p.bounds() else {
            return;
        };
        self.snap().push_fill(&p, gsk::FillRule::Winding);
        let r = Rect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height());
        self.fill_paint(r, paint);
        self.snap().pop();
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
        let s = self.snap();
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

    fn pixels(&mut self, pixels: &faderframe_ui_canvas::Pixels<'_>, dst: Rect) {
        if dst.is_empty() || pixels.width == 0 || pixels.height == 0 {
            return;
        }
        let Some(tex) = pixel_texture(pixels) else {
            return;
        };
        let s = self.snap();
        s.push_clip(&grect(dst));
        s.append_scaled_texture(&tex, gsk::ScalingFilter::Linear, &grect(dst));
        s.pop();
    }

    fn external(&mut self, image: &faderframe_ui_canvas::External<'_>, dst: Rect) -> bool {
        if dst.is_empty() || image.width == 0 || image.height == 0 {
            return true;
        }
        let Some(tex) = external_texture(image) else {
            return false;
        };
        let s = self.snap();
        s.push_clip(&grect(dst));
        s.append_scaled_texture(&tex, gsk::ScalingFilter::Linear, &grect(dst));
        s.pop();
        true
    }

    fn push_transform(&mut self, dx: f32, dy: f32, scale: f32) {
        self.snap().save();
        self.snap().translate(&graphene::Point::new(dx, dy));
        self.snap().scale(scale, scale);
    }

    fn pop_transform(&mut self) {
        self.snap().restore();
    }

    fn shadow(&mut self, rect: Rect, radius: f32, color: Color, dx: f32, dy: f32, blur: f32) {
        if !rect.is_empty() {
            self.snap().append_outset_shadow(
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
            self.snap().append_inset_shadow(
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
        let (node, h) = self.text_node(text, rect.w, style);
        let Some(node) = node else {
            return;
        };
        let y = match style.valign {
            Align::Start => rect.y,
            Align::Center => rect.y + (rect.h - h as f32) * 0.5,
            Align::End => rect.bottom() - h as f32,
        };
        self.snap().save();
        self.snap()
            .translate(&graphene::Point::new(rect.x, y.round()));
        self.snap().append_node(&node);
        self.snap().restore();
    }

    fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.layout(text, 0.0, style).pixel_size().0 as f32
    }

    fn push_clip(&mut self, rect: Rect) {
        self.snap().push_clip(&grect(rect));
    }

    fn pop_clip(&mut self) {
        self.snap().pop();
    }

    fn scale_factor(&self) -> f32 {
        self.scale
    }

    fn show_kept(&mut self, key: u64, rect: Rect) -> bool {
        let Some(cache) = self.kept else {
            return false;
        };
        let (rect, size) = on_device_pixels(rect, self.scale);
        let mut cache = cache.borrow_mut();
        let frame = cache.frame;
        let Some(k) = cache.pictures.get_mut(&key) else {
            return false;
        };
        if k.scale != self.scale.to_bits() || k.size != size {
            return false;
        }
        k.used = frame;
        let texture = k.texture.clone();
        drop(cache);
        self.show_texture(&texture, rect);
        true
    }

    fn begin_keep(&mut self, key: u64, rect: Rect) -> bool {
        let Some(cache) = self.kept else {
            return false;
        };
        {
            let mut cache = cache.borrow_mut();
            if cache.made >= KeptCache::PER_FRAME || !self.layers.is_empty() {
                return false;
            }
            cache.made += 1;
        }
        let (rect, _) = on_device_pixels(rect, self.scale);
        let snapshot = gtk::Snapshot::new();
        // Drawn in the picture's own coordinates.
        snapshot.translate(&graphene::Point::new(-rect.x, -rect.y));
        self.layers.push(Layer {
            snapshot,
            key,
            rect,
        });
        true
    }

    fn end_keep(&mut self) {
        let Some(layer) = self.layers.pop() else {
            return;
        };
        let (rect, size) = on_device_pixels(layer.rect, self.scale);
        let Some(node) = layer.snapshot.to_node() else {
            return;
        };
        // Rendered at device pixels into a texture, kept and shown; without
        // a renderer (not realised) the layer is just drawn.
        let renderer = self.widget.native().and_then(|n| n.renderer());
        let texture = renderer.filter(|_| size.0 > 0 && size.1 > 0).map(|r| {
            let scaled = gsk::TransformNode::new(
                &node,
                Some(&gsk::Transform::new().scale(self.scale, self.scale)),
            );
            r.render_texture(
                &scaled,
                Some(&graphene::Rect::new(0.0, 0.0, size.0 as f32, size.1 as f32)),
            )
        });
        match texture {
            Some(texture) => {
                if let Some(cache) = self.kept {
                    let mut cache = cache.borrow_mut();
                    let frame = cache.frame;
                    cache.bytes += size.0 as usize * size.1 as usize * 4;
                    if let Some(old) = cache.pictures.insert(
                        layer.key,
                        Kept {
                            texture: texture.clone(),
                            scale: self.scale.to_bits(),
                            size,
                            used: frame,
                        },
                    ) {
                        cache.bytes = cache.bytes.saturating_sub(
                            old.texture.width() as usize * old.texture.height() as usize * 4,
                        );
                    }
                }
                self.show_texture(&texture, rect);
            }
            None => {
                let s = self.snap();
                s.save();
                s.translate(&graphene::Point::new(rect.x, rect.y));
                s.append_node(&node);
                s.restore();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_ui_canvas::Point;

    #[test]
    fn redraws_reuse_native_path_identity() {
        let mut cache = PathCache::default();
        let shape = || Path::circle(Point::new(40.0, 40.0), 12.0);
        let first = cache.get(&shape());
        // Views rebuild their toolkit-independent geometry on every paint.
        // Equal-looking GSK paths with different pointers miss its GPU cache.
        for _ in 0..100 {
            cache.begin_frame();
            assert_eq!(first.as_ptr(), cache.get(&shape()).as_ptr());
        }
        let changed = Path::circle(Point::new(40.0, 40.0), 13.0);
        let other = cache.get(&changed);
        assert_ne!(first.as_ptr(), other.as_ptr());
        assert_eq!(other.to_string(), gsk_path(&changed).to_string());
    }

    #[test]
    fn animated_paths_expire_without_evicting_unchanged_controls() {
        let mut cache = PathCache::default();
        let knob = Path::circle(Point::new(40.0, 40.0), 12.0);
        let first = cache.get(&knob);
        for frame in 0..100 {
            cache.begin_frame();
            assert_eq!(first.as_ptr(), cache.get(&knob).as_ptr());
            cache.get(&Path::circle(Point::new(frame as f32, 0.0), 1.0));
            assert!(cache.paths.len() <= 4);
        }
        for _ in 0..3 {
            cache.begin_frame();
        }
        assert!(cache.paths.is_empty());
        assert_eq!(cache.words, 0);
    }

    #[test]
    fn oversized_frames_and_paths_do_not_grow_the_cache_without_bound() {
        let mut cache = PathCache::default();
        for i in 0..PathCache::MAX_PATHS * 2 {
            let mut path = Path::new();
            path.move_to(Point::new(i as f32, 0.0))
                .line_to(Point::new(i as f32, 1.0));
            cache.get(&path);
        }
        assert!(cache.paths.len() <= PathCache::MAX_PATHS);
        assert!(cache.words <= PathCache::MAX_WORDS);

        let mut large = Path::new();
        large.move_to(Point::new(0.0, 0.0));
        for i in 0..PathCache::MAX_WORDS / 3 {
            large.line_to(Point::new(i as f32, (i % 2) as f32));
        }
        let mut cache = PathCache::default();
        let native = cache.get(&large);
        assert_eq!(native.to_string(), gsk_path(&large).to_string());
        assert!(cache.paths.is_empty());
        assert_eq!(cache.words, 0);
    }
}
