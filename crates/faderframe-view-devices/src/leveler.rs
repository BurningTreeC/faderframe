//! The 2A Leveler's front panel, drawn for FaderFrame in the colours of the
//! skin (the console's panel, legends, knobs and VU meter colours, so it
//! sits right in every theme, light or dark): the big Gain and Peak
//! Reduction knobs either side of a VU meter (gain reduction, or the
//! output at +4 or +10), and under the meter the Compress/Limit toggle,
//! the meter's selector and a window onto the T4 cell's glowing panel. Mix,
//! the sidechain's HF emphasis trim (R37), the stereo link and the external
//! sidechain sit in the strip above.
//!
//! Everything is laid out in the panel's own coordinates (1220 × 330 with
//! a 44 high strip above) and scaled to the window. Knobs and the trim:
//! drag up and down (Shift: finer), scroll, double-click for the default,
//! right-click for the menu (default, automation, MIDI learn). The toggle
//! flips on a click; the selector takes a click on a position's legend (or
//! steps round on its knob).

use crate::common::Device;
use crate::fet76::Needle;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::devices::leveler::{self as lev, id, value};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{
    Accent, CanvasView, Color, EventCx, FontWeight, HostRequest, MenuItem, Paint, Painter, Path,
    Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::time::Instant;

// --- geometry (panel coordinates) ---------------------------------------------

pub const PANEL_W: f32 = 1220.0;
pub const PANEL_H: f32 = 330.0;
/// The strip above the panel.
pub const HEADER_H: f32 = 44.0;
pub const TOTAL_H: f32 = PANEL_H + HEADER_H;

const EAR: f32 = 48.0;
/// Pixels of drag for a knob's whole sweep.
const DRAG_RANGE: f32 = 260.0;
const FINE: f32 = 0.15;

/// The big knobs.
pub const GAIN_AT: Point = Point { x: 230.0, y: 196.0 };
pub const PEAK_AT: Point = Point { x: 990.0, y: 196.0 };
const KNOB_R: f32 = 62.0;

/// The VU meter's face, its needle's pivot (below the window) and scale.
pub const VU: Rect = Rect {
    x: 450.0,
    y: 26.0,
    w: 320.0,
    h: 206.0,
};
const PIVOT: Point = Point { x: 610.0, y: 272.0 };
const ARC_R: f32 = 196.0;
/// Half the needle's swing (degrees either side of upright).
const SWING: f32 = 40.0;

/// The row under the meter: the toggle, the selector, the cell's window.
pub const TOGGLE_AT: Point = Point { x: 470.0, y: 284.0 };
pub const SELECTOR_AT: Point = Point { x: 610.0, y: 290.0 };
const SELECTOR_R: f32 = 17.0;
const GLOW_AT: Point = Point { x: 752.0, y: 284.0 };

// --- the panel's colours, from the skin ------------------------------------------

/// The panel's colours, derived from the theme (never fixed: they follow
/// the console's panel, legends, knobs and meter colours of the skin).
#[derive(Clone, Copy, Debug)]
struct Palette {
    top: Color,
    bottom: Color,
    /// Legends: the console's, or the UI's text where that reads better on
    /// the panel.
    ink: Color,
    ink_dim: Color,
    accent: Color,
    /// Lit things (the glow, the toggle's lamp): the reduction colour.
    glow: Color,
    well: Color,
    shade: Color,
    light: Color,
}

impl Palette {
    fn of(theme: &Theme) -> Self {
        let c = &theme.console;
        let top = c.panel_top;
        let bottom = c.panel_bottom;
        let mid = top.mix(bottom, 0.5);
        let ink = best_ink(mid, &[c.panel_label, theme.ui.text, c.well_text]);
        Self {
            top,
            bottom,
            ink,
            ink_dim: ink.mix(mid, 0.3),
            accent: theme.device.accent(Accent::Dynamics),
            glow: theme.device.reduction,
            well: c.well,
            shade: c.panel_edge_dark,
            light: c.panel_edge_light,
        }
    }
}

/// The candidate with the most contrast against `on`.
fn best_ink(on: Color, candidates: &[Color]) -> Color {
    candidates
        .iter()
        .copied()
        .max_by(|a, b| a.contrast(on).total_cmp(&b.contrast(on)))
        .unwrap_or(Color::BLACK)
}

/// The contrast of the panel's legends on its face (the tests hold every
/// skin to it).
pub fn legend_contrast(theme: &Theme) -> f32 {
    let p = Palette::of(theme);
    p.ink.contrast(p.top.mix(p.bottom, 0.5))
}

fn polar(c: Point, r: f32, degrees: f32) -> Point {
    let a = degrees.to_radians();
    Point::new(c.x + r * a.cos(), c.y + r * a.sin())
}

fn circle_rect(c: Point, r: f32) -> Rect {
    Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r)
}

