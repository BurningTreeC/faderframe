//! The 76 Compressor's front panel, drawn for FaderFrame: a black
//! anodised rack panel with the Input and Output knobs, Attack and Release,
//! lit push-buttons for the ratio and the meter, a backlit VU meter (gain
//! reduction, or the output at +4 or +8) and input and output peak meters
//! either side of it. Mix, the sidechain high-pass and the stereo link sit
//! in the strip above.
//!
//! Everything is laid out in the panel's own coordinates (1220 × 300 with a
//! 40 high strip above) and scaled to the window. Knobs: drag up and down
//! (Shift: finer), scroll, double-click for the default, right-click for
//! the menu (automation, MIDI learn). A ratio button pressed alone lets
//! the others out (they interlock); Shift- or Ctrl-click presses another in
//! with it, or lets one out — any combination, all four or none, as
//! pressing them together does on the hardware. Meter Off is the power
//! switch: the lamps and the meter go dark. A click on a meter's figure
//! clears its held peak.

use crate::common::Device;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::devices::fet76::{self as fet, id, value};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FontWeight, HostRequest, MenuItem, Paint, Painter, Path, Point,
    PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::time::Instant;

// --- geometry (panel coordinates) ---------------------------------------------

pub const PANEL_W: f32 = 1220.0;
pub const PANEL_H: f32 = 300.0;
/// The strip above the panel.
pub const HEADER_H: f32 = 40.0;
pub const TOTAL_H: f32 = PANEL_H + HEADER_H;

const EAR: f32 = 48.0;
/// A knob sweeps 280 degrees, its lowest value at the lower left.
const SWEEP: f32 = 280.0;
/// Pixels of drag for a knob's whole sweep.
const DRAG_RANGE: f32 = 220.0;
const FINE: f32 = 0.15;

/// The button columns: the ratio's and the meter's.
const RATIO_X: f32 = 548.0;
const METER_X: f32 = 638.0;
const BUTTON_W: f32 = 66.0;
const BUTTON_H: f32 = 40.0;
const BUTTON_TOP: f32 = 40.0;
const BUTTON_PITCH: f32 = 48.0;

/// The VU meter's face, its needle's pivot (below the window) and scale.
const VU: Rect = Rect {
    x: 800.0,
    y: 36.0,
    w: 300.0,
    h: 208.0,
};
const PIVOT: Point = Point { x: 950.0, y: 282.0 };
const ARC_R: f32 = 192.0;
const NUMERAL_R: f32 = 210.0;
/// Half the needle's swing (degrees either side of upright).
const SWING: f32 = 40.0;

/// The peak meters: the left edge of each pair of columns.
const IN_X: f32 = 736.0;
const OUT_X: f32 = 1128.0;
const LADDER_TOP: f32 = 40.0;
const LADDER_BOTTOM: f32 = 244.0;
const LADDER_FLOOR: f32 = -48.0;
const SEGMENTS: usize = 24;
const COLUMN_W: f32 = 12.0;

// --- colours -------------------------------------------------------------------

const LETTER: Color = Color::hex(0xdfe3e8);
const LETTER_DIM: Color = Color::hex(0x8d949c);
const AMBER: Color = Color::hex(0xffb347);
const FACE_LIT: Color = Color::hex(0xfff3d4);
const FACE_EDGE: Color = Color::hex(0xdcc490);
const INK: Color = Color::hex(0x1b1a17);
const RED_ZONE: Color = Color::hex(0xc0392b);

fn black(a: f32) -> Color {
    Color::BLACK.with_alpha(a)
}

fn white(a: f32) -> Color {
    Color::WHITE.with_alpha(a)
}

fn linear(x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Color)>) -> Paint {
    Paint::Linear {
        start: Point::new(x0, y0),
        end: Point::new(x1, y1),
        stops,
    }
}

fn radial(c: Point, r: f32, stops: Vec<(f32, Color)>) -> Paint {
    Paint::Radial {
        center: c,
        radius: r,
        stops,
    }
}

fn polar(c: Point, r: f32, degrees: f32) -> Point {
    let a = degrees.to_radians();
    Point::new(c.x + r * a.cos(), c.y + r * a.sin())
}

fn circle_rect(c: Point, r: f32) -> Rect {
    Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r)
}

// --- the knobs -----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Knob {
    param: u32,
    at: Point,
    r: f32,
    name: &'static str,
    /// Numerals round it: (value, text).
    scale: &'static [(f64, &'static str)],
}

const ATTACK_STEPS: &[(f64, &str)] = &[
    (0.0, "OFF"),
    (1.0, "1"),
    (2.0, "2"),
    (3.0, "3"),
    (4.0, "4"),
    (5.0, "5"),
    (6.0, "6"),
    (7.0, "7"),
];

const STEPS: &[(f64, &str)] = &[
    (1.0, "1"),
    (2.0, "2"),
    (3.0, "3"),
    (4.0, "4"),
    (5.0, "5"),
    (6.0, "6"),
    (7.0, "7"),
];

const KNOBS: [Knob; 4] = [
    Knob {
        param: id::INPUT,
        at: Point { x: 140.0, y: 190.0 },
        r: 46.0,
        name: "INPUT",
        scale: &[
            (-20.0, "-20"),
            (-10.0, "-10"),
            (0.0, "0"),
            (10.0, "10"),
            (20.0, "20"),
            (30.0, "30"),
            (40.0, "40"),
        ],
    },
    Knob {
        param: id::OUTPUT,
        at: Point { x: 298.0, y: 190.0 },
        r: 46.0,
        name: "OUTPUT",
        scale: &[
            (-30.0, "-30"),
            (-20.0, "-20"),
            (-10.0, "-10"),
            (0.0, "0"),
            (10.0, "+10"),
            (20.0, "+20"),
        ],
    },
    Knob {
        param: id::ATTACK,
        at: Point { x: 452.0, y: 94.0 },
        r: 28.0,
        name: "ATTACK",
        scale: ATTACK_STEPS,
    },
    Knob {
        param: id::RELEASE,
        at: Point { x: 452.0, y: 206.0 },
        r: 28.0,
        name: "RELEASE",
        scale: STEPS,
    },
];

/// The angle (degrees, clockwise from +x) of a knob at `t` ∈ 0…1.
pub fn knob_angle(t: f32) -> f32 {
    90.0 + (360.0 - SWEEP) / 2.0 + t.clamp(0.0, 1.0) * SWEEP
}

// --- the meters ----------------------------------------------------------------

/// Where the VU needle sits (0…1 of its swing) for `vu` dB on its scale:
/// the swing follows the voltage, 0 VU at 71 %, +3 at the end (a little
/// further pins it).
pub fn deflection(vu: f32) -> f32 {
    (10f32.powf(vu / 20.0) / 10f32.powf(3.0 / 20.0)).clamp(0.0, 1.04)
}

fn needle_degrees(d: f32) -> f32 {
    -90.0 - SWING + d * 2.0 * SWING
}

/// A VU needle: a damped movement that reaches 99 % of a step in 300 ms
/// and overshoots it by 1.5 %, as the standard asks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Needle {
    pub pos: f32,
    vel: f32,
}

impl Needle {
    const OMEGA: f32 = 13.1;
    const ZETA: f32 = 0.8;

