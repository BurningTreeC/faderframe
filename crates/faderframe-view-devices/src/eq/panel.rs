//! The selected bands' controls, floating over the display under the band
//! in focus. Changes reach every selected band (frequencies and Qs by the
//! same ratio, gains by the same step).
//!
//! The gain knob carries the dynamic range as a ring: drag the ring to make
//! the band dynamic (red, the range; yellow, where the band is right now).
//! The dynamics run in auto mode until ">>" opens their own settings: the
//! threshold (its top is "auto"; the trigger's level shows behind it),
//! whether the sidechain triggers, attack and release (50 % is automatic),
//! the trigger's filtering (the band's region, or free low and high cuts;
//! hold Audition to hear it), and for spectral bands the density and the
//! 3 dB/oct tilt.

use super::geometry::Layout;
use super::paint::{cross_icon, headphones_icon, power_icon};
use super::{EqView, Hit};
use faderframe_plugin_host::eq::design::{self, BandType};
use faderframe_plugin_host::eq::{
    BandParams, Field, Placement, band_index, format_hz, global, value,
};
use faderframe_plugin_host::tap::AnalysisTap;
use faderframe_session::Session;
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{Color, FontWeight, Paint, Painter, Path, Point, Rect, TextStyle};

/// The controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelItem {
    Bypass,
    Shape,
    Slope,
    Placement,
    Split,
    Knob(Field),
    /// The dynamic range ring round the gain knob.
    Ring,
    Expand,
    Spectral,
    DynBypass,
    DynClear,
    GainQ,
    Prev,
    Next,
    Solo,
    Delete,
    /// The custom dynamics.
    Threshold,
    Key,
    Trigger,
    Audition,
    Density,
    Tilt,
    /// The panel itself (nothing there).
    Body,
}

pub(crate) const PANEL_W: f32 = 660.0;
const ROW1_H: f32 = 116.0;
const ROW2_H: f32 = 98.0;

/// A knob's range: low, high, logarithmic.
pub(crate) fn knob_spec(field: Field) -> (f64, f64, bool) {
    match field {
        Field::Freq | Field::TriggerLow | Field::TriggerHigh => (10.0, 30_000.0, true),
        Field::Q => (0.025, 40.0, true),
        Field::Gain | Field::Range => (-30.0, 30.0, false),
        Field::Threshold => (-80.0, 0.0, false),
        Field::Slope => (0.0, design::BRICKWALL, false),
        _ => (0.0, 1.0, false),
    }
}

pub(crate) fn to_normalized(field: Field, v: f64) -> f32 {
    let (lo, hi, log) = knob_spec(field);
    let t = if log {
        (v.max(lo) / lo).ln() / (hi / lo).ln()
    } else {
        (v - lo) / (hi - lo)
    };
    t.clamp(0.0, 1.0) as f32
}

pub(crate) fn from_normalized(field: Field, t: f32) -> f64 {
    let (lo, hi, log) = knob_spec(field);
    let t = f64::from(t.clamp(0.0, 1.0));
    if log {
        lo * (hi / lo).powf(t)
    } else {
        lo + t * (hi - lo)
    }
}

/// Whether a field moves other selected bands by a ratio (else by a step).
pub(crate) fn by_ratio(field: Field) -> bool {
    knob_spec(field).2
}

impl EqView {
    /// The panel's rectangle under the focused band, if one is selected.
    pub(crate) fn panel_rect(
        &self,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
    ) -> Option<(Rect, usize, BandParams)> {
        let band = self.focus.filter(|f| self.is_selected(*f))?;
        let p = BandParams::read(&tap.params, band);
        if !p.used || self.grab.is_some() {
            return None;
        }
        let g = &l.graph;
        let expanded = p.dynamic() && p.custom;
        let h = ROW1_H + if expanded { ROW2_H } else { 0.0 };
        let x = self.axis(model).x(g, p.freq) - PANEL_W / 2.0;
        let x = x.clamp(g.x + 4.0, (g.right() - PANEL_W - 4.0).max(g.x + 4.0));
        Some((Rect::new(x, g.bottom() - h - 6.0, PANEL_W, h), band, p))
    }

