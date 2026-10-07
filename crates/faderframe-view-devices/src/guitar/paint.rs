//! The Guitar Station drawn: a pedalboard, an amplifier's head, a cabinet
//! with two microphones and an output strip, lit like the Program EQ.

use super::layout::{
    self, AmpLook, CONE_R, CONE_X, CONE_Y, CabLook, Finish, KnobAt, Look, OutLook, PEDAL_H,
    PEDAL_Y, PedalLook, SWITCH_R, Target,
};
use super::{AMP_Y, BOARD_H, BOARD_Y, CAB_H, CAB_Y, HEADER_H, OUT_Y, PANEL_W};
use crate::program_eq::{
    KNOB_FRAMES, KNOB_LARGE, KNOB_LARGE_DRAW, KNOB_LARGE_SPAN, LAMP_DARK, LAMP_DRAW, LAMP_LIT,
    LAMP_SPAN, Needle, SCREWS, contact_shadow, deflection, linear, polar, radial, rgb, rgba,
    sprite, whole,
};
use faderframe_guitar::acoustics::cabinet::CabinetProfile;
use faderframe_guitar::chain::{amp_name, power_name};
use faderframe_guitar::lists;
use faderframe_guitar::pedal::Stomp;
use faderframe_guitar::voice::{CabinetChoice, Pedal};
use faderframe_plugin_host::devices::guitar::{id, value};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_ui_canvas::{
    Color, FontFamily, FontWeight, Paint, Painter, Path, Point, Rect, TextStyle,
};

/// What a frame paints.
pub struct Scene<'a> {
    pub look: &'a Look,
    pub tap: &'a AnalysisTap,
    pub hover: Option<Target>,
    /// A pedal being carried: its place and its left edge now.
    pub dragging: Option<(usize, f32)>,
    pub rate: f32,
}

impl Scene<'_> {
    fn get(&self, id: u32) -> f64 {
        f64::from(self.tap.params.get(id as usize))
    }

    fn norm(&self, id: u32) -> f32 {
        let Some(info) = self.tap.params.infos().get(id as usize) else {
            return 0.0;
        };
        if info.max <= info.min {
            return 0.0;
        }
        ((self.get(id) - info.min) / (info.max - info.min)).clamp(0.0, 1.0) as f32
    }

    fn hovered(&self, t: Target) -> bool {
        self.hover == Some(t)
    }

    fn knob_hovered(&self, id: u32) -> bool {
        matches!(self.hover, Some(Target::Knob { id: h, .. }) if h == id)
    }

    fn place_hovered(&self, place: usize) -> bool {
        matches!(
            self.hover,
            Some(
                Target::PedalBody { place: p }
                    | Target::StompPlate { place: p }
                    | Target::Footswitch { place: p }
                    | Target::Remove { place: p }
                    | Target::Treadle { place: p, .. }
            ) if p == place
        )
    }
}

// --- text ---------------------------------------------------------------------------

fn style(size: f32, color: Color) -> TextStyle {
    TextStyle::new(size, color)
        .weight(FontWeight::Bold)
        .center()
}

/// Lettering with a shadow below it (engraved or printed on a lit panel).
fn letters(p: &mut dyn Painter, text: &str, r: Rect, s: TextStyle, shadow: f32) {
    if shadow > 0.0 {
        p.text(text, r.translate(0.0, 1.0), &s.color(rgba(0, shadow)));
    }
    p.text(text, r, &s);
}

// --- shared hardware ----------------------------------------------------------------------

/// A knob from the Program EQ's render, at `normalised` travel.
fn knob(p: &mut dyn Painter, at: Point, r: f32, normalised: f32, lit: f32) {
    let frame = (normalised.clamp(0.0, 1.0) * (KNOB_FRAMES - 1) as f32).round() as usize;
    contact_shadow(p, at.x, at.y, r * KNOB_LARGE_DRAW / 2.0);
    let cell = KNOB_LARGE.height as f32 / KNOB_FRAMES as f32;
    let src = Rect::new(0.0, frame as f32 * cell, KNOB_LARGE.width as f32, cell);
    sprite(
        p,
        &KNOB_LARGE,
        src,
        at.x,
        at.y,
        r * KNOB_LARGE_DRAW / KNOB_LARGE_SPAN,
        lit,
    );
}

fn hover_ring(p: &mut dyn Painter, at: Point, r: f32, color: Color) {
    p.fill_path_paint(
        &Path::circle(at, r * 1.55),
        &radial(
            at.x,
            at.y,
            r * 0.9,
            r * 1.55,
            color.with_alpha(0.22),
            color.with_alpha(0.0),
        ),
    );
}

fn screw(p: &mut dyn Painter, i: usize, at: Point, size: f32) {
    let s = &SCREWS[i % SCREWS.len()];
    sprite(p, s, whole(s), at.x, at.y, size, 0.95);
}

fn lamp(p: &mut dyn Painter, at: Point, r: f32, lit: bool, glow: Color) {
    let body = r * LAMP_DRAW / 2.0;
    if lit {
        p.fill_path_paint(
            &Path::circle(at, body * 2.6),
            &radial(
                at.x,
                at.y,
                body * 0.8,
                body * 2.6,
                glow.with_alpha(0.34),
                glow.with_alpha(0.0),
            ),
        );
    }
    contact_shadow(p, at.x, at.y, body);
    let image = if lit { &LAMP_LIT } else { &LAMP_DARK };
    sprite(
        p,
        image,
        whole(image),
        at.x,
        at.y,
        r * LAMP_DRAW / LAMP_SPAN,
        1.0,
    );
}

/// A small LED: red when lit.
fn led(p: &mut dyn Painter, at: Point, lit: bool) {
    if lit {
        p.fill_path_paint(
            &Path::circle(at, 14.0),
            &radial(
                at.x,
                at.y,
                2.0,
                14.0,
                rgba(0xff2a1a, 0.55),
                rgba(0xff2a1a, 0.0),
            ),
        );
    }
    p.circle(at, 5.2, rgba(0, 0.6));
    p.fill_path_paint(
        &Path::circle(at, 4.2),
        &radial(
            at.x - 1.2,
            at.y - 1.4,
            0.0,
            4.6,
            if lit { rgb(0xffd2c2) } else { rgb(0x6b1d18) },
            if lit { rgb(0xe8261a) } else { rgb(0x2a0a08) },
        ),
    );
}

/// A chrome stomp switch.
fn footswitch(p: &mut dyn Painter, at: Point, pressed: bool) {
    let r = SWITCH_R;
    contact_shadow(p, at.x, at.y, r * 1.1);
    // The hex nut.
    let mut nut = Path::new();
    for k in 0..6 {
        let a = std::f32::consts::FRAC_PI_3 * k as f32 + 0.2;
        let q = Point::new(at.x + (r + 4.0) * a.cos(), at.y + (r + 4.0) * a.sin());
        if k == 0 {
            nut.move_to(q);
        } else {
            nut.line_to(q);
        }
    }
    nut.close();
    p.fill_path_paint(
        &nut,
        &linear(
            at.x - r,
            at.y - r,
            at.x + r,
            at.y + r,
            vec![(0.0, rgb(0xd5d8db)), (1.0, rgb(0x5e6266))],
        ),
    );
    let top = if pressed { 0xb9bcbf } else { 0xf4f5f6 };
    p.fill_path_paint(
        &Path::circle(at, r),
        &radial(
            at.x - r * 0.35,
            at.y - r * 0.45,
            0.0,
            r * 1.25,
            rgb(top),
            rgb(0x6f7378),
        ),
    );
    p.fill_path_paint(
        &Path::circle(at, r * 0.62),
        &radial(
            at.x + r * 0.1,
            at.y + r * 0.15,
            0.0,
            r * 0.7,
            rgb(0x8c9095),
            rgb(0xe9ebed),
        ),
    );
    p.stroke_path(&Path::circle(at, r), 1.0, rgba(0, 0.45));
}

/// A recessed plate holding a choice (a menu).
fn plate(p: &mut dyn Painter, r: Rect, text: &str, hovered: bool, accent: Color) {
    p.fill_rounded(
        r,
        4.0,
        &linear(
            r.x,
            r.y,
            r.x,
            r.bottom(),
            vec![(0.0, rgb(0x0b0c0d)), (1.0, rgb(0x1a1c1f))],
        ),
    );
    p.stroke_rounded(
        r,
        4.0,
        1.0,
        if hovered {
            accent.with_alpha(0.8)
        } else {
            rgba(0xffffff, 0.12)
        },
    );
    let s = TextStyle::new(10.5, rgb(0xf3e6c4))
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold);
    p.text(text, r.inset_xy(8.0, 0.0), &s);
    p.text(
        "▾",
        Rect::new(r.right() - 16.0, r.y, 12.0, r.h),
        &s.color(rgba(0xf3e6c4, 0.6)),
    );
}