    pub fn follow(&mut self, target: f32, dt: f32) {
        let steps = (dt / 0.001).ceil().max(1.0);
        let h = dt / steps;
        for _ in 0..steps as usize {
            let a = Self::OMEGA * Self::OMEGA * (target - self.pos)
                - 2.0 * Self::ZETA * Self::OMEGA * self.vel;
            self.vel += a * h;
            self.pos += self.vel * h;
        }
        // The pins.
        if self.pos < -0.01 {
            self.pos = -0.01;
            self.vel = 0.0;
        } else if self.pos > 1.06 {
            self.pos = 1.06;
            self.vel = 0.0;
        }
    }
}

/// A peak meter column as shown: the peak falling 24 dB a second and a
/// marker held for 1.2 s.
#[derive(Clone, Copy, Debug)]
struct Column {
    peak: f32,
    hold: f32,
    held_for: f32,
}

impl Default for Column {
    fn default() -> Self {
        Self {
            peak: -120.0,
            hold: -120.0,
            held_for: 0.0,
        }
    }
}

impl Column {
    fn feed(&mut self, db: f32, dt: f32) {
        self.peak = db.max(self.peak - 24.0 * dt);
        if db >= self.hold {
            self.hold = db;
            self.held_for = 0.0;
        } else {
            self.held_for += dt;
            if self.held_for > 1.2 {
                self.hold -= 30.0 * dt;
            }
        }
    }
}

fn db_of(x: f32) -> f32 {
    20.0 * x.max(1e-7).log10()
}

fn ladder_y(db: f32) -> f32 {
    let t = ((db - LADDER_FLOOR) / -LADDER_FLOOR).clamp(0.0, 1.0);
    LADDER_BOTTOM - t * (LADDER_BOTTOM - LADDER_TOP)
}

fn segment_colour(top_db: f32) -> Color {
    if top_db > -2.0 {
        Color::hex(0xff4a3d)
    } else if top_db > -6.0 {
        Color::hex(0xffae42)
    } else if top_db > -18.0 {
        Color::hex(0xe6dc5c)
    } else {
        Color::hex(0x5fd47f)
    }
}

// --- hits ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Knob(usize),
    /// Ratio button `n` (20, 12, 8, 4 from the top).
    Ratio(usize),
    /// GR, +8, +4, Off from the top.
    MeterMode(usize),
    Peak(bool),
    Mix,
    Filter(usize),
    Link,
}

/// The ratio button `n` from the top is bit… of the Ratio Buttons value.
const RATIO_BIT: [u8; 4] = [3, 2, 1, 0];
const RATIO_TEXT: [&str; 4] = ["20", "12", "8", "4"];
/// The meter button `n` from the top sets the Meter parameter to…
const METER_OF_BUTTON: [usize; 4] = [0, 2, 1, 3];
const METER_TEXT: [&str; 4] = ["GR", "+8", "+4", "OFF"];

fn button_rect(x: f32, n: usize) -> Rect {
    Rect::new(x, BUTTON_TOP + n as f32 * BUTTON_PITCH, BUTTON_W, BUTTON_H)
}

fn mix_track() -> Rect {
    Rect::new(104.0, -HEADER_H + 15.0, 250.0, 10.0)
}

fn filter_rect(i: usize) -> Rect {
    Rect::new(
        596.0 + i as f32 * 56.0,
        -HEADER_H + 8.0,
        52.0,
        HEADER_H - 16.0,
    )
}

fn link_rect() -> Rect {
    Rect::new(890.0, -HEADER_H + 8.0, 130.0, HEADER_H - 16.0)
}

fn peak_rect(output: bool) -> Rect {
    let x = if output { OUT_X } else { IN_X };
    Rect::new(x - 8.0, 254.0, 2.0 * COLUMN_W + 20.0, 18.0)
}

// --- the view ------------------------------------------------------------------

enum Drag {
    Knob { param: u32, t: f32, y: f32 },
    Mix,
}

pub struct Fet76View {
    device: Device,
    drag: Option<Drag>,
    needle: Needle,
    columns: [[Column; 2]; 2],
    last_frame: Option<Instant>,
}

impl Fet76View {
    pub fn new(plugin: PluginInstanceId, _theme: &Theme) -> Self {
        Self {
            device: Device::new(plugin),
            drag: None,
            needle: Needle {
                pos: deflection(0.0),
                vel: 0.0,
            },
            columns: [[Column::default(); 2]; 2],
            last_frame: None,
        }
    }

    /// Scale and offset of the panel in the view.
    fn fit(size: Size) -> (f32, f32, f32) {
        let s = (size.w / PANEL_W).min(size.h / TOTAL_H).max(0.01);
        (
            (size.w - PANEL_W * s) / 2.0,
            (size.h - TOTAL_H * s) / 2.0,
            s,
        )
    }

    /// A view point in panel coordinates (the panel's top at 0).
    fn to_panel(size: Size, pos: Point) -> (Point, f32) {
        let (ox, oy, s) = Self::fit(size);
        (Point::new((pos.x - ox) / s, (pos.y - oy) / s - HEADER_H), s)
    }

    fn info(param: u32) -> faderframe_plugin_host::ParameterInfo {
        let all = fet::parameters();
        all.iter()
            .find(|p| p.id == ParameterId(param))
            .cloned()
            .unwrap_or_else(|| all[0].clone())
    }

    fn value(&self, model: &Session, param: u32) -> f64 {
        self.device.value(model, param as usize)
    }

    fn normalized(&self, model: &Session, param: u32) -> f32 {
        let info = Self::info(param);
        ((self.value(model, param) - info.min) / (info.max - info.min)).clamp(0.0, 1.0) as f32
    }

    fn from_normalized(param: u32, t: f32) -> f64 {
        let info = Self::info(param);
        info.min + f64::from(t.clamp(0.0, 1.0)) * (info.max - info.min)
    }

    fn set_once(&self, model: &Session, cx: &mut EventCx<'_, Action>, param: u32, v: f64) {
        self.device.begin(cx, "76 Compressor");
        self.device.set(model, cx, ParameterId(param), v);
        self.device.end(cx);
    }

    fn hit(&self, at: Point) -> Option<Hit> {
        if let Some(i) = KNOBS.iter().position(|k| at.distance(k.at) <= k.r * 1.3) {
            return Some(Hit::Knob(i));
        }
        if let Some(n) = (0..4).find(|n| button_rect(RATIO_X, *n).contains(at)) {
            return Some(Hit::Ratio(n));
        }
        if let Some(n) = (0..4).find(|n| button_rect(METER_X, *n).contains(at)) {
            return Some(Hit::MeterMode(n));
        }
        for output in [false, true] {
            let x = if output { OUT_X } else { IN_X };
            let ladder = Rect::new(
                x - 4.0,
                LADDER_TOP - 4.0,
                2.0 * COLUMN_W + 12.0,
                LADDER_BOTTOM - LADDER_TOP + 8.0,
            );
            if peak_rect(output).contains(at) || ladder.contains(at) {
                return Some(Hit::Peak(output));
            }
        }
        if mix_track().inset_xy(-10.0, -8.0).contains(at) {
            return Some(Hit::Mix);
        }
        if let Some(i) = (0..4).find(|i| filter_rect(*i).contains(at)) {
            return Some(Hit::Filter(i));
        }
        link_rect().contains(at).then_some(Hit::Link)
    }

