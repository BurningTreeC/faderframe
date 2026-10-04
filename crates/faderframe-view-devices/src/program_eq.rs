//! The Program EQ's front panel: PultEQFx's faceplate by Simon Huber (used
//! in FaderFrame under the MIT licence), drawn with FaderFrame's painter.
//!
//! Everything is laid out in the panel's own coordinates — a 19 inch rack
//! panel of 1160 × 322 with a 34 high strip above it for the oversampling
//! — and scaled to the window. The panel is lit from one lamp near its top
//! left corner: the enamel's highlight, every contact shadow (down and to
//! the right) and the tint of each rendered part (fainter the further from
//! the lamp) agree with the light baked into the renders. Knobs are
//! filmstrips (a frame per position, never a rotated picture, which would
//! carry the light round); switches are one render with the pointer drawn
//! on top.
//!
//! Knobs: drag up and down (Shift: finer), scroll, double-click or
//! right-click for the default. Switches: drag, scroll, or click a position
//! engraved round them; the two way ones (EQ IN/OUT, OFF/ON) also throw
//! with a click. Clicking a meter or its PEAK figure clears it.

use crate::common::Device;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::program_eq::{
    HIGH_ATTEN_LABELS, HIGH_BOOST_LABELS, LOW_FREQ_LABELS, OVERSAMPLING_LABELS, param, parameters,
};
use faderframe_plugin_host::tap::{self, AnalysisTap, Meter};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FontWeight, Image, Paint, Painter, Path, Point, PointerButton,
    Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::cell::Cell;
use std::time::Instant;

// --- geometry (panel coordinates) ---------------------------------------------

pub const PANEL_W: f32 = 1160.0;
pub const PANEL_H: f32 = 322.0;
/// The strip above the panel.
pub const HEADER_H: f32 = 34.0;
pub const TOTAL_H: f32 = PANEL_H + HEADER_H;

const TOP_ROW: f32 = 88.0;
const BOTTOM_ROW: f32 = 234.0;
const R_LARGE: f32 = 34.0;
const R_SELECTOR: f32 = 22.0;
const R_SMALL: f32 = 19.0;
const R_TRIM: f32 = 15.0;
const SCALE_RADIUS: f32 = 48.0;
const SELECTOR_RADIUS: f32 = 54.0;
/// A knob sweeps 250 degrees, zero at the lower left.
const SWEEP: f32 = 250.0;

const LOW_BOOST_X: f32 = 330.0;
const LOW_ATTEN_X: f32 = 488.0;
const HIGH_BOOST_X: f32 = 683.0;
const HIGH_ATTEN_X: f32 = 831.0;
const ATTEN_SEL_X: f32 = 965.0;
const EQ_SWITCH_X: f32 = 293.0;
const LOW_FREQ_X: f32 = 400.0;
const BANDWIDTH_X: f32 = 580.0;
const HIGH_FREQ_X: f32 = 754.0;
const POWER_X: f32 = 985.0;
const NAMEPLATE_X: f32 = 154.0;
const LAMP_X: f32 = 934.0;
const LAMP_Y: f32 = 167.0;
const INPUT_METER_X: f32 = 100.0;
const OUTPUT_METER_X: f32 = 1071.0;
const METER_TOP: f32 = 34.0;
const TRIM_X: f32 = 1112.0;
const DRIVE_Y: f32 = 92.0;
const OUTPUT_TRIM_Y: f32 = 182.0;
const METER_SCALE_GAP: f32 = 13.0;
const METER_NUMERAL_W: f32 = 22.0;
const READOUT_W: f32 = 48.0;
const READOUT_H: f32 = 16.0;
const PEAK_CAPTION_Y: f32 = 251.0;
const PEAK_READOUT_Y: f32 = 257.0;
const RMS_CAPTION_Y: f32 = 283.0;
const RMS_READOUT_Y: f32 = 289.0;
const LABEL_H: f32 = 18.0;
const NUMERAL_W: f32 = 26.0;
const WORD_W: f32 = 30.0;
const TRIM_TEXT_W: f32 = 56.0;
const SCREW_X: [f32; 2] = [0.0330, 0.9670];
const HARDWARE_Y: [f32; 2] = [0.135, 0.865];
const SCREW_SIZE: f32 = 15.0;

// The meter window.
const METER_W: f32 = 26.0;
const METER_H: f32 = 206.0;
const INSET: f32 = 3.0;
const CLIP_H: f32 = 5.0;
const CLIP_GAP: f32 = 2.0;
const SCALE_TOP: f32 = INSET + CLIP_H + CLIP_GAP;
const SCALE_H: f32 = METER_H - SCALE_TOP - INSET;
const TICKS: [f32; 12] = [
    6.0, 3.0, 0.0, -3.0, -6.0, -9.0, -12.0, -18.0, -24.0, -30.0, -40.0, -60.0,
];
const FALL: f32 = 20.0 / 1.7;
const HOLD: f32 = 2.0;
const FLOOR: f32 = -70.0;

// --- colours ---------------------------------------------------------------------

const PANEL_TOP: u32 = 0x365660;
const PANEL_BOTTOM: u32 = 0x1e353c;
const LETTER: u32 = 0xeaecf0;
const NUMERAL: u32 = 0xeff1f3;
const GREEN: u32 = 0x39c95c;
const YELLOW: u32 = 0xe2cf3c;
const ORANGE: u32 = 0xf29a2e;
const RED: u32 = 0xef4136;

fn rgb(hex: u32) -> Color {
    Color::hex(hex)
}

fn rgba(hex: u32, a: f32) -> Color {
    Color::hex(hex).with_alpha(a)
}

// --- the renders -------------------------------------------------------------------

