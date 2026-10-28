//! Analogue-console-style controls: panels, knobs, faders, segmented
//! meters, LED buttons, scribble strips.
//!
//! Each control is a pure drawing function plus (where interactive) a small
//! geometry type for hit testing, so views stay in control of state and
//! interaction. Everything scales with the theme sizes; nothing assumes a
//! device-pixel ratio.

use crate::{
    Align, ButtonKind, Color, FaderKind, FontFamily, FontWeight, KnobKind, MeterKind, Paint,
    Painter, Path, Point, Rect, TextStyle, Theme,
};
use std::f32::consts::PI;

/// Corner radius of a plane ([`crate::PanelKind::Plane`]).
pub const PLANE_RADIUS: f32 = 7.0;

/// Knob sweep: 270° starting at the lower left (135°, clockwise).
const KNOB_START: f32 = PI * 0.75;
const KNOB_SWEEP: f32 = PI * 1.5;

/// A padlock filling `r` (body below, shackle above) in `color`: the mark
/// of material locked in time (time-locked tracks and their clips).
pub fn lock_glyph(p: &mut dyn Painter, r: Rect, color: Color) {
    let w = r.w.min(r.h * 0.85);
    let x = r.x + (r.w - w) / 2.0;
    let body_h = r.h * 0.55;
    let body = Rect::new(x, r.bottom() - body_h, w, body_h);
    let radius = (w * 0.3).max(1.0);
    let line = (w * 0.16).clamp(1.0, 1.6);
    let cx = x + w / 2.0;
    let top = r.y + radius + line / 2.0;
    let mut shackle = Path::new();
    shackle.move_to(Point::new(cx - radius, body.y + 0.5));
    shackle.line_to(Point::new(cx - radius, top));
    shackle.arc(Point::new(cx, top), radius, PI, 2.0 * PI, true);
    shackle.line_to(Point::new(cx + radius, body.y + 0.5));
    p.stroke_path(&shackle, line, color);
    p.fill_rounded(body, 1.2, &Paint::Solid(color));
}

/// Normalised value change for a vertical drag of `dy` pixels.
/// Moving up increases the value; `fine` gives 5× more resolution.
pub fn drag_delta(dy: f32, fine: bool) -> f32 {
    -dy / if fine { 900.0 } else { 180.0 }
}

pub fn knob_angle(value: f32) -> f32 {
    KNOB_START + KNOB_SWEEP * value.clamp(0.0, 1.0)
}

/// A stable pseudo-random value in 0..1 for `i` (panel grain, wood).
fn grain(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9e37_79b9) ^ 0x85eb_ca6b;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2c1b_3c6d);
    x ^= x >> 12;
    (x & 0xffff) as f32 / 65535.0
}

/// Brushed-panel background with bevelled edges.
pub fn panel(p: &mut dyn Painter, rect: Rect, top: Color, bottom: Color, theme: &Theme) {
    let c = &theme.console;
    let look = &c.look;
    if look.planes() {
        plane(p, rect, top, bottom, theme);
        return;
    }
    if look.flat {
        p.fill(rect, top.mix(bottom, 0.5));
        p.vline(rect.x, rect.y, rect.bottom(), c.panel_edge_light);
        p.vline(rect.right() - 1.0, rect.y, rect.bottom(), c.panel_edge_dark);
        return;
    }
    p.fill_rect(
        rect,
        &Paint::vertical_stops(
            rect,
            vec![
                (0.0, top.lighten(0.04)),
                (0.08, top),
                (0.55, top.mix(bottom, 0.6)),
                (1.0, bottom),
            ],
        ),
    );
    // A few soft horizontal sheen bands suggest brushed metal.
    if look.sheen > 0.0 {
        for (i, a) in [(0.18f32, 0.025f32), (0.42, 0.018), (0.71, 0.022)] {
            let y = rect.y + rect.h * i;
            p.fill(
                Rect::new(rect.x, y, rect.w, rect.h * 0.06),
                Color::rgba(1.0, 1.0, 1.0, a * look.sheen),
            );
        }
    }
    // Brushed grain: fine horizontal hairlines.
    if look.brushed > 0.0 {
        let seed = (rect.x.to_bits() >> 8) ^ (rect.w as u32) << 3;
        let mut y = rect.y + 1.0;
        let mut i = 0u32;
        while y < rect.bottom() {
            let g = grain(seed.wrapping_add(i));
            let color = if g > 0.5 {
                Color::rgba(1.0, 1.0, 1.0, look.brushed * 0.05 * (g - 0.5) * 2.0)
            } else {
                Color::rgba(0.0, 0.0, 0.0, look.brushed * 0.08 * (0.5 - g) * 2.0)
            };
            p.hline(rect.x + 1.0, rect.right() - 1.0, y, color);
            y += 2.0 + grain(seed ^ i.wrapping_mul(7)) * 2.0;
            i += 1;
        }
    }
    p.vline(rect.x, rect.y, rect.bottom(), c.panel_edge_light);
    p.vline(
        rect.right() - 1.0 / p.scale_factor().max(1.0),
        rect.y,
        rect.bottom(),
        c.panel_edge_dark,
    );
    p.hline(rect.x, rect.right(), rect.y, c.panel_edge_light);
}

/// A plane: a flat rounded surface a step of luminance above what it lies
/// on, with a hairline of light along its edge.
pub fn plane(p: &mut dyn Painter, rect: Rect, top: Color, bottom: Color, theme: &Theme) {
    let r = PLANE_RADIUS.min(rect.w * 0.25).min(rect.h * 0.25).max(0.0);
    p.fill_rounded(rect, r, &Paint::vertical(rect, top, bottom));
    let px = 1.0 / p.scale_factor().max(1.0);
    p.stroke_rounded(rect.inset(px * 0.5), r, px, theme.console.panel_edge_light);
}

/// Thin engraved divider between console sections (a hairline on
/// planes).
pub fn section_line(p: &mut dyn Painter, x0: f32, x1: f32, y: f32, theme: &Theme) {
    p.hline(x0, x1, y, theme.console.section_line);
    if theme.console.look.planes() {
        return;
    }
    p.hline(x0, x1, y + 1.0, theme.console.panel_edge_light);
}

/// Small-caps style legend with an engraved shadow (spaced wider, with no
/// shadow, on planes).
pub fn engraved(p: &mut dyn Painter, text: &str, rect: Rect, theme: &Theme, align: Align) {
    let style = TextStyle::new(theme.fonts.tiny, theme.console.panel_label)
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold)
        .align(align)
        .tracking(if theme.console.look.planes() {
            0.9
        } else {
            0.6
        });
    let shadow = theme.console.look.engrave;
    if shadow.a > 0.0 {
        p.text(text, rect.translate(0.0, 1.0), &style.color(shadow));
    }
    p.text(text, rect, &style);
}

/// Recessed well (insert slots, readouts). Returns the inner rect.
pub fn well(p: &mut dyn Painter, rect: Rect, theme: &Theme) -> Rect {
    let c = &theme.console;
    if c.look.planes() {
        // A darker inset, flat.
        p.fill_rounded(rect, 4.0f32.min(rect.h * 0.5), &Paint::Solid(c.well));
        return rect.inset_xy(4.0, 1.0);
    }
    p.fill_rounded(rect, 2.5, &Paint::Solid(c.well));
    p.inset_shadow(rect, 2.5, Color::rgba(0.0, 0.0, 0.0, 0.8), 0.0, 1.0, 3.0);
    p.hline(
        rect.x + 2.0,
        rect.right() - 2.0,
        rect.bottom(),
        c.panel_edge_light,
    );
    rect.inset_xy(4.0, 1.0)
}

/// Text in a recessed well.
pub fn well_label(p: &mut dyn Painter, rect: Rect, text: &str, empty: bool, theme: &Theme) {
    let inner = well(p, rect, theme);
    let c = &theme.console;
    let style = TextStyle::new(
        theme.fonts.tiny + 0.5,
        if empty {
            c.well_text_empty
        } else {
            c.well_text
        },
    )
    .family(FontFamily::Condensed)
    .align(Align::Center);
    p.text(text, inner, &style);
}

#[derive(Clone, Copy, Debug)]
pub struct KnobLook {
    pub cap: Color,
    pub ring: Color,
}