/// A row of segments, the current one lit.
fn segments(p: &mut dyn Painter, rects: &[Rect], labels: &[&str], current: usize, accent: Color) {
    for (i, (r, label)) in rects.iter().zip(labels).enumerate() {
        let on = i == current;
        p.fill_rounded(
            *r,
            3.0,
            &Paint::Solid(if on {
                accent.with_alpha(0.32)
            } else {
                rgb(0x111316)
            }),
        );
        p.stroke_rounded(
            *r,
            3.0,
            1.0,
            if on {
                accent.with_alpha(0.85)
            } else {
                rgba(0xffffff, 0.09)
            },
        );
        p.text(
            label,
            *r,
            &style(9.5, if on { rgb(0xf6f1e4) } else { rgb(0x8f979e) }),
        );
    }
}

const ACCENT: u32 = 0xf2a03d;
const MIC_A: u32 = 0xf2a03d;
const MIC_B: u32 = 0x56b3e8;

// --- the whole panel ----------------------------------------------------------------------

pub fn all(p: &mut dyn Painter, sc: &Scene<'_>, needles: &mut [[Needle; 2]; 2], dt: f32) {
    header(p, sc);
    board(p, sc);
    amp(p, sc);
    cab(p, sc);
    out(p, sc, needles, dt);
}

fn header(p: &mut dyn Painter, sc: &Scene<'_>) {
    let bar = Rect::new(0.0, -HEADER_H, PANEL_W, HEADER_H);
    p.fill_rect(
        bar,
        &linear(
            0.0,
            bar.y,
            0.0,
            bar.bottom(),
            vec![(0.0, rgb(0x1f2226)), (1.0, rgb(0x0d0f12))],
        ),
    );
    p.fill(Rect::new(0.0, -0.5, PANEL_W, 0.5), rgba(0, 0.8));
    let name = TextStyle::new(10.0, rgb(0xa8b4be))
        .weight(FontWeight::Bold)
        .tracking(1.6);
    p.text(
        "FADERFRAME GUITAR STATION",
        Rect::new(16.0, bar.y, 320.0, bar.h),
        &name,
    );
    let rects = layout::quality_rects();
    let caption = Rect::new(rects[0].x - 100.0, bar.y, 90.0, bar.h);
    p.text(
        "QUALITY",
        caption,
        &TextStyle::new(9.0, rgb(0xa8b4be))
            .weight(FontWeight::Bold)
            .tracking(1.2)
            .right(),
    );
    let labels: Vec<&str> = lists::QUALITY.iter().map(|q| q.0).collect();
    segments(
        p,
        &rects,
        &labels,
        sc.get(id::QUALITY).round() as usize,
        rgb(ACCENT),
    );
}

// --- the pedalboard ------------------------------------------------------------------------

/// What a pedal's box looks like: its body, its lettering, the colour of
/// its plate.
fn enclosure(stomp: Stomp) -> (u32, u32) {
    match stomp {
        Stomp::Pedal(p) => match p {
            Pedal::Green808 => (0x2f8f52, 0xf2f2e8),
            Pedal::Green9 => (0x1f7a45, 0xf4f4ea),
            Pedal::GoldDrive => (0xc7a24a, 0x2a1d07),
            Pedal::BritDrive => (0x26262a, 0xd9b45a),
            Pedal::CleanBoost => (0xb5babd, 0x1a1a1c),
            Pedal::TrebleBoost => (0x7b7f84, 0xf0f0ea),
            Pedal::Rodent => (0x1e1e21, 0xf2f2f2),
            Pedal::YellowDist => (0xe8c22e, 0x1d1a10),
            Pedal::OrangeDist => (0xec7b2c, 0x1f1408),
            Pedal::HeavyMetal => (0x2c2e33, 0xf08a2c),
            Pedal::MetalZone => (0x232427, 0xe8e8e8),
            Pedal::Modern33 => (0xe3e0d6, 0x1c1c1c),
            Pedal::ModernPurple => (0x6b3fa4, 0xf4ecff),
            Pedal::BigMuff => (0xc9cbc7, 0xb3261e),
            Pedal::RoundFuzz => (0xb63a2f, 0xf6eee0),
            Pedal::BassDriver => (0x3b3f44, 0xe9e2c8),
            Pedal::OrangePhase => (0xf07e22, 0x1a1208),
            Pedal::BlueChorus => (0x3b7ccc, 0xf2f6ff),
            Pedal::None => (0x333333, 0xffffff),
        },
        Stomp::Wah(faderframe_guitar::circuits::wah::Build::CryBaby) => (0x18181a, 0xe6e6e6),
        Stomp::Wah(faderframe_guitar::circuits::wah::Build::V847) => (0xc6cacd, 0x1a1a1a),
        Stomp::Empty => (0x333333, 0xffffff),
    }
}

fn jack_points(r: Rect) -> (Point, Point) {
    (
        Point::new(r.x, r.y + 58.0),
        Point::new(r.right(), r.y + 58.0),
    )
}

fn cable(p: &mut dyn Painter, a: Point, b: Point, lift: f32) {
    let mut path = Path::new();
    let mid = (a.y.min(b.y) - lift).max(BOARD_Y + 4.0);
    path.move_to(a)
        .cubic_to(Point::new(a.x + 18.0, mid), Point::new(b.x - 18.0, mid), b);
    p.stroke_path(&path, 6.0, rgba(0, 0.55));
    p.stroke_path(&path, 4.6, rgb(0x141417));
    p.stroke_path(&path, 1.4, rgba(0xffffff, 0.12));
    for (end, dir) in [(a, 1.0f32), (b, -1.0)] {
        let plug = Rect::new(
            end.x - if dir > 0.0 { 0.0 } else { 9.0 },
            end.y - 3.5,
            9.0,
            7.0,
        );
        p.fill_rounded(
            plug,
            2.0,
            &linear(
                plug.x,
                plug.y,
                plug.x,
                plug.bottom(),
                vec![(0.0, rgb(0xe6e8ea)), (1.0, rgb(0x6b6f74))],
            ),
        );
    }
}

/// The amplifier's lead: down from the last jack, along the board's floor
/// under the pedals, and up into the plate.
fn floor_cable(p: &mut dyn Painter, a: Point, b: Point) {
    let floor = BOARD_Y + BOARD_H - 13.0;
    let mut path = Path::new();
    path.move_to(a)
        .cubic_to(
            Point::new(a.x + 26.0, a.y),
            Point::new(a.x + 10.0, floor),
            Point::new(a.x + 46.0, floor),
        )
        .line_to(Point::new(b.x - 46.0, floor))
        .cubic_to(
            Point::new(b.x - 10.0, floor),
            Point::new(b.x - 26.0, b.y),
            b,
        );
    p.stroke_path(&path, 6.0, rgba(0, 0.55));
    p.stroke_path(&path, 4.6, rgb(0x141417));
    p.stroke_path(&path, 1.4, rgba(0xffffff, 0.12));
    let plug = Rect::new(b.x - 9.0, b.y - 3.5, 9.0, 7.0);
    p.fill_rounded(
        plug,
        2.0,
        &linear(
            plug.x,
            plug.y,
            plug.x,
            plug.bottom(),
            vec![(0.0, rgb(0xe6e8ea)), (1.0, rgb(0x6b6f74))],
        ),
    );
}