const KNOB_LARGE: Image = Image {
    key: "program-eq/knob_large",
    png: include_bytes!("../assets/program-eq/knob_large.png"),
    width: 176,
    height: 8448,
};
const KNOB_FRAMES: usize = 48;
const KNOB_LARGE_SPAN: f32 = 0.9852;
const KNOB_LARGE_DRAW: f32 = 2.50;
const KNOB_METAL: Image = Image {
    key: "program-eq/knob_metal",
    png: include_bytes!("../assets/program-eq/knob_metal.png"),
    width: 320,
    height: 320,
};
const KNOB_METAL_SPAN: f32 = 0.9804;
const KNOB_METAL_DRAW: f32 = 2.44;
const LAMP_LIT: Image = Image {
    key: "program-eq/lamp_lit",
    png: include_bytes!("../assets/program-eq/lamp_lit.png"),
    width: 256,
    height: 256,
};
const LAMP_DARK: Image = Image {
    key: "program-eq/lamp_dark",
    png: include_bytes!("../assets/program-eq/lamp_dark.png"),
    width: 256,
    height: 256,
};
const LAMP_SPAN: f32 = 0.9434;
const LAMP_DRAW: f32 = 2.30;
const SCREWS: [Image; 4] = [
    Image {
        key: "program-eq/screw_1",
        png: include_bytes!("../assets/program-eq/screw_1.png"),
        width: 288,
        height: 288,
    },
    Image {
        key: "program-eq/screw_2",
        png: include_bytes!("../assets/program-eq/screw_2.png"),
        width: 288,
        height: 288,
    },
    Image {
        key: "program-eq/screw_3",
        png: include_bytes!("../assets/program-eq/screw_3.png"),
        width: 288,
        height: 288,
    },
    Image {
        key: "program-eq/screw_4",
        png: include_bytes!("../assets/program-eq/screw_4.png"),
        width: 288,
        height: 288,
    },
];

// --- the light ---------------------------------------------------------------------

/// Offsets a shadow takes, in multiples of the caster's radius.
const SHADOW_X: f32 = 0.11;
const SHADOW_Y: f32 = 0.15;
const LIGHT_X: f32 = PANEL_W * 0.045;
const LIGHT_Y: f32 = -PANEL_H * 0.12;

/// How much of the lamp's light reaches a point of the panel.
pub fn light_at(x: f32, y: f32) -> f32 {
    const REACH: f32 = 1165.0;
    const FALL_OFF: f32 = 0.26;
    let d = ((x - LIGHT_X).powi(2) + (y - LIGHT_Y).powi(2)).sqrt();
    1.0 - FALL_OFF * (d / REACH).clamp(0.0, 1.0)
}

/// Position on a circle, angles clockwise from twelve o'clock.
fn polar(cx: f32, cy: f32, r: f32, degrees: f32) -> (f32, f32) {
    let a = degrees.to_radians();
    (cx + r * a.sin(), cy - r * a.cos())
}

fn knob_angle(normalized: f32) -> f32 {
    (normalized - 0.5) * SWEEP
}

/// Detent angles of an `n` position switch (a two way one throws to 45°).
pub fn selector_angle(index: usize, count: usize) -> f32 {
    if count < 2 {
        return 0.0;
    }
    let step = if count == 2 {
        90.0
    } else {
        32.0_f32.min(120.0 / (count - 1) as f32)
    };
    -step * (count - 1) as f32 / 2.0 + step * index as f32
}

fn radial(cx: f32, cy: f32, inner: f32, outer: f32, c0: Color, c1: Color) -> Paint {
    Paint::Radial {
        center: Point::new(cx, cy),
        radius: outer,
        stops: vec![(0.0, c0), ((inner / outer).clamp(0.0, 1.0), c0), (1.0, c1)],
    }
}

fn linear(x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Color)>) -> Paint {
    Paint::Linear {
        start: Point::new(x0, y0),
        end: Point::new(x1, y1),
        stops,
    }
}

fn line(p: &mut dyn Painter, a: (f32, f32), b: (f32, f32), width: f32, color: Color) {
    let mut path = Path::new();
    path.move_to(Point::new(a.0, a.1))
        .line_to(Point::new(b.0, b.1));
    p.stroke_path(&path, width, color);
}

/// The shadow a control casts onto the panel.
fn contact_shadow(p: &mut dyn Painter, cx: f32, cy: f32, r: f32) {
    let (ox, oy) = (cx + r * SHADOW_X, cy + r * SHADOW_Y);
    p.fill_path_paint(
        &Path::ellipse(Point::new(ox, oy), r * 1.20, r * 1.14),
        &radial(ox, oy, r * 0.72, r * 1.20, rgba(0, 0.60), rgba(0, 0.0)),
    );
}

/// The pointer painted on a switch knob, with its groove's shadow.
fn switch_pointer(p: &mut dyn Painter, cx: f32, cy: f32, r: f32, angle: f32, bar: bool) {
    let (sa, ca) = angle.to_radians().sin_cos();
    let (from, to) = if bar { (0.26, 0.94) } else { (0.42, 0.94) };
    let at = |t: f32| (cx + r * t * sa, cy - r * t * ca);
    let (a, b) = (at(from), at(to));
    let drop = r * 0.045;
    line(
        p,
        (a.0 + drop, a.1 + drop),
        (b.0 + drop, b.1 + drop),
        r * 0.185,
        rgba(0, 0.55),
    );
    line(p, a, b, r * 0.185, rgba(0x101112, 0.92));
    line(p, a, b, r * 0.095, rgb(0xf6f3ec));
}

/// A square render centred on a point, `height` high.
fn sprite(p: &mut dyn Painter, image: &Image, src: Rect, cx: f32, cy: f32, height: f32, lit: f32) {
    let w = height * src.w / src.h;
    p.image(
        image,
        src,
        Rect::new(cx - w / 2.0, cy - height / 2.0, w, height),
        lit,
    );
}

fn whole(image: &Image) -> Rect {
    Rect::new(0.0, 0.0, image.width as f32, image.height as f32)
}

// --- lettering -----------------------------------------------------------------------

fn letter_style(size: f32, color: Color) -> TextStyle {
    TextStyle::new(size, color)
        .weight(FontWeight::Bold)
        .center()
}

