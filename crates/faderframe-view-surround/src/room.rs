//! The room seen from above: a bed's speakers lit by what they play, where
//! a track sits, its spread and its height — large in the panner view,
//! small in a mixer strip.

use faderframe_core::{ChannelLayout, SurroundFormat, SurroundPan};
use faderframe_ui_canvas::{Align, Color, Painter, Point, Rect, TextStyle, Theme};

/// A place (`x` −1 left … 1 right, `y` −1 back … 1 front) in `room`.
pub fn to_view(room: Rect, x: f32, y: f32) -> Point {
    Point::new(
        room.x + (x.clamp(-1.0, 1.0) + 1.0) * 0.5 * room.w,
        room.y + (1.0 - y.clamp(-1.0, 1.0)) * 0.5 * room.h,
    )
}

/// The place under `p` (clamped to the room).
pub fn from_view(room: Rect, p: Point) -> (f32, f32) {
    let x = ((p.x - room.x) / room.w.max(1.0)) * 2.0 - 1.0;
    let y = 1.0 - ((p.y - room.y) / room.h.max(1.0)) * 2.0;
    (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0))
}

/// Where a source's channels sit (stereo: `width` either side of the
/// position, as the panner places them).
pub fn sources(pan: &SurroundPan, layout: ChannelLayout) -> Vec<(f32, f32)> {
    if layout == ChannelLayout::Stereo {
        vec![
            ((pan.x - pan.width).clamp(-1.0, 1.0), pan.y),
            ((pan.x + pan.width).clamp(-1.0, 1.0), pan.y),
        ]
    } else {
        vec![(pan.x, pan.y)]
    }
}

/// The floor inside a room's `rect`: positions map onto it, the speakers
/// stand just outside it.
pub fn floor(rect: Rect, compact: bool) -> Rect {
    rect.inset(if compact { 3.5 } else { 30.0 })
}

/// A speaker's icon: floor speakers just outside the walls (a puck on a
/// wall leaves them visible), the heights inside (overhead).
fn speaker_at(floor: Rect, s: &faderframe_core::surround::Speaker, out: f32) -> Point {
    if s.z > 0.0 {
        return to_view(floor, s.x * 0.6, s.y * 0.6);
    }
    let p = to_view(floor, s.x, s.y);
    let (dx, dy) = (
        s.x.signum() * (s.x.abs() > 0.5) as i32 as f32,
        -s.y.signum() * (s.y.abs() > 0.5) as i32 as f32,
    );
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    Point::new(p.x + dx / len * out, p.y + dy / len * out)
}

/// How lit a speaker is by its meter level (dBFS).
fn lit(db: f32) -> f32 {
    ((db + 54.0) / 54.0).clamp(0.0, 1.0)
}

/// What to draw.
pub struct Room<'a> {
    pub format: SurroundFormat,
    pub source: ChannelLayout,
    pub pan: SurroundPan,
    /// Each speaker's level (dBFS) in channel order; empty: unlit.
    pub levels: &'a [f32],
    pub puck: Color,
    /// The mixer's small one: no labels, no LFE.
    pub compact: bool,
}