/// A rotary control: an illuminated value ring around a knob, a vintage
/// knob on a skirt with a printed scale, or a flat disc — as the theme's
/// [`crate::ConsoleLook`] says.
pub fn knob(
    p: &mut dyn Painter,
    rect: Rect,
    value: f32,
    bipolar: bool,
    look: KnobLook,
    theme: &Theme,
) {
    let k = &theme.console.knob;
    let style = &theme.console.look;
    let c = rect.center();
    let r = rect.w.min(rect.h) * 0.5;
    if r < 4.0 {
        return;
    }
    if let Some(skirt) = style.knob_skirt {
        skirted_knob(p, c, r, value, look.cap, skirt, theme);
        return;
    }
    if style.knob_kind == KnobKind::Halo {
        halo_knob(p, c, r, value, bipolar, look, theme);
        return;
    }
    let ring_r = r - 1.6;
    if style.knob_ring {
        let mut track = Path::new();
        track.arc(c, ring_r, KNOB_START, KNOB_START + KNOB_SWEEP, false);
        p.stroke_path(&track, 2.4, k.ring_track);
        let (a0, a1) = if bipolar {
            let mid = KNOB_START + KNOB_SWEEP * 0.5;
            let a = knob_angle(value);
            (mid.min(a), mid.max(a))
        } else {
            (KNOB_START, knob_angle(value))
        };
        if a1 - a0 > 0.01 {
            let mut arc = Path::new();
            arc.arc(c, ring_r, a0, a1, false);
            p.stroke_path(&arc, 2.4, look.ring);
        }
    }

    let body_r = r - 4.6;
    let body = Rect::new(c.x - body_r, c.y - body_r, body_r * 2.0, body_r * 2.0);
    let cap_r = body_r * 0.74;
    let cap = Rect::new(c.x - cap_r, c.y - cap_r, cap_r * 2.0, cap_r * 2.0);
    if style.flat {
        p.fill_rounded(body, body_r, &Paint::Solid(k.body_light));
        p.fill_rounded(cap, cap_r, &Paint::Solid(k.cap_top.mix(look.cap, 0.5)));
    } else {
        p.shadow(body, body_r, k.shadow, 0.0, 1.6, 3.5);
        p.fill_rounded(
            body,
            body_r,
            &Paint::Radial {
                center: Point::new(c.x - body_r * 0.35, c.y - body_r * 0.45),
                radius: body_r * 1.6,
                stops: vec![(0.0, k.body_light), (1.0, k.body_dark)],
            },
        );
        p.fill_rounded(
            cap,
            cap_r,
            &Paint::vertical(
                cap,
                k.cap_top.mix(look.cap, 0.55).lighten(0.08),
                k.cap_bottom.mix(look.cap, 0.35),
            ),
        );
        p.stroke_rounded(cap, cap_r, 0.8, Color::rgba(0.0, 0.0, 0.0, 0.45));
    }
    let a = knob_angle(value);
    let (s, co) = (a.sin(), a.cos());
    p.line(
        Point::new(c.x + co * cap_r * 0.2, c.y + s * cap_r * 0.2),
        Point::new(c.x + co * (body_r - 1.2), c.y + s * (body_r - 1.2)),
        2.0,
        k.pointer,
    );
}

/// The value arc of a knob: from the start, or (bipolar) from the middle.
fn value_arc(value: f32, bipolar: bool) -> (f32, f32) {
    if bipolar {
        let mid = KNOB_START + KNOB_SWEEP * 0.5;
        let a = knob_angle(value);
        (mid.min(a), mid.max(a))
    } else {
        (KNOB_START, knob_angle(value))
    }
}

/// A halo knob: a fine ring over the sweep with the value as an arc of
/// `look.ring` (a soft glow under it as the look's glow says), a flat disc
/// inside (tinted a little by the cap colour) with a small dot pointing at
/// the value.
fn halo_knob(
    p: &mut dyn Painter,
    c: Point,
    r: f32,
    value: f32,
    bipolar: bool,
    look: KnobLook,
    theme: &Theme,
) {
    let k = &theme.console.knob;
    let glow = theme.console.look.glow.clamp(0.0, 2.0);
    let w = (r * 0.13).clamp(1.5, 2.6);
    let ring_r = r - w * 0.5 - 0.5 - glow.min(1.0);
    let mut track = Path::new();
    track.arc(c, ring_r, KNOB_START, KNOB_START + KNOB_SWEEP, false);
    p.stroke_path(&track, w, k.ring_track);
    let (a0, a1) = value_arc(value, bipolar);
    if a1 - a0 > 0.01 {
        let mut arc = Path::new();
        arc.arc(c, ring_r, a0, a1, false);
        if glow > 0.0 {
            p.stroke_path(&arc, w + 3.0 * glow, look.ring.with_alpha(0.16 * glow));
        }
        p.stroke_path(&arc, w, look.ring);
    }
    let disc_r = ring_r - w * 0.5 - (r * 0.14).max(2.0);
    if disc_r < 2.5 {
        return;
    }
    let disc = Rect::new(c.x - disc_r, c.y - disc_r, disc_r * 2.0, disc_r * 2.0);
    let body = k.body_light.mix(look.cap, 0.1);
    p.fill_rounded(
        disc,
        disc_r,
        &Paint::vertical(disc, body.lighten(0.05), body),
    );
    let px = 1.0 / p.scale_factor().max(1.0);
    p.stroke_rounded(disc.inset(px * 0.5), disc_r, px, k.cap_top);
    let a = knob_angle(value);
    let dot_r = (disc_r * 0.15).clamp(1.2, 2.2);
    let d = disc_r - dot_r - (disc_r * 0.16).max(1.5);
    p.circle(
        Point::new(c.x + a.cos() * d, c.y + a.sin() * d),
        dot_r,
        k.pointer,
    );
}

/// A vintage knob: a coloured cap with a pointer line on a dark skirt
/// printed with eleven scale marks.
fn skirted_knob(
    p: &mut dyn Painter,
    c: Point,
    r: f32,
    value: f32,
    cap_color: Color,
    skirt: Color,
    theme: &Theme,
) {
    let k = &theme.console.knob;
    let label = theme.console.panel_label;
    // The scale printed around the skirt.
    for i in 0..=10 {
        let a = KNOB_START + KNOB_SWEEP * i as f32 / 10.0;
        let (s, co) = (a.sin(), a.cos());
        let inner = if i % 5 == 0 { r - 3.6 } else { r - 2.4 };
        p.line(
            Point::new(c.x + co * inner, c.y + s * inner),
            Point::new(c.x + co * (r - 0.4), c.y + s * (r - 0.4)),
            if i % 5 == 0 { 1.4 } else { 0.9 },
            label.with_alpha(0.85),
        );
    }
    let skirt_r = r - 3.0;
    let disc = Rect::new(c.x - skirt_r, c.y - skirt_r, skirt_r * 2.0, skirt_r * 2.0);
    p.shadow(disc, skirt_r, k.shadow, 0.0, 2.0, 4.0);
    p.fill_rounded(
        disc,
        skirt_r,
        &Paint::Radial {
            center: Point::new(c.x - skirt_r * 0.3, c.y - skirt_r * 0.4),
            radius: skirt_r * 1.5,
            stops: vec![(0.0, skirt.lighten(0.22)), (1.0, skirt.darken(0.25))],
        },
    );
    // The skirt's own fine scale.
    for i in 0..=20 {
        let a = KNOB_START + KNOB_SWEEP * i as f32 / 20.0;
        let (s, co) = (a.sin(), a.cos());
        p.line(
            Point::new(c.x + co * (skirt_r - 2.0), c.y + s * (skirt_r - 2.0)),
            Point::new(c.x + co * (skirt_r - 0.6), c.y + s * (skirt_r - 0.6)),
            0.7,
            Color::rgba(1.0, 1.0, 1.0, 0.35),
        );
    }
    let cap_r = skirt_r * 0.66;
    let cap = Rect::new(c.x - cap_r, c.y - cap_r, cap_r * 2.0, cap_r * 2.0);
    p.fill_rounded(
        cap,
        cap_r,
        &Paint::vertical(cap, cap_color.lighten(0.3), cap_color.darken(0.25)),
    );
    p.stroke_rounded(cap, cap_r, 0.8, Color::rgba(0.0, 0.0, 0.0, 0.5));
    // A highlight across the cap's top edge.
    let mut glint = Path::new();
    glint.arc(c, cap_r - 1.2, PI * 1.15, PI * 1.85, false);
    p.stroke_path(&glint, 1.0, Color::rgba(1.0, 1.0, 1.0, 0.35));
    let a = knob_angle(value);
    let (s, co) = (a.sin(), a.cos());
    p.line(
        Point::new(c.x + co * cap_r * 0.15, c.y + s * cap_r * 0.15),
        Point::new(c.x + co * (skirt_r - 0.8), c.y + s * (skirt_r - 0.8)),
        1.8,
        k.pointer,
    );
}