/// Engraved lettering: the shadow half, then the lit half. `spaced` tracks
/// the letters out like the hardware's.
fn engraved(p: &mut dyn Painter, text: &str, x: f32, y: f32, size: f32, spaced: bool) {
    let width = size * text.chars().count() as f32 * 0.9 + 40.0;
    let track = if spaced { size * 0.16 } else { 0.0 };
    let r = Rect::new(x - width / 2.0, y - LABEL_H / 2.0, width, LABEL_H);
    p.text(
        text,
        r.translate(0.0, 1.0),
        &letter_style(size, rgba(0, 0.43)).tracking(track),
    );
    p.text(text, r, &letter_style(size, rgb(LETTER)).tracking(track));
}

fn numeral(p: &mut dyn Painter, text: &str, x: f32, y: f32, size: f32, width: f32) {
    let r = Rect::new(x - width / 2.0, y - LABEL_H / 2.0, width, LABEL_H);
    p.text(
        text,
        r.translate(0.0, 1.0),
        &letter_style(size, rgba(0, 0.43)),
    );
    p.text(text, r, &letter_style(size, rgb(NUMERAL)));
}

/// The nameplate block, set flush left.
fn plate(p: &mut dyn Painter, text: &str, x: f32, y: f32, size: f32) {
    let r = Rect::new(x, y - LABEL_H / 2.0, 220.0, LABEL_H);
    let style = |c| {
        TextStyle::new(size, c)
            .weight(FontWeight::Bold)
            .tracking(size * 0.16)
    };
    p.text(text, r.translate(0.0, 1.0), &style(rgba(0, 0.43)));
    p.text(text, r, &style(rgb(LETTER)));
}

// --- the meters ----------------------------------------------------------------------

/// Height on the peak meter scale (IEC 60268-18 as Ardour draws it), 0 at
/// the bottom to 1 at +6 dBFS.
pub fn deflection(db: f32) -> f32 {
    let percent = if db.is_nan() || db < -70.0 {
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
    } else if db < 6.0 {
        (db + 20.0) * 2.5 + 50.0
    } else {
        115.0
    };
    percent / 115.0
}

fn scale_y(db: f32) -> f32 {
    SCALE_TOP + SCALE_H * (1.0 - deflection(db))
}

/// A bar's ballistics: rises at once, falls 20 dB in 1.7 s, holds its
/// highest point for two seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Needle {
    pub level: f32,
    pub hold: f32,
    held_for: f32,
}

impl Default for Needle {
    fn default() -> Self {
        Self {
            level: FLOOR,
            hold: FLOOR,
            held_for: 0.0,
        }
    }
}

impl Needle {
    pub fn step(&mut self, peak: f32, dt: f32) {
        let fallen = (self.level - FALL * dt).max(FLOOR);
        self.level = if peak > fallen { peak } else { fallen };
        if self.level >= self.hold {
            self.hold = self.level;
            self.held_for = 0.0;
        } else {
            self.held_for += dt;
            if self.held_for > HOLD {
                self.hold = (self.hold - FALL * dt).max(self.level);
            }
        }
    }
}

fn scale_paint(bottom: f32, top: f32, alpha: f32) -> Paint {
    let stops = [
        (0.0, GREEN),
        (deflection(-18.0), GREEN),
        (deflection(-6.0), YELLOW),
        (deflection(-1.0), ORANGE),
        (deflection(0.0), RED),
        (1.0, RED),
    ];
    linear(
        0.0,
        bottom,
        0.0,
        top,
        stops.iter().map(|&(at, c)| (at, rgba(c, alpha))).collect(),
    )
}

// --- the controls --------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// A large knob or a trim (filmstrip).
    Knob,
    /// A rotary switch: positions, the long pointer, reversed engraving.
    Switch {
        positions: usize,
        bar: bool,
        reversed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Control {
    kind: Kind,
    param: usize,
    x: f32,
    y: f32,
    r: f32,
}

const fn knob(param: usize, x: f32, y: f32, r: f32) -> Control {
    Control {
        kind: Kind::Knob,
        param,
        x,
        y,
        r,
    }
}

const fn switch(param: usize, x: f32, y: f32, r: f32, positions: usize, bar: bool) -> Control {
    Control {
        kind: Kind::Switch {
            positions,
            bar,
            reversed: false,
        },
        param,
        x,
        y,
        r,
    }
}

const CONTROLS: [Control; 12] = [
    knob(param::LOW_BOOST, LOW_BOOST_X, TOP_ROW, R_LARGE),
    knob(param::LOW_ATTEN, LOW_ATTEN_X, TOP_ROW, R_LARGE),
    knob(param::HIGH_BOOST, HIGH_BOOST_X, TOP_ROW, R_LARGE),
    knob(param::HIGH_ATTEN, HIGH_ATTEN_X, TOP_ROW, R_LARGE),
    switch(
        param::HIGH_ATTEN_FREQ,
        ATTEN_SEL_X,
        TOP_ROW,
        R_SELECTOR,
        3,
        true,
    ),
    // IN on the left, which is the parameter's 1: engraved reversed.
    Control {
        kind: Kind::Switch {
            positions: 2,
            bar: false,
            reversed: true,
        },
        param: param::EQ_IN,
        x: EQ_SWITCH_X,
        y: BOTTOM_ROW,
        r: R_SMALL,
    },
    switch(param::LOW_FREQ, LOW_FREQ_X, BOTTOM_ROW, R_SELECTOR, 4, true),
    knob(param::BANDWIDTH, BANDWIDTH_X, BOTTOM_ROW, R_LARGE),
    switch(
        param::HIGH_BOOST_FREQ,
        HIGH_FREQ_X,
        BOTTOM_ROW,
        R_SELECTOR,
        7,
        true,
    ),
    switch(param::POWER, POWER_X, BOTTOM_ROW, R_SMALL, 2, false),
    knob(param::DRIVE, TRIM_X, DRIVE_Y, R_TRIM),
    knob(param::OUTPUT, TRIM_X, OUTPUT_TRIM_Y, R_TRIM),
];

/// A clickable engraved position: setting `value` of `param`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Detent {
    param: usize,
    value: f64,
    rect: Rect,
}