    /// A control's menu: its default, its automation, MIDI learn and the
    /// mappings it has.
    fn control_menu(&self, model: &Session, param: u32, at: Point) -> Option<HostRequest<Action>> {
        let info = Self::info(param);
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        let id = ParameterId(param);
        let target = faderframe_automation::AutomationTarget::PluginParameter {
            plugin: self.device.plugin,
            parameter: id,
        };
        let text = fet::format(id, info.default).unwrap_or_else(|| format!("{:.1}", info.default));
        let mut items = vec![
            MenuItem::disabled(info.name.clone()),
            MenuItem::new(
                format!("Default ({text})"),
                Action::Edit(faderframe_project::Command::SetPluginParameter {
                    track,
                    plugin: self.device.plugin,
                    parameter: id,
                    value: Some(info.default),
                }),
            )
            .separated(),
        ];
        if info.automatable {
            items.push(
                MenuItem::new("Show Automation", Action::ShowAutomation { track, target })
                    .separated(),
            );
            items.extend(crate::kit::learn_items(
                model,
                faderframe_project::MappingTarget::Parameter { track, target },
            ));
        }
        Some(HostRequest::ContextMenu { at, items })
    }

    fn text_of(param: u32, v: f64) -> String {
        fet::format(ParameterId(param), v).unwrap_or_else(|| match param {
            id::MIX => format!("{:.0} %", v * 100.0),
            _ => format!("{v:+.1} dB"),
        })
    }

    // --- painting --------------------------------------------------------------

    fn paint_header(&self, p: &mut dyn Painter, model: &Session) {
        let bar = Rect::new(0.0, -HEADER_H, PANEL_W, HEADER_H);
        p.fill_rect(
            bar,
            &linear(
                0.0,
                bar.y,
                0.0,
                bar.bottom(),
                vec![(0.0, Color::hex(0x23262b)), (1.0, Color::hex(0x101215))],
            ),
        );
        p.fill(Rect::new(0.0, -1.0, PANEL_W, 1.0), black(0.8));
        let label = TextStyle::new(9.5, LETTER_DIM)
            .weight(FontWeight::Bold)
            .tracking(1.6);
        // Mix: a groove, its fill and a cap.
        p.text("MIX", Rect::new(60.0, -HEADER_H, 40.0, HEADER_H), &label);
        let track = mix_track();
        let mix = self.value(model, id::MIX).clamp(0.0, 1.0) as f32;
        p.fill_rounded(track, 5.0, &Paint::Solid(Color::hex(0x07080a)));
        p.inset_shadow(track, 5.0, black(0.9), 0.0, 1.0, 3.0);
        let fill = Rect::new(track.x, track.y, track.w * mix, track.h);
        p.fill_rounded(
            fill.inset(2.0),
            3.0,
            &linear(
                0.0,
                fill.y,
                0.0,
                fill.bottom(),
                vec![(0.0, AMBER.lighten(0.2)), (1.0, AMBER.darken(0.25))],
            ),
        );
        let cap = Rect::new(track.x + track.w * mix - 9.0, track.y - 6.0, 18.0, 22.0);
        p.shadow(cap, 4.0, black(0.6), 1.0, 2.0, 4.0);
        p.fill_rounded(
            cap,
            4.0,
            &linear(
                0.0,
                cap.y,
                0.0,
                cap.bottom(),
                vec![(0.0, Color::hex(0xd9dde1)), (1.0, Color::hex(0x8a9096))],
            ),
        );
        p.fill(
            Rect::new(cap.center().x - 0.75, cap.y + 4.0, 1.5, 14.0),
            black(0.55),
        );
        p.text(
            &format!("{:.0} %", mix * 100.0),
            Rect::new(track.right() + 14.0, -HEADER_H, 60.0, HEADER_H),
            &TextStyle::new(11.0, LETTER).weight(FontWeight::Bold),
        );
        // The sidechain's high-pass.
        p.text(
            "SIDECHAIN HPF",
            Rect::new(470.0, -HEADER_H, 120.0, HEADER_H),
            &label,
        );
        let filter = self.value(model, id::SC_HPF).round() as usize;
        for (i, name) in fet::SC_FILTERS.iter().enumerate() {
            small_button(p, filter_rect(i), name, i == filter);
        }
        // The stereo link.
        small_button(
            p,
            link_rect(),
            "STEREO LINK",
            self.value(model, id::LINK) >= 0.5,
        );
    }

    fn paint_faceplate(&self, p: &mut dyn Painter) {
        let plate = Rect::new(0.0, 0.0, PANEL_W, PANEL_H);
        p.fill_rect(
            plate,
            &linear(
                0.0,
                0.0,
                0.0,
                PANEL_H,
                vec![
                    (0.0, Color::hex(0x24272b)),
                    (0.5, Color::hex(0x17191c)),
                    (1.0, Color::hex(0x0f1012)),
                ],
            ),
        );
        // Brushed: faint lines across, some lighter, some darker.
        let mut seed = 0x9e37_79b9u32;
        let mut y = 0.5;
        while y < PANEL_H {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let r = (seed % 1000) as f32 / 1000.0;
            let c = if r > 0.5 {
                white((r - 0.5) * 0.05)
            } else {
                black(r * 0.12)
            };
            p.fill(Rect::new(0.0, y, PANEL_W, 0.6), c);
            y += 1.4;
        }
        // The light along the top edge, the shade along the bottom.
        p.fill(Rect::new(0.0, 0.0, PANEL_W, 1.0), white(0.16));
        p.fill(Rect::new(0.0, PANEL_H - 1.5, PANEL_W, 1.5), black(0.7));
        // The rack ears: a fold, the slots and their screws.
        for x in [EAR, PANEL_W - EAR] {
            p.fill(Rect::new(x - 1.0, 0.0, 1.0, PANEL_H), black(0.6));
            p.fill(Rect::new(x, 0.0, 1.0, PANEL_H), white(0.07));
        }
        for x in [EAR / 2.0, PANEL_W - EAR / 2.0] {
            for y in [40.0, PANEL_H - 40.0] {
                let slot = Rect::new(x - 14.0, y - 8.0, 28.0, 16.0);
                p.fill_rounded(slot, 8.0, &Paint::Solid(Color::hex(0x050506)));
                p.inset_shadow(slot, 8.0, black(1.0), 0.0, 2.0, 4.0);
                screw(p, Point::new(x + 3.0, y), 6.5, 30.0);
            }
        }
    }