/// Geometry of a vertical fader for drawing and hit testing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaderGeometry {
    pub area: Rect,
    pub slot: Rect,
    /// y of the cap centre at full travel.
    pub top: f32,
    /// y of the cap centre at zero.
    pub bottom: f32,
    pub cap_w: f32,
    pub cap_h: f32,
}

impl FaderGeometry {
    pub fn new(area: Rect, theme: &Theme) -> Self {
        let f = &theme.console.fader;
        let cap_h = f.cap_height.min(area.h * 0.3).max(18.0);
        let cap_w = f.cap_width.min(area.w);
        let top = area.y + cap_h * 0.5;
        let bottom = (area.bottom() - cap_h * 0.5).max(top + 1.0);
        let slot = Rect::new(area.center().x - 3.0, top - 6.0, 6.0, bottom - top + 12.0);
        Self {
            area,
            slot,
            top,
            bottom,
            cap_w,
            cap_h,
        }
    }

    pub fn y_for(&self, pos: f32) -> f32 {
        self.bottom - pos.clamp(0.0, 1.0) * (self.bottom - self.top)
    }

    pub fn pos_for(&self, y: f32) -> f32 {
        ((self.bottom - y) / (self.bottom - self.top)).clamp(0.0, 1.0)
    }

    pub fn travel(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn cap_rect(&self, pos: f32) -> Rect {
        let y = self.y_for(pos);
        Rect::new(
            self.area.center().x - self.cap_w * 0.5,
            y - self.cap_h * 0.5,
            self.cap_w,
            self.cap_h,
        )
    }
}

/// Fader slot, scale and cap. `scale` is `(position, label)` pairs.
pub fn fader(
    p: &mut dyn Painter,
    geo: &FaderGeometry,
    pos: f32,
    cap_color: Color,
    scale: &[(f32, &str)],
    theme: &Theme,
) {
    if theme.console.look.fader_kind == FaderKind::Pill {
        pill_fader(p, geo, pos, cap_color, scale, theme);
        return;
    }
    let f = &theme.console.fader;
    // Slot.
    p.fill_rounded(geo.slot, 3.0, &Paint::Solid(f.slot));
    p.inset_shadow(
        geo.slot,
        3.0,
        Color::rgba(0.0, 0.0, 0.0, 0.9),
        0.0,
        1.0,
        2.0,
    );
    p.vline(
        geo.slot.right(),
        geo.slot.y + 2.0,
        geo.slot.bottom() - 2.0,
        f.slot_edge,
    );
    // Scale: ticks both sides of the slot, legends on the left.
    let label = TextStyle::new(theme.fonts.tiny, f.scale_text)
        .family(FontFamily::Condensed)
        .align(Align::End);
    for &(sp, text) in scale {
        let y = geo.y_for(sp);
        let major = !text.is_empty();
        let len = if major { 6.0 } else { 3.0 };
        p.hline(geo.slot.x - 3.0 - len, geo.slot.x - 3.0, y, f.scale_tick);
        p.hline(
            geo.slot.right() + 3.0,
            geo.slot.right() + 3.0 + len,
            y,
            f.scale_tick,
        );
        if major {
            let w = geo.area.center().x - geo.area.x - 13.0;
            p.text(text, Rect::new(geo.area.x - 2.0, y - 6.0, w, 12.0), &label);
        }
    }
    // Cap.
    let cap = geo.cap_rect(pos);
    if theme.console.look.flat {
        let fill = f.cap_top.mix(cap_color, 0.5);
        p.fill_rounded(cap, 2.0, &Paint::Solid(fill));
        p.stroke_rounded(cap, 2.0, 1.0, fill.darken(0.45));
        p.fill(
            Rect::new(cap.x + 2.0, cap.center().y - 1.0, cap.w - 4.0, 2.0),
            if fill.luminance() > 0.5 {
                Color::hex(0x111111)
            } else {
                Color::WHITE
            },
        );
        return;
    }
    p.shadow(cap, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.65), 0.0, 4.0, 7.0);
    let top = f.cap_top.mix(cap_color, 0.45);
    let bottom = f.cap_bottom.mix(cap_color, 0.35);
    p.fill_rounded(
        cap,
        3.0,
        &Paint::vertical_stops(
            cap,
            vec![
                (0.0, top.lighten(0.25)),
                (0.12, top),
                (0.48, top.mix(bottom, 0.45)),
                (0.52, bottom.darken(0.15)),
                (0.88, bottom),
                (1.0, bottom.darken(0.35)),
            ],
        ),
    );
    p.stroke_rounded(cap, 3.0, 0.8, Color::rgba(0.0, 0.0, 0.0, 0.6));
    // Grip ridges and the centre index line.
    let cy = cap.center().y;
    for i in 1..=3 {
        let d = i as f32 * 4.0;
        for y in [cy - d - 2.0, cy + d + 1.0] {
            p.hline(cap.x + 4.0, cap.right() - 4.0, y, f.cap_grip);
            p.hline(
                cap.x + 4.0,
                cap.right() - 4.0,
                y + 1.0,
                Color::rgba(1.0, 1.0, 1.0, 0.18),
            );
        }
    }
    p.fill(
        Rect::new(cap.x + 2.0, cy - 1.0, cap.w - 4.0, 2.0),
        f.cap_line,
    );
    p.hline(
        cap.x + 2.0,
        cap.right() - 2.0,
        cy + 1.0,
        Color::rgba(1.0, 1.0, 1.0, 0.35),
    );
}

/// A pill fader: a slim track lit from the bottom up to the level in the
/// cap colour, hairline scale ticks with legends in the numerals face, and
/// a pill-shaped thumb floating over it with an index line of the cap
/// colour.
fn pill_fader(
    p: &mut dyn Painter,
    geo: &FaderGeometry,
    pos: f32,
    cap_color: Color,
    scale: &[(f32, &str)],
    theme: &Theme,
) {
    let f = &theme.console.fader;
    let glow = theme.console.look.glow.clamp(0.0, 2.0);
    let cx = geo.slot.center().x;
    let tw = 3.0;
    let track = Rect::new(cx - tw * 0.5, geo.top - 2.0, tw, geo.travel() + 4.0);
    p.fill_rounded(track, tw * 0.5, &Paint::Solid(f.slot));
    let y = geo.y_for(pos);
    if track.bottom() - y > 0.5 {
        let lit = Rect::new(track.x, y, tw, track.bottom() - y);
        p.fill_rounded(
            lit,
            tw * 0.5,
            &Paint::vertical(lit, cap_color.with_alpha(0.8), cap_color.with_alpha(0.06)),
        );
    }
    let label = TextStyle::new(theme.fonts.tiny, f.scale_text)
        .family(theme.fonts.numerals)
        .align(Align::End);
    for &(sp, text) in scale {
        let yy = geo.y_for(sp);
        let major = !text.is_empty();
        let len = if major { 5.0 } else { 2.5 };
        p.hline(track.x - 5.0 - len, track.x - 5.0, yy, f.scale_tick);
        if major {
            let w = geo.area.center().x - geo.area.x - 13.0;
            p.text(text, Rect::new(geo.area.x - 2.0, yy - 6.0, w, 12.0), &label);
        }
    }
    let cap = geo.cap_rect(pos);
    let h = (cap.h * 0.62).clamp(10.0, 16.0);
    let thumb = Rect::new(cap.x, y - h * 0.5, cap.w, h);
    p.shadow(
        thumb,
        h * 0.5,
        Color::rgba(0.0, 0.0, 0.0, 0.55),
        0.0,
        2.0,
        6.0,
    );
    p.fill_rounded(
        thumb,
        h * 0.5,
        &Paint::vertical(thumb, f.cap_top, f.cap_bottom),
    );
    let px = 1.0 / p.scale_factor().max(1.0);
    p.stroke_rounded(
        thumb.inset(px * 0.5),
        h * 0.5,
        px,
        theme
            .console
            .panel_edge_light
            .with_alpha((theme.console.panel_edge_light.a * 3.0).min(1.0)),
    );
    let index = Rect::new(thumb.x + 6.0, y - 1.0, thumb.w - 12.0, 2.0);
    if glow > 0.0 {
        p.shadow(index, 1.0, cap_color.with_alpha(0.45 * glow), 0.0, 0.0, 4.0);
    }
    p.fill_rounded(index, 1.0, &Paint::Solid(cap_color));
}

