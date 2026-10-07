//! Single-line text with parley, drawn as vello glyph runs: the toolkit
//! painter's font families, weights, tracking, alignment and ellipsis.

use faderframe_ui_canvas::{Align, FontFamily, FontWeight, Rect, TextStyle};
use parley::{FontContext, Layout, LayoutContext, StyleProperty};
use std::collections::HashMap;
use vello::kurbo::Affine;
use vello::peniko::Fill;

/// Font family lists as the toolkit painter asks for them.
fn families(f: FontFamily) -> &'static str {
    match f {
        FontFamily::Sans => "Inter, Noto Sans, Fira Sans, Cantarell, DejaVu Sans, sans-serif",
        FontFamily::Condensed => {
            "Fira Sans Condensed, Roboto Condensed, Noto Sans, DejaVu Sans Condensed, sans-serif"
        }
        FontFamily::Mono => {
            "JetBrains Mono, Fira Mono, Noto Sans Mono, DejaVu Sans Mono, monospace"
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size: u32,
    family: u8,
    weight: u8,
    tracking: u32,
    /// Width it is fitted to (0: as wide as it is).
    width: i32,
}

pub(crate) struct TextSystem {
    fonts: FontContext,
    layouts: LayoutContext<()>,
    cache: HashMap<Key, (Layout<()>, u64)>,
    frame: u64,
}

impl TextSystem {
    const MAX: usize = 4096;

    pub(crate) fn new() -> Self {
        Self {
            fonts: FontContext::new(),
            layouts: LayoutContext::new(),
            cache: HashMap::new(),
            frame: 0,
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.frame += 1;
        if self.cache.len() > Self::MAX {
            let keep_from = self.frame.saturating_sub(2);
            self.cache.retain(|_, (_, used)| *used >= keep_from);
        }
    }

    fn build(&mut self, text: &str, style: &TextStyle) -> Layout<()> {
        let mut b = self
            .layouts
            .ranged_builder(&mut self.fonts, text, 1.0, false);
        b.push_default(StyleProperty::FontFamily(families(style.family).into()));
        b.push_default(StyleProperty::FontSize(style.size));
        b.push_default(StyleProperty::FontWeight(match style.weight {
            FontWeight::Normal => parley::FontWeight::NORMAL,
            FontWeight::Medium => parley::FontWeight::MEDIUM,
            FontWeight::Bold => parley::FontWeight::BOLD,
        }));
        if style.family == FontFamily::Condensed {
            b.push_default(StyleProperty::FontWidth(parley::FontWidth::SEMI_CONDENSED));
        }
        if style.tracking > 0.0 {
            b.push_default(StyleProperty::LetterSpacing(style.tracking));
        }
        let mut layout = b.build(text);
        layout.break_all_lines(None);
        layout
    }

    /// `text` laid out, ellipsised to fit `width` (> 0).
    fn layout(&mut self, text: &str, width: f32, style: &TextStyle) -> &Layout<()> {
        let key = Key {
            text: text.to_string(),
            size: style.size.to_bits(),
            family: style.family as u8,
            weight: style.weight as u8,
            tracking: style.tracking.to_bits(),
            width: width.round() as i32,
        };
        let frame = self.frame;
        if !self.cache.contains_key(&key) {
            let mut layout = self.build(text, style);
            if width > 0.0 && layout.width() > width + 0.5 {
                // The longest start that fits with an ellipsis.
                let ends: Vec<usize> = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .skip(1)
                    .chain([text.len()])
                    .collect();
                let (mut lo, mut hi) = (0usize, ends.len());
                let mut best = self.build("…", style);
                while lo < hi {
                    let mid = (lo + hi).div_ceil(2);
                    let candidate = format!("{}…", text[..ends[mid - 1]].trim_end());
                    let l = self.build(&candidate, style);
                    if l.width() <= width + 0.5 {
                        best = l;
                        lo = mid;
                    } else {
                        hi = mid - 1;
                    }
                }
                layout = best;
            }
            self.cache.insert(key.clone(), (layout, frame));
        }
        let entry = self.cache.get_mut(&key).map(|(l, used)| {
            *used = frame;
            &*l
        });
        // Inserted above.
        match entry {
            Some(l) => l,
            None => unreachable!("the layout was just cached"),
        }
    }

    pub(crate) fn width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.layout(text, 0.0, style).width()
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        scale: f32,
        text: &str,
        rect: Rect,
        style: &TextStyle,
    ) {
        let layout = self.layout(text, rect.w, style);
        let (w, h) = (layout.width(), layout.height());
        let x = match style.align {
            Align::Start => rect.x,
            Align::Center => rect.x + (rect.w - w) * 0.5,
            Align::End => rect.x + rect.w - w,
        };
        let y = match style.valign {
            Align::Start => rect.y,
            Align::Center => rect.y + (rect.h - h) * 0.5,
            Align::End => rect.y + rect.h - h,
        };
        // On whole device pixels vertically, like the toolkit's text.
        let y = (y * scale).round() / scale;
        let at = transform * Affine::translate((f64::from(x), f64::from(y)));
        let brush = crate::scene::color(style.color);
        for line in layout.lines() {
            for item in line.items() {
                let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let r = run.run();
                scene
                    .draw_glyphs(r.font())
                    .font_size(r.font_size())
                    .transform(at)
                    .normalized_coords(r.normalized_coords())
                    .hint(true)
                    .brush(brush)
                    .draw(
                        Fill::NonZero,
                        run.positioned_glyphs().map(|g| vello::Glyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                        }),
                    );
            }
        }
    }
}
