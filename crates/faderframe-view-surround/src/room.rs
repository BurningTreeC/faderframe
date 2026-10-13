//! The room seen from above: a bed's speakers lit by what they play, where
//! a track sits, its spread and its height — large in the panner view,
//! small in a mixer strip. The room panner draws the room box (positions
//! `x`, `y`); VBAP draws the circle of directions around the listener
//! (front up, left on the left; speakers above ear level and a source's
//! elevation closer to the middle, by the cosine of their elevation).

use faderframe_core::surround::{StereoSource, SurroundPanner};
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

/// The circle of directions inside a room's floor: its centre and radius.
pub fn circle(floor: Rect) -> (Point, f32) {
    (floor.center(), floor.w.min(floor.h) * 0.5 * 0.82)
}

/// A direction (degrees; azimuth positive to the left) on the circle.
pub fn direction_to_view(floor: Rect, azimuth: f32, elevation: f32) -> Point {
    let (c, r) = circle(floor);
    let v = faderframe_core::vbap::unit(azimuth, elevation);
    Point::new(c.x + v[0] * r, c.y - v[1] * r)
}

/// The azimuth (degrees, −180…180) of the point `p` seen from the
/// listener in the middle.
pub fn azimuth_at(floor: Rect, p: Point) -> f32 {
    let c = floor.center();
    let (right, front) = (p.x - c.x, c.y - p.y);
    if right.abs() < 1e-6 && front.abs() < 1e-6 {
        return 0.0;
    }
    (-right).atan2(front).to_degrees()
}

/// Whether `pan` is drawn (and dragged) as directions: VBAP, unless the
/// track is an object (objects are placed in the room).
pub fn by_direction(pan: &SurroundPan, object: bool) -> bool {
    pan.panner == SurroundPanner::Vbap && !object
}

/// How a stereo `layout` source goes in by `pan` (mono: as an image of one).
pub fn stereo_mode(pan: &SurroundPan, layout: ChannelLayout, object: bool) -> StereoSource {
    if layout != ChannelLayout::Stereo || object {
        return StereoSource::Image;
    }
    match pan.stereo {
        StereoSource::Dual if !by_direction(pan, object) => StereoSource::Image,
        m => m,
    }
}

/// Where a source's channels sit (stereo: `width` either side of the
/// position, as the room panner places them).
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

/// The directions (azimuth, elevation) a source's channels come from by
/// VBAP: an image `30° × width` either side of its azimuth, dual pan's own
/// two, or one.
pub fn directions(pan: &SurroundPan, layout: ChannelLayout) -> Vec<(f32, f32)> {
    match stereo_mode(pan, layout, false) {
        _ if layout != ChannelLayout::Stereo => vec![(pan.azimuth, pan.elevation)],
        StereoSource::Dual => vec![(pan.left, pan.elevation), (pan.right, pan.elevation)],
        _ => vec![
            (pan.azimuth + 30.0 * pan.width, pan.elevation),
            (pan.azimuth - 30.0 * pan.width, pan.elevation),
        ],
    }
}

/// The floor inside a room's `rect`: positions map onto it, the speakers
/// stand just outside it.
pub fn floor(rect: Rect, compact: bool) -> Rect {
    rect.inset(if compact { 3.5 } else { 30.0 })
}