    pub(crate) fn panel_items(&self, r: &Rect, p: &BandParams) -> Vec<(PanelItem, Rect)> {
        let mut out = vec![
            (
                PanelItem::Bypass,
                Rect::new(r.x + 10.0, r.y + 10.0, 26.0, 22.0),
            ),
            (
                PanelItem::Shape,
                Rect::new(r.x + 40.0, r.y + 10.0, 112.0, 22.0),
            ),
        ];
        if p.kind.has_slope() {
            out.push((
                PanelItem::Slope,
                Rect::new(r.x + 10.0, r.y + 38.0, 142.0, 22.0),
            ));
        }
        out.push((
            PanelItem::Placement,
            Rect::new(r.x + 10.0, r.y + 66.0, 108.0, 22.0),
        ));
        out.push((
            PanelItem::Split,
            Rect::new(r.x + 122.0, r.y + 66.0, 30.0, 22.0),
        ));
        out.push((
            PanelItem::Knob(Field::Freq),
            Rect::new(r.x + 166.0, r.y + 8.0, 70.0, 96.0),
        ));
        if p.kind.has_gain() {
            out.push((
                PanelItem::Knob(Field::Gain),
                Rect::new(r.x + 246.0, r.y + 8.0, 84.0, 96.0),
            ));
            out.push((
                PanelItem::Spectral,
                Rect::new(r.x + 334.0, r.y + 14.0, 30.0, 18.0),
            ));
            if p.dynamic() {
                out.push((
                    PanelItem::Expand,
                    Rect::new(r.x + 334.0, r.y + 36.0, 30.0, 18.0),
                ));
                out.push((
                    PanelItem::DynBypass,
                    Rect::new(r.x + 334.0, r.y + 58.0, 30.0, 18.0),
                ));
                out.push((
                    PanelItem::DynClear,
                    Rect::new(r.x + 334.0, r.y + 80.0, 30.0, 18.0),
                ));
            }
        }
        if p.kind == BandType::Bell {
            out.push((
                PanelItem::GainQ,
                Rect::new(r.x + 370.0, r.y + 46.0, 30.0, 18.0),
            ));
        }
        if p.kind.has_q(p.slope) {
            out.push((
                PanelItem::Knob(Field::Q),
                Rect::new(r.x + 404.0, r.y + 8.0, 70.0, 96.0),
            ));
        }
        out.push((
            PanelItem::Prev,
            Rect::new(r.x + 488.0, r.y + 10.0, 24.0, 22.0),
        ));
        out.push((
            PanelItem::Next,
            Rect::new(r.x + 584.0, r.y + 10.0, 24.0, 22.0),
        ));
        out.push((
            PanelItem::Delete,
            Rect::new(r.right() - 36.0, r.y + 10.0, 26.0, 22.0),
        ));
        out.push((
            PanelItem::Solo,
            Rect::new(r.x + 488.0, r.y + 40.0, 120.0, 22.0),
        ));
        if p.dynamic() && p.custom {
            let y = r.y + ROW1_H;
            out.push((
                PanelItem::Threshold,
                Rect::new(r.x + 16.0, y + 24.0, 214.0, 16.0),
            ));
            out.push((PanelItem::Key, Rect::new(r.x + 240.0, y + 20.0, 44.0, 22.0)));
            if p.is_spectral() {
                out.push((
                    PanelItem::Density,
                    Rect::new(r.x + 16.0, y + 70.0, 214.0, 16.0),
                ));
                out.push((
                    PanelItem::Tilt,
                    Rect::new(r.x + 240.0, y + 62.0, 44.0, 22.0),
                ));
            }
            out.push((
                PanelItem::Knob(Field::Attack),
                Rect::new(r.x + 294.0, y + 4.0, 58.0, 88.0),
            ));
            out.push((
                PanelItem::Knob(Field::Release),
                Rect::new(r.x + 356.0, y + 4.0, 58.0, 88.0),
            ));
            out.push((
                PanelItem::Trigger,
                Rect::new(r.x + 424.0, y + 16.0, 92.0, 22.0),
            ));
            out.push((
                PanelItem::Audition,
                Rect::new(r.x + 424.0, y + 46.0, 92.0, 22.0),
            ));
            if p.free {
                out.push((
                    PanelItem::Knob(Field::TriggerLow),
                    Rect::new(r.x + 524.0, y + 4.0, 58.0, 88.0),
                ));
                out.push((
                    PanelItem::Knob(Field::TriggerHigh),
                    Rect::new(r.x + 586.0, y + 4.0, 58.0, 88.0),
                ));
            }
        }
        out
    }