fn board(p: &mut dyn Painter, sc: &Scene<'_>) {
    let r = Rect::new(0.0, BOARD_Y, PANEL_W, BOARD_H);
    p.fill_rect(
        r,
        &linear(
            0.0,
            r.y,
            0.0,
            r.bottom(),
            vec![(0.0, rgb(0x2b2e33)), (1.0, rgb(0x16181b))],
        ),
    );
    // The board's rails, each with its strip of hook-and-loop.
    for i in 0..5 {
        let y = BOARD_Y + 18.0 + i as f32 * 46.0;
        let rail = Rect::new(14.0, y, PANEL_W - 28.0, 34.0);
        p.fill_rounded(
            rail,
            3.0,
            &linear(
                0.0,
                rail.y,
                0.0,
                rail.bottom(),
                vec![
                    (0.0, rgb(0x474c53)),
                    (0.5, rgb(0x32363b)),
                    (1.0, rgb(0x24272b)),
                ],
            ),
        );
        p.fill(Rect::new(rail.x, rail.y, rail.w, 1.0), rgba(0xffffff, 0.14));
        p.fill(
            Rect::new(rail.x, rail.bottom() - 1.0, rail.w, 1.0),
            rgba(0, 0.6),
        );
        let strip = rail.inset_xy(10.0, 9.0);
        p.fill_rounded(strip, 2.0, &Paint::Solid(rgb(0x121315)));
        let mut x = strip.x + 3.0;
        while x < strip.right() - 2.0 {
            p.fill(
                Rect::new(x, strip.y + 2.0, 1.0, strip.h - 4.0),
                rgba(0xffffff, 0.035),
            );
            x += 4.0;
        }
    }
    // The guitar's lead in, the amplifier's out.
    let jack_y = PEDAL_Y + 58.0;
    let plate_in = Rect::new(16.0, PEDAL_Y + 26.0, 48.0, 66.0);
    let plate_out = Rect::new(PANEL_W - 64.0, PEDAL_Y + 26.0, 48.0, 66.0);
    for (r, label) in [(plate_in, "IN"), (plate_out, "AMP")] {
        p.shadow(r, 5.0, rgba(0, 0.5), 0.0, 3.0, 8.0);
        p.fill_rounded(
            r,
            5.0,
            &linear(
                r.x,
                r.y,
                r.right(),
                r.bottom(),
                vec![(0.0, rgb(0x5a5f66)), (1.0, rgb(0x2c2f34))],
            ),
        );
        p.text(
            label,
            Rect::new(r.x, r.y + 4.0, r.w, 14.0),
            &style(8.5, rgb(0xd8dde2)),
        );
        let c = Point::new(r.center().x, jack_y);
        p.circle(c, 8.0, rgb(0x0b0c0d));
        p.circle(c, 4.5, rgb(0x2a2c30));
    }
    let carried = sc.dragging.map(|(place, _)| place);
    // Cables behind the pedals: the guitar, between the boxes, to the amp.
    let mut from = Point::new(plate_in.center().x + 6.0, jack_y);
    for (place, ped) in sc.look.pedals.iter().enumerate() {
        if Some(place) == carried {
            continue;
        }
        let (l, rgt) = jack_points(ped.rect);
        cable(p, from, l, 26.0);
        from = rgt;
    }
    floor_cable(p, from, Point::new(plate_out.center().x - 6.0, jack_y));
    for (place, ped) in sc.look.pedals.iter().enumerate() {
        if Some(place) != carried {
            pedal(p, sc, place, ped, 0.0, false);
        }
    }
    if let Some(r) = sc.look.add {
        add_slot(p, r, sc.hovered(Target::Add), carried.is_none());
    }
    if let Some((place, x)) = sc.dragging
        && let Some(ped) = sc.look.pedals.get(place)
    {
        let to = layout::place_at(x, sc.look.pedals.len());
        let marker = layout::pedal_rect(to);
        p.fill_rounded(
            Rect::new(marker.x - 6.0, marker.y - 6.0, 4.0, PEDAL_H + 12.0),
            2.0,
            &Paint::Solid(rgb(ACCENT)),
        );
        pedal(p, sc, place, ped, x - ped.rect.x, true);
    }
}

fn add_slot(p: &mut dyn Painter, r: Rect, hovered: bool, show: bool) {
    if !show {
        return;
    }
    let c = if hovered {
        rgb(ACCENT)
    } else {
        rgba(0xffffff, 0.32)
    };
    p.fill_rounded(
        r,
        10.0,
        &Paint::Solid(rgba(0, if hovered { 0.28 } else { 0.16 })),
    );
    // A dashed outline.
    let mut x = r.x + 10.0;
    while x < r.right() - 10.0 {
        p.fill(Rect::new(x, r.y, 6.0, 1.5), c);
        p.fill(Rect::new(x, r.bottom() - 1.5, 6.0, 1.5), c);
        x += 11.0;
    }
    let mut y = r.y + 10.0;
    while y < r.bottom() - 10.0 {
        p.fill(Rect::new(r.x, y, 1.5, 6.0), c);
        p.fill(Rect::new(r.right() - 1.5, y, 1.5, 6.0), c);
        y += 11.0;
    }
    let mid = r.center();
    p.fill_rounded(
        Rect::new(mid.x - 14.0, mid.y - 22.0, 28.0, 4.0),
        2.0,
        &Paint::Solid(c),
    );
    p.fill_rounded(
        Rect::new(mid.x - 2.0, mid.y - 34.0, 4.0, 28.0),
        2.0,
        &Paint::Solid(c),
    );
    p.text(
        "ADD PEDAL",
        Rect::new(r.x, mid.y + 6.0, r.w, 16.0),
        &style(9.5, c).tracking(1.2),
    );
    p.text(
        "a stage of its own",
        Rect::new(r.x, mid.y + 22.0, r.w, 14.0),
        &TextStyle::new(8.0, c.with_alpha(0.7)).center(),
    );
}

fn pedal(
    p: &mut dyn Painter,
    sc: &Scene<'_>,
    place: usize,
    ped: &PedalLook,
    dx: f32,
    lifted: bool,
) {
    let dy = if lifted { -8.0 } else { 0.0 };
    let r = ped.rect.translate(dx, dy);
    let (body, ink) = enclosure(ped.stomp);
    let body = rgb(body);
    let ink = rgb(ink);
    let wah = matches!(ped.stomp, Stomp::Wah(_));
    let round = matches!(ped.stomp, Stomp::Pedal(Pedal::RoundFuzz));
    let radius = if round { 30.0 } else { 10.0 };
    p.shadow(
        r,
        radius,
        rgba(0, if lifted { 0.75 } else { 0.6 }),
        0.0,
        if lifted { 14.0 } else { 6.0 },
        if lifted { 24.0 } else { 12.0 },
    );
    // The box: painted metal, lit from the top left.
    p.fill_rounded(
        r,
        radius,
        &linear(
            r.x,
            r.y,
            r.x + r.w * 0.4,
            r.bottom(),
            vec![
                (0.0, body.lighten(0.22)),
                (0.45, body),
                (1.0, body.darken(0.35)),
            ],
        ),
    );
    p.fill_rounded(
        r,
        radius,
        &radial(
            r.x + 18.0,
            r.y + 10.0,
            0.0,
            r.h * 0.9,
            rgba(0xffffff, 0.16),
            rgba(0xffffff, 0.0),
        ),
    );
    p.stroke_rounded(r, radius, 1.2, rgba(0, 0.55));
    p.stroke_rounded(r.inset(1.2), radius - 1.0, 1.0, rgba(0xffffff, 0.16));
    // The stomp area, a shade darker.
    let foot = Rect::new(r.x + 4.0, r.y + 154.0, r.w - 8.0, r.h - 158.0);
    p.fill_rounded(foot, radius.min(8.0), &Paint::Solid(body.darken(0.22)));
    p.fill(
        Rect::new(foot.x + 6.0, foot.y, foot.w - 12.0, 1.0),
        rgba(0xffffff, 0.12),
    );
    // The jacks.
    let (jl, jr) = jack_points(r);
    for (j, left) in [(jl, true), (jr, false)] {
        let jack = Rect::new(
            if left { j.x - 3.0 } else { j.x - 5.0 },
            j.y - 7.0,
            8.0,
            14.0,
        );
        p.fill_rounded(jack, 2.0, &Paint::Solid(rgb(0x0e0f10)));
    }
    let shift = |k: &KnobAt| Point::new(k.at.x + dx, k.at.y + dy);
    for k in &ped.knobs {
        let at = shift(k);
        if sc.knob_hovered(k.id) {
            hover_ring(p, at, k.r, rgb(ACCENT));
        }
        knob(p, at, k.r, sc.norm(k.id), 1.0);
        let label = TextStyle::new(7.0, ink)
            .family(FontFamily::Condensed)
            .weight(FontWeight::Bold)
            .center();
        p.text(
            &k.label.to_uppercase(),
            Rect::new(at.x - 24.0, at.y + k.r + 3.0, 48.0, 10.0),
            &label,
        );
        if sc.knob_hovered(k.id) {
            let v = super::GuitarView::text(sc.tap, k.id, sc.get(k.id));
            let tag = Rect::new(at.x - 22.0, at.y - k.r - 15.0, 44.0, 13.0);
            p.fill_rounded(tag, 3.0, &Paint::Solid(rgba(0, 0.78)));
            p.text(&v, tag, &style(8.0, rgb(0xf6f1e4)));
        }
    }
    if !round {
        for (i, (x, y)) in [
            (r.x + 7.0, r.y + 7.0),
            (r.right() - 7.0, r.y + 7.0),
            (r.x + 7.0, r.bottom() - 7.0),
            (r.right() - 7.0, r.bottom() - 7.0),
        ]
        .into_iter()
        .enumerate()
        {
            screw(p, i + place, Point::new(x, y), 6.5);
        }
    }
    if wah && let (Some(t), Some(a)) = (ped.treadle, ped.auto) {
        treadle(p, sc, place, ped, t.translate(dx, dy));
        let a = a.translate(dx, dy);
        let auto = sc.get(id::slot(ped.slot, id::AUTO)) >= 0.5;
        p.fill_rounded(
            a,
            4.0,
            &Paint::Solid(if auto {
                rgba(ACCENT, 0.45)
            } else {
                rgba(0, 0.45)
            }),
        );
        p.stroke_rounded(a, 4.0, 1.0, rgba(0xffffff, 0.2));
        p.text(
            "AUTO",
            a,
            &style(
                7.5,
                if auto {
                    rgb(0xfff4e0)
                } else {
                    ink.with_alpha(0.75)
                },
            ),
        );
    }
    led(p, Point::new(ped.led.x + dx, ped.led.y + dy), ped.on);
    // The name, on a plate that opens the pedal menu.
    let plate_r = ped.plate.translate(dx, dy);
    let hovered = sc.hovered(Target::StompPlate { place });
    if hovered {
        p.fill_rounded(plate_r, 4.0, &Paint::Solid(rgba(0, 0.25)));
    }
    let name = ped.stomp.name().to_uppercase();
    let name_style = TextStyle::new(11.5, ink)
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold)
        .center()
        .tracking(0.8);
    letters(p, &name, plate_r, name_style, 0.35);
    footswitch(p, Point::new(ped.switch.x + dx, ped.switch.y + dy), !ped.on);
    if !ped.on {
        p.fill_rounded(r, radius, &Paint::Solid(rgba(0, 0.16)));
    }
    if sc.place_hovered(place) || lifted {
        let x = ped.remove.translate(dx, dy);
        let h = sc.hovered(Target::Remove { place });
        p.circle(
            x.center(),
            8.0,
            if h { rgb(0xd8402e) } else { rgba(0, 0.55) },
        );
        let c = x.center();
        p.line(
            Point::new(c.x - 3.5, c.y - 3.5),
            Point::new(c.x + 3.5, c.y + 3.5),
            1.6,
            rgb(0xffffff),
        );
        p.line(
            Point::new(c.x + 3.5, c.y - 3.5),
            Point::new(c.x - 3.5, c.y + 3.5),
            1.6,
            rgb(0xffffff),
        );
    }
}