/// IEC 60268-18 style meter scale: dBFS → 0..1 deflection.
pub fn meter_scale(db: f32) -> f32 {
    let pct = if db < -70.0 {
        0.0
    } else if db < -60.0 {
        (db + 70.0) * 0.25
    } else if db < -50.0 {
        (db + 60.0) * 0.5 + 2.5
    } else if db < -40.0 {
        (db + 50.0) * 0.75 + 7.5
    } else if db < -30.0 {
        (db + 40.0) * 1.5 + 15.0
    } else if db < -20.0 {
        (db + 30.0) * 2.0 + 30.0
    } else if db < 0.0 {
        (db + 20.0) * 2.5 + 50.0
    } else {
        100.0
    };
    pct / 100.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterLevel {
    /// The bar's level (dBFS).
    pub level_db: f32,
    pub hold_db: f32,
    pub clipped: bool,
    /// A second level drawn inside the bar (the RMS of a Peak + RMS meter).
    pub inner_db: Option<f32>,
}

impl MeterLevel {
    pub fn new(level_db: f32, hold_db: f32, clipped: bool) -> Self {
        Self {
            level_db,
            hold_db,
            clipped,
            inner_db: None,
        }
    }
}

/// How a meter's colours are laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeterZones {
    /// Digital peaks: yellow from −18 dBFS, orange from −6, red from −2.
    Digital,
    /// The EBU PPM: yellow from TEST (−18 dBFS), orange from +6 (−12),
    /// red from +9 (−9 dBFS, the permitted maximum).
    Ppm,
    /// A K-System meter with its 0 at this dBFS: green below 0, yellow up
    /// to +4, red above.
    K(f32),
    /// A VU meter with 0 VU at this dBFS RMS, on the VU's own scale (by
    /// voltage, +3 VU at the top): green below −3 VU, yellow to 0, red
    /// above.
    Vu(f32),
}

impl MeterZones {
    /// Where yellow, orange and red begin (dBFS).
    pub fn bounds(self) -> (f32, f32, f32) {
        match self {
            MeterZones::Digital => (-18.0, -6.0, -2.0),
            MeterZones::Ppm => (-18.0, -12.0, -9.0),
            MeterZones::K(zero) => (zero, zero + 4.0, zero + 4.0),
            MeterZones::Vu(zero) => (zero - 3.0, zero, zero),
        }
    }

    /// Where a level stands on the meter (0 bottom, 1 top).
    pub fn position(self, db: f32) -> f32 {
        match self {
            MeterZones::Vu(zero) => vu_deflection(db - zero).min(1.0),
            _ => meter_scale(db),
        }
    }