    fn paint_lettering(&self, p: &mut dyn Painter) {
        let x = 72.0;
        engraved(
            p,
            "FADERFRAME",
            Rect::new(x, 16.0, 220.0, 16.0),
            11.0,
            4.0,
            LETTER_DIM,
            false,
        );
        engraved(
            p,
            "76",
            Rect::new(x - 2.0, 34.0, 70.0, 44.0),
            42.0,
            0.0,
            LETTER,
            false,
        );
        engraved(
            p,
            "COMPRESSOR",
            Rect::new(x + 62.0, 38.0, 220.0, 20.0),
            17.0,
            3.0,
            LETTER,
            false,
        );
        engraved(
            p,
            "FET LIMITING AMPLIFIER",
            Rect::new(x + 63.0, 60.0, 220.0, 14.0),
            8.5,
            2.2,
            LETTER_DIM,
            false,
        );
        // A thin rule under the name.
        p.fill(Rect::new(x, 86.0, 250.0, 1.0), white(0.18));
        p.fill(Rect::new(x, 87.0, 250.0, 1.0), black(0.5));
        // The button columns' headings.
        for (x, text) in [(RATIO_X, "RATIO"), (METER_X, "METER")] {
            engraved(
                p,
                text,
                Rect::new(x - 10.0, 14.0, BUTTON_W + 20.0, 16.0),
                10.0,
                2.4,
                LETTER,
                true,
            );
        }
        for (x, text) in [(IN_X, "IN"), (OUT_X, "OUT")] {
            engraved(
                p,
                text,
                Rect::new(x - 10.0, 14.0, 2.0 * COLUMN_W + 24.0, 16.0),
                10.0,
                2.0,
                LETTER,
                true,
            );
        }
    }

    fn paint_knob(&self, p: &mut dyn Painter, model: &Session, k: &Knob) {
        let info = Self::info(k.param);
        let range = (info.max - info.min).max(1e-9);
        let big = k.r > 40.0;
        // The scale: ticks and numerals.
        let ticks = if big { 31 } else { 13 };
        for i in 0..ticks {
            let t = i as f32 / (ticks - 1) as f32;
            let a = knob_angle(t);
            let major = i % if big { 5 } else { 2 } == 0;
            let (r0, r1) = if major {
                (k.r + 5.0, k.r + 12.0)
            } else {
                (k.r + 6.0, k.r + 9.0)
            };
            p.line(
                polar(k.at, r0, a),
                polar(k.at, r1, a),
                if major { 1.6 } else { 1.0 },
                if major { LETTER } else { LETTER_DIM },
            );
        }
        let size = if big { 10.5 } else { 9.5 };
        for (v, text) in k.scale {
            let t = ((v - info.min) / range) as f32;
            let c = polar(k.at, k.r + if big { 25.0 } else { 21.0 }, knob_angle(t));
            engraved(
                p,
                text,
                Rect::new(c.x - 20.0, c.y - 8.0, 40.0, 16.0),
                size,
                0.0,
                LETTER,
                true,
            );
        }
        engraved(
            p,
            k.name,
            Rect::new(
                k.at.x - 60.0,
                k.at.y + k.r + if big { 18.0 } else { 12.0 },
                120.0,
                16.0,
            ),
            if big { 12.0 } else { 10.0 },
            2.6,
            LETTER,
            true,
        );
        let t = self.normalized(model, k.param);
        knob(p, k.at, k.r, knob_angle(t));
    }

    fn powered(&self, model: &Session) -> bool {
        self.value(model, id::METER).round() as usize != fet::POWER_OFF
    }

    fn paint_buttons(&self, p: &mut dyn Painter, model: &Session) {
        let mask = fet::buttons(self.value(model, id::RATIO));
        let power = self.powered(model);
        for (n, text) in RATIO_TEXT.iter().enumerate() {
            let on = mask >> RATIO_BIT[n] & 1 == 1;
            push_button(p, button_rect(RATIO_X, n), text, on, power);
        }
        // How to press several (the slot under the four).
        let hint = button_rect(RATIO_X, 4);
        let style = TextStyle::new(7.5, LETTER_DIM)
            .weight(FontWeight::Bold)
            .tracking(0.8)
            .center();
        p.text(
            "SHIFT-CLICK",
            Rect::new(hint.x - 10.0, hint.y + 4.0, hint.w + 20.0, 11.0),
            &style,
        );
        p.text(
            "PRESSES",
            Rect::new(hint.x - 10.0, hint.y + 15.0, hint.w + 20.0, 11.0),
            &style,
        );
        p.text(
            "SEVERAL",
            Rect::new(hint.x - 10.0, hint.y + 26.0, hint.w + 20.0, 11.0),
            &style,
        );
        let meter = self.value(model, id::METER).round() as usize;
        for (n, text) in METER_TEXT.iter().enumerate() {
            let on = METER_OF_BUTTON[n] == meter;
            push_button(p, button_rect(METER_X, n), text, on, power);
        }
    }

    fn paint_gr_readout(&self, p: &mut dyn Painter, model: &Session, tap: Option<&AnalysisTap>) {
        let r = button_rect(METER_X, 4);
        glass(p, r);
        if !self.powered(model) {
            return;
        }
        let gr = tap.map_or(0.0, |t| t.value(value::GR));
        p.text(
            &format!("{:.1}", gr.max(0.0)),
            Rect::new(r.x, r.y + 2.0, r.w - 8.0, 24.0),
            &TextStyle::new(17.0, AMBER)
                .weight(FontWeight::Bold)
                .family(faderframe_ui_canvas::FontFamily::Mono)
                .right(),
        );
        p.text(
            "dB GR",
            Rect::new(r.x, r.bottom() - 15.0, r.w - 8.0, 12.0),
            &TextStyle::new(7.5, AMBER.with_alpha(0.6))
                .weight(FontWeight::Bold)
                .right(),
        );
    }