fn detents() -> Vec<Detent> {
    let mut out = Vec::new();
    let mut scale = |param: usize, x: f32, y: f32, labels: &[&str]| {
        let count = labels.len();
        let at = |i: usize| polar(x, y, SELECTOR_RADIUS, selector_angle(i, count));
        let width = if count > 1 {
            let ((x0, y0), (x1, y1)) = (at(0), at(1));
            ((x1 - x0).hypot(y1 - y0) - 1.0).min(NUMERAL_W)
        } else {
            NUMERAL_W
        };
        for i in 0..count {
            let (nx, ny) = at(i);
            out.push(Detent {
                param,
                value: i as f64,
                rect: Rect::new(nx - width / 2.0, ny - LABEL_H / 2.0, width, LABEL_H),
            });
        }
    };
    scale(
        param::HIGH_ATTEN_FREQ,
        ATTEN_SEL_X,
        TOP_ROW,
        &HIGH_ATTEN_LABELS,
    );
    scale(param::LOW_FREQ, LOW_FREQ_X, BOTTOM_ROW, &LOW_FREQ_LABELS);
    scale(
        param::HIGH_BOOST_FREQ,
        HIGH_FREQ_X,
        BOTTOM_ROW,
        &HIGH_BOOST_LABELS,
    );
    let word = |param, value, x: f32| Detent {
        param,
        value,
        rect: Rect::new(
            x - WORD_W / 2.0,
            BOTTOM_ROW - 28.0 - LABEL_H / 2.0,
            WORD_W,
            LABEL_H,
        ),
    };
    out.push(word(param::EQ_IN, 1.0, EQ_SWITCH_X - 28.0));
    out.push(word(param::EQ_IN, 0.0, EQ_SWITCH_X + 28.0));
    out.push(word(param::POWER, 0.0, POWER_X - 28.0));
    out.push(word(param::POWER, 1.0, POWER_X + 28.0));
    out
}

/// Pixels of drag (panel) for a knob's whole range.
const DRAG_RANGE: f32 = 260.0;
const FINE: f32 = 0.15;
const CLICK_SLOP: f32 = 3.0;

enum Drag {
    Knob {
        control: Control,
        normalized: f32,
        y: f32,
    },
    Switch {
        control: Control,
        index: isize,
        last_y: f32,
        pressed_y: f32,
        travel: f32,
        wandered: bool,
    },
}

/// Something the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Control(usize),
    Detent(Detent),
    Meter(bool),
    Readout(bool),
    Oversampling(usize),
}

pub struct ProgramEqView {
    device: Device,
    drag: Option<Drag>,
    needles: Cell<[[Needle; 2]; 2]>,
    last_frame: Cell<Option<Instant>>,
    detents: Vec<Detent>,
}

impl ProgramEqView {
    pub fn new(plugin: PluginInstanceId, _theme: &Theme) -> Self {
        Self {
            device: Device::new(plugin),
            drag: None,
            needles: Cell::new([[Needle::default(); 2]; 2]),
            last_frame: Cell::new(None),
            detents: detents(),
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

    fn info(param: usize) -> faderframe_plugin_host::ParameterInfo {
        parameters()
            .into_iter()
            .nth(param)
            .unwrap_or_else(|| parameters().remove(0))
    }

    fn normalized(&self, model: &Session, c: &Control) -> f32 {
        let info = Self::info(c.param);
        let v = self.device.value(model, c.param);
        ((v - info.min) / (info.max - info.min)).clamp(0.0, 1.0) as f32
    }

    fn oversampling_rects() -> [Rect; 4] {
        let w = 34.0;
        let x0 = PANEL_W - 24.0 - 4.0 * (w + 4.0);
        std::array::from_fn(|i| {
            Rect::new(
                x0 + i as f32 * (w + 4.0),
                -HEADER_H + 8.0,
                w,
                HEADER_H - 16.0,
            )
        })
    }

    fn hit(&self, at: Point) -> Option<Hit> {
        if let Some(d) = self.detents.iter().find(|d| d.rect.contains(at)) {
            return Some(Hit::Detent(*d));
        }
        for (i, c) in CONTROLS.iter().enumerate() {
            let reach = c.r * 1.25;
            if at.distance(Point::new(c.x, c.y)) <= reach {
                return Some(Hit::Control(i));
            }
        }
        for (output, x) in [(false, INPUT_METER_X), (true, OUTPUT_METER_X)] {
            let window = Rect::new(x - METER_W / 2.0, METER_TOP, METER_W, METER_H);
            if window.contains(at) {
                return Some(Hit::Meter(output));
            }
            let readout = Rect::new(x - READOUT_W / 2.0, PEAK_READOUT_Y, READOUT_W, READOUT_H);
            if readout.contains(at) {
                return Some(Hit::Readout(output));
            }
        }
        Self::oversampling_rects()
            .iter()
            .position(|r| r.contains(at))
            .map(Hit::Oversampling)
    }

    fn set(&self, model: &Session, cx: &mut EventCx<'_, Action>, param: usize, value: f64) {
        self.device.set(model, cx, ParameterId(param as u32), value);
    }

    fn set_once(&self, model: &Session, cx: &mut EventCx<'_, Action>, param: usize, value: f64) {
        self.device.begin(cx, "Program EQ");
        self.set(model, cx, param, value);
        self.device.end(cx);
    }

    fn switch_index(&self, model: &Session, c: &Control) -> isize {
        self.device.value(model, c.param).round() as isize
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
                vec![(0.0, rgb(0x1d2125)), (1.0, rgb(0x0d0f12))],
            ),
        );
        p.fill(Rect::new(0.0, -0.5, PANEL_W, 0.5), rgba(0, 0.8));
        let label = TextStyle::new(9.0, rgb(0xa8b4be))
            .weight(FontWeight::Bold)
            .tracking(1.2);
        let rects = Self::oversampling_rects();
        let caption = Rect::new(rects[0].x - 130.0, bar.y, 120.0, bar.h);
        p.text("OVERSAMPLING", caption, &label.right());
        let current = self.device.value(model, param::OVERSAMPLING).round() as usize;
        for (i, r) in rects.iter().enumerate() {
            let on = i == current;
            p.fill_rounded(
                *r,
                3.0,
                &Paint::Solid(if on { rgb(0x3a4a52) } else { rgb(0x15181b) }),
            );
            p.stroke_rounded(*r, 3.0, 1.0, rgba(0xffffff, if on { 0.22 } else { 0.08 }));
            p.text(
                OVERSAMPLING_LABELS[i],
                *r,
                &TextStyle::new(9.0, if on { rgb(0xf2f4f6) } else { rgb(0x8c98a2) })
                    .weight(FontWeight::Bold)
                    .center(),
            );
        }
        let name = TextStyle::new(10.0, rgb(0xa8b4be))
            .weight(FontWeight::Bold)
            .tracking(1.6);
        p.text(
            "FADERFRAME PROGRAM EQ",
            Rect::new(16.0, bar.y, 300.0, bar.h),
            &name,
        );
    }