    /// The level at a position (the inverse of [`Self::position`]).
    fn level_at(self, norm: f32) -> f32 {
        let (mut lo, mut hi) = (-100.0f32, 10.0f32);
        for _ in 0..28 {
            let mid = (lo + hi) / 2.0;
            if self.position(mid) < norm {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi
    }

    fn colour(self, db: f32, theme: &Theme) -> Color {
        let m = &theme.console.meter;
        let (y, o, r) = self.bounds();
        if db >= r {
            m.red
        } else if db >= o {
            m.orange
        } else if db >= y {
            m.yellow
        } else {
            m.green
        }
    }

    /// Gradient stops for a bar body (top = 0).
    fn stops(self, theme: &Theme) -> Vec<(f32, Color)> {
        let m = &theme.console.meter;
        let (y, o, r) = self.bounds();
        let at = |db: f32| 1.0 - self.position(db);
        vec![
            (0.0, m.red),
            (at(r), m.orange),
            (at(o), m.yellow),
            (at(y), m.green),
            (1.0, m.green.darken(0.2)),
        ]
    }
}

/// A level meter with peak hold and clip indicators, drawn as the theme
/// says: a segmented LED ladder, a continuous bar, plasma columns or
/// edgewise VU meters (driven by the bar's level).
pub fn meter(p: &mut dyn Painter, rect: Rect, levels: &[MeterLevel], theme: &Theme) {
    meter_zoned(p, rect, levels, MeterZones::Digital, theme);
}

/// [`meter`] with the colours of a metering standard.
pub fn meter_zoned(
    p: &mut dyn Painter,
    rect: Rect,
    levels: &[MeterLevel],
    zones: MeterZones,
    theme: &Theme,
) {
    match theme.console.look.meter {
        MeterKind::Ladder => ladder_meter(p, rect, levels, zones, theme),
        MeterKind::Bar => bar_meter(p, rect, levels, zones, theme),
        MeterKind::Edgewise => {
            let needles: Vec<(f32, bool)> = levels
                .iter()
                .map(|l| (vu_scale(l.level_db), l.clipped))
                .collect();
            vu_edgewise(p, rect, &needles, theme)
        }
        MeterKind::Plasma => plasma_meter(p, rect, levels, zones, theme),
        MeterKind::Gradient => gradient_meter(p, rect, levels, zones, theme),
    }
}

/// VU deflection (0..1) of a level: 0 VU = −18 dBFS, scale −20…+3 VU,
/// deflection proportional to voltage like a moving-coil meter.
pub fn vu_scale(dbfs: f32) -> f32 {
    vu_deflection(dbfs + 18.0)
}

/// The deflection of a VU reading: proportional to voltage like a
/// moving coil's, +3 VU full scale (0 VU at 71 %, −20 VU at 7 %), resting
/// on the stop at 0 and pinning a little past full scale.
pub fn vu_deflection(vu: f32) -> f32 {
    (10f32.powf((vu.min(4.0) - 3.0) / 20.0)).clamp(0.0, 1.06)
}

/// Columns of a meter: (clip LED, body) per channel.
fn meter_columns(rect: Rect, n: usize) -> Vec<(Rect, Rect)> {
    let inner = rect.inset(2.0);
    let n = n.max(1) as f32;
    let col_gap = 1.5;
    let col_w = (inner.w - col_gap * (n - 1.0)) / n;
    let clip_h = 4.0;
    (0..n as usize)
        .map(|i| {
            let x = inner.x + i as f32 * (col_w + col_gap);
            (
                Rect::new(x, inner.y, col_w, clip_h),
                Rect::new(x, inner.y + clip_h + 2.0, col_w, inner.h - clip_h - 2.0),
            )
        })
        .collect()
}

fn clip_led(p: &mut dyn Painter, r: Rect, clipped: bool, theme: &Theme) {
    let m = &theme.console.meter;
    p.fill(
        r,
        if clipped {
            m.clip
        } else {
            m.clip.mix(m.background, 0.82)
        },
    );
}

/// A continuous bar coloured by zone, with a hold line; with a second
/// level (RMS) the bar is that and the peak a line above it.
fn bar_meter(
    p: &mut dyn Painter,
    rect: Rect,
    levels: &[MeterLevel],
    zones: MeterZones,
    theme: &Theme,
) {
    let m = &theme.console.meter;
    p.fill_rounded(rect, 2.0, &Paint::Solid(m.background));
    for (lv, (clip, body)) in levels.iter().zip(meter_columns(rect, levels.len())) {
        clip_led(p, clip, lv.clipped, theme);
        p.fill(body, m.green.mix(m.background, 1.0 - m.unlit));
        let fill_to = |db: f32| {
            let lit = zones.position(db);
            let top = body.bottom() - body.h * lit;
            (lit > 0.0).then(|| Rect::new(body.x, top, body.w, body.bottom() - top))
        };
        let gradient = Paint::vertical_stops(body, zones.stops(theme));
        // With a second level (RMS) that is the bar, and the peak a line
        // above it.
        let bar = lv.inner_db.unwrap_or(lv.level_db);
        if let Some(fill) = fill_to(bar) {
            p.fill_rect(fill, &gradient);
        }
        if lv.inner_db.is_some() {
            let lit = zones.position(lv.level_db);
            if lit > 0.0 {
                let y = body.bottom() - body.h * lit;
                p.fill(
                    Rect::new(body.x, y - 1.0, body.w, 2.0),
                    zones.colour(lv.level_db, theme),
                );
            }
        }
        let hold = zones.position(lv.hold_db);
        if hold > 0.01 {
            let y = body.bottom() - body.h * hold;
            p.fill(Rect::new(body.x, y - 1.0, body.w, 2.0), m.peak);
        }
    }
}

/// Fine gradient bars: each channel a slim rounded column in one smooth
/// gradient through the zone colours (cool to hot) on a faint track, a
/// bright edge and a soft bloom at the top of the level (as strong as the
/// look's glow), the hold as a hairline, a clip dot above; with a second
/// level (RMS) that is the solid bar and the peak a translucent column
/// over it.
fn gradient_meter(
    p: &mut dyn Painter,
    rect: Rect,
    levels: &[MeterLevel],
    zones: MeterZones,
    theme: &Theme,
) {
    let m = &theme.console.meter;
    let glow = theme.console.look.glow.clamp(0.0, 2.0);
    let stops = zones.stops(theme);
    // Fine columns (5 px at most) centred in the meter's room, no box
    // around them: their faint tracks draw the meter.
    let n = levels.len().max(1) as f32;
    let inner = rect.inset(2.0);
    let gap = 2.0;
    let w = ((inner.w - gap * (n - 1.0)) / n).clamp(1.0, 5.0);
    let x0 = inner.x + (inner.w - (w * n + gap * (n - 1.0))) * 0.5;
    let columns = (0..levels.len()).map(|i| {
        let x = x0 + i as f32 * (w + gap);
        (
            Rect::new(x, inner.y, w, 3.0),
            Rect::new(x, inner.y + 6.0, w, (inner.h - 6.0).max(0.0)),
        )
    });
    for (lv, (clip, body)) in levels.iter().zip(columns) {
        let r = (body.w * 0.5).min(1.5);
        p.fill_rounded(
            clip,
            (clip.h * 0.5).min(clip.w * 0.5),
            &Paint::Solid(if lv.clipped {
                m.clip
            } else {
                m.clip.mix(m.background, 0.86)
            }),
        );
        p.fill_rounded(
            body,
            r,
            &Paint::Solid(m.green.mix(m.background, 1.0 - m.unlit)),
        );
        let top_of = |db: f32| {
            let lit = zones.position(db).min(1.0);
            (lit > 0.0).then(|| body.bottom() - body.h * lit)
        };
        if lv.inner_db.is_some()
            && let Some(y) = top_of(lv.level_db)
        {
            let faint = stops.iter().map(|(t, c)| (*t, c.with_alpha(0.3))).collect();
            p.fill_rounded(
                Rect::new(body.x, y, body.w, body.bottom() - y),
                r,
                &Paint::vertical_stops(body, faint),
            );
        }
        let level = lv.inner_db.unwrap_or(lv.level_db);
        if let Some(y) = top_of(level) {
            let hot = zones.colour(level, theme);
            let edge = Rect::new(body.x, y, body.w, 1.0);
            if glow > 0.0 {
                p.shadow(edge, r, hot.with_alpha(0.5 * glow), 0.0, 0.0, 5.0);
            }
            p.fill_rounded(
                Rect::new(body.x, y, body.w, body.bottom() - y),
                r,
                &Paint::vertical_stops(body, stops.clone()),
            );
            p.fill(edge, hot.lighten(0.45));
        }
        let hold = zones.position(lv.hold_db).min(1.0);
        if hold > 0.01 {
            let y = body.bottom() - body.h * hold;
            p.fill(Rect::new(body.x, y - 0.5, body.w, 1.0), m.peak);
        }
    }
}

/// Gas-plasma bar graphs: each channel a glowing column up to its level
/// (zone colours from the theme), with fine dark lines across it like the
/// discharge cells of a plasma display, a dimly lit unlit part and a peak
/// hold mark; with a second level (RMS) the column is that and the peak a
/// glowing line above it.
fn plasma_meter(
    p: &mut dyn Painter,
    rect: Rect,
    levels: &[MeterLevel],
    zones: MeterZones,
    theme: &Theme,
) {
    let m = &theme.console.meter;
    p.fill_rounded(rect, 2.0, &Paint::Solid(m.background));
    for (lv, (clip, body)) in levels.iter().zip(meter_columns(rect, levels.len())) {
        clip_led(p, clip, lv.clipped, theme);
        let stops = zones.stops(theme);
        // The unlit cells glow faintly.
        let dim: Vec<(f32, Color)> = stops
            .iter()
            .map(|(t, c)| (*t, c.mix(m.background, 1.0 - m.unlit)))
            .collect();
        p.fill_rect(body, &Paint::vertical_stops(body, dim));
        let column = |db: f32| {
            let lit = zones.position(db);
            let top = body.bottom() - body.h * lit;
            (lit > 0.0).then(|| Rect::new(body.x, top, body.w, body.bottom() - top))
        };
        // With a second level (RMS) that is the column, and the peak a
        // glowing line above it.
        if let Some(fill) = column(lv.inner_db.unwrap_or(lv.level_db)) {
            p.shadow(fill, 1.0, m.orange.with_alpha(0.45), 0.0, 0.0, 4.0);
            p.fill_rect(fill, &Paint::vertical_stops(body, stops));
        }
        if lv.inner_db.is_some() {
            let lit = zones.position(lv.level_db);
            if lit > 0.0 {
                let y = body.bottom() - body.h * lit;
                let line = Rect::new(body.x, y - 1.0, body.w, 2.0);
                let c = zones.colour(lv.level_db, theme);
                p.shadow(line, 1.0, c.with_alpha(0.5), 0.0, 0.0, 3.0);
                p.fill(line, c);
            }
        }
        // Cell lines.
        let pitch = (m.segment + m.gap).max(2.0);
        let line = m.background.with_alpha(0.55);
        let mut yy = body.bottom() - pitch;
        while yy > body.y {
            p.fill(Rect::new(body.x, yy, body.w, m.gap.clamp(0.6, 1.2)), line);
            yy -= pitch;
        }
        let hold = zones.position(lv.hold_db);
        if hold > 0.01 {
            let yh = body.bottom() - body.h * hold;
            p.fill(Rect::new(body.x, yh - 1.0, body.w, 2.0), m.peak);
        }
    }
}

/// Edgewise moving-coil VU meters: a backlit scale with a red zone above
/// 0 VU and a needle across it at each channel's deflection
/// ([`vu_deflection`]); the clip LED with it.
pub fn vu_edgewise(p: &mut dyn Painter, rect: Rect, needles: &[(f32, bool)], theme: &Theme) {
    let m = &theme.console.meter;
    p.fill_rounded(rect, 2.0, &Paint::Solid(m.background));
    for (&(deflection, clipped), (clip, face)) in
        needles.iter().zip(meter_columns(rect, needles.len()))
    {
        clip_led(p, clip, clipped, theme);
        // The backlit face: brightest in the middle.
        p.fill_rect(
            face,
            &Paint::vertical_stops(
                face,
                vec![
                    (0.0, m.vu_face.darken(0.25)),
                    (0.45, m.vu_face),
                    (1.0, m.vu_face.darken(0.35)),
                ],
            ),
        );
        let y_of = |d: f32| face.bottom() - face.h * d;
        // Red zone above 0 VU.
        let zero = y_of(vu_deflection(0.0));
        p.fill(
            Rect::new(face.x, face.y, face.w, zero - face.y),
            m.red.with_alpha(0.35),
        );
        // Scale marks: −20, −10, −7, −5, −3, −2, −1, 0, +1, +2, +3 VU.
        for vu in [
            -20.0f32, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0,
        ] {
            let y = y_of(vu_deflection(vu));
            let major = vu == 0.0 || vu == -10.0 || vu == -20.0 || vu == 3.0;
            let w = if major { face.w * 0.4 } else { face.w * 0.22 };
            let color = if vu > 0.0 {
                m.red.darken(0.3)
            } else {
                m.vu_needle
            };
            p.hline(face.x, face.x + w, y, color.with_alpha(0.4));
        }
        // The needle: a dark bar across the face with a soft shadow below,
        // resting on its stop at the bottom when there is no signal.
        let y = y_of(deflection).clamp(face.y + 1.5, face.bottom() - 1.5);
        p.fill(
            Rect::new(face.x, y + 1.0, face.w, 2.5),
            Color::rgba(0.0, 0.0, 0.0, 0.22),
        );
        p.fill(Rect::new(face.x, y - 1.25, face.w, 2.5), m.vu_needle);
        p.inset_shadow(face, 1.0, Color::rgba(0.0, 0.0, 0.0, 0.55), 0.0, 1.0, 3.0);
    }
}

/// A moving-coil VU meter with an arc scale (the meter bridge's): the
/// backlit face, −20…+3 VU with the red arc past 0, the needle at
/// `deflection` ([`vu_deflection`]) rising from behind the bezel at the
/// bottom, a peak LED, and the caption under the meter on the panel.
pub fn vu_arc(
    p: &mut dyn Painter,
    rect: Rect,
    deflection: f32,
    peak: bool,
    caption: &str,
    theme: &Theme,
) {
    let m = &theme.console.meter;
    let caption_h = if caption.is_empty() { 0.0 } else { 12.0 };
    let face = Rect::new(
        rect.x + 1.0,
        rect.y + 1.0,
        rect.w - 2.0,
        rect.h - 2.0 - caption_h,
    );
    if face.w < 8.0 || face.h < 8.0 {
        return;
    }
    p.shadow(face, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.5), 0.0, 1.0, 3.0);
    p.fill_rounded(
        face,
        3.0,
        &Paint::vertical_stops(
            face,
            vec![
                (0.0, m.vu_face.darken(0.18)),
                (0.45, m.vu_face),
                (1.0, m.vu_face.darken(0.28)),
            ],
        ),
    );
    p.push_clip(face);
    // The pivot sits behind the bezel at the bottom; the scale swings 40°
    // each side of upright.
    const SWING: f32 = 40.0;
    let bezel_h = (face.h * 0.2).clamp(6.0, 14.0);
    let radius = (face.w * 0.66).min((face.h - bezel_h) * 1.15).max(8.0);
    let top = face.y + face.h * 0.16;
    let pivot = Point::new(face.x + face.w / 2.0, top + radius);
    let angle = |d: f32| (-90.0 - SWING + d * 2.0 * SWING).to_radians();
    let at = |d: f32, r: f32| {
        let a = angle(d);
        Point::new(pivot.x + r * a.cos(), pivot.y + r * a.sin())
    };
    let arc_r = radius * 0.9;
    // The scale's arc: dark up to 0 VU, red past it.
    let zero = vu_deflection(0.0);
    let mut scale = Path::new();
    scale.arc(pivot, arc_r, angle(0.0), angle(zero), false);
    p.stroke_path(&scale, 1.0, m.vu_needle.with_alpha(0.8));
    let mut red = Path::new();
    red.arc(pivot, arc_r, angle(zero), angle(1.0), false);
    p.stroke_path(&red, 2.5, m.red.darken(0.1));
    // Numerals as the width allows.
    let numerals: &[f32] = if face.w >= 120.0 {
        &[-20.0, -10.0, -7.0, -5.0, -3.0, 0.0, 3.0]
    } else if face.w >= 64.0 {
        &[-20.0, -10.0, -5.0, 0.0, 3.0]
    } else {
        &[]
    };
    for vu in [
        -20.0f32, -10.0, -7.0, -5.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0,
    ] {
        let d = vu_deflection(vu);
        let major = matches!(vu as i32, -20 | -10 | -7 | -5 | -3 | 0 | 3);
        let len = if major { radius * 0.11 } else { radius * 0.06 };
        let color = if vu > 0.0 {
            m.red.darken(0.1)
        } else {
            m.vu_needle
        };
        p.line(at(d, arc_r), at(d, arc_r - len), 1.0, color);
        if numerals.contains(&vu) {
            let n = at(d, arc_r + 6.0);
            let label = if vu > 0.0 {
                format!("+{}", vu as i32)
            } else {
                format!("{}", (vu as i32).abs())
            };
            p.text(
                &label,
                Rect::new(n.x - 9.0, n.y - 5.0, 18.0, 10.0),
                &TextStyle::new(theme.fonts.tiny - 1.0, color).center(),
            );
        }
    }
    // "VU" on the face.
    let bezel = Rect::new(face.x, face.bottom() - bezel_h, face.w, bezel_h);
    p.text(
        "VU",
        Rect::new(pivot.x - 14.0, bezel.y - 15.0, 28.0, 12.0),
        &TextStyle::new(theme.fonts.tiny, m.vu_needle.with_alpha(0.75))
            .weight(FontWeight::Bold)
            .center(),
    );
    // The needle and its shadow.
    let tip = at(deflection.clamp(-0.01, 1.06), radius * 0.97);
    p.line(
        Point::new(pivot.x + 1.5, pivot.y + 2.0),
        Point::new(tip.x + 1.5, tip.y + 2.0),
        1.6,
        Color::rgba(0.0, 0.0, 0.0, 0.16),
    );
    p.line(pivot, tip, 1.2, m.vu_needle);
    // The bezel over the pivot.
    p.fill_rect(
        bezel,
        &Paint::vertical(bezel, m.vu_needle.mix(m.background, 0.6), m.background),
    );
    p.pop_clip();
    // The peak LED, in the bezel.
    let led = Rect::new(
        face.x + face.w - 9.0,
        bezel.y + (bezel.h - 5.0) / 2.0,
        5.0,
        5.0,
    );
    p.fill_rounded(
        led,
        2.5,
        &Paint::Solid(if peak {
            m.clip
        } else {
            m.clip.mix(m.background, 0.7)
        }),
    );
    p.inset_shadow(face, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 1.0, 3.0);
    if !caption.is_empty() {
        p.text(
            caption,
            Rect::new(rect.x, rect.bottom() - caption_h, rect.w, caption_h),
            &TextStyle::new(theme.fonts.tiny, theme.console.panel_label).center(),
        );
    }
}

/// Segmented LED-ladder meter with peak hold and clip indicators; with a
/// second level (RMS) the lit bar is that and the peak one LED above it.
fn ladder_meter(
    p: &mut dyn Painter,
    rect: Rect,
    levels: &[MeterLevel],
    zones: MeterZones,
    theme: &Theme,
) {
    let m = &theme.console.meter;
    p.fill_rounded(rect, 2.0, &Paint::Solid(m.background));
    p.inset_shadow(rect, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.9), 0.0, 1.0, 2.0);
    if levels.is_empty() {
        return;
    }
    let inner = rect.inset(2.0);
    let n = levels.len() as f32;
    let col_gap = 1.5;
    let col_w = (inner.w - col_gap * (n - 1.0)) / n;
    let clip_h = 4.0;
    let ladder = Rect::new(
        inner.x,
        inner.y + clip_h + 2.0,
        inner.w,
        inner.h - clip_h - 2.0,
    );
    let pitch = m.segment + m.gap;
    let segments = (ladder.h / pitch).floor().max(1.0) as usize;
    // The level a segment stands for (to colour it).
    let db_of = |norm: f32| zones.level_at(norm);
    for (i, lv) in levels.iter().enumerate() {
        let x = inner.x + i as f32 * (col_w + col_gap);
        let lit = zones.position(lv.level_db);
        let inner_lit = lv.inner_db.map(meter_scale);
        let hold = zones.position(lv.hold_db);
        // Clip LED.
        let clip = Rect::new(x, inner.y, col_w, clip_h);
        p.fill(
            clip,
            if lv.clipped {
                m.clip
            } else {
                m.clip.mix(m.background, 0.82)
            },
        );
        let segment_of = |v: f32| ((v * segments as f32).ceil() as usize).min(segments);
        let hold_seg = segment_of(hold);
        // With a second level (RMS) the bar is that, and the peak one LED
        // lit at its level (an average meter with a peak dot).
        let bar = inner_lit.unwrap_or(lit);
        let peak_seg = inner_lit.map(|_| segment_of(lit));
        let y_of = |s: usize| ladder.bottom() - (s as f32 + 1.0) * pitch + m.gap;
        // The ladder unlit, and lit up to the bar (segments whose middle is
        // at or under it): two pictures kept for every meter of the size,
        // the lit one shown clipped — not a rectangle per segment a frame.
        let column = Rect::new(x, ladder.y, col_w, ladder.h);
        let key = |on: bool| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (
                "ladder",
                on,
                segments,
                column.w.to_bits(),
                column.h.to_bits(),
            )
                .hash(&mut h);
            match zones {
                MeterZones::Digital => (0u8, 0u32),
                MeterZones::Ppm => (1, 0),
                MeterZones::K(z) => (2, z.to_bits()),
                MeterZones::Vu(z) => (3, z.to_bits()),
            }
            .hash(&mut h);
            for c in [m.background, m.green, m.yellow, m.orange, m.red] {
                [c.r, c.g, c.b, c.a].map(f32::to_bits).hash(&mut h);
            }
            [m.unlit, m.segment, m.gap].map(f32::to_bits).hash(&mut h);
            h.finish()
        };
        let segments_in = |p: &mut dyn Painter, on: bool| {
            for s in 0..segments {
                let norm = (s as f32 + 0.5) / segments as f32;
                let base = zones.colour(db_of(norm), theme);
                let color = if on {
                    base
                } else {
                    base.mix(m.background, 1.0 - m.unlit)
                };
                p.fill(Rect::new(x, y_of(s), col_w, m.segment), color);
            }
        };
        crate::kept(p, key(false), column, |p| segments_in(p, false));
        let lit_count =
            ((bar * segments as f32 - 0.5).floor() + 1.0).clamp(0.0, segments as f32) as usize;
        if lit_count > 0 {
            let top = y_of(lit_count - 1);
            p.push_clip(Rect::new(x, top, col_w, ladder.bottom() - top));
            crate::kept(p, key(true), column, |p| segments_in(p, true));
            p.pop_clip();
        }
        // The hold and peak LEDs above the bar.
        for s in [
            (hold_seg > 0 && hold > 0.01).then(|| hold_seg - 1),
            peak_seg.filter(|&p| p > 0 && lit > 0.01).map(|p| p - 1),
        ]
        .into_iter()
        .flatten()
        {
            if s >= lit_count {
                let norm = (s as f32 + 0.5) / segments as f32;
                p.fill(
                    Rect::new(x, y_of(s), col_w, m.segment),
                    zones.colour(db_of(norm), theme),
                );
            }
        }
    }
}

