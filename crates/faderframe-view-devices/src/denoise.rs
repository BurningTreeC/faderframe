//! De-noise's face: the spectrum going in and coming out with the noise the
//! device works against (the learned profile, or what it tracks) and the
//! gain it applies per band along the top; a Learn button (press, play a
//! stretch of noise alone, press again: the profile is kept as one undo
//! step) and Forget; the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::denoise::{self as dn, BANDS, band_centre, id, value};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// Where a noise level per sample reads in the spectrum's Hann-4096
/// analyser (a full-scale sine at 0 dB): 10·log10(6 / 4096).
const ANALYSER_OFFSET: f32 = -28.3;
/// The gain curve's depth across the top of the display (dB).
const GAIN_RANGE: f32 = 40.0;

pub(crate) struct DenoiseFace {
    accent: Color,
    spectrum: kit::Spectrum,
}

impl DenoiseFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::DeEsser),
            spectrum: kit::Spectrum::new(),
        }
    }

    /// The spectrum's area and the buttons over it (Learn, Forget).
    pub(crate) fn layout(r: Rect) -> (Rect, Rect, Rect) {
        let spec = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 16.0, r.h - 16.0);
        let learn = Rect::new(spec.x + 10.0, spec.y + 10.0, 150.0, 26.0);
        let forget = Rect::new(learn.right() + 8.0, learn.y, 80.0, 26.0);
        (spec, learn, forget)
    }
}

