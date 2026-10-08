//! Music glyphs as outlines: moves, lines and cubic curves in staff spaces,
//! y up (as in the font they come from).

/// A piece of an outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    M(f32, f32),
    L(f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

/// A glyph: its SMuFL name and code point, advance width, bounding box
/// (x0, y0, x1, y1), the noteheads' stem anchors and its outline.
#[derive(Debug)]
pub struct Glyph {
    pub name: &'static str,
    pub code: u32,
    pub advance: f32,
    pub bbox: [f32; 4],
    pub stem_up: [f32; 2],
    pub stem_down: [f32; 2],
    pub path: &'static [Seg],
}

/// The glyph called `name` (a SMuFL name the engraver uses).
pub fn glyph(name: &str) -> Option<&'static Glyph> {
    crate::assets::GLYPHS.iter().find(|g| g.name == name)
}