    fn paint_faceplate(&self, p: &mut dyn Painter) {
        let b = Rect::new(0.0, 0.0, PANEL_W, PANEL_H);
        p.fill_rect(
            b,
            &linear(
                0.0,
                0.0,
                0.0,
                PANEL_H,
                vec![(0.0, rgb(PANEL_TOP)), (1.0, rgb(PANEL_BOTTOM))],
            ),
        );
        let (lx, ly) = (LIGHT_X, LIGHT_Y);
        p.fill_rect(
            b,
            &radial(
                lx,
                ly,
                0.0,
                PANEL_W * 0.95,
                rgba(0xffffff, 0.100),
                rgba(0xffffff, 0.0),
            ),
        );
        p.fill_rect(
            b,
            &radial(
                lx,
                ly,
                0.0,
                PANEL_H * 1.15,
                rgba(0xffffff, 0.085),
                rgba(0xffffff, 0.0),
            ),
        );
        p.fill_rect(
            b,
            &linear(
                0.0,
                0.0,
                PANEL_W,
                PANEL_H,
                vec![(0.0, rgba(0, 0.0)), (1.0, rgba(0, 0.26))],
            ),
        );
        // A very fine grain in the paint.
        let lines = (PANEL_H / 3.0) as usize;
        for i in 0..lines {
            let y = (i as f32 + 0.5) * PANEL_H / lines as f32;
            let shade = if i % 2 == 0 { 0x000000 } else { 0xffffff };
            p.fill(Rect::new(0.0, y - 0.5, PANEL_W, 1.0), rgba(shade, 0.012));
        }
        for (sx, sy, ex, ey, alpha) in [
            (0.0, 0.0, PANEL_W * 0.08, 0.0, 0.07),
            (PANEL_W, 0.0, PANEL_W * 0.92, 0.0, 0.20),
            (0.0, PANEL_H, 0.0, PANEL_H * 0.84, 0.20),
        ] {
            p.fill_rect(
                b,
                &linear(
                    sx,
                    sy,
                    ex,
                    ey,
                    vec![(0.0, rgba(0, alpha)), (1.0, rgba(0, 0.0))],
                ),
            );
        }
        p.fill_rect(
            b,
            &radial(
                PANEL_W,
                PANEL_H,
                0.0,
                PANEL_H * 1.30,
                rgba(0, 0.16),
                rgba(0, 0.0),
            ),
        );
        for (i, &sx) in SCREW_X.iter().enumerate() {
            for (j, &sy) in HARDWARE_Y.iter().enumerate() {
                let (x, y) = (PANEL_W * sx, PANEL_H * sy);
                let screw = &SCREWS[i * 2 + j];
                sprite(p, screw, whole(screw), x, y, SCREW_SIZE, light_at(x, y));
            }
        }
        // Bevelled top and bottom edges.
        p.fill(Rect::new(0.0, 0.0, PANEL_W, 2.0), rgba(0xffffff, 0.24));
        p.fill(Rect::new(0.0, PANEL_H - 2.5, PANEL_W, 2.5), rgba(0, 0.5));
    }

    fn paint_lettering(&self, p: &mut dyn Painter) {
        for (text, x) in [
            ("BOOST", LOW_BOOST_X),
            ("ATTEN", LOW_ATTEN_X),
            ("BOOST", HIGH_BOOST_X),
            ("ATTEN", HIGH_ATTEN_X),
            ("ATTEN SEL", ATTEN_SEL_X),
        ] {
            engraved(p, text, x, 21.0, 11.0, true);
        }
        for x in [LOW_BOOST_X, LOW_ATTEN_X, HIGH_BOOST_X, HIGH_ATTEN_X] {
            for i in 0..=10 {
                let (nx, ny) = polar(x, TOP_ROW, SCALE_RADIUS, knob_angle(i as f32 / 10.0));
                numeral(p, &i.to_string(), nx, ny, 9.5, NUMERAL_W);
            }
        }
        for i in 0..=10 {
            let (nx, ny) = polar(
                BANDWIDTH_X,
                BOTTOM_ROW,
                SCALE_RADIUS,
                knob_angle(i as f32 / 10.0),
            );
            numeral(p, &i.to_string(), nx, ny, 9.5, NUMERAL_W);
        }
        let selector_scale = |p: &mut dyn Painter, x: f32, y: f32, labels: &[&str]| {
            for (i, text) in labels.iter().enumerate() {
                let (nx, ny) = polar(x, y, SELECTOR_RADIUS, selector_angle(i, labels.len()));
                numeral(p, text, nx, ny, 9.5, NUMERAL_W);
            }
        };
        selector_scale(p, ATTEN_SEL_X, TOP_ROW, &HIGH_ATTEN_LABELS);
        selector_scale(p, LOW_FREQ_X, BOTTOM_ROW, &LOW_FREQ_LABELS);
        selector_scale(p, HIGH_FREQ_X, BOTTOM_ROW, &HIGH_BOOST_LABELS);
        engraved(p, "IN", EQ_SWITCH_X - 28.0, BOTTOM_ROW - 28.0, 9.0, false);
        engraved(p, "OUT", EQ_SWITCH_X + 28.0, BOTTOM_ROW - 28.0, 9.0, false);
        engraved(p, "CPS", LOW_FREQ_X, 172.0, 10.0, true);
        engraved(p, "LOW FREQUENCY", LOW_FREQ_X, 302.0, 11.0, true);
        engraved(p, "SHARP", BANDWIDTH_X - 74.0, 286.0, 8.5, false);
        engraved(p, "BROAD", BANDWIDTH_X + 74.0, 286.0, 8.5, false);
        engraved(p, "BANDWIDTH", BANDWIDTH_X, 305.0, 11.0, true);
        engraved(p, "KCS", HIGH_FREQ_X, 172.0, 10.0, true);
        engraved(p, "HIGH FREQUENCY", HIGH_FREQ_X, 302.0, 11.0, true);
        engraved(p, "OFF", POWER_X - 28.0, BOTTOM_ROW - 28.0, 9.0, false);
        engraved(p, "ON", POWER_X + 28.0, BOTTOM_ROW - 28.0, 9.0, false);
        plate(p, "PROGRAM EQ", NAMEPLATE_X, 126.0, 11.0);
        plate(
            p,
            concat!("V", env!("CARGO_PKG_VERSION")),
            NAMEPLATE_X,
            142.0,
            7.5,
        );
        plate(p, "TUBE PROGRAM EQUALIZER", NAMEPLATE_X, 158.0, 11.0);
        plate(p, "FADERFRAME", NAMEPLATE_X, 177.0, 11.0);
    }