/// A wah's rocker in perspective, heel at the bottom: its toe dips as the
/// treadle goes forward. The live position (the follower's, in Auto) is a
/// marker beside it.
fn treadle(p: &mut dyn Painter, sc: &Scene<'_>, place: usize, ped: &PedalLook, t: Rect) {
    let set = sc.get(id::slot(ped.slot, id::TREADLE)).clamp(0.0, 1.0) as f32;
    let live = sc.tap.value(value::TREADLE + ped.slot);
    let auto = sc.get(id::slot(ped.slot, id::AUTO)) >= 0.5;
    let pos = if auto && ped.on {
        live.clamp(0.0, 1.0)
    } else {
        set
    };
    // Toe forward (1): the far edge is lower and narrower.
    let inset = 4.0 + 12.0 * pos;
    let top = t.y + 14.0 * pos;
    let mut path = Path::new();
    path.move_to(Point::new(t.x + inset, top))
        .line_to(Point::new(t.right() - inset, top))
        .line_to(Point::new(t.right(), t.bottom()))
        .line_to(Point::new(t.x, t.bottom()))
        .close();
    p.fill_path_paint(
        &path,
        &linear(
            t.x,
            top,
            t.x,
            t.bottom(),
            vec![
                (0.0, rgb(0x2b2c2f).lighten(0.25 * (1.0 - pos))),
                (1.0, rgb(0x0f1011)),
            ],
        ),
    );
    // The rubber's ridges.
    let ridges = 9;
    for i in 1..ridges {
        let f = i as f32 / ridges as f32;
        let y = top + (t.bottom() - top) * f;
        let x0 = t.x + inset * (1.0 - f);
        let x1 = t.right() - inset * (1.0 - f);
        p.line(
            Point::new(x0 + 4.0, y),
            Point::new(x1 - 4.0, y),
            1.0,
            rgba(0xffffff, 0.07),
        );
        p.line(
            Point::new(x0 + 4.0, y + 1.0),
            Point::new(x1 - 4.0, y + 1.0),
            1.0,
            rgba(0, 0.4),
        );
    }
    p.stroke_path(&path, 1.0, rgba(0xffffff, 0.14));
    // Where it is: a bar from heel to toe, the set point and the live one.
    let bar = Rect::new(t.right() + 1.0, t.y, 3.0, t.h);
    p.fill(bar, rgba(0, 0.5));
    let y_of = |v: f32| t.bottom() - v * t.h;
    p.fill(
        Rect::new(bar.x - 1.0, y_of(set) - 1.0, 5.0, 2.0),
        rgba(0xffffff, 0.7),
    );
    if auto && ped.on {
        p.fill(
            Rect::new(bar.x - 1.0, y_of(live) - 1.5, 5.0, 3.0),
            rgb(ACCENT),
        );
    }
    if sc.hovered(Target::Treadle {
        id: id::slot(ped.slot, id::TREADLE),
        place,
    }) {
        p.stroke_path(&path, 1.5, rgba(ACCENT, 0.8));
    }
}

// --- the amplifier --------------------------------------------------------------------------

struct Palette {
    plate_top: Color,
    plate_bottom: Color,
    ink: Color,
    piping: Color,
    accent: Option<Color>,
    brushed: bool,
}

fn palette(f: Finish) -> Palette {
    match f {
        Finish::Brit => Palette {
            plate_top: rgb(0xd8b45a),
            plate_bottom: rgb(0x9f7c32),
            ink: rgb(0x1a1307),
            piping: rgb(0xe9dcae),
            accent: None,
            brushed: true,
        },
        Finish::Copper => Palette {
            plate_top: rgb(0xa8764a),
            plate_bottom: rgb(0x5f3b20),
            ink: rgb(0xf5ead2),
            piping: rgb(0xd8c49a),
            accent: None,
            brushed: true,
        },
        Finish::Blackface => Palette {
            plate_top: rgb(0x252528),
            plate_bottom: rgb(0x0f0f11),
            ink: rgb(0xf2f2f2),
            piping: rgb(0xc9c9c2),
            accent: Some(rgb(0xb9bdc1)),
            brushed: false,
        },
        Finish::Silver => Palette {
            plate_top: rgb(0xdfe2e4),
            plate_bottom: rgb(0xa4a8ac),
            ink: rgb(0x121314),
            piping: rgb(0xe8e8e2),
            accent: None,
            brushed: true,
        },
        Finish::Modern => Palette {
            plate_top: rgb(0x202022),
            plate_bottom: rgb(0x0b0b0c),
            ink: rgb(0xeeeeee),
            piping: rgb(0xb8bcc0),
            accent: Some(rgb(0xd3392b)),
            brushed: false,
        },
        Finish::Bass => Palette {
            plate_top: rgb(0x2a2d31),
            plate_bottom: rgb(0x141619),
            ink: rgb(0xeceef0),
            piping: rgb(0xc9ccd0),
            accent: Some(rgb(0x3c7bd0)),
            brushed: false,
        },
    }
}