    fn paint_vu(
        &mut self,
        p: &mut dyn Painter,
        model: &Session,
        tap: Option<&AnalysisTap>,
        dt: f32,
    ) {
        let mode = self.value(model, id::METER).round() as usize;
        let lit = mode != fet::POWER_OFF;
        // With more than one ratio button in the meter's bias moves too:
        // the more buttons, the more the reduction swings it the other way
        // (all four: it goes wild, often pinned at the top, as the
        // hardware's manual warns).
        let bias = fet::network(fet::buttons(self.value(model, id::RATIO))).map_or(0.0, |m| m.bias);
        // Where the needle is going.
        let target = match (mode, tap) {
            (_, None) | (fet::POWER_OFF, _) => 0.0,
            (0, Some(t)) => {
                let gr = t.value(value::GR).max(0.0);
                deflection(-gr + bias as f32 * gr * 1.6)
            }
            (m, Some(t)) => {
                // 0 VU at a sine of −18 dBFS (+4) or −14 dBFS (+8).
                let reference = if m == 2 { -14.0f32 } else { -18.0 };
                let rms = t.value(value::OUT_MS).max(0.0).sqrt();
                let zero = 10f32.powf(reference / 20.0) / std::f32::consts::SQRT_2;
                deflection(db_of(rms / zero))
            }
        };
        self.needle.follow(target, dt);
        // The bezel.
        let bezel = VU.inset(-9.0);
        p.shadow(bezel, 10.0, black(0.7), 2.0, 4.0, 10.0);
        p.fill_rounded(
            bezel,
            10.0,
            &linear(
                0.0,
                bezel.y,
                0.0,
                bezel.bottom(),
                vec![(0.0, Color::hex(0x34383d)), (1.0, Color::hex(0x0b0c0e))],
            ),
        );
        p.stroke_rounded(bezel, 10.0, 1.0, white(0.12));
        // The face, lit from behind (or not).
        let (centre, edge) = if lit {
            (FACE_LIT, FACE_EDGE)
        } else {
            (Color::hex(0x9b9179), Color::hex(0x6f6652))
        };
        p.fill_rounded(
            VU,
            4.0,
            &radial(
                Point::new(VU.center().x, VU.bottom() - 30.0),
                VU.w * 0.75,
                vec![(0.0, centre), (0.65, centre.mix(edge, 0.5)), (1.0, edge)],
            ),
        );
        p.push_clip(VU);
        let ink = if lit { INK } else { INK.with_alpha(0.75) };
        let red = if lit { RED_ZONE } else { RED_ZONE.darken(0.3) };
        // The scale: the arc (red past 0 VU), its ticks and numerals.
        let zero = deflection(0.0);
        let arc = |p: &mut dyn Painter, d0: f32, d1: f32, r: f32, w: f32, c: Color| {
            let mut path = Path::new();
            path.arc(
                PIVOT,
                r,
                needle_degrees(d0).to_radians(),
                needle_degrees(d1).to_radians(),
                false,
            );
            p.stroke_path(&path, w, c);
        };
        arc(p, deflection(-20.0), zero, ARC_R, 1.6, ink);
        arc(p, zero, 1.0, ARC_R + 2.5, 6.0, red);
        let marks: [(f32, &str, bool); 11] = [
            (-20.0, "20", true),
            (-10.0, "10", true),
            (-7.0, "7", true),
            (-5.0, "5", true),
            (-3.0, "3", true),
            (-2.0, "2", true),
            (-1.0, "1", true),
            (0.0, "0", true),
            (1.0, "1", true),
            (2.0, "2", true),
            (3.0, "3", true),
        ];
        let numeral = TextStyle::new(12.5, ink).weight(FontWeight::Bold).center();
        for (vu, text, _) in marks {
            let a = needle_degrees(deflection(vu));
            let c = if vu > 0.0 { red } else { ink };
            p.line(
                polar(PIVOT, ARC_R, a),
                polar(PIVOT, ARC_R + 13.0, a),
                1.6,
                c,
            );
            let at = polar(PIVOT, NUMERAL_R + 6.0, a);
            p.text(
                text,
                Rect::new(at.x - 14.0, at.y - 9.0, 28.0, 18.0),
                &numeral.color(c),
            );
        }
        for vu in [-15.0, -8.0, -6.0, -4.0, -2.5, -1.5, -0.5, 0.5, 1.5, 2.5] {
            let a = needle_degrees(deflection(vu));
            p.line(
                polar(PIVOT, ARC_R, a),
                polar(PIVOT, ARC_R + 7.0, a),
                1.0,
                if vu > 0.0 { red } else { ink },
            );
        }
        // The signs at the ends.
        let sign = TextStyle::new(16.0, ink).weight(FontWeight::Bold).center();
        let at = polar(PIVOT, NUMERAL_R - 26.0, needle_degrees(-0.02));
        p.text("−", Rect::new(at.x - 10.0, at.y - 10.0, 20.0, 20.0), &sign);
        let at = polar(PIVOT, NUMERAL_R - 26.0, needle_degrees(1.02));
        p.text(
            "+",
            Rect::new(at.x - 10.0, at.y - 10.0, 20.0, 20.0),
            &sign.color(red),
        );
        // The percentage scale under the arc.
        arc(p, 0.0, 1.0, ARC_R - 16.0, 1.0, ink.with_alpha(0.7));
        let small = TextStyle::new(7.5, ink.with_alpha(0.8)).center();
        for pc in [0, 20, 40, 60, 80, 100] {
            let d = pc as f32 / 100.0 * zero;
            let a = needle_degrees(d);
            p.line(
                polar(PIVOT, ARC_R - 16.0, a),
                polar(PIVOT, ARC_R - 21.0, a),
                1.0,
                ink.with_alpha(0.7),
            );
            let at = polar(PIVOT, ARC_R - 30.0, a);
            p.text(
                &pc.to_string(),
                Rect::new(at.x - 12.0, at.y - 6.0, 24.0, 12.0),
                &small,
            );
        }
        // The lettering on the face.
        p.text(
            "VU",
            Rect::new(PIVOT.x - 40.0, VU.y + 128.0, 80.0, 34.0),
            &TextStyle::new(30.0, ink).weight(FontWeight::Bold).center(),
        );
        let caption = match mode {
            0 => "GAIN REDUCTION",
            1 => "OUTPUT  +4",
            2 => "OUTPUT  +8",
            _ => "",
        };
        let face_small = TextStyle::new(7.5, ink.with_alpha(0.75))
            .weight(FontWeight::Bold)
            .tracking(1.4);
        p.text(
            caption,
            Rect::new(VU.x + 12.0, VU.bottom() - 26.0, 120.0, 12.0),
            &face_small,
        );
        p.text(
            "FADERFRAME",
            Rect::new(VU.right() - 132.0, VU.bottom() - 26.0, 120.0, 12.0),
            &face_small.right(),
        );
        // The needle and its shadow on the face.
        let a = needle_degrees(self.needle.pos);
        let tip = polar(PIVOT, ARC_R + 12.0, a);
        let base = polar(PIVOT, 40.0, a);
        p.line(
            base.offset(3.0, 4.0),
            tip.offset(3.0, 4.0),
            2.2,
            black(0.16),
        );
        p.line(base, tip, 1.8, Color::hex(0x111111));
        p.line(
            polar(PIVOT, ARC_R - 30.0, a),
            tip,
            1.2,
            if lit { RED_ZONE.darken(0.2) } else { INK },
        );
        // The movement's cover over the pivot.
        let cover = circle_rect(PIVOT, 58.0);
        p.fill_rounded(
            cover,
            58.0,
            &radial(
                Point::new(PIVOT.x - 12.0, PIVOT.y - 50.0),
                80.0,
                vec![(0.0, Color::hex(0x3a3d42)), (1.0, Color::hex(0x08090a))],
            ),
        );
        p.stroke_rounded(cover, 58.0, 1.0, white(0.18));
        // The glass: a sheen from the top left.
        p.fill_rect(
            VU,
            &linear(
                VU.x,
                VU.y,
                VU.x + VU.w * 0.55,
                VU.y + VU.h,
                vec![(0.0, white(0.22)), (0.45, white(0.04)), (1.0, white(0.0))],
            ),
        );
        p.pop_clip();
        p.inset_shadow(VU, 4.0, black(0.7), 0.0, 3.0, 8.0);
    }