    /// The gain knob's centre and radii (knob, ring).
    pub(crate) fn gain_knob(r: &Rect) -> (Point, f32, f32) {
        let c = Point::new(r.x + r.w / 2.0, r.y + 15.0 + 26.0);
        (c, 22.0, 31.0)
    }

    pub(crate) fn panel_hit(
        &self,
        pos: Point,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
    ) -> Option<Hit> {
        let (r, _, p) = self.panel_rect(l, model, tap)?;
        if !r.contains(pos) {
            return None;
        }
        for (item, ir) in self.panel_items(&r, &p) {
            if item == PanelItem::Knob(Field::Gain) {
                let (c, knob, ring) = Self::gain_knob(&ir);
                let d = c.distance(pos);
                if d <= ring + 4.0 && d > knob + 2.0 {
                    return Some(Hit::Panel(PanelItem::Ring));
                }
            }
            if ir.contains(pos) {
                return Some(Hit::Panel(item));
            }
        }
        Some(Hit::Panel(PanelItem::Body))
    }

    pub(crate) fn paint_panel(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
    ) {
        let Some((r, band, bp)) = self.panel_rect(l, model, tap) else {
            return;
        };
        let th = &self.theme;
        let color = Self::band_color(band);
        p.shadow(r, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.45), 0.0, 3.0, 16.0);
        p.fill_rounded(r, 8.0, &Paint::Solid(th.eq.panel.with_alpha(0.97)));
        p.stroke_rounded(r, 8.0, 1.0, th.eq.panel_edge);
        p.fill(
            Rect::new(r.x + 12.0, r.y, r.w - 24.0, 2.0),
            color.with_alpha(0.9),
        );
        if bp.dynamic() && bp.custom {
            p.hline(r.x + 10.0, r.right() - 10.0, r.y + ROW1_H, th.eq.panel_edge);
        }
        let scale = Self::scale(tap);
        let piano = self.settings(model).piano;
        let dim_text = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .tracking(0.6);
        for (item, ir) in self.panel_items(&r, &bp) {
            match item {
                PanelItem::Bypass => {
                    self.button_tinted(p, ir, "", !bp.enabled, th.eq.dyn_range);
                    power_icon(
                        p,
                        ir.center(),
                        5.5,
                        if bp.enabled {
                            th.ui.text
                        } else {
                            th.eq.dyn_range
                        },
                    );
                }
                PanelItem::Shape => self.button(p, ir, &format!("{} ▾", bp.kind.name()), false),
                PanelItem::Slope => {
                    let s = bp.kind.snap_slope(bp.slope);
                    self.button(p, ir, &format!("{} ▾", design::slope_name(s)), false);
                }
                PanelItem::Placement => self.button(
                    p,
                    ir,
                    &format!("{} ▾", bp.placement.name()),
                    bp.placement != Placement::Stereo,
                ),
                PanelItem::Split => self.button(p, ir, "✂", false),
                PanelItem::Knob(field) => {
                    self.panel_knob(p, ir, band, field, &bp, tap, (scale, piano), color);
                }
                PanelItem::Ring => {}
                PanelItem::Expand => {
                    self.button(p, ir, if bp.custom { "<<" } else { ">>" }, bp.custom)
                }
                PanelItem::Spectral => self.button_tinted(p, ir, "S", bp.spectral, th.eq.side),
                PanelItem::DynBypass => {
                    self.button_tinted(p, ir, "", bp.dyn_bypass, th.eq.dyn_range);
                    power_icon(
                        p,
                        ir.center(),
                        4.5,
                        if bp.dyn_bypass {
                            th.eq.dyn_range
                        } else {
                            th.ui.text_dim
                        },
                    );
                }
                PanelItem::DynClear => {
                    self.button(p, ir, "", false);
                    cross_icon(p, ir.center(), 3.5, th.ui.text_dim);
                }
                PanelItem::GainQ => self.button(p, ir, "GQ", tap.params.get(global::GAIN_Q) >= 0.5),
                PanelItem::Prev => self.button(p, ir, "◀", false),
                PanelItem::Next => {
                    self.button(p, ir, "▶", false);
                    let label = Rect::new(r.x + 512.0, r.y + 10.0, 72.0, 22.0);
                    p.text(
                        &format!("Band {}", band + 1),
                        label,
                        &TextStyle::new(th.fonts.small, color)
                            .weight(FontWeight::Bold)
                            .center(),
                    );
                    let n = self.selected.len();
                    if n > 1 {
                        p.text(
                            &format!("{n} selected"),
                            Rect::new(label.x, label.bottom() + 46.0, label.w, 14.0),
                            &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
                        );
                    }
                }
                PanelItem::Solo => {
                    self.button(p, ir, "     Solo (hold)", self.listening);
                    headphones_icon(
                        p,
                        Point::new(ir.x + 18.0, ir.center().y - 1.0),
                        5.5,
                        th.ui.text,
                    );
                }
                PanelItem::Delete => {
                    self.button(p, ir, "", false);
                    cross_icon(p, ir.center(), 4.0, th.ui.text);
                }
                PanelItem::Threshold => {
                    let auto = bp.auto_threshold();
                    let thr = if auto {
                        f64::from(tap.value(value::THRESHOLD + band))
                    } else {
                        bp.threshold
                    };
                    let level = f64::from(tap.value(value::KEY + band));
                    p.text(
                        &format!(
                            "THRESHOLD {}",
                            if auto {
                                "AUTO".to_string()
                            } else {
                                format!("{thr:.1} dB").replace('-', "−")
                            }
                        ),
                        Rect::new(ir.x, ir.y - 16.0, ir.w, 14.0),
                        &dim_text,
                    );
                    self.slider(
                        p,
                        ir,
                        to_normalized(Field::Threshold, thr),
                        Some(to_normalized(Field::Threshold, level)),
                        th.eq.dyn_range,
                        auto,
                    );
                }
                PanelItem::Density => {
                    p.text(
                        &format!("DENSITY {:.0} %", bp.density * 100.0),
                        Rect::new(ir.x, ir.y - 16.0, ir.w, 14.0),
                        &dim_text,
                    );
                    self.slider(p, ir, bp.density as f32, None, th.eq.side, false);
                }
                PanelItem::Key => self.button_tinted(p, ir, "SC", bp.external, th.eq.external),
                PanelItem::Tilt => self.button(p, ir, "Tilt", bp.spectral_tilt),
                PanelItem::Trigger => self.button(
                    p,
                    ir,
                    if bp.free {
                        "Trigger: Free"
                    } else {
                        "Trigger: Band"
                    },
                    bp.free,
                ),
                PanelItem::Audition => self.button(p, ir, "Audition", self.listening),
                PanelItem::Body => {}
            }
        }
    }

    /// A horizontal slider: `t` its value, `level` a meter behind it, the
    /// top meaning "auto" when `auto`.
    fn slider(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        t: f32,
        level: Option<f32>,
        color: Color,
        auto: bool,
    ) {
        let th = &self.theme;
        let track = Rect::new(r.x, r.y + r.h / 2.0 - 3.0, r.w, 6.0);
        p.fill_rounded(track, 3.0, &Paint::Solid(th.eq.display));
        if let Some(l) = level {
            p.fill_rounded(
                Rect::new(track.x, track.y, track.w * l.clamp(0.0, 1.0), track.h),
                3.0,
                &Paint::Solid(th.eq.dyn_live.with_alpha(0.55)),
            );
        }
        let x = track.x + track.w * t.clamp(0.0, 1.0);
        let knob = Rect::new(x - 5.0, r.y, 10.0, r.h);
        p.fill_rounded(
            knob,
            3.0,
            &Paint::Solid(if auto { th.ui.accent } else { color }),
        );
        if auto {
            p.text(
                "A",
                knob.inset_xy(-2.0, 0.0),
                &TextStyle::new(th.fonts.tiny, th.eq.node_text)
                    .bold()
                    .center(),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn panel_knob(
        &self,
        p: &mut dyn Painter,
        r: Rect,
        band: usize,
        field: Field,
        bp: &BandParams,
        tap: &AnalysisTap,
        (scale, piano): (f64, bool),
        color: Color,
    ) {
        let th = &self.theme;
        let value = f64::from(tap.params.get(band_index(band, field)));
        let name = match field {
            Field::Freq => "FREQ",
            Field::Gain => "GAIN",
            Field::Q => "Q",
            Field::Attack => "ATTACK",
            Field::Release => "RELEASE",
            Field::TriggerLow => "LOW CUT",
            Field::TriggerHigh => "HIGH CUT",
            _ => "",
        };
        let text = match field {
            Field::Freq | Field::TriggerLow | Field::TriggerHigh => {
                if piano {
                    super::geometry::note_label(value)
                } else {
                    format_hz(value)
                }
            }
            Field::Q => format!("{value:.2}"),
            Field::Gain => {
                let mut s = super::geometry::db_text(value * scale);
                if bp.dynamic() {
                    s = format!("{s} / {:+.1}", bp.range * scale).replace('-', "−");
                }
                s
            }
            Field::Attack | Field::Release => {
                if (value - 0.5).abs() < 0.005 {
                    "Auto".into()
                } else {
                    format!("{:.0} %", value * 100.0)
                }
            }
            _ => format!("{value:.1}"),
        };
        if field == Field::Gain {
            let (c, knob, ring) = Self::gain_knob(&r);
            let kr = Rect::new(c.x - knob, c.y - knob, knob * 2.0, knob * 2.0);
            controls::knob(
                p,
                kr,
                to_normalized(Field::Gain, value),
                true,
                KnobLook {
                    cap: th.console.knob.cap_top,
                    ring: color,
                },
                th,
            );
            // The dynamic range ring.
            let angle = |db: f64| controls::knob_angle(to_normalized(Field::Gain, db));
            let mut track = Path::new();
            track.arc(c, ring, angle(-30.0), angle(30.0), false);
            p.stroke_path(&track, 3.0, th.eq.display);
            if bp.dynamic() {
                let (a, b) = (angle(value), angle(value + bp.range));
                let mut arc = Path::new();
                arc.arc(c, ring, a.min(b), a.max(b), false);
                p.stroke_path(
                    &arc,
                    3.0,
                    th.eq
                        .dyn_range
                        .with_alpha(if bp.dyn_bypass { 0.35 } else { 0.95 }),
                );
                let moved = f64::from(tap.value(value::DYN + band)) / scale.max(1e-3);
                if moved.abs() > 0.05 {
                    let m = angle(value + moved);
                    let mut live = Path::new();
                    live.arc(c, ring - 0.5, a.min(m), a.max(m), false);
                    p.stroke_path(&live, 3.0, th.eq.dyn_live);
                }
            }
            p.text(
                name,
                Rect::new(r.x - 6.0, r.y - 4.0, r.w + 12.0, 13.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                    .bold()
                    .center()
                    .tracking(0.6),
            );
            p.text(
                &text,
                Rect::new(r.x - 12.0, r.bottom() - 18.0, r.w + 24.0, 14.0),
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
            );
            return;
        }
        let bipolar = false;
        self.small_knob(
            p,
            r,
            name,
            &text,
            to_normalized(field, value),
            bipolar,
            color,
        );
    }
}