/// Illuminated push button (mute/solo/record ...).
pub fn led_button(
    p: &mut dyn Painter,
    rect: Rect,
    label: &str,
    on: bool,
    color: Color,
    theme: &Theme,
) {
    if theme.console.look.button_kind == ButtonKind::Pill {
        pill_button(p, rect, label, on, color, theme);
        return;
    }
    let l = &theme.console.led;
    if on {
        p.shadow(rect, 3.0, color.with_alpha(0.55), 0.0, 0.0, 7.0);
        p.fill_rounded(
            rect,
            3.0,
            &Paint::vertical(rect, color.lighten(0.25), color.darken(0.12)),
        );
    } else {
        p.shadow(rect, 3.0, Color::rgba(0.0, 0.0, 0.0, 0.5), 0.0, 1.0, 2.0);
        p.fill_rounded(
            rect,
            3.0,
            &Paint::vertical(rect, l.off.lighten(0.08), l.off.darken(0.2)),
        );
    }
    p.stroke_rounded(rect, 3.0, 0.8, l.bezel);
    let style = TextStyle::new(
        theme.fonts.tiny + 0.5,
        if on { l.label_on } else { l.label_off },
    )
    .weight(FontWeight::Bold)
    .center();
    p.text(label, rect, &style);
}

/// The colour of a lit pill's label (`color` lightened until it reads on
/// the pill).
pub fn pill_label(color: Color) -> Color {
    color.lighten(0.72)
}