/// A speaker's icon in the room box: floor speakers just outside the walls
/// (a puck on a wall leaves them visible), the heights inside (overhead).
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
    /// An object: its puck is square (and it is placed in the room).
    pub object: bool,
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
        let circular = by_direction(&self.pan, self.object);
        let mode = stereo_mode(&self.pan, self.source, self.object);
        // The listener and the axes.
        let c = room.center();
        p.hline(room.x + 2.0, room.right() - 2.0, c.y, line.with_alpha(0.35));
        p.vline(
            c.x,
            room.y + 2.0,
            room.bottom() - 2.0,
            line.with_alpha(0.35),
        );
        if circular {
            let (c, r) = circle(room);
            let mut ring = faderframe_ui_canvas::Path::new();
            ring.arc(c, r, 0.0, std::f32::consts::TAU, false);
            p.stroke_path(&ring, 1.0, line.with_alpha(0.6));
        } else if !self.compact {
            let mut ring = faderframe_ui_canvas::Path::new();
            ring.arc(c, room.w * 0.31, 0.0, std::f32::consts::TAU, false);
            p.stroke_path(&ring, 1.0, line.with_alpha(0.3));
        }
        if !self.compact {
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
            let at = if circular {
                direction_to_view(room, s.azimuth, s.elevation)
            } else {
                speaker_at(room, s, out)
            };
            let l = self.levels.get(i).map_or(0.0, |&db| lit(db));
            let col = line.mix(bright, 0.25 + 0.75 * l);
            let r = Rect::new(at.x - dot, at.y - dot, dot * 2.0, dot * 2.0);
            if s.z > 0.0 {
                p.stroke_rounded(r, dot * 0.4, if self.compact { 1.0 } else { 1.5 }, col);
            } else if s.elevation < 0.0 {
                // Below ear level: a ring.
                let mut ring = faderframe_ui_canvas::Path::new();
                ring.arc(at, dot * 0.8, 0.0, std::f32::consts::TAU, false);
                p.stroke_path(&ring, if self.compact { 1.0 } else { 1.5 }, col);
            } else {
                p.fill_rounded(r, dot * 0.4, &col.into());
            }
            if !self.compact {
                // The label outside the icon (beside it for side speakers).
                let (lx, ly) = if circular {
                    let v = faderframe_core::vbap::unit(s.azimuth, 0.0);
                    let k = if s.z > 0.0 { -1.0 } else { 1.0 };
                    (
                        at.x + v[0] * (dot + 14.0) * k - 24.0,
                        at.y - v[1] * (dot + 12.0) * k - 6.0,
                    )
                } else if s.z > 0.0 {
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
        if mode == StereoSource::Balance {
            // Not placed: the left and right channels on L and R.
            if !self.compact {
                p.text(
                    "Balanced on L and R",
                    Rect::new(room.x, c.y + 8.0, room.w, 14.0),
                    &TextStyle::new(th.fonts.small, line.mix(bright, 0.6)).align(Align::Center),
                );
            }
            return;
        }
        // The track: spread as a halo, height as the puck's size (room) or
        // its distance from the circle (VBAP).
        let places: Vec<Point> = if circular {
            directions(&self.pan, self.source)
                .into_iter()
                .map(|(a, e)| direction_to_view(room, a, e))
                .collect()
        } else {
            sources(&self.pan, self.source)
                .into_iter()
                .map(|(x, y)| to_view(room, x, y))
                .collect()
        };
        let centre = if circular {
            direction_to_view(room, self.pan.azimuth, self.pan.elevation)
        } else {
            to_view(room, self.pan.x, self.pan.y)
        };
        let unit = room.w.min(room.h) * 0.5;
        if self.pan.spread > 0.0 {
            // Around the position (dual pan has none: around each channel).
            let halos: &[Point] = if mode == StereoSource::Dual {
                &places
            } else {
                std::slice::from_ref(&centre)
            };
            // Inside the room (a wide spread fills it, never more); a
            // diffuse one with a rim.
            p.push_clip(room);
            for &at in halos {
                let r = (self.pan.spread * unit * 1.4).max(3.0);
                p.circle(at, r, self.puck.with_alpha(0.16));
                if self.pan.diffuse && !self.compact {
                    let mut rim = faderframe_ui_canvas::Path::new();
                    rim.arc(at, r, 0.0, std::f32::consts::TAU, false);
                    p.stroke_path(&rim, 1.0, self.puck.with_alpha(0.5));
                }
            }
            p.pop_clip();
        }
        let lift = if circular { 0.0 } else { self.pan.z };
        let size = if self.compact { 2.6 } else { 9.0 } * (1.0 + lift * 0.45);
        if places.len() == 2 && mode == StereoSource::Image {
            p.line(
                places[0],
                places[1],
                if self.compact { 1.0 } else { 2.0 },
                self.puck.with_alpha(0.6),
            );
        }
        for (i, &at) in places.iter().enumerate() {
            if self.object {
                let r = |d: f32| Rect::new(at.x - d, at.y - d, d * 2.0, d * 2.0);
                p.fill_rounded(r(size + 1.5), 2.0, &ground.into());
                p.fill_rounded(r(size), 1.5, &self.puck.into());
            } else {
                p.circle(at, size + 1.5, ground);
                p.circle(at, size, self.puck);
            }
            if lift > 0.0 && !self.compact {
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
        if places.len() == 2 && mode == StereoSource::Image && !self.compact {
            p.circle(centre, 3.5, self.puck);
        }
    }
}

/// A direction as a short readout: `C` ahead, `L40°`/`R110°` either side,
/// `180°` behind; above ear level `↑30°`.
pub fn format_direction(azimuth: f32, elevation: f32) -> String {
    let a = azimuth.round();
    let across = if a == 0.0 {
        "C".to_string()
    } else if a.abs() >= 180.0 {
        "180°".to_string()
    } else if a > 0.0 {
        format!("L{a:.0}°")
    } else {
        format!("R{:.0}°", -a)
    };
    let e = elevation.round();
    if e > 0.0 {
        format!("{across} ↑{e:.0}°")
    } else if e < 0.0 {
        format!("{across} ↓{:.0}°", -e)
    } else {
        across
    }
}

/// A short readout of a position: room: `C`, `L40`, `R100` across, `F100`
/// (front) … `B100` (back) along; VBAP: the direction
/// ([`format_direction`]), dual pan's two; balanced: `L/R`.
pub fn format_place(pan: &SurroundPan) -> String {
    if pan.panner == SurroundPanner::Vbap {
        return match pan.stereo {
            StereoSource::Dual => format!(
                "{} · {}",
                format_direction(pan.left, pan.elevation),
                format_direction(pan.right, pan.elevation)
            ),
            _ => format_direction(pan.azimuth, pan.elevation),
        };
    }
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
    fn directions_round_trip_through_the_circle() {
        let floor = Rect::new(0.0, 0.0, 200.0, 200.0);
        for az in [0.0f32, 30.0, -30.0, 90.0, -110.0, 135.0, 179.0] {
            let p = direction_to_view(floor, az, 0.0);
            assert!((azimuth_at(floor, p) - az).abs() < 1e-3, "{az}");
        }
        // Ahead is up, left (positive) is left.
        let ahead = direction_to_view(floor, 0.0, 0.0);
        let left = direction_to_view(floor, 90.0, 0.0);
        assert!(ahead.y < 100.0 && (ahead.x - 100.0).abs() < 1e-3);
        assert!(left.x < 100.0 && (left.y - 100.0).abs() < 1e-3);
        // Overhead is the middle.
        let up = direction_to_view(floor, 40.0, 90.0);
        assert!((up.x - 100.0).abs() < 1e-3 && (up.y - 100.0).abs() < 1e-3);
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
        // By direction: 30° × width either side.
        let v = SurroundPan {
            panner: SurroundPanner::Vbap,
            azimuth: 10.0,
            width: 0.5,
            ..SurroundPan::default()
        };
        assert_eq!(
            directions(&v, ChannelLayout::Stereo),
            [(25.0, 0.0), (-5.0, 0.0)]
        );
        assert_eq!(format_place(&v), "L10°");
        assert_eq!(format_direction(-110.0, 0.0), "R110°");
        assert_eq!(format_direction(180.0, 30.0), "180° ↑30°");
        assert_eq!(format_direction(45.0, -40.0), "L45° ↓40°");
        let dual = SurroundPan {
            stereo: StereoSource::Dual,
            left: 110.0,
            right: -110.0,
            ..v
        };
        assert_eq!(
            directions(&dual, ChannelLayout::Stereo),
            [(110.0, 0.0), (-110.0, 0.0)]
        );
        assert_eq!(format_place(&dual), "L110° · R110°");
    }
}