impl Face for DenoiseFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(960.0, 500.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 70.0);
        let s = kit::sections(
            deck,
            &[
                ("NOISE", 5.4),
                ("CHARACTER", 5.6),
                ("BAND", 2.6),
                ("OUTPUT", 2.6),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, 120.0]);
        c.push(Ctl::knob(pid(id::REDUCTION), "REDUCTION", k[0]));
        c.push(Ctl::knob(pid(id::THRESHOLD), "THRESHOLD", k[1]).bipolar());
        c.push(Ctl::segments(
            pid(id::SOURCE),
            "SOURCE",
            Rect::new(k[2].x, k[2].y + 4.0, k[2].w, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::MODE),
            "MODE",
            Rect::new(k[2].x, k[2].y + 50.0, k[2].w, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::SMOOTHING), "SMOOTHING", k[0]));
        c.push(Ctl::knob(pid(id::SPECTRAL), "SPECTRAL", k[1]));
        c.push(Ctl::knob(pid(id::TILT), "TILT", k[2]).bipolar());
        c.push(Ctl::knob(pid(id::TRANSIENTS), "TRANSIENTS", k[3]));
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, SMALL.1), &[SMALL.0, SMALL.0]);
        c.push(Ctl::small(pid(id::LOW), "LOW", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::HIGH), "HIGH", k[1]).scaled(Scale::Log));
        let r = kit::inside(&s[3]);
        c.push(Ctl::segments(
            pid(id::LISTEN),
            "LISTEN",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::LINK),
            "Stereo Link",
            Rect::new(r.x + 4.0, r.bottom() - SWITCH_H, r.w - 8.0, SWITCH_H),
        ));
        let w = (meter.w - 8.0) / 3.0;
        let meters = vec![
            Meter {
                rect: Rect::new(meter.x, meter.y, w, meter.h),
                kind: MeterKind::Input,
                label: "IN",
            },
            Meter {
                rect: Rect::new(meter.x + w + 4.0, meter.y, w, meter.h),
                kind: MeterKind::Reduction(value::REDUCTION),
                label: "NR",
            },
            Meter {
                rect: Rect::new(meter.x + 2.0 * (w + 4.0), meter.y, w, meter.h),
                kind: MeterKind::Output,
                label: "OUT",
            },
        ];
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters,
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        self.spectrum.update(cx);
        let th = cx.theme;
        let (spec, learn, forget) = Self::layout(r);
        let floor = -110.0;
        self.spectrum.paint(p, spec, cx, floor, true);
        let y = |d: f32| spec.bottom() - spec.h * ((d - floor) / -floor).clamp(0.0, 1.0);
        // The noise: per band, where the analyser reads it (with its tilt).
        let noise: Vec<Point> = (0..BANDS)
            .map(|b| {
                let f = band_centre(b);
                let tilt = 3.0 * (f / 1_000.0).log2() as f32;
                let db = cx.published(value::NOISE + b) + ANALYSER_OFFSET + tilt;
                Point::new(kit::Spectrum::x_of(spec, f), y(db))
            })
            .collect();
        let mut fill = Path::polyline(&noise);
        fill.line_to(Point::new(spec.right(), spec.bottom()))
            .line_to(Point::new(spec.x, spec.bottom()))
            .close();
        p.fill_path(&fill, self.accent.with_alpha(0.14));
        p.stroke_path(&Path::polyline(&noise), 1.8, self.accent);
        // The gain per band, down from the top.
        let gy = |d: f32| spec.y + spec.h * 0.45 * (-d / GAIN_RANGE).clamp(0.0, 1.0);
        let gain: Vec<Point> = (0..BANDS)
            .map(|b| {
                let f = band_centre(b);
                Point::new(
                    kit::Spectrum::x_of(spec, f),
                    gy(cx.published(value::GAIN + b)),
                )
            })
            .collect();
        let mut area = Path::new();
        area.move_to(Point::new(spec.x, spec.y));
        for q in &gain {
            area.line_to(*q);
        }
        area.line_to(Point::new(spec.right(), spec.y)).close();
        p.fill_path(&area, th.device.reduction.with_alpha(0.22));
        p.stroke_path(&Path::polyline(&gain), 1.4, th.device.reduction);
        // The band it works in: shaded outside.
        let (lo, hi) = (cx.value(pid(id::LOW)), cx.value(pid(id::HIGH)));
        let shade = th.device.display.with_alpha(0.55);
        let xl = kit::Spectrum::x_of(spec, lo);
        if xl > spec.x {
            p.fill(Rect::new(spec.x, spec.y, xl - spec.x, spec.h), shade);
        }
        let xh = kit::Spectrum::x_of(spec, hi);
        if xh < spec.right() {
            p.fill(Rect::new(xh, spec.y, spec.right() - xh, spec.h), shade);
        }
        // Learn and Forget, and what the noise is.
        let learning = cx.on(pid(id::LEARN));
        let label = if learning {
            format!("STOP LEARNING · {:.1} s", cx.published(value::LEARNED))
        } else {
            "LEARN NOISE".to_string()
        };
        if learning {
            p.shadow(learn, 4.0, self.accent.with_alpha(0.5), 0.0, 0.0, 10.0);
            p.fill_rounded(learn, 4.0, &Paint::Solid(self.accent.with_alpha(0.45)));
        }
        kit::display_button(p, learn, &label, self.accent, th);
        let has_profile = cx.on(pid(id::PROFILE_SET));
        if has_profile {
            kit::display_button(p, forget, "FORGET", self.accent, th);
        }
        let status = if learning {
            "Play noise alone…"
        } else if cx.published(value::ADAPTIVE) >= 0.5 {
            if cx.on(pid(id::SOURCE)) && !has_profile {
                "No profile yet: tracking the noise"
            } else {
                "Tracking the noise"
            }
        } else {
            "Learned profile"
        };
        let at = if has_profile {
            forget.right()
        } else {
            learn.right()
        };
        p.text(
            status,
            Rect::new(at + 12.0, learn.y, 260.0, learn.h),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (_, learn, forget) = Self::layout(r);
        let ViewEvent::PointerDown { pos, .. } = *ev else {
            return false;
        };
        if learn.contains(pos) {
            if !cx.on(pid(id::LEARN)) {
                edit.begin("Learn Noise");
                edit.set(pid(id::LEARN), 1.0);
                edit.end();
            } else if let Some(values) = dn::learned_profile(cx.tap) {
                edit.begin("Learn Noise Profile");
                for (p, v) in values {
                    edit.set(p, v);
                }
                edit.end();
            } else {
                edit.set_once(pid(id::LEARN), 0.0);
            }
            return true;
        }
        if forget.contains(pos) && cx.on(pid(id::PROFILE_SET)) {
            edit.begin("Forget Noise Profile");
            edit.set(pid(id::PROFILE_SET), 0.0);
            edit.set(pid(id::SOURCE), 0.0);
            edit.end();
            return true;
        }
        false
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        dn::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::REDUCTION => "The most the noise is turned down",
            id::THRESHOLD => "How far over the noise a sound must stand to be kept",
            id::SOURCE => "Adaptive follows the noise; Learned uses the profile Learn took",
            id::MODE => "Low Latency for live use, Quality (with lookahead) for mixing",
            id::SMOOTHING => "Steadier gains: fewer artefacts, slower to follow",
            id::SPECTRAL => "Gains smoothed across frequency: less 'musical noise'",
            id::TILT => "More reduction above 1 kHz (positive) or below it",
            id::TRANSIENTS => "Onsets open the gains: attacks keep their edge",
            id::LISTEN => "Hear the result, or only the noise taken out",
            _ => return None,
        })
    }
}