fn amp(p: &mut dyn Painter, sc: &Scene<'_>) {
    let section = Rect::new(0.0, AMP_Y, PANEL_W, super::AMP_H);
    p.fill_rect(
        section,
        &linear(
            0.0,
            section.y,
            0.0,
            section.bottom(),
            vec![(0.0, rgb(0x141517)), (1.0, rgb(0x1c1d20))],
        ),
    );
    let a = &sc.look.amp;
    let pal = palette(layout::finish(a.amp));
    let r = layout::amp_rect();
    p.shadow(r, 14.0, rgba(0, 0.7), 0.0, 8.0, 18.0);
    // Tolex, with its grain.
    p.fill_rounded(
        r,
        14.0,
        &linear(
            r.x,
            r.y,
            r.x,
            r.bottom(),
            vec![(0.0, rgb(0x2a2826)), (1.0, rgb(0x0e0e0e))],
        ),
    );
    let mut y = r.y + 3.0;
    let mut k = 0;
    while y < r.bottom() - 3.0 {
        let alpha = if k % 3 == 0 { 0.035 } else { 0.015 };
        p.fill(
            Rect::new(r.x + 8.0, y, r.w - 16.0, 1.0),
            rgba(0xffffff, alpha),
        );
        y += 2.5;
        k += 1;
    }
    p.stroke_rounded(r.inset(6.0), 10.0, 1.4, pal.piping.with_alpha(0.55));
    // Corner protectors.
    for (cx, cy) in [
        (r.x, r.y),
        (r.right(), r.y),
        (r.x, r.bottom()),
        (r.right(), r.bottom()),
    ] {
        let c = Rect::new(cx - 13.0, cy - 13.0, 26.0, 26.0).intersection(&r);
        p.fill_rounded(
            c,
            6.0,
            &linear(
                c.x,
                c.y,
                c.right(),
                c.bottom(),
                vec![(0.0, rgb(0xd4d7da)), (1.0, rgb(0x5e6267))],
            ),
        );
    }
    let plate = layout::amp_plate();
    p.fill_rounded(
        plate,
        6.0,
        &linear(
            plate.x,
            plate.y,
            plate.x,
            plate.bottom(),
            vec![(0.0, pal.plate_top), (1.0, pal.plate_bottom)],
        ),
    );
    if pal.brushed {
        let mut x = plate.x + 2.0;
        let mut i = 0;
        while x < plate.right() - 2.0 {
            let a = [0.05, 0.02, 0.035, 0.015][i % 4];
            p.fill(
                Rect::new(x, plate.y + 1.0, 1.0, plate.h - 2.0),
                rgba(0xffffff, a),
            );
            x += 2.0;
            i += 1;
        }
    }
    p.fill_rect(
        plate,
        &radial(
            plate.x + 120.0,
            plate.y - 40.0,
            0.0,
            plate.w * 0.8,
            rgba(0xffffff, 0.14),
            rgba(0xffffff, 0.0),
        ),
    );
    p.fill(
        Rect::new(plate.x, plate.y, plate.w, 1.5),
        rgba(0xffffff, 0.3),
    );
    p.fill(
        Rect::new(plate.x, plate.bottom() - 2.0, plate.w, 2.0),
        rgba(0, 0.45),
    );
    if let Some(accent) = pal.accent {
        p.fill(
            Rect::new(plate.x + 8.0, plate.bottom() - 9.0, plate.w - 16.0, 2.5),
            accent.with_alpha(0.85),
        );
    }
    for (i, (x, yy)) in [
        (plate.x + 10.0, plate.y + 10.0),
        (plate.right() - 10.0, plate.y + 10.0),
        (plate.x + 10.0, plate.bottom() - 10.0),
        (plate.right() - 10.0, plate.bottom() - 10.0),
    ]
    .into_iter()
    .enumerate()
    {
        screw(p, i, Point::new(x, yy), 11.0);
    }
    amp_badge(p, sc, a, &pal);
    // The knobs, with their scales.
    for k in &a.knobs {
        if sc.knob_hovered(k.id) {
            hover_ring(p, k.at, k.r, pal.ink.mix(rgb(ACCENT), 0.6));
        }
        for i in 0..=10 {
            let (nx, ny) = polar(k.at.x, k.at.y, k.r + 13.0, (i as f32 / 10.0 - 0.5) * 300.0);
            p.text(
                &i.to_string(),
                Rect::new(nx - 8.0, ny - 6.0, 16.0, 12.0),
                &TextStyle::new(7.0, pal.ink.with_alpha(0.85))
                    .weight(FontWeight::Bold)
                    .center(),
            );
        }
        knob(p, k.at, k.r, sc.norm(k.id), 1.0);
        letters(
            p,
            k.label,
            Rect::new(k.at.x - 50.0, k.at.y - k.r - 32.0, 100.0, 14.0),
            style(9.5, pal.ink).tracking(1.2),
            if pal.ink.luminance() > 0.5 { 0.5 } else { 0.0 },
        );
        if sc.knob_hovered(k.id) {
            let v = super::GuitarView::text(sc.tap, k.id, sc.get(k.id));
            p.text(
                &v,
                Rect::new(k.at.x - 40.0, k.at.y + k.r + 12.0, 80.0, 12.0),
                &style(8.5, pal.ink),
            );
        }
    }
    // The circuit's own switches.
    for s in &a.switches {
        let now = (sc.get(s.id).round().max(0.0) as usize).min(s.positions.saturating_sub(1));
        let hovered = sc.hovered(Target::Switch {
            id: s.id,
            positions: s.positions,
            value: None,
        });
        let box_r = s.rect;
        p.fill_rounded(
            box_r,
            4.0,
            &Paint::Solid(rgba(0, if hovered { 0.42 } else { 0.3 })),
        );
        p.stroke_rounded(
            box_r,
            4.0,
            1.0,
            rgba(0xffffff, if hovered { 0.3 } else { 0.12 }),
        );
        // A bat-handle toggle, thrown left to right through the positions.
        let base = Point::new(box_r.x + 16.0, box_r.center().y);
        p.circle(base, 6.0, rgb(0x9da2a7));
        p.circle(base, 4.0, rgb(0x55595e));
        let throw = if s.positions > 1 {
            now as f32 / (s.positions - 1) as f32
        } else {
            0.0
        };
        let tip = Point::new(base.x - 7.0 + 14.0 * throw, base.y - 9.0);
        p.line(base, tip, 3.0, rgb(0xe9ebee));
        p.circle(tip, 2.2, rgb(0xf6f7f8));
        let caption = TextStyle::new(7.5, rgb(0xd9d4c4))
            .weight(FontWeight::Bold)
            .tracking(0.8);
        p.text(
            s.caption,
            Rect::new(box_r.x + 30.0, box_r.y + 1.0, box_r.w - 34.0, 11.0),
            &caption,
        );
        p.text(
            s.labels[now],
            Rect::new(box_r.x + 30.0, box_r.y + 11.0, box_r.w - 34.0, 13.0),
            &TextStyle::new(9.5, rgb(0xf6f1e4))
                .family(FontFamily::Condensed)
                .weight(FontWeight::Bold),
        );
    }
    if let Some(g) = &a.graphic {
        graphic(p, sc, g, &pal);
    }
}

/// The amplifier's nameplate, its lamp, its power stage and its mains.
fn amp_badge(p: &mut dyn Painter, sc: &Scene<'_>, a: &AmpLook, pal: &Palette) {
    let n = a.name;
    let hovered = sc.hovered(Target::Menu { id: id::AMP });
    // A raised nameplate, like a head's logo.
    p.shadow(n, 6.0, rgba(0, 0.45), 0.0, 3.0, 6.0);
    p.fill_rounded(
        n,
        6.0,
        &linear(
            n.x,
            n.y,
            n.x,
            n.bottom(),
            vec![(0.0, rgb(0x1b1b1d)), (1.0, rgb(0x070708))],
        ),
    );
    p.stroke_rounded(
        n,
        6.0,
        1.2,
        if hovered {
            rgb(ACCENT)
        } else {
            pal.piping.with_alpha(0.7)
        },
    );
    let logo = TextStyle::new(23.0, rgb(0xf3e9cf))
        .family(FontFamily::Condensed)
        .weight(FontWeight::Bold)
        .center()
        .tracking(1.6);
    letters(
        p,
        &amp_name(a.amp).to_uppercase(),
        Rect::new(n.x + 6.0, n.y + 6.0, n.w - 12.0, 30.0),
        logo,
        0.6,
    );
    p.text(
        "AMPLIFIER  ▾",
        Rect::new(n.x, n.y + 37.0, n.w, 14.0),
        &TextStyle::new(8.0, rgba(0xf3e9cf, 0.55))
            .weight(FontWeight::Bold)
            .center()
            .tracking(1.6),
    );
    lamp(p, a.lamp, 11.0, true, rgb(0xff3a18));
    p.text(
        "POWER",
        Rect::new(a.lamp.x - 24.0, a.lamp.y + 16.0, 48.0, 12.0),
        &TextStyle::new(7.5, pal.ink)
            .weight(FontWeight::Bold)
            .center(),
    );
    let resolved = a
        .power
        .resolved(a.amp)
        .map(|m| match m {
            faderframe_guitar::voice::PowerModel::Jazz120SS => "transistor",
            faderframe_guitar::voice::PowerModel::AmericanSS800 => "transistor, 300 W",
            _ => "",
        })
        .unwrap_or("");
    let label = match a.power {
        faderframe_guitar::voice::PowerAmp::Matched => {
            let own = match a.power.resolved(a.amp) {
                Some(_) if !resolved.is_empty() => resolved.to_string(),
                Some(m) => power_name(power_for(m)).to_string(),
                None => "none".into(),
            };
            format!("POWER AMP  Matched · {own}")
        }
        other => format!("POWER AMP  {}", power_name(other)),
    };
    plate(
        p,
        a.power_menu,
        &label,
        sc.hovered(Target::Menu { id: id::POWER }),
        rgb(ACCENT),
    );
    let caption = TextStyle::new(7.5, pal.ink)
        .weight(FontWeight::Bold)
        .tracking(1.0);
    p.text(
        "MAINS",
        Rect::new(a.mains[0].x, a.mains[0].y - 13.0, 80.0, 11.0),
        &caption,
    );
    let labels: Vec<&str> = lists::MAINS.iter().map(|m| m.0).collect();
    segments(
        p,
        &a.mains,
        &labels,
        sc.get(id::MAINS).round() as usize,
        rgb(ACCENT),
    );
}

/// The power selection that names a power model (for "Matched · …").
fn power_for(m: faderframe_guitar::voice::PowerModel) -> faderframe_guitar::voice::PowerAmp {
    use faderframe_guitar::voice::{PowerAmp, PowerModel};
    match m {
        PowerModel::Cali6L6 => PowerAmp::Cali6L6,
        PowerModel::American6L6Clean => PowerAmp::American6L6Clean,
        PowerModel::American6L6HighGain => PowerAmp::American6L6HighGain,
        PowerModel::BritEL34 => PowerAmp::BritEL34,
        PowerModel::BritPlexiEL34 => PowerAmp::BritPlexiEL34,
        PowerModel::AC30EL84 => PowerAmp::AC30EL84,
        PowerModel::DR103EL34 | PowerModel::DR103EL34Return => PowerAmp::DR103EL34,
        PowerModel::Recto6L6 => PowerAmp::Recto6L6,
        PowerModel::Recto6L6Tube => PowerAmp::Recto6L6Tube,
        PowerModel::AmericanDeluxe6V6 => PowerAmp::AmericanDeluxe6V6,
        PowerModel::Brit2205EL34 => PowerAmp::Brit2205EL34,
        PowerModel::BritPlexiBassEL34 => PowerAmp::BritPlexiBassEL34,
        PowerModel::BrumEL34 => PowerAmp::BrumEL34,
        PowerModel::Oregon6550 => PowerAmp::Oregon6550,
        PowerModel::Svt6550 => PowerAmp::Svt6550,
        PowerModel::AmericanSS800 => PowerAmp::AmericanSS800,
        PowerModel::Brit45KT66 => PowerAmp::Brit45KT66,
        PowerModel::American7027A => PowerAmp::American7027A,
        PowerModel::AmericanV4b7027A => PowerAmp::AmericanV4b7027A,
        PowerModel::Jazz120SS | PowerModel::British73Out => PowerAmp::Matched,
    }
}

