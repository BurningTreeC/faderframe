//! Analogue-console-style controls: panels, knobs, faders, segmented
//! meters, LED buttons, scribble strips.
//!
//! Each control is a pure drawing function plus (where interactive) a small
//! geometry type for hit testing, so views stay in control of state and
//! interaction. Everything scales with the theme sizes; nothing assumes a
//! device-pixel ratio.

use crate::{
    Align, Color, FontFamily, FontWeight, Paint, Painter, Path, Point, Rect, TextStyle, Theme,
};
use std::f32::consts::PI;

/// Knob sweep: 270° starting at the lower left (135°, clockwise).
const KNOB_START: f32 = PI * 0.75;
const KNOB_SWEEP: f32 = PI * 1.5;

/// Normalised value change for a vertical drag of `dy` pixels.
/// Moving up increases the value; `fine` gives 5× more resolution.
pub fn drag_delta(dy: f32, fine: bool) -> f32 {
    -dy / if fine { 900.0 } else { 180.0 }
}

pub fn knob_angle(value: f32) -> f32 {
    KNOB_START + KNOB_SWEEP * value.clamp(0.0, 1.0)
}

/// Brushed-panel background with bevelled edges.
pub fn panel(p: &mut dyn Painter, rect: Rect, top: Color, bottom: Color, theme: &Theme) {
    let c = &theme.console;
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
    for (i, a) in [(0.18f32, 0.025f32), (0.42, 0.018), (0.71, 0.022)] {
        let y = rect.y + rect.h * i;
        p.fill(
            Rect::new(rect.x, y, rect.w, rect.h * 0.06),
            Color::rgba(1.0, 1.0, 1.0, a),
        );
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

/// Thin engraved divider between console sections.
pub fn section_line(p: &mut dyn Painter, x0: f32, x1: f32, y: f32, theme: &Theme) {
    p.hline(x0, x1, y, theme.console.section_line);
    p.hline(x0, x1, y + 1.0, theme.console.panel_edge_light);
}

/// Small-caps style legend with an engraved shadow.
pub fn engraved(p: &mut dyn Painter, text: &str, rect: Rect, theme: &Theme, align: Align) {
    let style = TextStyle::new(theme.fonts.tiny, theme.console.panel_label)
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold)
        .align(align)
        .tracking(0.6);
    p.text(
        text,
        rect.translate(0.0, 1.0),
        &style.color(Color::rgba(0.0, 0.0, 0.0, 0.55)),
    );
    p.text(text, rect, &style);
}

/// Recessed well (insert slots, readouts). Returns the inner rect.
pub fn well(p: &mut dyn Painter, rect: Rect, theme: &Theme) -> Rect {
    let c = &theme.console;
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

/// A rotary control with an LED-ring style value arc.
pub fn knob(
    p: &mut dyn Painter,
    rect: Rect,
    value: f32,
    bipolar: bool,
    look: KnobLook,
    theme: &Theme,
) {
    let k = &theme.console.knob;
    let c = rect.center();
    let r = rect.w.min(rect.h) * 0.5;
    if r < 4.0 {
        return;
    }
    let ring_r = r - 1.6;
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

    let body_r = r - 4.6;
    let body = Rect::new(c.x - body_r, c.y - body_r, body_r * 2.0, body_r * 2.0);
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
    let cap_r = body_r * 0.74;
    let cap = Rect::new(c.x - cap_r, c.y - cap_r, cap_r * 2.0, cap_r * 2.0);
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
    let a = knob_angle(value);
    let (s, co) = (a.sin(), a.cos());
    p.line(
        Point::new(c.x + co * cap_r * 0.2, c.y + s * cap_r * 0.2),
        Point::new(c.x + co * (body_r - 1.2), c.y + s * (body_r - 1.2)),
        2.0,
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
    pub level_db: f32,
    pub hold_db: f32,
    pub clipped: bool,
}

/// Segmented LED-ladder meter with peak hold and clip indicators.
pub fn meter(p: &mut dyn Painter, rect: Rect, levels: &[MeterLevel], theme: &Theme) {
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
    let zone = |norm: f32| -> Color {
        if norm >= meter_scale(-2.0) {
            m.red
        } else if norm >= meter_scale(-6.0) {
            m.orange
        } else if norm >= meter_scale(-18.0) {
            m.yellow
        } else {
            m.green
        }
    };
    for (i, lv) in levels.iter().enumerate() {
        let x = inner.x + i as f32 * (col_w + col_gap);
        let lit = meter_scale(lv.level_db);
        let hold = meter_scale(lv.hold_db);
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
        let hold_seg = ((hold * segments as f32).ceil() as usize).min(segments);
        for s in 0..segments {
            let norm = (s as f32 + 0.5) / segments as f32;
            let y = ladder.bottom() - (s as f32 + 1.0) * pitch + m.gap;
            let base = zone(norm);
            let on = norm <= lit;
            let is_hold = hold_seg > 0 && s + 1 == hold_seg && hold > 0.01;
            let color = if on || is_hold {
                base
            } else {
                base.mix(m.background, 1.0 - m.unlit)
            };
            p.fill(Rect::new(x, y, col_w, m.segment), color);
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

/// Cream "tape" label with a track-colour stripe, like console scribble strips.
pub fn scribble(p: &mut dyn Painter, rect: Rect, text: &str, stripe: Color, theme: &Theme) {
    let c = &theme.console;
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
        .family(FontFamily::Mono)
        .center();
    p.text(text, inner, &style);
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
    use crate::RecordingPainter;

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
            &[MeterLevel {
                level_db: -6.0,
                hold_db: -3.0,
                clipped: false,
            }],
            &theme,
        );
        assert!(p.ops.len() > 20);
        assert!(p.texts().contains(&"M"));
        assert!(p.balanced_clips());
    }
}