    fn paint_ladder(
        &mut self,
        p: &mut dyn Painter,
        tap: Option<&AnalysisTap>,
        output: bool,
        dt: f32,
    ) {
        let x = if output { OUT_X } else { IN_X };
        let side = usize::from(output);
        let (mut peaks, mut held) = ([0.0f32; 2], 0.0f32);
        if let Some(t) = tap {
            let m = if output { &t.meter_out } else { &t.meter_in };
            for (c, v) in peaks.iter_mut().enumerate() {
                *v = m.take_peak(c);
                held = held.max(m.held(c));
            }
        }
        for (c, v) in peaks.iter().enumerate() {
            self.columns[side][c].feed(db_of(*v), dt);
        }
        // The window.
        let window = Rect::new(
            x - 4.0,
            LADDER_TOP - 4.0,
            2.0 * COLUMN_W + 12.0,
            LADDER_BOTTOM - LADDER_TOP + 8.0,
        );
        p.fill_rounded(window, 4.0, &Paint::Solid(Color::hex(0x060708)));
        p.inset_shadow(window, 4.0, black(1.0), 0.0, 2.0, 5.0);
        p.stroke_rounded(window.inset(-1.0), 5.0, 1.0, white(0.08));
        let pitch = (LADDER_BOTTOM - LADDER_TOP) / SEGMENTS as f32;
        let step = -LADDER_FLOOR / SEGMENTS as f32;
        for c in 0..2 {
            let col = self.columns[side][c];
            let cx = x + 2.0 + c as f32 * (COLUMN_W + 4.0);
            for i in 0..SEGMENTS {
                let low = LADDER_FLOOR + i as f32 * step;
                let top = low + step;
                let y = LADDER_BOTTOM - (i + 1) as f32 * pitch;
                let seg = Rect::new(cx, y + 1.0, COLUMN_W - 4.0, pitch - 2.2);
                let colour = segment_colour(top);
                let on = col.peak >= low + step * 0.5;
                let hold = col.hold >= low && col.hold < top && col.hold > LADDER_FLOOR;
                if on || hold {
                    p.fill_rounded(seg.inset(-1.5), 2.0, &Paint::Solid(colour.with_alpha(0.25)));
                    p.fill_rounded(seg, 1.0, &Paint::Solid(colour));
                    p.fill(Rect::new(seg.x, seg.y, seg.w, 1.0), white(0.35));
                } else {
                    p.fill_rounded(seg, 1.0, &Paint::Solid(colour.with_alpha(0.1)));
                }
            }
        }
        // The scale, on the VU's side.
        let (nx, align_right) = if output {
            (x - 30.0, true)
        } else {
            (x + 2.0 * COLUMN_W + 10.0, false)
        };
        let style = TextStyle::new(7.5, LETTER_DIM).weight(FontWeight::Bold);
        let style = if align_right { style.right() } else { style };
        for db in [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0] {
            let y = ladder_y(db);
            p.text(
                &format!("{}", db as i32),
                Rect::new(nx, y - 5.0, 22.0, 10.0),
                &style,
            );
        }
        // The held peak's figure (a click clears it).
        let r = peak_rect(output);
        glass(p, r);
        let db = db_of(held);
        let text = if held <= 1e-6 {
            "—".to_string()
        } else {
            format!("{db:.1}")
        };
        p.text(
            &text,
            r,
            &TextStyle::new(
                10.5,
                if db > -0.1 {
                    Color::hex(0xff5a4a)
                } else {
                    AMBER
                },
            )
            .weight(FontWeight::Bold)
            .family(faderframe_ui_canvas::FontFamily::Mono)
            .center(),
        );
    }
}

// --- parts -------------------------------------------------------------------------

/// Lettering cut into the panel: its shadow a pixel down, then the letters.
fn engraved(
    p: &mut dyn Painter,
    text: &str,
    r: Rect,
    size: f32,
    tracking: f32,
    colour: Color,
    centre: bool,
) {
    let style = TextStyle::new(size, colour)
        .weight(FontWeight::Bold)
        .tracking(tracking);
    let style = if centre { style.center() } else { style };
    p.text(text, r.translate(0.0, 1.0), &style.color(black(0.6)));
    p.text(text, r, &style);
}

/// A screw head in a slot: a dome, its cross cut at `cut` degrees.
fn screw(p: &mut dyn Painter, c: Point, r: f32, cut: f32) {
    let rect = circle_rect(c, r);
    p.shadow(rect, r, black(0.7), 1.0, 1.5, 3.0);
    p.fill_rounded(
        rect,
        r,
        &radial(
            Point::new(c.x - r * 0.4, c.y - r * 0.5),
            r * 1.6,
            vec![
                (0.0, Color::hex(0xf2f4f6)),
                (0.5, Color::hex(0xa4aab0)),
                (1.0, Color::hex(0x4b5056)),
            ],
        ),
    );
    for a in [cut, cut + 90.0] {
        let (p0, p1) = (polar(c, r * 0.75, a), polar(c, r * 0.75, a + 180.0));
        p.line(p0, p1, 1.6, black(0.65));
        p.line(p0.offset(0.0, 0.8), p1.offset(0.0, 0.8), 0.6, white(0.35));
    }
}

/// A knob at `angle` (degrees): a knurled skirt, a domed cap, an
/// aluminium insert and the pointer, lit from the top left.
fn knob(p: &mut dyn Painter, c: Point, r: f32, angle: f32) {
    let rect = circle_rect(c, r);
    p.shadow(rect, r, black(0.75), 3.0, 6.0, 12.0);
    // The skirt and its knurling (it turns with the knob).
    p.fill_rounded(
        rect,
        r,
        &radial(
            Point::new(c.x - r * 0.35, c.y - r * 0.45),
            r * 1.5,
            vec![(0.0, Color::hex(0x3b3f45)), (1.0, Color::hex(0x060708))],
        ),
    );
    let ridges = if r > 40.0 { 60 } else { 40 };
    for i in 0..ridges {
        let a = angle + i as f32 * 360.0 / ridges as f32;
        // Ridges facing the light are lighter.
        let facing = ((a - 225.0).to_radians().cos() * 0.5 + 0.5).clamp(0.0, 1.0);
        p.line(
            polar(c, r - 5.5, a),
            polar(c, r - 0.8, a),
            1.1,
            white(0.05 + 0.22 * facing),
        );
    }
    p.stroke_rounded(rect, r, 1.0, black(0.8));
    // The cap.
    let cap_r = r * 0.8;
    let cap = circle_rect(c, cap_r);
    p.shadow(cap, cap_r, black(0.6), 1.0, 2.0, 4.0);
    p.fill_rounded(
        cap,
        cap_r,
        &radial(
            Point::new(c.x - cap_r * 0.4, c.y - cap_r * 0.55),
            cap_r * 1.7,
            vec![
                (0.0, Color::hex(0x4a4f56)),
                (0.45, Color::hex(0x24272c)),
                (1.0, Color::hex(0x0d0e10)),
            ],
        ),
    );
    let mut rim = Path::new();
    rim.arc(
        c,
        cap_r - 0.5,
        200f32.to_radians(),
        290f32.to_radians(),
        false,
    );
    p.stroke_path(&rim, 1.2, white(0.25));
    // The aluminium insert: brushed in circles, a highlight across.
    let ins_r = r * 0.36;
    let insert = circle_rect(c, ins_r);
    p.fill_rounded(
        insert,
        ins_r,
        &radial(
            Point::new(c.x - ins_r * 0.3, c.y - ins_r * 0.4),
            ins_r * 1.5,
            vec![
                (0.0, Color::hex(0xeef0f2)),
                (0.6, Color::hex(0xaab0b6)),
                (1.0, Color::hex(0x6d7379)),
            ],
        ),
    );
    let rings = 6;
    for i in 1..rings {
        let rr = ins_r * i as f32 / rings as f32;
        p.stroke_rounded(
            circle_rect(c, rr),
            rr,
            0.6,
            if i % 2 == 0 { white(0.18) } else { black(0.08) },
        );
    }
    p.stroke_rounded(insert, ins_r, 1.0, black(0.45));
    // The pointer: a white line on the cap, its shadow under it.
    let (p0, p1) = (polar(c, ins_r + 3.0, angle), polar(c, cap_r - 3.0, angle));
    p.line(p0.offset(0.8, 1.2), p1.offset(0.8, 1.2), 3.4, black(0.6));
    p.line(p0, p1, 3.0, Color::hex(0xf4f5f6));
}