impl Room<'_> {
    /// Paint the room into `rect` (its floor is [`floor`]).
    pub fn paint(&self, p: &mut dyn Painter, rect: Rect, th: &Theme) {
        let room = floor(rect, self.compact);
        let ground = th.ui.lcd_bg;
        let line = th.ui.lcd_dim;
        let bright = th.ui.lcd_text;
        let radius = if self.compact { 2.0 } else { 6.0 };
        p.fill_rounded(room, radius, &ground.into());
        p.stroke_rounded(room, radius, 1.0, line.with_alpha(0.7));
        // The listener and the axes.
        let c = room.center();
        p.hline(room.x + 2.0, room.right() - 2.0, c.y, line.with_alpha(0.35));
        p.vline(
            c.x,
            room.y + 2.0,
            room.bottom() - 2.0,
            line.with_alpha(0.35),
        );
        if !self.compact {
            let mut ring = faderframe_ui_canvas::Path::new();
            ring.arc(c, room.w * 0.31, 0.0, std::f32::consts::TAU, false);
            p.stroke_path(&ring, 1.0, line.with_alpha(0.3));
            p.circle(c, 3.0, line.with_alpha(0.6));
            let dim = TextStyle::new(th.fonts.tiny, line.mix(bright, 0.25)).align(Align::Center);
            p.text("FRONT", Rect::new(room.x, room.y + 6.0, room.w, 12.0), &dim);
            p.text(
                "BACK",
                Rect::new(room.x, room.bottom() - 18.0, room.w, 12.0),
                &dim,
            );
        }
        // The speakers, lit by what they play.
        let dot = if self.compact { 1.8 } else { 7.0 };
        let out = if self.compact { 1.5 } else { dot + 5.0 };
        for (i, s) in self.format.speakers().iter().enumerate() {
            if s.lfe {
                continue;
            }
            let at = speaker_at(room, s, out);
            let l = self.levels.get(i).map_or(0.0, |&db| lit(db));
            let col = line.mix(bright, 0.25 + 0.75 * l);
            let r = Rect::new(at.x - dot, at.y - dot, dot * 2.0, dot * 2.0);
            if s.z > 0.0 {
                p.stroke_rounded(r, dot * 0.4, if self.compact { 1.0 } else { 1.5 }, col);
            } else {
                p.fill_rounded(r, dot * 0.4, &col.into());
            }
            if !self.compact {
                // The label outside the icon (beside it for side speakers).
                let (lx, ly) = if s.z > 0.0 {
                    (at.x - 24.0, at.y + dot + 2.0)
                } else if s.y.abs() <= 0.5 {
                    (at.x + s.x.signum() * (dot + 26.0) - 24.0, at.y - 6.0)
                } else if s.y > 0.0 {
                    (at.x - 24.0, at.y - dot - 15.0)
                } else {
                    (at.x - 24.0, at.y + dot + 3.0)
                };
                p.text(
                    s.label,
                    Rect::new(lx, ly, 48.0, 12.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_dim).align(Align::Center),
                );
            }
        }
        if !self.compact && self.format.has_lfe() {
            let i = self.format.speakers().iter().position(|s| s.lfe);
            let l = i
                .and_then(|i| self.levels.get(i))
                .map_or(0.0, |&db| lit(db));
            let r = Rect::new(c.x - 17.0, room.bottom() + 18.0, 34.0, 12.0);
            p.stroke_rounded(r, 3.0, 1.0, line.mix(bright, 0.25 + 0.75 * l));
            p.text(
                "LFE",
                r,
                &TextStyle::new(th.fonts.tiny, line.mix(bright, 0.25 + 0.75 * l))
                    .align(Align::Center),
            );
        }
        // The track: spread as a halo, height as the puck's size.
        let places = sources(&self.pan, self.source);
        let unit = room.w.min(room.h) * 0.5;
        if self.pan.spread > 0.0 {
            let at = to_view(room, self.pan.x, self.pan.y);
            p.circle(
                at,
                (self.pan.spread * unit * 1.4).max(3.0),
                self.puck.with_alpha(0.16),
            );
        }
        let size = if self.compact { 2.6 } else { 9.0 } * (1.0 + self.pan.z * 0.45);
        if places.len() == 2 {
            let (a, b) = (
                to_view(room, places[0].0, places[0].1),
                to_view(room, places[1].0, places[1].1),
            );
            p.line(
                a,
                b,
                if self.compact { 1.0 } else { 2.0 },
                self.puck.with_alpha(0.6),
            );
        }
        for (i, &(x, y)) in places.iter().enumerate() {
            let at = to_view(room, x, y);
            p.circle(at, size + 1.5, ground);
            p.circle(at, size, self.puck);
            if self.pan.z > 0.0 && !self.compact {
                let mut ring = faderframe_ui_canvas::Path::new();
                ring.arc(at, size + 4.0, 0.0, std::f32::consts::TAU, false);
                p.stroke_path(&ring, 1.0, self.puck.with_alpha(0.7));
            }
            if !self.compact && places.len() == 2 {
                p.text(
                    if i == 0 { "L" } else { "R" },
                    Rect::new(at.x - size, at.y - size, size * 2.0, size * 2.0),
                    &TextStyle::new(th.fonts.tiny, ground)
                        .bold()
                        .align(Align::Center),
                );
            }
        }
        if places.len() == 2 && !self.compact {
            let at = to_view(room, self.pan.x, self.pan.y);
            p.circle(at, 3.5, self.puck);
        }
    }
}

/// A short readout of a position: `C`, `L40`, `R100` across, `F100`
/// (front) … `B100` (back) along.
pub fn format_place(pan: &SurroundPan) -> String {
    let across = if pan.x.abs() < 0.005 {
        "C".to_string()
    } else {
        format!(
            "{}{:.0}",
            if pan.x < 0.0 { "L" } else { "R" },
            pan.x.abs() * 100.0
        )
    };
    let along = if pan.y.abs() < 0.005 {
        "M".to_string()
    } else {
        format!(
            "{}{:.0}",
            if pan.y > 0.0 { "F" } else { "B" },
            pan.y.abs() * 100.0
        )
    };
    format!("{across} {along}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_round_trip_through_the_drawing() {
        let room = Rect::new(10.0, 20.0, 200.0, 200.0);
        for (x, y) in [(-1.0, 1.0), (0.0, 0.0), (0.5, -0.25), (1.0, -1.0)] {
            let p = to_view(room, x, y);
            let (bx, by) = from_view(room, p);
            assert!((bx - x).abs() < 1e-5 && (by - y).abs() < 1e-5);
        }
        // Front is up, left is left.
        assert_eq!(to_view(room, -1.0, 1.0), Point::new(10.0, 20.0));
        assert_eq!(from_view(room, Point::new(-50.0, 900.0)), (-1.0, -1.0));
    }

    #[test]
    fn a_stereo_source_sits_width_apart() {
        let pan = SurroundPan {
            x: 0.2,
            width: 0.5,
            ..SurroundPan::default()
        };
        let s = sources(&pan, ChannelLayout::Stereo);
        assert!((s[0].0 + 0.3).abs() < 1e-6 && (s[1].0 - 0.7).abs() < 1e-6);
        assert_eq!(sources(&pan, ChannelLayout::Mono).len(), 1);
        assert_eq!(format_place(&SurroundPan::default()), "C F100");
        assert_eq!(
            format_place(&SurroundPan {
                x: -0.4,
                y: -1.0,
                ..SurroundPan::default()
            }),
            "L40 B100"
        );
    }
}