/// A pill switch: off, a ghost pill with a hairline edge; on, lit from
/// within in `color` with a hairline of it, a light label and a soft glow
/// (as strong as the look's glow).
fn pill_button(
    p: &mut dyn Painter,
    rect: Rect,
    label: &str,
    on: bool,
    color: Color,
    theme: &Theme,
) {
    let l = &theme.console.led;
    let glow = theme.console.look.glow.clamp(0.0, 2.0);
    let r = rect.h.min(rect.w) * 0.5;
    let px = 1.0 / p.scale_factor().max(1.0);
    let ink = if on {
        if glow > 0.0 {
            p.shadow(rect, r, color.with_alpha(0.3 * glow), 0.0, 0.0, 6.0);
        }
        p.fill_rounded(
            rect,
            r,
            &Paint::Radial {
                center: rect.center(),
                radius: rect.w.max(rect.h) * 0.7,
                stops: vec![(0.0, color.with_alpha(0.46)), (1.0, color.with_alpha(0.26))],
            },
        );
        p.stroke_rounded(rect.inset(px * 0.5), r, px, color);
        pill_label(color)
    } else {
        p.fill_rounded(rect, r, &Paint::Solid(l.off));
        p.stroke_rounded(rect.inset(px * 0.5), r, px, l.bezel);
        l.label_off
    };
    let style = TextStyle::new(theme.fonts.tiny + 0.5, ink)
        .weight(FontWeight::Bold)
        .center();
    p.text(label, rect, &style);
}

/// [`led_button`] with the first of `labels` that fits (the last
/// otherwise).
pub fn led_button_fit(
    p: &mut dyn Painter,
    rect: Rect,
    labels: &[&str],
    on: bool,
    color: Color,
    theme: &Theme,
) {
    let style =
        TextStyle::new(theme.fonts.tiny + 0.5, theme.console.led.label_on).weight(FontWeight::Bold);
    let label = labels
        .iter()
        .find(|l| p.text_width(l, &style) <= rect.w - 4.0)
        .or(labels.last())
        .copied()
        .unwrap_or("");
    led_button(p, rect, label, on, color, theme);
}

/// Cream "tape" label with a track-colour stripe, like console scribble strips.
pub fn scribble(p: &mut dyn Painter, rect: Rect, text: &str, stripe: Color, theme: &Theme) {
    let c = &theme.console;
    if c.look.planes() {
        // A chip: a darker inset with a fine line of the track colour
        // along its top and the name in the UI face.
        p.fill_rounded(rect, 4.0, &Paint::Solid(c.scribble_bg));
        let line = Rect::new(rect.x + 6.0, rect.y, (rect.w - 12.0).max(0.0), 2.0);
        let glow = c.look.glow.clamp(0.0, 2.0);
        if glow > 0.0 {
            p.shadow(line, 1.0, stripe.with_alpha(0.45 * glow), 0.0, 0.0, 4.0);
        }
        p.fill_rounded(line, 1.0, &Paint::Solid(stripe));
        let style = TextStyle::new(theme.fonts.small, c.scribble_text)
            .weight(FontWeight::Medium)
            .center();
        p.text(text, rect.inset_xy(4.0, 0.0).translate(0.0, 1.0), &style);
        return;
    }
    p.shadow(rect, 2.0, Color::rgba(0.0, 0.0, 0.0, 0.5), 0.0, 1.0, 2.0);
    p.fill_rounded(
        rect,
        2.0,
        &Paint::vertical(
            rect,
            c.scribble_bg.lighten(0.05),
            c.scribble_bg.darken(0.08),
        ),
    );
    p.fill(
        Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w - 2.0, 3.0),
        stripe,
    );
    let style = TextStyle::new(theme.fonts.small, c.scribble_text)
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold)
        .center();
    p.text(text, rect.inset_xy(3.0, 0.0).translate(0.0, 1.5), &style);
}

/// Small LCD-like numeric readout.
pub fn readout(p: &mut dyn Painter, rect: Rect, text: &str, theme: &Theme) {
    let inner = well(p, rect, theme);
    let style = TextStyle::new(theme.fonts.tiny + 0.5, theme.console.well_text)
        .family(theme.fonts.numerals)
        .center();
    p.text(text, inner, &style);
}

/// A walnut cheek (the wooden end panel of a console), with screws when
/// the theme has them.
pub fn wood_cheek(p: &mut dyn Painter, rect: Rect, theme: &Theme) {
    let look = &theme.console.look;
    let Some((light, dark)) = look.wood else {
        p.fill(rect, theme.ui.background);
        return;
    };
    p.fill_rect(
        rect,
        &Paint::Linear {
            start: Point::new(rect.x, rect.y),
            end: Point::new(rect.right(), rect.y),
            stops: vec![
                (0.0, dark),
                (0.25, light),
                (0.7, light.mix(dark, 0.4)),
                (1.0, dark.darken(0.2)),
            ],
        },
    );
    // Grain: long, slightly wavy lines of varying darkness.
    let seed = rect.x.to_bits() >> 4;
    for i in 0..((rect.w / 2.2) as u32) {
        let x0 = rect.x + 1.0 + i as f32 * 2.2 + grain(seed + i) * 1.2;
        let a = 0.08 + grain(seed ^ (i * 31)) * 0.18;
        let mut path = Path::new();
        let mut y = rect.y;
        path.move_to(Point::new(x0, y));
        while y < rect.bottom() {
            y += 40.0;
            let dx = (grain(seed + i * 7 + y as u32) - 0.5) * 1.6;
            path.line_to(Point::new(x0 + dx, y.min(rect.bottom())));
        }
        p.stroke_path(&path, 0.8, dark.darken(0.4).with_alpha(a));
    }
    p.vline(
        rect.x,
        rect.y,
        rect.bottom(),
        Color::rgba(0.0, 0.0, 0.0, 0.6),
    );
    p.vline(
        rect.right() - 1.0,
        rect.y,
        rect.bottom(),
        Color::rgba(0.0, 0.0, 0.0, 0.6),
    );
    if look.screws && rect.w > 10.0 {
        let x = rect.center().x;
        for y in [rect.y + 14.0, rect.bottom() - 14.0] {
            screw(p, Point::new(x, y), 3.2, theme);
        }
    }
}