fn graphic(p: &mut dyn Painter, sc: &Scene<'_>, g: &[Rect; 5], pal: &Palette) {
    let first = g[0];
    p.text(
        "GRAPHIC",
        Rect::new(first.x - 10.0, first.y - 18.0, 200.0, 12.0),
        &TextStyle::new(8.5, pal.ink)
            .weight(FontWeight::Bold)
            .tracking(1.2),
    );
    for (b, (r, label)) in g
        .iter()
        .zip(["80", "240", "750", "2.2k", "6.6k"])
        .enumerate()
    {
        let id = id::GRAPHIC + b as u32;
        let slot = Rect::new(r.center().x - 2.0, r.y, 4.0, r.h);
        p.fill_rounded(slot, 2.0, &Paint::Solid(rgba(0, 0.7)));
        p.fill(
            Rect::new(r.x + 2.0, r.center().y, r.w - 4.0, 1.0),
            pal.ink.with_alpha(0.4),
        );
        let v = sc.norm(id);
        let y = r.bottom() - v * r.h;
        let cap = Rect::new(r.x, y - 6.0, r.w, 12.0);
        p.shadow(cap, 2.0, rgba(0, 0.5), 0.0, 2.0, 3.0);
        p.fill_rounded(
            cap,
            2.0,
            &linear(
                cap.x,
                cap.y,
                cap.x,
                cap.bottom(),
                vec![(0.0, rgb(0xf2f3f4)), (1.0, rgb(0x8f9398))],
            ),
        );
        p.fill(
            Rect::new(cap.x + 2.0, cap.center().y - 0.5, cap.w - 4.0, 1.0),
            rgb(0x1a1a1a),
        );
        if matches!(sc.hover, Some(Target::Slider { id: h }) if h == id) {
            p.stroke_rounded(cap, 2.0, 1.0, rgb(ACCENT));
        }
        p.text(
            label,
            Rect::new(r.x - 8.0, r.bottom() + 4.0, r.w + 16.0, 11.0),
            &TextStyle::new(7.5, pal.ink)
                .weight(FontWeight::Bold)
                .center(),
        );
    }
}

// --- the cabinet ------------------------------------------------------------------------------

fn grille_colour(cab: &CabinetProfile) -> (Color, Color) {
    let name = cab.name;
    if name.starts_with("Brit") {
        (rgb(0x8a7a5c), rgb(0x2a2418))
    } else if name.starts_with("American Open") {
        (rgb(0xb8b4a2), rgb(0x3a382f))
    } else if name.starts_with("Jazz") {
        (rgb(0x5b5d60), rgb(0x1e1f20))
    } else {
        (rgb(0x2b2b2c), rgb(0x0c0c0d))
    }
}

fn cab(p: &mut dyn Painter, sc: &Scene<'_>) {
    let section = Rect::new(0.0, CAB_Y, PANEL_W, CAB_H);
    p.fill_rect(
        section,
        &linear(
            0.0,
            section.y,
            0.0,
            section.bottom(),
            vec![(0.0, rgb(0x1c1d20)), (1.0, rgb(0x121315))],
        ),
    );
    let c = &sc.look.cab;
    let r = layout::cab_rect();
    p.fill_rounded(r, 8.0, &Paint::Solid(rgba(0, 0.28)));
    p.stroke_rounded(r, 8.0, 1.0, rgba(0xffffff, 0.06));
    let caption = TextStyle::new(9.0, rgb(0x9aa5ae))
        .weight(FontWeight::Bold)
        .tracking(1.4);
    p.text(
        "CABINET",
        Rect::new(r.x + 16.0, r.y + 4.0, 200.0, 14.0),
        &caption,
    );
    p.text(
        "MICROPHONES",
        Rect::new(452.0, r.y + 4.0, 120.0, 14.0),
        &caption,
    );
    let entry = lists::cabinet(c.cabinet);
    match entry.choice {
        CabinetChoice::Model(profile) => cabinet_front(p, profile),
        CabinetChoice::Bypass => open_baffle(p),
        CabinetChoice::Legacy => legacy_cabinet(p, entry.name),
    }
    if c.physical {
        cone(p, sc);
        side_view(p, sc);
    } else {
        let note = Rect::new(layout::CONE_X - 150.0, layout::CONE_Y - 20.0, 300.0, 40.0);
        p.text(
            if entry.legacy == faderframe_guitar::voice::Cabinet::Off {
                "The power stage into a resistor: no speaker, no microphones"
            } else {
                "A baked cabinet filter: no microphones to place"
            },
            note,
            &TextStyle::new(10.0, rgb(0x8f979e)).center(),
        );
    }
    cab_controls(p, sc, c);
}

/// The cabinet's front, to scale: tolex, the grille cloth and the speakers
/// seen through it.
fn cabinet_front(p: &mut dyn Painter, cab: &CabinetProfile) {
    let area = layout::cab_area();
    let scale = (area.h / cab.height as f32).min(area.w / cab.width as f32);
    let w = cab.width as f32 * scale;
    let h = cab.height as f32 * scale;
    let b = Rect::new(area.center().x - w / 2.0, area.center().y - h / 2.0, w, h);
    p.shadow(b, 8.0, rgba(0, 0.7), 0.0, 6.0, 14.0);
    p.fill_rounded(
        b,
        8.0,
        &linear(
            b.x,
            b.y,
            b.x,
            b.bottom(),
            vec![(0.0, rgb(0x2a2826)), (1.0, rgb(0x0f0f0f))],
        ),
    );
    let edge = 0.06 * w.min(h);
    let grille = b.inset(edge);
    let (cloth, dark) = grille_colour(cab);
    p.fill_rounded(
        grille,
        4.0,
        &linear(
            grille.x,
            grille.y,
            grille.x,
            grille.bottom(),
            vec![(0.0, cloth), (1.0, cloth.darken(0.35))],
        ),
    );
    // The speakers behind the cloth.
    p.push_clip(grille);
    let radius = |sd: f64| ((sd / std::f64::consts::PI).sqrt() as f32 * scale * 1.12).max(8.0);
    let r_cone = radius(cab.default_speaker.sd);
    for (i, &(x, y)) in cab.positions.iter().take(cab.drivers).enumerate() {
        let c = Point::new(
            b.center().x + x as f32 * scale,
            b.center().y - y as f32 * scale,
        );
        p.fill_path_paint(
            &Path::circle(c, r_cone),
            &radial(
                c.x,
                c.y,
                r_cone * 0.15,
                r_cone,
                dark.with_alpha(0.75),
                dark.with_alpha(0.25),
            ),
        );
        p.stroke_path(&Path::circle(c, r_cone), 1.0, rgba(0, 0.35));
        if i == 0 {
            // The microphones' speaker.
            p.stroke_path(&Path::circle(c, r_cone + 3.0), 1.5, rgba(ACCENT, 0.55));
        }
    }
    if let Some(horn) = cab.horn {
        let c = Point::new(
            b.center().x + horn.position.0 as f32 * scale,
            b.center().y - horn.position.1 as f32 * scale,
        );
        p.fill_rounded(
            Rect::new(c.x - 18.0, c.y - 10.0, 36.0, 20.0),
            4.0,
            &Paint::Solid(dark.with_alpha(0.7)),
        );
    }
    // The weave.
    let mut x = grille.x - grille.h;
    while x < grille.right() {
        p.line(
            Point::new(x, grille.bottom()),
            Point::new(x + grille.h, grille.y),
            1.0,
            rgba(0, 0.12),
        );
        p.line(
            Point::new(x, grille.y),
            Point::new(x + grille.h, grille.bottom()),
            1.0,
            rgba(0xffffff, 0.05),
        );
        x += 4.0;
    }
    p.pop_clip();
    p.fill_rect(
        grille,
        &radial(
            grille.x + grille.w * 0.25,
            grille.y,
            0.0,
            grille.w,
            rgba(0xffffff, 0.10),
            rgba(0xffffff, 0.0),
        ),
    );
    // Piping and a small badge.
    p.stroke_rounded(grille, 4.0, 1.5, rgba(0xe9dcae, 0.45));
    let badge = Rect::new(grille.x + 10.0, grille.y + 8.0, 86.0, 16.0);
    p.fill_rounded(badge, 3.0, &Paint::Solid(rgba(0, 0.55)));
    p.text(
        "FADERFRAME",
        badge,
        &style(8.0, rgb(0xe9dcae)).tracking(1.4),
    );
    let name = TextStyle::new(9.5, rgb(0xcfd5da))
        .weight(FontWeight::Bold)
        .center();
    let open = if cab.open_fraction > 0.0 {
        " · open back"
    } else {
        ""
    };
    p.text(
        &format!("{}{}", cab.name, open),
        Rect::new(area.x, area.bottom() + 2.0, area.w, 14.0),
        &name,
    );
}