    fn paint_control(&self, p: &mut dyn Painter, model: &Session, c: &Control) {
        match c.kind {
            Kind::Knob => {
                let n = self.normalized(model, c);
                let frame = (n * (KNOB_FRAMES - 1) as f32).round() as usize;
                let body = c.r * KNOB_LARGE_DRAW / 2.0;
                contact_shadow(p, c.x, c.y, body);
                let cell = KNOB_LARGE.height as f32 / KNOB_FRAMES as f32;
                let src = Rect::new(0.0, frame as f32 * cell, KNOB_LARGE.width as f32, cell);
                sprite(
                    p,
                    &KNOB_LARGE,
                    src,
                    c.x,
                    c.y,
                    c.r * KNOB_LARGE_DRAW / KNOB_LARGE_SPAN,
                    light_at(c.x, c.y),
                );
            }
            Kind::Switch {
                positions,
                bar,
                reversed,
            } => {
                let index = self.switch_index(model, c).clamp(0, positions as isize - 1) as usize;
                let shown = if reversed {
                    positions - 1 - index
                } else {
                    index
                };
                let body = c.r * KNOB_METAL_DRAW / 2.0;
                contact_shadow(p, c.x, c.y, body);
                sprite(
                    p,
                    &KNOB_METAL,
                    whole(&KNOB_METAL),
                    c.x,
                    c.y,
                    c.r * KNOB_METAL_DRAW / KNOB_METAL_SPAN,
                    light_at(c.x, c.y),
                );
                switch_pointer(p, c.x, c.y, body, selector_angle(shown, positions), bar);
            }
        }
    }

    fn paint_trim_lettering(&self, p: &mut dyn Painter, model: &Session) {
        for (name, param, y) in [
            ("DRIVE", param::DRIVE, DRIVE_Y),
            ("OUTPUT", param::OUTPUT, OUTPUT_TRIM_Y),
        ] {
            let below = y + R_TRIM * KNOB_LARGE_DRAW / 2.0;
            engraved(p, name, TRIM_X, below + 11.0, 8.0, false);
            let v = self.device.value(model, param);
            let text = format!("{:.1} dB", v);
            let r = Rect::new(
                TRIM_X - TRIM_TEXT_W / 2.0,
                below + 23.0 - LABEL_H / 2.0,
                TRIM_TEXT_W,
                LABEL_H,
            );
            p.text(
                &text,
                r.translate(0.0, 1.0),
                &letter_style(8.0, rgba(0, 0.43)),
            );
            p.text(&text, r, &letter_style(8.0, rgb(LETTER)));
        }
    }

    fn paint_lamp(&self, p: &mut dyn Painter, model: &Session) {
        let lit = self.device.value(model, param::POWER) >= 0.5;
        let r = 14.0;
        let body = r * LAMP_DRAW / 2.0;
        if lit {
            p.fill_path_paint(
                &Path::circle(Point::new(LAMP_X, LAMP_Y), body * 2.4),
                &radial(
                    LAMP_X,
                    LAMP_Y,
                    body * 0.85,
                    body * 2.4,
                    rgba(0xff3a18, 0.30),
                    rgba(0xff3a18, 0.0),
                ),
            );
        }
        contact_shadow(p, LAMP_X, LAMP_Y, body);
        let image = if lit { &LAMP_LIT } else { &LAMP_DARK };
        sprite(
            p,
            image,
            whole(image),
            LAMP_X,
            LAMP_Y,
            r * LAMP_DRAW / LAMP_SPAN,
            if lit { 1.0 } else { light_at(LAMP_X, LAMP_Y) },
        );
    }