/// Dark glass for a figure.
fn glass(p: &mut dyn Painter, r: Rect) {
    p.fill_rounded(r, 4.0, &Paint::Solid(Color::hex(0x0a0907)));
    p.inset_shadow(r, 4.0, black(1.0), 0.0, 2.0, 5.0);
    p.stroke_rounded(r.inset(-1.0), 5.0, 1.0, white(0.09));
    p.fill_rect(
        Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h * 0.4),
        &linear(
            0.0,
            r.y,
            0.0,
            r.y + r.h * 0.4,
            vec![(0.0, white(0.07)), (1.0, white(0.0))],
        ),
    );
}

/// A push-button: a black cap in a recess with a lamp slit along its top;
/// in, the cap sits lower and darker and its lamp glows (while the unit
/// has power).
fn push_button(p: &mut dyn Painter, r: Rect, text: &str, on: bool, power: bool) {
    // The recess.
    let well = r.inset(-3.0);
    p.fill_rounded(well, 7.0, &Paint::Solid(Color::hex(0x050607)));
    p.inset_shadow(well, 7.0, black(1.0), 0.0, 2.0, 4.0);
    p.stroke_rounded(well.inset(-0.5), 7.5, 1.0, white(0.07));
    let cap = if on { r.translate(0.0, 1.5) } else { r };
    if !on {
        p.shadow(cap, 5.0, black(0.8), 0.0, 3.0, 4.0);
    }
    let (top, bottom) = if on {
        (Color::hex(0x24272b), Color::hex(0x15171a))
    } else {
        (Color::hex(0x41454c), Color::hex(0x1d2024))
    };
    p.fill_rounded(
        cap,
        5.0,
        &linear(
            0.0,
            cap.y,
            0.0,
            cap.bottom(),
            vec![(0.0, top), (1.0, bottom)],
        ),
    );
    // The bevel: light on top, shade below.
    p.fill(
        Rect::new(cap.x + 4.0, cap.y + 0.5, cap.w - 8.0, 1.0),
        white(if on { 0.08 } else { 0.22 }),
    );
    p.fill(
        Rect::new(cap.x + 4.0, cap.bottom() - 1.5, cap.w - 8.0, 1.0),
        black(0.5),
    );
    if on {
        p.inset_shadow(cap, 5.0, black(0.6), 0.0, 2.0, 3.0);
    }
    // The lamp.
    let lamp = Rect::new(cap.x + 12.0, cap.y + 6.0, cap.w - 24.0, 4.0);
    if on && power {
        p.fill_rounded(
            lamp.inset_xy(-10.0, -7.0),
            8.0,
            &radial(
                lamp.center(),
                lamp.w * 0.8,
                vec![(0.0, AMBER.with_alpha(0.45)), (1.0, AMBER.with_alpha(0.0))],
            ),
        );
        p.fill_rounded(
            lamp,
            2.0,
            &linear(
                0.0,
                lamp.y,
                0.0,
                lamp.bottom(),
                vec![(0.0, Color::hex(0xfff1c8)), (1.0, AMBER)],
            ),
        );
    } else {
        p.fill_rounded(lamp, 2.0, &Paint::Solid(Color::hex(0x2a1e10)));
        p.fill(Rect::new(lamp.x, lamp.y, lamp.w, 1.0), black(0.5));
    }
    // The engraving.
    let label = Rect::new(cap.x, cap.y + 12.0, cap.w, cap.h - 14.0);
    let size = if text.len() > 2 { 12.0 } else { 15.0 };
    let style = TextStyle::new(size, if on { LETTER } else { LETTER.darken(0.08) })
        .weight(FontWeight::Bold)
        .tracking(if text.len() > 2 { 1.2 } else { 0.0 })
        .center();
    p.text(text, label.translate(0.0, 1.0), &style.color(black(0.7)));
    p.text(text, label, &style);
}

/// A small push-button for the strip: its text lit when in.
fn small_button(p: &mut dyn Painter, r: Rect, text: &str, on: bool) {
    p.fill_rounded(r.inset(-2.0), 6.0, &Paint::Solid(Color::hex(0x050607)));
    let cap = if on { r.translate(0.0, 1.0) } else { r };
    let (top, bottom) = if on {
        (Color::hex(0x2a2d31), Color::hex(0x17191c))
    } else {
        (Color::hex(0x3d4147), Color::hex(0x1e2125))
    };
    if !on {
        p.shadow(cap, 4.0, black(0.7), 0.0, 2.0, 3.0);
    }
    p.fill_rounded(
        cap,
        4.0,
        &linear(
            0.0,
            cap.y,
            0.0,
            cap.bottom(),
            vec![(0.0, top), (1.0, bottom)],
        ),
    );
    p.fill(
        Rect::new(cap.x + 3.0, cap.y + 0.5, cap.w - 6.0, 1.0),
        white(if on { 0.06 } else { 0.2 }),
    );
    if on {
        p.fill_rounded(
            cap.inset_xy(-6.0, -4.0),
            8.0,
            &radial(
                cap.center(),
                cap.w * 0.6,
                vec![(0.0, AMBER.with_alpha(0.16)), (1.0, AMBER.with_alpha(0.0))],
            ),
        );
    }
    p.text(
        text,
        cap,
        &TextStyle::new(9.5, if on { AMBER } else { LETTER_DIM })
            .weight(FontWeight::Bold)
            .tracking(1.0)
            .center(),
    );
}