fn open_baffle(p: &mut dyn Painter) {
    let area = layout::cab_area();
    let b = Rect::new(area.center().x - 120.0, area.y + 10.0, 240.0, area.h - 20.0);
    p.fill_rounded(
        b,
        4.0,
        &linear(
            b.x,
            b.y,
            b.right(),
            b.bottom(),
            vec![(0.0, rgb(0x7a5b3a)), (1.0, rgb(0x3d2a18))],
        ),
    );
    let c = b.center();
    p.fill_path_paint(
        &Path::circle(c, 82.0),
        &radial(c.x, c.y, 10.0, 82.0, rgb(0x1a1714), rgb(0x302a24)),
    );
    p.circle(c, 22.0, rgb(0x14110e));
    p.text(
        "speaker on an open baffle",
        Rect::new(area.x, area.bottom() + 2.0, area.w, 14.0),
        &TextStyle::new(9.5, rgb(0xcfd5da)).center(),
    );
}

fn legacy_cabinet(p: &mut dyn Painter, name: &str) {
    let area = layout::cab_area();
    let b = Rect::new(area.center().x - 150.0, area.y + 20.0, 300.0, area.h - 40.0);
    p.fill_rounded(b, 8.0, &Paint::Solid(rgba(0xffffff, 0.04)));
    p.stroke_rounded(b, 8.0, 1.5, rgba(0xffffff, 0.18));
    p.text(
        name,
        b,
        &TextStyle::new(14.0, rgb(0xaab3bb))
            .weight(FontWeight::Bold)
            .center(),
    );
}

/// The microphones' speaker, close up: the cone, the dust cap, the two
/// microphones where they point.
fn cone(p: &mut dyn Painter, sc: &Scene<'_>) {
    let c = Point::new(CONE_X, CONE_Y);
    p.fill_path_paint(
        &Path::circle(c, CONE_R + 12.0),
        &radial(
            c.x,
            c.y,
            CONE_R,
            CONE_R + 12.0,
            rgb(0x2b2b2b),
            rgb(0x101010),
        ),
    );
    p.fill_path_paint(
        &Path::circle(c, CONE_R),
        &radial(
            c.x - 20.0,
            c.y - 24.0,
            4.0,
            CONE_R * 1.1,
            rgb(0x3e3a35),
            rgb(0x16130f),
        ),
    );
    for k in 1..7 {
        p.stroke_path(
            &Path::circle(c, CONE_R * k as f32 / 7.0),
            1.0,
            rgba(0xffffff, 0.035),
        );
    }
    p.fill_path_paint(
        &Path::circle(c, CONE_R * 0.28),
        &radial(
            c.x - 6.0,
            c.y - 8.0,
            0.0,
            CONE_R * 0.32,
            rgb(0x5b5650),
            rgb(0x1b1815),
        ),
    );
    p.stroke_path(&Path::circle(c, CONE_R), 1.0, rgba(0, 0.6));
    // The scale from the dust cap to the edge, either way.
    for side in [-1.0f32, 1.0] {
        for i in 0..=4 {
            let x = c.x + side * CONE_R * i as f32 / 4.0;
            p.fill(
                Rect::new(x - 0.5, c.y + CONE_R + 15.0, 1.0, 5.0),
                rgba(0xffffff, 0.3),
            );
        }
    }
    p.text(
        "CAP",
        Rect::new(c.x - 20.0, c.y + CONE_R + 20.0, 40.0, 11.0),
        &TextStyle::new(7.0, rgba(0xffffff, 0.4)).center(),
    );
    for (x, t) in [(c.x - CONE_R, "EDGE"), (c.x + CONE_R, "EDGE")] {
        p.text(
            t,
            Rect::new(x - 20.0, c.y + CONE_R + 20.0, 40.0, 11.0),
            &TextStyle::new(7.0, rgba(0xffffff, 0.4)).center(),
        );
    }
    for mic in 0..2 {
        if mic == 1 && !sc.look.cab.mic_b {
            continue;
        }
        let pos = sc.get(if mic == 0 {
            id::A_POSITION
        } else {
            id::B_POSITION
        });
        let at = layout::mic_front(mic, pos);
        let color = rgb(if mic == 0 { MIC_A } else { MIC_B });
        let hovered = sc.hovered(Target::MicFront { mic });
        // The capsule seen end on: a grille ring.
        p.fill_path_paint(
            &Path::circle(at, 15.0),
            &radial(
                at.x,
                at.y,
                4.0,
                15.0,
                color.with_alpha(0.32),
                color.with_alpha(0.0),
            ),
        );
        p.circle(at, 9.5, rgba(0, 0.7));
        p.fill_path_paint(
            &Path::circle(at, 8.0),
            &radial(
                at.x - 2.0,
                at.y - 2.0,
                0.0,
                9.0,
                rgb(0xe3e6e9),
                rgb(0x6c7176),
            ),
        );
        p.stroke_path(
            &Path::circle(at, 8.0),
            if hovered { 2.2 } else { 1.4 },
            color,
        );
        p.text(
            if mic == 0 { "A" } else { "B" },
            Rect::new(at.x - 8.0, at.y - 7.0, 16.0, 14.0),
            &style(9.0, rgb(0x15161a)),
        );
    }
}

/// The microphones from the side: their distance from the grille and their
/// angle off the cone's axis.
fn side_view(p: &mut dyn Painter, sc: &Scene<'_>) {
    let r = layout::side_rect();
    p.fill_rounded(r, 4.0, &Paint::Solid(rgba(0, 0.35)));
    // The cabinet's side and its grille, at the left.
    let baffle = Rect::new(r.x + 34.0, r.y + 4.0, 10.0, r.h - 8.0);
    p.fill_rounded(baffle, 2.0, &Paint::Solid(rgb(0x3a352e)));
    p.fill(
        Rect::new(baffle.right() - 2.0, baffle.y, 2.0, baffle.h),
        rgba(0xe9dcae, 0.5),
    );
    for (d, label) in [
        (0.01, "1 cm"),
        (0.03, ""),
        (0.1, "10 cm"),
        (0.3, ""),
        (1.0, "1 m"),
    ] {
        let x = layout::side_x(layout::distance_norm(d));
        p.fill(
            Rect::new(x - 0.5, r.bottom() - 7.0, 1.0, 4.0),
            rgba(0xffffff, 0.3),
        );
        if !label.is_empty() {
            p.text(
                label,
                Rect::new(x - 18.0, r.bottom() - 18.0, 36.0, 10.0),
                &TextStyle::new(6.5, rgba(0xffffff, 0.4)).center(),
            );
        }
    }
    for mic in 0..2 {
        if mic == 1 && !sc.look.cab.mic_b {
            continue;
        }
        let (d, a) = if mic == 0 {
            (id::A_DISTANCE, id::A_ANGLE)
        } else {
            (id::B_DISTANCE, id::B_ANGLE)
        };
        let color = rgb(if mic == 0 { MIC_A } else { MIC_B });
        let at = Point::new(
            layout::side_x(layout::distance_norm(sc.get(d))),
            r.y + 16.0 + 20.0 * mic as f32,
        );
        let angle = (sc.get(a) as f32).to_radians();
        // The microphone's body, pointing back at the grille.
        let len = 30.0;
        let tail = Point::new(at.x + len * angle.cos(), at.y - len * angle.sin());
        p.line(at, tail, 7.0, rgb(0x2c2f33));
        p.line(at, tail, 5.0, color.darken(0.25));
        p.circle(at, 5.0, rgb(0xd9dde1));
        p.stroke_path(
            &Path::circle(at, 5.0),
            if sc.hovered(Target::MicSide { mic }) {
                2.0
            } else {
                1.0
            },
            color,
        );
        let pos = sc.get(if mic == 0 {
            id::A_POSITION
        } else {
            id::B_POSITION
        });
        let x = CONE_X + CONE_R + 22.0;
        let y = CONE_Y - 44.0 + 50.0 * mic as f32;
        let mono = TextStyle::new(8.5, color).weight(FontWeight::Bold);
        p.text(
            if mic == 0 { "MIC A" } else { "MIC B" },
            Rect::new(x, y, 90.0, 12.0),
            &mono,
        );
        let dim = TextStyle::new(8.0, rgb(0xc9d0d6));
        p.text(
            &format!("{:.0} % out", pos * 100.0),
            Rect::new(x, y + 12.0, 90.0, 11.0),
            &dim,
        );
        p.text(
            &format!(
                "{} · {}",
                super::GuitarView::text(sc.tap, d, sc.get(d)),
                super::GuitarView::text(sc.tap, a, sc.get(a))
            ),
            Rect::new(x, y + 23.0, 90.0, 11.0),
            &dim,
        );
    }
}