    fn paint_meter(
        &self,
        p: &mut dyn Painter,
        tap: Option<&AnalysisTap>,
        output: bool,
        needles: &mut [Needle; 2],
        dt: f32,
    ) {
        let x = if output {
            OUTPUT_METER_X
        } else {
            INPUT_METER_X
        };
        engraved(
            p,
            if output { "OUTPUT" } else { "INPUT" },
            x,
            21.0,
            11.0,
            true,
        );
        let b = Rect::new(x - METER_W / 2.0, METER_TOP, METER_W, METER_H);
        p.fill_rounded(b, 3.0, &Paint::Solid(rgb(0x0a0c0e)));
        p.fill_rounded(
            b,
            3.0,
            &linear(
                0.0,
                b.y,
                0.0,
                b.y + 8.0,
                vec![(0.0, rgba(0, 0.55)), (1.0, rgba(0, 0.0))],
            ),
        );
        line(
            p,
            (b.right() + 0.5, b.y + 3.0),
            (b.right() + 0.5, b.bottom() + 0.5),
            1.0,
            rgba(0xffffff, 0.16),
        );
        line(
            p,
            (b.x + 3.0, b.bottom() + 0.5),
            (b.right() + 0.5, b.bottom() + 0.5),
            1.0,
            rgba(0xffffff, 0.16),
        );
        line(
            p,
            (b.x - 0.5, b.bottom() - 3.0),
            (b.x - 0.5, b.y - 0.5),
            1.0,
            rgba(0, 0.45),
        );
        line(
            p,
            (b.x - 0.5, b.y - 0.5),
            (b.right() - 3.0, b.y - 0.5),
            1.0,
            rgba(0, 0.45),
        );
        let meter: Option<&Meter> = tap.map(|t| if output { &t.meter_out } else { &t.meter_in });
        let (gap, channels) = (2.0, 2usize);
        let span = METER_W - 2.0 * INSET;
        let bar_w = (span - gap) / channels as f32;
        let top = b.y + SCALE_TOP;
        let bottom = top + SCALE_H;
        let full = scale_paint(bottom, top, 1.0);
        let faint = scale_paint(bottom, top, 0.42);
        for (c, needle) in needles.iter_mut().enumerate() {
            let bx = b.x + INSET + c as f32 * (bar_w + gap);
            let peak = meter.map_or(0.0, |m| m.take_peak(c));
            needle.step(tap::db(peak), dt);
            let over = meter.is_some_and(|m| m.held(c) >= 1.0);
            p.fill(
                Rect::new(bx, b.y + INSET, bar_w, CLIP_H),
                if over { rgb(RED) } else { rgb(0x341311) },
            );
            p.fill(Rect::new(bx, top, bar_w, SCALE_H), rgb(0x14171a));
            for tick in TICKS {
                let y = top + SCALE_H * (1.0 - deflection(tick));
                let alpha = match tick as i32 {
                    0 => 0.22,
                    -18 => 0.14,
                    _ => 0.07,
                };
                p.fill(Rect::new(bx, y - 0.375, bar_w, 0.75), rgba(0xffffff, alpha));
            }
            let peak_at = deflection(needle.level);
            let rms_at =
                deflection(tap::db_power(meter.map_or(0.0, |m| m.mean_square(c)))).min(peak_at);
            if peak_at > 0.0 {
                p.fill_rect(
                    Rect::new(bx, bottom - SCALE_H * peak_at, bar_w, SCALE_H * peak_at),
                    &faint,
                );
            }
            if rms_at > 0.0 {
                p.fill_rect(
                    Rect::new(bx, bottom - SCALE_H * rms_at, bar_w, SCALE_H * rms_at),
                    &full,
                );
            }
            let hold_at = deflection(needle.hold);
            if hold_at > 0.0 {
                p.fill_rect(
                    Rect::new(bx, bottom - SCALE_H * hold_at - 1.0, bar_w, 2.0),
                    &full,
                );
            }
        }
        // The engraved scale, left of the window.
        let figures_x = x - METER_W / 2.0 - METER_SCALE_GAP;
        for tick in TICKS {
            let text = if tick > 0.0 {
                format!("+{tick}")
            } else {
                format!("{tick}")
            };
            numeral(
                p,
                &text,
                figures_x,
                METER_TOP + scale_y(tick),
                7.5,
                METER_NUMERAL_W,
            );
        }
        // The figures.
        let held = meter.map_or(0.0, |m| (0..2).map(|c| m.held(c)).fold(0.0, f32::max));
        let figure = meter.map_or(0.0, |m| (0..2).map(|c| m.figure(c)).fold(0.0, f32::max));
        let over = held >= 1.0;
        for (caption, caption_y, box_y, text, red) in [
            (
                "PEAK",
                PEAK_CAPTION_Y,
                PEAK_READOUT_Y,
                tap::readout(tap::db(held)),
                over,
            ),
            (
                "RMS",
                RMS_CAPTION_Y,
                RMS_READOUT_Y,
                tap::readout(tap::db_power(figure)),
                false,
            ),
        ] {
            engraved(p, caption, x, caption_y, 8.0, false);
            let r = Rect::new(x - READOUT_W / 2.0, box_y, READOUT_W, READOUT_H);
            let (fill, edge) = if red {
                (rgb(0x7c1712), rgba(RED, 0.9))
            } else {
                (rgb(0x0a0c0e), rgba(0xffffff, 0.14))
            };
            p.fill_rounded(r, 2.5, &Paint::Solid(fill));
            p.stroke_rounded(r, 2.5, 1.0, edge);
            p.text(&text, r, &letter_style(10.0, rgb(0xe6ecf0)));
        }
    }
}