/// Decorative panel screw.
pub fn screw(p: &mut dyn Painter, center: Point, r: f32, theme: &Theme) {
    let k = &theme.console.knob;
    p.circle(center, r + 0.6, Color::rgba(0.0, 0.0, 0.0, 0.5));
    let rect = Rect::new(center.x - r, center.y - r, r * 2.0, r * 2.0);
    p.fill_rounded(
        rect,
        r,
        &Paint::vertical(rect, k.cap_top.lighten(0.2), k.cap_bottom),
    );
    p.line(
        Point::new(center.x - r * 0.6, center.y + r * 0.3),
        Point::new(center.x + r * 0.6, center.y - r * 0.3),
        1.0,
        Color::rgba(0.0, 0.0, 0.0, 0.6),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawOp, RecordingPainter};

    #[test]
    fn meter_scale_is_monotonic() {
        let mut last = -1.0;
        for i in -800..=60 {
            let v = meter_scale(i as f32 / 10.0);
            assert!(v >= last);
            last = v;
        }
        assert_eq!(meter_scale(0.0), 1.0);
        assert_eq!(meter_scale(-80.0), 0.0);
        assert!((meter_scale(-20.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn fader_geometry_round_trips() {
        let theme = Theme::default();
        let g = FaderGeometry::new(Rect::new(0.0, 100.0, 60.0, 300.0), &theme);
        for pos in [0.0, 0.25, 0.75, 1.0] {
            assert!((g.pos_for(g.y_for(pos)) - pos).abs() < 1e-5);
        }
        assert!(g.cap_rect(1.0).y >= g.area.y - 0.01);
        assert!(g.cap_rect(0.0).bottom() <= g.area.bottom() + 0.01);
    }

    #[test]
    fn controls_draw_something() {
        let theme = Theme::default();
        let mut p = RecordingPainter::new();
        knob(
            &mut p,
            Rect::new(0.0, 0.0, 30.0, 30.0),
            0.3,
            true,
            KnobLook {
                cap: theme.console.pan_cap,
                ring: Color::WHITE,
            },
            &theme,
        );
        led_button(
            &mut p,
            Rect::new(0.0, 0.0, 20.0, 14.0),
            "M",
            true,
            theme.console.led.mute,
            &theme,
        );
        meter(
            &mut p,
            Rect::new(0.0, 0.0, 12.0, 200.0),
            &[MeterLevel::new(-6.0, -3.0, false)],
            &theme,
        );
        // Every metering standard, with an RMS inside; the VU faces.
        for zones in [MeterZones::Digital, MeterZones::Ppm, MeterZones::K(-14.0)] {
            meter_zoned(
                &mut p,
                Rect::new(0.0, 0.0, 12.0, 200.0),
                &[MeterLevel {
                    inner_db: Some(-20.0),
                    ..MeterLevel::new(-6.0, -3.0, false)
                }],
                zones,
                &theme,
            );
        }
        vu_edgewise(
            &mut p,
            Rect::new(0.0, 0.0, 12.0, 200.0),
            &[(0.5, false)],
            &theme,
        );
        vu_arc(
            &mut p,
            Rect::new(0.0, 0.0, 90.0, 70.0),
            0.7,
            true,
            "Bass",
            &theme,
        );
        assert!(p.texts().contains(&"Bass"));
        assert!(p.ops.len() > 20);
        assert!(p.texts().contains(&"M"));
        assert!(p.balanced_clips());
    }

    /// Draws every control with `theme` (at sizes from tiny to large).
    fn every_control(theme: &Theme) -> RecordingPainter {
        let mut p = RecordingPainter::new();
        let c = &theme.console;
        for size in [3.0, 9.0, 18.0, 30.0, 46.0] {
            for (v, bipolar) in [(0.0, false), (0.5, true), (0.8, true), (1.0, false)] {
                knob(
                    &mut p,
                    Rect::new(0.0, 0.0, size, size),
                    v,
                    bipolar,
                    KnobLook {
                        cap: c.send_cap,
                        ring: c.send_cap,
                    },
                    theme,
                );
            }
        }
        for area in [
            Rect::new(0.0, 0.0, 60.0, 300.0),
            Rect::new(0.0, 0.0, 20.0, 40.0),
        ] {
            let geo = FaderGeometry::new(area, theme);
            for pos in [0.0, 0.7, 1.0] {
                fader(&mut p, &geo, pos, c.fader_cap_master, &[(0.7, "0")], theme);
            }
        }
        for on in [false, true] {
            led_button(
                &mut p,
                Rect::new(0.0, 0.0, 22.0, 16.0),
                "S",
                on,
                c.led.solo,
                theme,
            );
        }
        for rect in [
            Rect::new(0.0, 0.0, 12.0, 200.0),
            Rect::new(0.0, 0.0, 3.0, 8.0),
        ] {
            for zones in [MeterZones::Digital, MeterZones::Vu(-18.0)] {
                meter_zoned(
                    &mut p,
                    rect,
                    &[
                        MeterLevel::new(-80.0, -80.0, false),
                        MeterLevel {
                            inner_db: Some(-14.0),
                            ..MeterLevel::new(3.0, 4.0, true)
                        },
                    ],
                    zones,
                    theme,
                );
            }
        }
        panel(
            &mut p,
            Rect::new(0.0, 0.0, 92.0, 400.0),
            c.panel_top,
            c.panel_bottom,
            theme,
        );
        section_line(&mut p, 0.0, 90.0, 40.0, theme);
        scribble(
            &mut p,
            Rect::new(0.0, 0.0, 84.0, 20.0),
            "Bass",
            Color::hex(0xe0a34a),
            theme,
        );
        readout(&mut p, Rect::new(0.0, 0.0, 40.0, 14.0), "-3.0", theme);
        p
    }

    #[test]
    fn the_newer_looks_draw_their_own_controls() {
        let studio = every_control(&Theme::default());
        let lumen = every_control(&Theme::by_id("lumen"));
        assert!(lumen.balanced_clips() && studio.balanced_clips());
        assert_ne!(
            lumen.ops, studio.ops,
            "halo knobs, pills, planes, fine bars"
        );
        for p in [&studio, &lumen] {
            for t in ["S", "Bass", "-3.0", "0"] {
                assert!(p.texts().contains(&t), "{t}");
            }
        }
        // Planes have no screws or engraving shadow; the pill switch lights
        // with a glow (a shadow) only when on.
        let th = Theme::by_id("lumen");
        let mut off = RecordingPainter::new();
        led_button(
            &mut off,
            Rect::new(0.0, 0.0, 22.0, 16.0),
            "M",
            false,
            th.console.led.mute,
            &th,
        );
        assert!(!off.ops.iter().any(|o| matches!(o, DrawOp::Shadow(_))));
        let mut on = RecordingPainter::new();
        led_button(
            &mut on,
            Rect::new(0.0, 0.0, 22.0, 16.0),
            "M",
            true,
            th.console.led.mute,
            &th,
        );
        assert!(on.ops.iter().any(|o| matches!(o, DrawOp::Shadow(_))));
        let mut engraved_ops = RecordingPainter::new();
        engraved(
            &mut engraved_ops,
            "SENDS",
            Rect::new(0.0, 0.0, 80.0, 12.0),
            &th,
            Align::Center,
        );
        assert_eq!(engraved_ops.texts(), ["SENDS"]);
        // A silent gradient meter draws no bar and no bloom.
        let mut silent = RecordingPainter::new();
        meter(
            &mut silent,
            Rect::new(0.0, 0.0, 12.0, 200.0),
            &[MeterLevel::new(-150.0, -150.0, false)],
            &th,
        );
        assert!(!silent.ops.iter().any(|o| matches!(o, DrawOp::Shadow(_))));
    }
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    #[test]
    fn vu_deflection_is_proportional_to_voltage() {
        assert!((vu_deflection(3.0) - 1.0).abs() < 1e-6);
        // 0 VU sits at about 71 % of the swing, −10 VU near 22 %, −20 VU
        // at 7 %.
        assert!((vu_deflection(0.0) - 0.708).abs() < 0.002);
        assert!((vu_deflection(-10.0) - 0.224).abs() < 0.002);
        assert!((vu_deflection(-20.0) - 0.0708).abs() < 0.001);
        // The stops.
        assert!(vu_deflection(-90.0) < 1e-4);
        assert_eq!(vu_deflection(20.0), 1.06);
        assert_eq!(vu_scale(-18.0), vu_deflection(0.0));
    }

    #[test]
    fn the_standards_colour_where_they_should() {
        assert_eq!(MeterZones::Digital.bounds(), (-18.0, -6.0, -2.0));
        assert_eq!(MeterZones::Ppm.bounds(), (-18.0, -12.0, -9.0));
        assert_eq!(MeterZones::K(-14.0).bounds(), (-14.0, -10.0, -10.0));
        // A VU on LEDs: green below −3 VU, red from 0 VU; on its own
        // scale, 0 VU at 71 % of the height.
        let vu = MeterZones::Vu(-18.0);
        assert_eq!(vu.bounds(), (-21.0, -18.0, -18.0));
        assert!((vu.position(-18.0) - 0.708).abs() < 0.002);
        assert!((vu.level_at(vu.position(-24.0)) + 24.0).abs() < 0.01);
        assert!((MeterZones::Digital.level_at(0.5) + 20.0).abs() < 0.01);
    }
}