fn cab_controls(p: &mut dyn Painter, sc: &Scene<'_>, c: &CabLook) {
    let caption = TextStyle::new(7.5, rgb(0x9aa5ae))
        .weight(FontWeight::Bold)
        .tracking(1.0);
    for (id, r, label) in &c.menus {
        p.text(label, Rect::new(r.x, r.y - 13.0, r.w, 11.0), &caption);
        let mut text = super::GuitarView::text(sc.tap, *id, sc.get(*id));
        if *id == id::SPEAKER
            && text == "Matched"
            && let CabinetChoice::Model(cab) = lists::cabinet(c.cabinet).choice
        {
            text = format!("Matched · {}", cab.default_speaker.name);
        }
        let enabled = c.physical || *id == id::CABINET;
        if enabled {
            plate(
                p,
                *r,
                &text,
                sc.hovered(Target::Menu { id: *id }),
                rgb(ACCENT),
            );
        } else {
            p.fill_rounded(*r, 4.0, &Paint::Solid(rgba(0xffffff, 0.03)));
            p.text(
                &text,
                r.inset_xy(8.0, 0.0),
                &TextStyle::new(10.0, rgb(0x5d646b)),
            );
        }
    }
    if !c.physical {
        return;
    }
    for k in &c.knobs {
        if sc.knob_hovered(k.id) {
            hover_ring(p, k.at, k.r, rgb(ACCENT));
        }
        knob(p, k.at, k.r, sc.norm(k.id), 1.0);
        p.text(
            k.label,
            Rect::new(k.at.x - 40.0, k.at.y - k.r - 15.0, 80.0, 11.0),
            &style(8.0, rgb(0xb7c0c8)).tracking(1.0),
        );
        let v = super::GuitarView::text(sc.tap, k.id, sc.get(k.id));
        p.text(
            &v,
            Rect::new(k.at.x - 40.0, k.at.y + k.r + 3.0, 80.0, 12.0),
            &style(8.5, rgb(0xe3e7ea)),
        );
    }
    for (id, r, label, states) in &c.toggles {
        let on = sc.get(*id) >= 0.5;
        let hovered = sc.hovered(Target::Toggle { id: *id });
        p.fill_rounded(
            *r,
            4.0,
            &Paint::Solid(if on {
                rgba(ACCENT, 0.28)
            } else {
                rgb(0x111316)
            }),
        );
        p.stroke_rounded(
            *r,
            4.0,
            1.0,
            if hovered || on {
                rgba(ACCENT, 0.8)
            } else {
                rgba(0xffffff, 0.1)
            },
        );
        p.circle(
            Point::new(r.x + 12.0, r.center().y),
            3.0,
            if on {
                rgb(ACCENT)
            } else {
                rgba(0xffffff, 0.25)
            },
        );
        p.text(
            &format!("{label}  {}", states[usize::from(on)]),
            Rect::new(r.x + 22.0, r.y, r.w - 26.0, r.h),
            &TextStyle::new(9.0, rgb(0xe6e2d6)).weight(FontWeight::Bold),
        );
    }
}

// --- the output strip --------------------------------------------------------------------------

fn out(p: &mut dyn Painter, sc: &Scene<'_>, needles: &mut [[Needle; 2]; 2], dt: f32) {
    let o: &OutLook = &sc.look.out;
    let section = Rect::new(0.0, OUT_Y, PANEL_W, super::OUT_H);
    p.fill_rect(section, &Paint::Solid(rgb(0x101113)));
    let r = layout::out_rect();
    p.fill_rounded(
        r,
        6.0,
        &linear(
            r.x,
            r.y,
            r.x,
            r.bottom(),
            vec![(0.0, rgb(0x2a2d31)), (1.0, rgb(0x191b1e))],
        ),
    );
    p.fill(
        Rect::new(r.x + 6.0, r.y, r.w - 12.0, 1.0),
        rgba(0xffffff, 0.14),
    );
    for (i, x) in [r.x + 10.0, r.right() - 10.0].into_iter().enumerate() {
        screw(p, i + 2, Point::new(x, r.center().y), 11.0);
    }
    for k in [&o.input, &o.mix, &o.output] {
        if sc.knob_hovered(k.id) {
            hover_ring(p, k.at, k.r, rgb(ACCENT));
        }
        knob(p, k.at, k.r, sc.norm(k.id), 1.0);
        p.text(
            k.label,
            Rect::new(k.at.x - 40.0, k.at.y - k.r - 14.0, 80.0, 11.0),
            &style(7.5, rgb(0xb7c0c8)).tracking(1.0),
        );
        let v = super::GuitarView::text(sc.tap, k.id, sc.get(k.id));
        p.text(
            &v,
            Rect::new(k.at.x - 40.0, k.at.y + k.r + 1.0, 80.0, 11.0),
            &style(8.0, rgb(0xe3e7ea)),
        );
    }
    for (i, m) in o.meters.iter().enumerate() {
        let meter = if i == 0 {
            &sc.tap.meter_in
        } else {
            &sc.tap.meter_out
        };
        level_meter(
            p,
            *m,
            meter,
            &mut needles[i],
            dt,
            if i == 0 { "IN" } else { "OUT" },
        );
    }
    let caption = TextStyle::new(7.5, rgb(0x9aa5ae))
        .weight(FontWeight::Bold)
        .tracking(1.0);
    p.text(
        "DI FROM",
        Rect::new(o.di[0].x, o.di[0].y - 13.0, 120.0, 11.0),
        &caption,
    );
    let labels: Vec<&str> = lists::DI_SOURCES.iter().map(|d| d.0).collect();
    segments(
        p,
        &o.di,
        &labels,
        sc.get(id::DI_SOURCE).round() as usize,
        rgb(ACCENT),
    );
    // What the line costs: its latency, and any late audio.
    let rr = o.readout;
    p.fill_rounded(rr, 4.0, &Paint::Solid(rgb(0x0a0b0c)));
    p.stroke_rounded(rr, 4.0, 1.0, rgba(0xffffff, 0.08));
    let samples = sc.tap.value(value::LATENCY);
    let ms = if sc.rate > 0.0 {
        samples / sc.rate * 1000.0
    } else {
        0.0
    };
    let lcd = TextStyle::new(10.0, rgb(0xe6c27a)).family(FontFamily::Mono);
    p.text(
        &format!("LATENCY {ms:.1} ms"),
        Rect::new(rr.x + 10.0, rr.y + 4.0, rr.w - 20.0, 14.0),
        &lcd,
    );
    p.text(
        &format!(
            "{} stage{} · {samples:.0} smp",
            sc.look.pedals.len() + 1,
            if sc.look.pedals.is_empty() { "" } else { "s" }
        ),
        Rect::new(rr.x + 10.0, rr.y + 18.0, rr.w - 20.0, 13.0),
        &lcd.color(rgba(0xe6c27a, 0.65)),
    );
    let late = sc.tap.value(value::UNDERRUNS);
    let note = if late > 0.0 {
        (format!("{late:.0} late frames"), rgb(0xef4136))
    } else {
        ("DI on output 2".to_string(), rgba(0xe6c27a, 0.65))
    };
    p.text(
        &note.0,
        Rect::new(rr.x + 10.0, rr.y + 32.0, rr.w - 20.0, 13.0),
        &lcd.color(note.1),
    );
}

/// A horizontal pair of peak bars with held peaks (the Program EQ's scale).
fn level_meter(
    p: &mut dyn Painter,
    r: Rect,
    meter: &faderframe_plugin_host::tap::Meter,
    needles: &mut [Needle; 2],
    dt: f32,
    label: &str,
) {
    p.text(
        label,
        Rect::new(r.x, r.y - 13.0, 100.0, 11.0),
        &TextStyle::new(7.5, rgb(0x9aa5ae))
            .weight(FontWeight::Bold)
            .tracking(1.0),
    );
    p.fill_rounded(r, 3.0, &Paint::Solid(rgb(0x070808)));
    let bar_h = (r.h - 6.0) / 2.0;
    for (c, needle) in needles.iter_mut().enumerate() {
        let peak = meter.take_peak(c);
        let db = if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            -150.0
        };
        needle.step(db, dt);
        let y = r.y + 2.0 + c as f32 * (bar_h + 2.0);
        let w = (r.w - 4.0) * deflection(needle.level);
        let paint = linear(
            r.x,
            0.0,
            r.right(),
            0.0,
            vec![
                (0.0, rgb(0x39c95c)),
                (deflection(-18.0), rgb(0x39c95c)),
                (deflection(-6.0), rgb(0xe2cf3c)),
                (deflection(-1.0), rgb(0xf29a2e)),
                (deflection(0.0), rgb(0xef4136)),
                (1.0, rgb(0xef4136)),
            ],
        );
        p.fill_rect(Rect::new(r.x + 2.0, y, w, bar_h), &paint);
        let hold = r.x + 2.0 + (r.w - 4.0) * deflection(needle.hold);
        p.fill(Rect::new(hold - 1.0, y, 2.0, bar_h), rgb(0xf6f1e4));
    }
    for db in [-40.0f32, -20.0, -12.0, -6.0, 0.0] {
        let x = r.x + 2.0 + (r.w - 4.0) * deflection(db);
        p.fill(
            Rect::new(x, r.bottom() + 1.0, 1.0, 3.0),
            rgba(0xffffff, 0.3),
        );
    }
}