/// A knob's pointer angle (degrees, clockwise from +x) at `t` ∈ 0…1, as
/// `controls::knob` draws it.
fn knob_degrees(t: f32) -> f32 {
    controls::knob_angle(t).to_degrees()
}

// --- the meter ------------------------------------------------------------------

/// Where the VU needle sits (0…1 of its swing) for a reading of `vu`.
pub fn deflection(vu: f32) -> f32 {
    controls::vu_deflection(vu)
}

fn needle_degrees(d: f32) -> f32 {
    -90.0 - SWING + d * 2.0 * SWING
}

/// The meter's reading (VU) for a position, the reduction (dB) and the
/// output's mean square: in Gain Reduction the needle rests at 0 and falls
/// by the reduction; at +4 and +10, 0 VU is a sine at −18 and −12 dBFS
/// (+4 and +10 dBu at the output, −18 dBFS being +4 dBu).
pub fn reading(position: usize, gr: f32, out_ms: f32) -> f32 {
    match position {
        0 => -gr.max(0.0),
        p => {
            let reference = if p == 1 { -18.0f32 } else { -12.0 };
            let rms = out_ms.max(0.0).sqrt();
            let zero = 10f32.powf(reference / 20.0) / std::f32::consts::SQRT_2;
            20.0 * (rms / zero).max(1e-7).log10()
        }
    }
}

// --- hits -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Gain,
    Peak,
    Toggle,
    /// The selector's knob (steps round).
    Selector,
    /// A selector position's legend.
    Position(usize),
    Glow,
    Meter,
    Mix,
    Emphasis,
    Link,
    External,
}

/// The selector's positions: angles (degrees) and legends.
const POSITIONS: [(f32, &str); 3] = [(-150.0, "GAIN RED."), (-90.0, "+4"), (-30.0, "+10")];

fn position_rect(n: usize) -> Rect {
    let (a, text) = POSITIONS[n];
    let c = polar(SELECTOR_AT, SELECTOR_R + 18.0, a);
    let w = 12.0 + 6.0 * text.len() as f32;
    Rect::new(c.x - w / 2.0, c.y - 8.0, w, 16.0)
}

fn toggle_rect() -> Rect {
    Rect::new(TOGGLE_AT.x - 40.0, TOGGLE_AT.y - 30.0, 80.0, 60.0)
}

fn mix_track() -> Rect {
    Rect::new(112.0, -HEADER_H + 17.0, 230.0, 10.0)
}

/// R37: a slotted trim in the strip.
const TRIM_AT: Point = Point {
    x: 590.0,
    y: -HEADER_H / 2.0,
};
const TRIM_R: f32 = 13.0;

fn link_rect() -> Rect {
    Rect::new(800.0, -HEADER_H + 9.0, 120.0, HEADER_H - 18.0)
}

fn external_rect() -> Rect {
    Rect::new(930.0, -HEADER_H + 9.0, 160.0, HEADER_H - 18.0)
}

// --- the view ---------------------------------------------------------------------

enum Drag {
    Knob { param: u32, t: f32, y: f32 },
    Mix,
}

pub struct LevelerView {
    device: Device,
    drag: Option<Drag>,
    needle: Needle,
    last_frame: Option<Instant>,
}