impl CanvasView<Session, Action> for Fet76View {
    /// Meters redrawn every frame.
    fn dense(&self) -> bool {
        true
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        p.fill(Rect::from_size(size), theme.ui.background);
        let tap = self.device.tap(model);
        if let Some(t) = &tap {
            t.watch();
        }
        let now = Instant::now();
        let dt = self
            .last_frame
            .replace(now)
            .map_or(0.0, |last| (now - last).as_secs_f32().min(0.25));
        let (ox, oy, s) = Self::fit(size);
        p.push_transform(ox, oy + HEADER_H * s, s);
        self.paint_header(p, model);
        self.paint_faceplate(p);
        self.paint_lettering(p);
        for k in &KNOBS {
            self.paint_knob(p, model, k);
        }
        self.paint_buttons(p, model);
        self.paint_gr_readout(p, model, tap.as_deref());
        self.paint_ladder(p, tap.as_deref(), false, dt);
        self.paint_vu(p, model, tap.as_deref(), dt);
        self.paint_ladder(p, tap.as_deref(), true, dt);
        p.pop_transform();
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match ev {
            ViewEvent::PointerDown {
                pos,
                button,
                clicks,
                modifiers,
            } => {
                let (at, _) = Self::to_panel(size, *pos);
                let Some(hit) = self.hit(at) else {
                    return false;
                };
                match (hit, button) {
                    (Hit::Knob(i), PointerButton::Primary) => {
                        let k = KNOBS[i];
                        if *clicks >= 2 {
                            self.set_once(model, cx, k.param, Self::info(k.param).default);
                        } else {
                            self.device.begin(cx, "76 Compressor");
                            self.drag = Some(Drag::Knob {
                                param: k.param,
                                t: self.normalized(model, k.param),
                                y: pos.y,
                            });
                        }
                    }
                    (Hit::Ratio(n), PointerButton::Primary) => {
                        // Alone: the others come out. With Shift or Ctrl:
                        // pressed in with the others, or let out.
                        let bit = 1u8 << RATIO_BIT[n];
                        let mask = fet::buttons(self.value(model, id::RATIO));
                        let mask = if modifiers.shift || modifiers.ctrl {
                            mask ^ bit
                        } else {
                            bit
                        };
                        self.set_once(model, cx, id::RATIO, f64::from(mask));
                    }
                    (Hit::MeterMode(n), PointerButton::Primary) => {
                        self.set_once(model, cx, id::METER, METER_OF_BUTTON[n] as f64);
                    }
                    (Hit::Filter(i), PointerButton::Primary) => {
                        self.set_once(model, cx, id::SC_HPF, i as f64);
                    }
                    (Hit::Link, PointerButton::Primary) => {
                        let on = self.value(model, id::LINK) >= 0.5;
                        self.set_once(model, cx, id::LINK, if on { 0.0 } else { 1.0 });
                    }
                    (Hit::Mix, PointerButton::Primary) => {
                        if *clicks >= 2 {
                            self.set_once(model, cx, id::MIX, 1.0);
                        } else {
                            self.device.begin(cx, "76 Compressor");
                            let t = mix_track();
                            let v = ((at.x - t.x) / t.w).clamp(0.0, 1.0);
                            self.device
                                .set(model, cx, ParameterId(id::MIX), f64::from(v));
                            self.drag = Some(Drag::Mix);
                        }
                    }
                    (Hit::Peak(output), PointerButton::Primary) => {
                        if let Some(t) = self.device.tap(model) {
                            if output {
                                t.meter_out.clear_held();
                            } else {
                                t.meter_in.clear_held();
                            }
                        }
                        cx.redraw();
                    }
                    (hit, PointerButton::Secondary) => {
                        let param = match hit {
                            Hit::Knob(i) => KNOBS[i].param,
                            Hit::Ratio(_) => id::RATIO,
                            Hit::MeterMode(_) => id::METER,
                            Hit::Filter(_) => id::SC_HPF,
                            Hit::Link => id::LINK,
                            Hit::Mix => id::MIX,
                            Hit::Peak(_) => return false,
                        };
                        if let Some(req) = self.control_menu(model, param, *pos) {
                            cx.request(req);
                        }
                    }
                    _ => return false,
                }
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                let (at, s) = Self::to_panel(size, *pos);
                let device = self.device;
                match &mut self.drag {
                    Some(Drag::Knob { param, t, y }) => {
                        let speed = if modifiers.shift { FINE } else { 1.0 };
                        *t = (*t + (*y - pos.y) / (DRAG_RANGE * s) * speed).clamp(0.0, 1.0);
                        *y = pos.y;
                        let v = Self::from_normalized(*param, *t);
                        device.set(model, cx, ParameterId(*param), v);
                        true
                    }
                    Some(Drag::Mix) => {
                        let t = mix_track();
                        let v = ((at.x - t.x) / t.w).clamp(0.0, 1.0);
                        device.set(model, cx, ParameterId(id::MIX), f64::from(v));
                        true
                    }
                    None => false,
                }
            }
            ViewEvent::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    self.device.end(cx);
                    true
                } else {
                    false
                }
            }
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => {
                let (at, _) = Self::to_panel(size, *pos);
                let up = -dy.signum();
                match self.hit(at) {
                    Some(Hit::Knob(i)) => {
                        let k = KNOBS[i];
                        let step = if modifiers.shift { 0.005 } else { 0.02 };
                        let t = (self.normalized(model, k.param) + up * step).clamp(0.0, 1.0);
                        self.set_once(model, cx, k.param, Self::from_normalized(k.param, t));
                        true
                    }
                    Some(Hit::Mix) => {
                        let v = (self.value(model, id::MIX) + f64::from(up) * 0.02).clamp(0.0, 1.0);
                        self.set_once(model, cx, id::MIX, v);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        // The meters follow the audio.
        true
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let (at, _) = Self::to_panel(size, pos);
        let named = |param: u32| {
            let info = Self::info(param);
            format!(
                "{}: {}",
                info.name,
                Self::text_of(param, self.value(model, param))
            )
        };
        Some(match self.hit(at)? {
            Hit::Knob(i) => named(KNOBS[i].param),
            Hit::Ratio(n) => format!(
                "{}:1 — Shift-click presses it in with the others (all four: the \
                 all-buttons sound). In now: {}",
                RATIO_TEXT[n],
                fet::buttons_name(fet::buttons(self.value(model, id::RATIO)))
            ),
            Hit::MeterMode(n) => match n {
                0 => "The VU meter shows the gain reduction".into(),
                1 => "The VU meter shows the output, 0 VU at −14 dBFS".into(),
                2 => "The VU meter shows the output, 0 VU at −18 dBFS".into(),
                _ => "Off: the unit's power switch (nothing comes through)".into(),
            },
            Hit::Peak(output) => format!(
                "{} peak (dBFS); click to clear the held figure",
                if output { "Output" } else { "Input" }
            ),
            Hit::Mix => named(id::MIX),
            Hit::Filter(_) => "Sidechain high-pass: the lows do not drive the compression".into(),
            Hit::Link => "Stereo link: one reduction for both sides".into(),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(PANEL_W * 0.5, TOTAL_H * 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_needle_moves_as_a_vu_meter_does() {
        let mut n = Needle::default();
        let mut t = 0.0;
        let mut reached = None;
        let mut most = 0.0f32;
        while t < 1.5 {
            n.follow(1.0, 0.001);
            t += 0.001;
            if reached.is_none() && n.pos >= 0.99 {
                reached = Some(t);
            }
            most = most.max(n.pos);
        }
        let reached = reached.unwrap_or(9.0);
        assert!((0.27..0.33).contains(&reached), "99 % after {reached} s");
        assert!(most > 1.005 && most < 1.02, "overshoot {most}");
        // Frames of any length come to the same.
        let (mut a, mut b) = (Needle::default(), Needle::default());
        for _ in 0..30 {
            a.follow(0.5, 1.0 / 60.0);
        }
        for _ in 0..120 {
            b.follow(0.5, 1.0 / 240.0);
        }
        assert!((a.pos - b.pos).abs() < 1e-3);
    }

    #[test]
    fn the_scale_puts_zero_vu_at_seventy_one_percent() {
        assert!((deflection(0.0) - 0.708).abs() < 0.001);
        assert!((deflection(3.0) - 1.0).abs() < 1e-6);
        assert!(deflection(-20.0) < 0.08);
        // The knobs sweep from the lower left round to the lower right.
        assert_eq!(knob_angle(0.0), 130.0);
        assert_eq!(knob_angle(1.0), 410.0);
    }
}