impl CanvasView<Session, Action> for ProgramEqView {
    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        p.fill(Rect::from_size(size), theme.ui.background);
        let tap = self.device.tap(model);
        if let Some(t) = &tap {
            t.watch();
        }
        let now = Instant::now();
        let dt = self
            .last_frame
            .replace(Some(now))
            .map_or(0.0, |last| (now - last).as_secs_f32().min(0.25));
        let (ox, oy, s) = Self::fit(size);
        p.push_transform(ox, oy + HEADER_H * s, s);
        self.paint_header(p, model);
        self.paint_faceplate(p);
        self.paint_lettering(p);
        for c in &CONTROLS {
            self.paint_control(p, model, c);
        }
        self.paint_trim_lettering(p, model);
        self.paint_lamp(p, model);
        let mut needles = self.needles.get();
        self.paint_meter(p, tap.as_deref(), false, &mut needles[0], dt);
        self.paint_meter(p, tap.as_deref(), true, &mut needles[1], dt);
        self.needles.set(needles);
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
                ..
            } => {
                let (at, _) = Self::to_panel(size, *pos);
                let Some(hit) = self.hit(at) else {
                    return false;
                };
                match (hit, button) {
                    (Hit::Detent(d), PointerButton::Primary) => {
                        self.set_once(model, cx, d.param, d.value);
                    }
                    (Hit::Oversampling(i), PointerButton::Primary) => {
                        self.set_once(model, cx, param::OVERSAMPLING, i as f64);
                    }
                    (Hit::Meter(output) | Hit::Readout(output), PointerButton::Primary) => {
                        if let Some(t) = self.device.tap(model) {
                            if output {
                                t.meter_out.clear_held();
                            } else {
                                t.meter_in.clear_held();
                            }
                        }
                        cx.redraw();
                    }
                    (Hit::Control(i), PointerButton::Primary) => {
                        let c = CONTROLS[i];
                        match c.kind {
                            Kind::Knob if *clicks >= 2 => {
                                let d = Self::info(c.param).default;
                                self.set_once(model, cx, c.param, d);
                            }
                            Kind::Knob => {
                                self.device.begin(cx, "Program EQ");
                                self.drag = Some(Drag::Knob {
                                    control: c,
                                    normalized: self.normalized(model, &c),
                                    y: pos.y,
                                });
                            }
                            Kind::Switch { .. } => {
                                self.device.begin(cx, "Program EQ");
                                self.drag = Some(Drag::Switch {
                                    control: c,
                                    index: self.switch_index(model, &c),
                                    last_y: pos.y,
                                    pressed_y: pos.y,
                                    travel: 0.0,
                                    wandered: false,
                                });
                            }
                        }
                    }
                    (Hit::Control(i), PointerButton::Secondary) => {
                        let c = CONTROLS[i];
                        if c.kind == Kind::Knob {
                            let d = Self::info(c.param).default;
                            self.set_once(model, cx, c.param, d);
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
                let (_, s) = Self::to_panel(size, *pos);
                let device = self.device;
                match &mut self.drag {
                    Some(Drag::Knob {
                        control,
                        normalized,
                        y,
                    }) => {
                        let speed = if modifiers.shift { FINE } else { 1.0 };
                        *normalized =
                            (*normalized + (*y - pos.y) / (DRAG_RANGE * s) * speed).clamp(0.0, 1.0);
                        *y = pos.y;
                        let info = Self::info(control.param);
                        let v = info.min + f64::from(*normalized) * (info.max - info.min);
                        device.set(model, cx, ParameterId(control.param as u32), v);
                        true
                    }
                    Some(Drag::Switch {
                        control,
                        index,
                        last_y,
                        pressed_y,
                        travel,
                        wandered,
                    }) => {
                        if (pos.y - *pressed_y).abs() > CLICK_SLOP * s {
                            *wandered = true;
                        }
                        *travel += (*last_y - pos.y) / (20.0 * s);
                        *last_y = pos.y;
                        let steps = travel.trunc();
                        if steps != 0.0 {
                            *travel -= steps;
                            let Kind::Switch {
                                positions,
                                reversed,
                                ..
                            } = control.kind
                            else {
                                return true;
                            };
                            let steps = if reversed { -steps } else { steps };
                            *index = (*index + steps as isize).clamp(0, positions as isize - 1);
                            device.set(model, cx, ParameterId(control.param as u32), *index as f64);
                        }
                        true
                    }
                    None => false,
                }
            }
            ViewEvent::PointerUp { .. } => match self.drag.take() {
                Some(Drag::Switch {
                    control,
                    index,
                    wandered: false,
                    ..
                }) => {
                    // A press that went nowhere throws a two way switch.
                    if let Kind::Switch { positions: 2, .. } = control.kind {
                        self.set(model, cx, control.param, (1 - index.clamp(0, 1)) as f64);
                    }
                    self.device.end(cx);
                    true
                }
                Some(_) => {
                    self.device.end(cx);
                    true
                }
                None => false,
            },
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => {
                let (at, _) = Self::to_panel(size, *pos);
                let Some(Hit::Control(i)) = self.hit(at) else {
                    return false;
                };
                let c = CONTROLS[i];
                let up = -dy.signum();
                match c.kind {
                    Kind::Knob => {
                        let step = if modifiers.shift { 0.005 } else { 0.02 };
                        let n = (self.normalized(model, &c) + up * step).clamp(0.0, 1.0);
                        let info = Self::info(c.param);
                        let v = info.min + f64::from(n) * (info.max - info.min);
                        self.set_once(model, cx, c.param, v);
                    }
                    Kind::Switch {
                        positions,
                        reversed,
                        ..
                    } => {
                        let step = if reversed { -up } else { up } as isize;
                        let i =
                            (self.switch_index(model, &c) + step).clamp(0, positions as isize - 1);
                        self.set_once(model, cx, c.param, i as f64);
                    }
                }
                true
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
        match self.hit(at)? {
            Hit::Control(i) => {
                let c = CONTROLS[i];
                let info = Self::info(c.param);
                let v = self.device.value(model, c.param);
                let text = faderframe_plugin_host::program_eq::format(info.id, v)
                    .unwrap_or_else(|| format!("{v:.1} dB"));
                Some(format!("{}: {text}", info.name))
            }
            Hit::Meter(_) | Hit::Readout(_) => Some("Click to clear the held peak".into()),
            Hit::Oversampling(_) => Some(
                "Oversampling for the tube stage's saturation (the latency stays 74 samples)"
                    .into(),
            ),
            Hit::Detent(_) => None,
        }
    }

    fn min_size(&self) -> Size {
        Size::new(PANEL_W * 0.5, TOTAL_H * 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_centred_and_two_way_switches_throw_to_their_engraving() {
        assert_eq!(selector_angle(0, 2), -45.0);
        assert_eq!(selector_angle(1, 2), 45.0);
        assert_eq!(selector_angle(0, 7), -60.0);
        for n in 2..=8 {
            assert!((selector_angle(0, n) + selector_angle(n - 1, n)).abs() < 1e-4);
        }
    }

    #[test]
    fn the_meter_scale_and_ballistics() {
        assert_eq!(deflection(-70.0), 0.0);
        assert_eq!(deflection(6.0), 1.0);
        let mut n = Needle::default();
        n.step(-6.0, 0.03);
        assert_eq!(n.level, -6.0);
        for _ in 0..170 {
            n.step(f32::NEG_INFINITY, 0.01);
        }
        assert!((n.level + 26.0).abs() < 0.01);
        for pair in TICKS.windows(2) {
            assert!(scale_y(pair[1]) - scale_y(pair[0]) >= 11.0);
        }
    }

    #[test]
    fn the_light_falls_away_from_the_lamp() {
        assert!(light_at(LIGHT_X, 0.0) > light_at(PANEL_W, PANEL_H));
        assert!(light_at(PANEL_W, PANEL_H) > 0.7);
        assert!(light_at(0.0, 0.0) <= 1.0);
    }
}