impl LevelerView {
    pub fn new(plugin: PluginInstanceId, _theme: &Theme) -> Self {
        // At rest the needle stands at 0 (no reduction).
        let mut needle = Needle::default();
        needle.pos = deflection(0.0);
        Self {
            device: Device::new(plugin),
            drag: None,
            needle,
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
        let all = lev::parameters();
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
        self.device.begin(cx, "2A Leveler");
        self.device.set(model, cx, ParameterId(param), v);
        self.device.end(cx);
    }

    fn hit(&self, at: Point) -> Option<Hit> {
        if at.distance(GAIN_AT) <= KNOB_R * 1.25 {
            return Some(Hit::Gain);
        }
        if at.distance(PEAK_AT) <= KNOB_R * 1.25 {
            return Some(Hit::Peak);
        }
        if toggle_rect().contains(at) {
            return Some(Hit::Toggle);
        }
        if let Some(n) = (0..POSITIONS.len()).find(|n| position_rect(*n).contains(at)) {
            return Some(Hit::Position(n));
        }
        if at.distance(SELECTOR_AT) <= SELECTOR_R + 4.0 {
            return Some(Hit::Selector);
        }
        if at.distance(GLOW_AT) <= 30.0 {
            return Some(Hit::Glow);
        }
        if VU.contains(at) {
            return Some(Hit::Meter);
        }
        if mix_track().inset_xy(-10.0, -8.0).contains(at) {
            return Some(Hit::Mix);
        }
        if at.distance(TRIM_AT) <= TRIM_R + 6.0 {
            return Some(Hit::Emphasis);
        }
        if link_rect().contains(at) {
            return Some(Hit::Link);
        }
        external_rect().contains(at).then_some(Hit::External)
    }

    /// The parameter a hit sets (none for the meter and the glow).
    fn param_of(hit: Hit) -> Option<u32> {
        Some(match hit {
            Hit::Gain => id::GAIN,
            Hit::Peak => id::PEAK_REDUCTION,
            Hit::Toggle => id::MODE,
            Hit::Selector | Hit::Position(_) => id::METER,
            Hit::Mix => id::MIX,
            Hit::Emphasis => id::EMPHASIS,
            Hit::Link => id::LINK,
            Hit::External => id::EXTERNAL,
            Hit::Glow | Hit::Meter => return None,
        })
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
        let mut items = vec![
            MenuItem::disabled(info.name.clone()),
            MenuItem::new(
                format!("Default ({})", Self::text_of(param, info.default)),
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
        lev::format(ParameterId(param), v).unwrap_or_else(|| format!("{:.0} %", v * 100.0))
    }

    // --- painting --------------------------------------------------------------

    fn paint_header(&self, p: &mut dyn Painter, model: &Session, theme: &Theme, pal: &Palette) {
        let bar = Rect::new(0.0, -HEADER_H, PANEL_W, HEADER_H);
        let deck = theme.device.deck;
        p.fill_rect(
            bar,
            &Paint::vertical(bar, deck.lighten(0.03), deck.darken(0.08)),
        );
        p.fill(Rect::new(0.0, -1.0, PANEL_W, 1.0), pal.shade);
        let text = best_ink(deck, &[theme.ui.text, theme.ui.text_dim]);
        let dim = text.mix(deck, 0.3);
        let label = TextStyle::new(9.5, dim)
            .weight(FontWeight::Bold)
            .tracking(1.6);
        // Mix: a groove, its fill and a cap.
        p.text("MIX", Rect::new(64.0, -HEADER_H, 44.0, HEADER_H), &label);
        let track = mix_track();
        let mix = self.value(model, id::MIX).clamp(0.0, 1.0) as f32;
        p.fill_rounded(track, 5.0, &Paint::Solid(theme.console.fader.slot));
        p.inset_shadow(track, 5.0, Color::BLACK.with_alpha(0.5), 0.0, 1.0, 3.0);
        let fill = Rect::new(track.x, track.y, track.w * mix, track.h);
        p.fill_rounded(fill.inset(2.0), 3.0, &Paint::Solid(pal.accent));
        let cap = Rect::new(track.x + track.w * mix - 9.0, track.y - 6.0, 18.0, 22.0);
        let f = &theme.console.fader;
        p.shadow(cap, 4.0, Color::BLACK.with_alpha(0.4), 1.0, 2.0, 4.0);
        p.fill_rounded(cap, 4.0, &Paint::vertical(cap, f.cap_top, f.cap_bottom));
        p.fill(
            Rect::new(cap.center().x - 0.75, cap.y + 4.0, 1.5, 14.0),
            f.cap_line,
        );
        p.text(
            &format!("{:.0} %", mix * 100.0),
            Rect::new(track.right() + 14.0, -HEADER_H, 60.0, HEADER_H),
            &TextStyle::new(11.0, text).weight(FontWeight::Bold),
        );
        // R37: the HF emphasis trim.
        p.text(
            "HF EMPHASIS (R37)",
            Rect::new(440.0, -HEADER_H, 130.0, HEADER_H),
            &label,
        );
        let t = self.normalized(model, id::EMPHASIS);
        trim(p, TRIM_AT, TRIM_R, knob_degrees(t), theme);
        p.text(
            &format!("{:.0} %", t * 100.0),
            Rect::new(TRIM_AT.x + TRIM_R + 10.0, -HEADER_H, 60.0, HEADER_H),
            &TextStyle::new(11.0, text).weight(FontWeight::Bold),
        );
        // The stereo link and the external sidechain.
        small_button(
            p,
            link_rect(),
            "STEREO LINK",
            self.value(model, id::LINK) >= 0.5,
            theme,
        );
        small_button(
            p,
            external_rect(),
            "EXT SIDECHAIN",
            self.value(model, id::EXTERNAL) >= 0.5,
            theme,
        );
    }

    fn paint_faceplate(&self, p: &mut dyn Painter, theme: &Theme, pal: &Palette) {
        let plate = Rect::new(0.0, 0.0, PANEL_W, PANEL_H);
        controls::panel(p, plate, pal.top, pal.bottom, theme);
        p.fill(Rect::new(0.0, PANEL_H - 1.5, PANEL_W, 1.5), pal.shade);
        // The rack ears: a fold, the slots and their screws.
        for x in [EAR, PANEL_W - EAR] {
            p.fill(Rect::new(x - 1.0, 0.0, 1.0, PANEL_H), pal.shade);
            p.fill(Rect::new(x, 0.0, 1.0, PANEL_H), pal.light);
        }
        for x in [EAR / 2.0, PANEL_W - EAR / 2.0] {
            for y in [44.0, PANEL_H - 44.0] {
                let slot = Rect::new(x - 14.0, y - 8.0, 28.0, 16.0);
                p.fill_rounded(slot, 8.0, &Paint::Solid(pal.well));
                p.inset_shadow(slot, 8.0, Color::BLACK.with_alpha(0.8), 0.0, 2.0, 4.0);
                controls::screw(p, Point::new(x + 3.0, y), 6.5, theme);
            }
        }
    }

    fn paint_lettering(&self, p: &mut dyn Painter, pal: &Palette) {
        let x = 72.0;
        engraved(
            p,
            "FADERFRAME",
            Rect::new(x, 16.0, 220.0, 16.0),
            11.0,
            4.0,
            pal.ink_dim,
            false,
        );
        engraved(
            p,
            "2A",
            Rect::new(x - 2.0, 34.0, 70.0, 44.0),
            40.0,
            0.0,
            pal.ink,
            false,
        );
        engraved(
            p,
            "LEVELER",
            Rect::new(x + 60.0, 38.0, 200.0, 20.0),
            17.0,
            3.0,
            pal.ink,
            false,
        );
        engraved(
            p,
            "OPTICAL LEVELING AMPLIFIER",
            Rect::new(x + 61.0, 60.0, 240.0, 14.0),
            8.0,
            2.0,
            pal.ink_dim,
            false,
        );
        p.fill(Rect::new(x, 84.0, 250.0, 1.0), pal.shade);
        p.fill(Rect::new(x, 85.0, 250.0, 1.0), pal.light);
        // The cell's plate, top right.
        let rx = PANEL_W - 72.0 - 250.0;
        engraved(
            p,
            "T4 ELECTRO-OPTICAL ATTENUATOR",
            Rect::new(rx, 22.0, 250.0, 14.0),
            8.0,
            1.8,
            pal.ink_dim,
            false,
        );
        engraved(
            p,
            "TUBE MAKE-UP AMPLIFIER",
            Rect::new(rx, 38.0, 250.0, 14.0),
            8.0,
            1.8,
            pal.ink_dim,
            false,
        );
        p.fill(Rect::new(rx, 58.0, 250.0, 1.0), pal.shade);
        p.fill(Rect::new(rx, 59.0, 250.0, 1.0), pal.light);
    }

    /// A big knob: (its parameter, where, its legend).
    fn paint_knob(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        theme: &Theme,
        pal: &Palette,
        (param, at, name): (u32, Point, &str),
    ) {
        // The dial: 0…100, a tick each 5, numerals each 10.
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let a = knob_degrees(t);
            let major = i % 2 == 0;
            let (r0, r1) = if major {
                (KNOB_R + 5.0, KNOB_R + 13.0)
            } else {
                (KNOB_R + 6.0, KNOB_R + 10.0)
            };
            p.line(
                polar(at, r0, a),
                polar(at, r1, a),
                if major { 1.6 } else { 1.0 },
                if major { pal.ink } else { pal.ink_dim },
            );
            if major {
                let c = polar(at, KNOB_R + 27.0, a);
                engraved(
                    p,
                    &format!("{}", i * 5),
                    Rect::new(c.x - 20.0, c.y - 8.0, 40.0, 16.0),
                    10.5,
                    0.0,
                    pal.ink,
                    true,
                );
            }
        }
        engraved(
            p,
            name,
            Rect::new(at.x - 90.0, at.y + KNOB_R + 26.0, 180.0, 16.0),
            12.5,
            3.0,
            pal.ink,
            true,
        );
        let t = self.normalized(model, param);
        controls::knob(
            p,
            circle_rect(at, KNOB_R),
            t,
            false,
            KnobLook {
                cap: theme.console.knob.cap_top,
                ring: pal.accent,
            },
            theme,
        );
    }

    fn paint_toggle(&self, p: &mut dyn Painter, model: &Session, theme: &Theme, pal: &Palette) {
        let limit = self.value(model, id::MODE) >= 0.5;
        // The legends: Limit up, Compress down.
        let style = |on: bool| {
            TextStyle::new(9.0, if on { pal.ink } else { pal.ink_dim })
                .weight(FontWeight::Bold)
                .tracking(1.4)
                .center()
        };
        p.text(
            "LIMIT",
            Rect::new(TOGGLE_AT.x - 40.0, TOGGLE_AT.y - 34.0, 80.0, 12.0),
            &style(limit),
        );
        p.text(
            "COMPRESS",
            Rect::new(TOGGLE_AT.x - 40.0, TOGGLE_AT.y + 22.0, 80.0, 12.0),
            &style(!limit),
        );
        bat_toggle(p, TOGGLE_AT, limit, theme, pal);
    }

    fn paint_selector(&self, p: &mut dyn Painter, model: &Session, theme: &Theme, pal: &Palette) {
        let at = self.value(model, id::METER).round().clamp(0.0, 2.0) as usize;
        for (n, (a, text)) in POSITIONS.iter().enumerate() {
            p.line(
                polar(SELECTOR_AT, SELECTOR_R + 3.0, *a),
                polar(SELECTOR_AT, SELECTOR_R + 8.0, *a),
                1.4,
                pal.ink,
            );
            let r = position_rect(n);
            p.text(
                text,
                r,
                &TextStyle::new(8.5, if n == at { pal.ink } else { pal.ink_dim })
                    .weight(FontWeight::Bold)
                    .tracking(0.6)
                    .center(),
            );
        }
        engraved(
            p,
            "METER",
            Rect::new(
                SELECTOR_AT.x - 40.0,
                SELECTOR_AT.y + SELECTOR_R + 4.0,
                80.0,
                12.0,
            ),
            8.5,
            2.0,
            pal.ink_dim,
            true,
        );
        // A pointer knob.
        let k = &theme.console.knob;
        let rect = circle_rect(SELECTOR_AT, SELECTOR_R);
        p.shadow(rect, SELECTOR_R, k.shadow, 1.0, 2.0, 4.0);
        p.fill_rounded(
            rect,
            SELECTOR_R,
            &Paint::vertical(rect, k.body_light, k.body_dark),
        );
        let a = POSITIONS[at].0;
        p.line(
            polar(SELECTOR_AT, 4.0, a),
            polar(SELECTOR_AT, SELECTOR_R - 2.0, a),
            3.0,
            k.pointer,
        );
    }

    fn paint_glow(
        &self,
        p: &mut dyn Painter,
        tap: Option<&AnalysisTap>,
        theme: &Theme,
        pal: &Palette,
    ) {
        let light = tap.map_or(0.0, |t| t.value(value::LIGHT)).clamp(0.0, 1.5);
        let lens = circle_rect(GLOW_AT, 15.0);
        p.fill_rounded(lens.inset(-5.0), 20.0, &Paint::Solid(pal.well));
        p.inset_shadow(
            lens.inset(-5.0),
            20.0,
            Color::BLACK.with_alpha(0.7),
            0.0,
            2.0,
            4.0,
        );
        // The panel's glow: the reduction colour, as bright as the light.
        let b = (light / 1.2).sqrt().min(1.0);
        let dark = theme.console.led.off;
        let lit = dark.mix(pal.glow, b);
        if b > 0.02 {
            p.fill_rounded(
                lens.inset(-12.0),
                28.0,
                &Paint::Radial {
                    center: GLOW_AT,
                    radius: 30.0,
                    stops: vec![
                        (0.0, pal.glow.with_alpha(0.35 * b)),
                        (1.0, pal.glow.with_alpha(0.0)),
                    ],
                },
            );
        }
        p.fill_rounded(
            lens,
            15.0,
            &Paint::Radial {
                center: Point::new(GLOW_AT.x - 4.0, GLOW_AT.y - 5.0),
                radius: 20.0,
                stops: vec![(0.0, lit.lighten(0.25 * b)), (1.0, lit.darken(0.25))],
            },
        );
        engraved(
            p,
            "T4",
            Rect::new(GLOW_AT.x - 30.0, GLOW_AT.y + 22.0, 60.0, 12.0),
            8.5,
            2.0,
            pal.ink_dim,
            true,
        );
    }

    fn paint_vu(
        &mut self,
        p: &mut dyn Painter,
        model: &Session,
        tap: Option<&AnalysisTap>,
        theme: &Theme,
        pal: &Palette,
        dt: f32,
    ) {
        let m = &theme.console.meter;
        let position = self.value(model, id::METER).round().clamp(0.0, 2.0) as usize;
        let target = match tap {
            None => deflection(0.0),
            Some(t) => deflection(reading(
                position,
                t.value(value::GR),
                t.value(value::OUT_MS),
            )),
        };
        self.needle.follow(target, dt);
        // The bezel.
        let bezel = VU.inset(-9.0);
        p.shadow(bezel, 10.0, Color::BLACK.with_alpha(0.5), 2.0, 4.0, 10.0);
        p.fill_rounded(
            bezel,
            10.0,
            &Paint::vertical(bezel, pal.well.lighten(0.12), pal.well),
        );
        p.stroke_rounded(bezel, 10.0, 1.0, pal.light);
        // The face, lit from behind.
        let face = m.vu_face;
        p.fill_rounded(
            VU,
            4.0,
            &Paint::Radial {
                center: Point::new(VU.center().x, VU.bottom() - 30.0),
                radius: VU.w * 0.75,
                stops: vec![
                    (0.0, face.lighten(0.08)),
                    (0.65, face),
                    (1.0, face.darken(0.18)),
                ],
            },
        );
        p.push_clip(VU);
        let ink = m.vu_needle;
        let red = m.red.darken(0.1);
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
        let numeral = TextStyle::new(12.0, ink).weight(FontWeight::Bold).center();
        for (vu, text) in [
            (-20.0f32, "20"),
            (-10.0, "10"),
            (-7.0, "7"),
            (-5.0, "5"),
            (-3.0, "3"),
            (-2.0, "2"),
            (-1.0, "1"),
            (0.0, "0"),
            (1.0, "1"),
            (2.0, "2"),
            (3.0, "3"),
        ] {
            let a = needle_degrees(deflection(vu));
            let c = if vu > 0.0 { red } else { ink };
            p.line(
                polar(PIVOT, ARC_R, a),
                polar(PIVOT, ARC_R + 13.0, a),
                1.6,
                c,
            );
            let at = polar(PIVOT, ARC_R + 26.0, a);
            p.text(
                text,
                Rect::new(at.x - 14.0, at.y - 9.0, 28.0, 18.0),
                &numeral.color(c),
            );
        }
        // The percentage scale under the arc.
        arc(p, 0.0, 1.0, ARC_R - 16.0, 1.0, ink.with_alpha(0.7));
        let small = TextStyle::new(7.5, ink.with_alpha(0.8)).center();
        for pc in [0, 20, 40, 60, 80, 100] {
            let a = needle_degrees(pc as f32 / 100.0 * zero);
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
        p.text(
            "VU",
            Rect::new(PIVOT.x - 40.0, VU.y + 112.0, 80.0, 34.0),
            &TextStyle::new(28.0, ink).weight(FontWeight::Bold).center(),
        );
        let caption = match position {
            0 => "GAIN REDUCTION",
            1 => "OUTPUT  +4",
            _ => "OUTPUT  +10",
        };
        let face_small = TextStyle::new(7.5, ink.with_alpha(0.75))
            .weight(FontWeight::Bold)
            .tracking(1.4);
        p.text(
            caption,
            Rect::new(VU.x + 12.0, VU.bottom() - 24.0, 130.0, 12.0),
            &face_small,
        );
        p.text(
            "FADERFRAME",
            Rect::new(VU.right() - 132.0, VU.bottom() - 24.0, 120.0, 12.0),
            &face_small.right(),
        );
        // The needle and its shadow on the face.
        let a = needle_degrees(self.needle.pos);
        let tip = polar(PIVOT, ARC_R + 12.0, a);
        let base = polar(PIVOT, 44.0, a);
        p.line(
            base.offset(3.0, 4.0),
            tip.offset(3.0, 4.0),
            2.2,
            ink.with_alpha(0.14),
        );
        p.line(base, tip, 1.8, ink);
        // The movement's cover over the pivot.
        let cover = circle_rect(PIVOT, 54.0);
        p.fill_rounded(
            cover,
            54.0,
            &Paint::Radial {
                center: Point::new(PIVOT.x - 12.0, PIVOT.y - 46.0),
                radius: 76.0,
                stops: vec![(0.0, pal.well.lighten(0.2)), (1.0, pal.well)],
            },
        );
        p.stroke_rounded(cover, 54.0, 1.0, pal.light);
        // The glass: a soft sheen from the top left.
        p.fill_rect(
            VU,
            &Paint::Linear {
                start: Point::new(VU.x, VU.y),
                end: Point::new(VU.x + VU.w * 0.55, VU.y + VU.h),
                stops: vec![
                    (0.0, face.lighten(0.6).with_alpha(0.22)),
                    (0.45, face.lighten(0.6).with_alpha(0.04)),
                    (1.0, face.with_alpha(0.0)),
                ],
            },
        );
        p.pop_clip();
        p.inset_shadow(VU, 4.0, Color::BLACK.with_alpha(0.55), 0.0, 3.0, 8.0);
    }
}

// --- parts ----------------------------------------------------------------------------

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
    // The shadow falls darker than the letters on a light panel, lighter on
    // a dark one (an engraving catches the light on its lower edge there).
    let shadow = if colour.luminance() > 0.5 {
        Color::BLACK.with_alpha(0.45)
    } else {
        Color::WHITE.with_alpha(0.25)
    };
    p.text(text, r.translate(0.0, 1.0), &style.color(shadow));
    p.text(text, r, &style);
}

/// A slotted trim pot (a screwdriver adjustment) at `angle` (degrees).
fn trim(p: &mut dyn Painter, c: Point, r: f32, angle: f32, theme: &Theme) {
    let k = &theme.console.knob;
    let rect = circle_rect(c, r);
    p.fill_rounded(rect.inset(-3.0), r + 3.0, &Paint::Solid(theme.console.well));
    p.fill_rounded(
        rect,
        r,
        &Paint::vertical(rect, k.cap_top.lighten(0.15), k.cap_bottom),
    );
    p.stroke_rounded(rect, r, 1.0, Color::BLACK.with_alpha(0.4));
    let (p0, p1) = (polar(c, r * 0.8, angle), polar(c, r * 0.8, angle + 180.0));
    p.line(p0, p1, 3.0, k.body_dark);
    // The pointer end of the slot.
    p.circle(polar(c, r * 0.55, angle), 1.6, k.pointer);
}

/// A bat-handle toggle, up when `up`.
fn bat_toggle(p: &mut dyn Painter, c: Point, up: bool, theme: &Theme, pal: &Palette) {
    let k = &theme.console.knob;
    // The bushing and its nut.
    let nut = circle_rect(c, 11.0);
    p.shadow(nut, 11.0, k.shadow, 0.0, 2.0, 4.0);
    p.fill_rounded(nut, 11.0, &Paint::vertical(nut, k.cap_top, k.cap_bottom));
    p.stroke_rounded(nut, 11.0, 1.0, pal.shade);
    let bush = circle_rect(c, 6.0);
    p.fill_rounded(bush, 6.0, &Paint::Solid(k.body_dark));
    // The lever: from the bushing to a rounded tip, up or down.
    let tip = Point::new(c.x, c.y + if up { -20.0 } else { 20.0 });
    p.line(c.offset(1.5, 2.0), tip.offset(1.5, 2.0), 7.0, k.shadow);
    p.line(c, tip, 6.0, k.cap_bottom.mix(k.cap_top, 0.5));
    p.line(
        c.offset(-1.0, 0.0),
        tip.offset(-1.0, 0.0),
        1.5,
        k.cap_top.lighten(0.3),
    );
    let ball = circle_rect(tip, 5.0);
    p.fill_rounded(
        ball,
        5.0,
        &Paint::vertical(ball, k.cap_top.lighten(0.25), k.cap_bottom),
    );
}

/// A small push-button for the strip: lit in the accent when in.
fn small_button(p: &mut dyn Painter, r: Rect, text: &str, on: bool, theme: &Theme) {
    let k = &theme.console.knob;
    let accent = theme.device.accent(Accent::Dynamics);
    p.fill_rounded(r.inset(-2.0), 6.0, &Paint::Solid(theme.console.well));
    let cap = if on { r.translate(0.0, 1.0) } else { r };
    if !on {
        p.shadow(cap, 4.0, k.shadow, 0.0, 2.0, 3.0);
    }
    let (top, bottom) = if on {
        (k.body_light.darken(0.1), k.body_dark)
    } else {
        (k.cap_top, k.cap_bottom)
    };
    p.fill_rounded(cap, 4.0, &Paint::vertical(cap, top, bottom));
    if on {
        p.fill_rounded(
            cap.inset_xy(-6.0, -4.0),
            8.0,
            &Paint::Radial {
                center: cap.center(),
                radius: cap.w * 0.6,
                stops: vec![
                    (0.0, accent.with_alpha(0.18)),
                    (1.0, accent.with_alpha(0.0)),
                ],
            },
        );
    }
    let ink = if on {
        accent
    } else {
        best_ink(top.mix(bottom, 0.5), &[theme.ui.text_dim, theme.ui.text])
    };
    p.text(
        text,
        cap,
        &TextStyle::new(9.5, ink)
            .weight(FontWeight::Bold)
            .tracking(1.0)
            .center(),
    );
}

impl CanvasView<Session, Action> for LevelerView {
    /// The meter redrawn every frame.
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
        let pal = Palette::of(theme);
        let (ox, oy, s) = Self::fit(size);
        p.push_transform(ox, oy + HEADER_H * s, s);
        self.paint_header(p, model, theme, &pal);
        self.paint_faceplate(p, theme, &pal);
        self.paint_lettering(p, &pal);
        self.paint_knob(p, model, theme, &pal, (id::GAIN, GAIN_AT, "GAIN"));
        self.paint_knob(
            p,
            model,
            theme,
            &pal,
            (id::PEAK_REDUCTION, PEAK_AT, "PEAK REDUCTION"),
        );
        self.paint_vu(p, model, tap.as_deref(), theme, &pal, dt);
        self.paint_toggle(p, model, theme, &pal);
        self.paint_selector(p, model, theme, &pal);
        self.paint_glow(p, tap.as_deref(), theme, &pal);
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
                if *button == PointerButton::Secondary {
                    let Some(param) = Self::param_of(hit) else {
                        return false;
                    };
                    if let Some(req) = self.control_menu(model, param, *pos) {
                        cx.request(req);
                    }
                    return true;
                }
                if *button != PointerButton::Primary {
                    return false;
                }
                match hit {
                    Hit::Gain | Hit::Peak | Hit::Emphasis => {
                        let param = Self::param_of(hit).unwrap_or(id::GAIN);
                        if *clicks >= 2 {
                            self.set_once(model, cx, param, Self::info(param).default);
                        } else {
                            self.device.begin(cx, "2A Leveler");
                            self.drag = Some(Drag::Knob {
                                param,
                                t: self.normalized(model, param),
                                y: pos.y,
                            });
                        }
                    }
                    Hit::Toggle => {
                        let limit = self.value(model, id::MODE) >= 0.5;
                        self.set_once(model, cx, id::MODE, if limit { 0.0 } else { 1.0 });
                    }
                    Hit::Selector => {
                        let n = self.value(model, id::METER).round() as usize;
                        self.set_once(model, cx, id::METER, ((n + 1) % POSITIONS.len()) as f64);
                    }
                    Hit::Position(n) => self.set_once(model, cx, id::METER, n as f64),
                    Hit::Link | Hit::External => {
                        let param = if hit == Hit::Link {
                            id::LINK
                        } else {
                            id::EXTERNAL
                        };
                        let on = self.value(model, param) >= 0.5;
                        self.set_once(model, cx, param, if on { 0.0 } else { 1.0 });
                    }
                    Hit::Mix => {
                        if *clicks >= 2 {
                            self.set_once(model, cx, id::MIX, 1.0);
                        } else {
                            self.device.begin(cx, "2A Leveler");
                            let t = mix_track();
                            let v = ((at.x - t.x) / t.w).clamp(0.0, 1.0);
                            self.device
                                .set(model, cx, ParameterId(id::MIX), f64::from(v));
                            self.drag = Some(Drag::Mix);
                        }
                    }
                    Hit::Glow | Hit::Meter => return false,
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
                    Some(hit @ (Hit::Gain | Hit::Peak | Hit::Emphasis | Hit::Mix)) => {
                        let param = Self::param_of(hit).unwrap_or(id::GAIN);
                        let step = if modifiers.shift { 0.005 } else { 0.02 };
                        let t = (self.normalized(model, param) + up * step).clamp(0.0, 1.0);
                        self.set_once(model, cx, param, Self::from_normalized(param, t));
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        // The meter and the glow follow the audio.
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
        let hit = self.hit(at)?;
        Some(match hit {
            Hit::Gain => format!("{} — the make-up amplifier's gain", named(id::GAIN)),
            Hit::Peak => format!(
                "{} — how hard the signal drives the cell's panel",
                named(id::PEAK_REDUCTION)
            ),
            Hit::Toggle => format!("{} — Limit makes the ratio steeper", named(id::MODE)),
            Hit::Selector | Hit::Position(_) => format!(
                "{} — gain reduction, or the output (0 VU at +4 or +10)",
                named(id::METER)
            ),
            Hit::Glow => "The T4 cell's panel: it glows with the reduction".into(),
            Hit::Meter => "VU meter".into(),
            Hit::Emphasis => format!(
                "{} — more reduction for high frequencies (the sidechain's R37)",
                named(id::EMPHASIS)
            ),
            hit => named(Self::param_of(hit).unwrap_or(id::MIX)),
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
    fn the_meter_reads_reduction_and_output() {
        assert_eq!(reading(0, 0.0, 0.0), 0.0);
        assert_eq!(reading(0, 6.0, 0.0), -6.0);
        // A sine at −18 dBFS reads 0 at +4, −6 at +10.
        let a = 10f32.powf(-18.0 / 20.0);
        let ms = a * a / 2.0;
        assert!(reading(1, 0.0, ms).abs() < 1e-3);
        assert!((reading(2, 0.0, ms) + 6.0).abs() < 1e-3);
    }

    #[test]
    fn every_skin_keeps_the_legends_readable() {
        for theme in Theme::builtin() {
            let c = legend_contrast(&theme);
            assert!(c >= 3.0, "{}: legends at {c:.2}:1", theme.id);
        }
    }
}
