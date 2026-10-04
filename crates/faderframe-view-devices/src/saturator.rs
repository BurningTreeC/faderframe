//! The Saturator's face: the curve as set (drive, bias, gain; the level
//! the signal reaches on it marked), the spectrum going in and coming out
//! (the harmonics it adds), the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::saturator::{self as sat, id, value};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct SaturatorFace {
    accent: Color,
    spectrum: kit::Spectrum,
    /// The input's peak (linear), falling.
    reach: f32,
}

impl SaturatorFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Saturation),
            spectrum: kit::Spectrum::new(),
            reach: 0.0,
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let side = (r.h - 16.0).min(r.w * 0.36);
        let curve = Rect::new(r.x + 12.0, r.y + 8.0, side, side);
        let spec = Rect::new(
            curve.right() + 14.0,
            r.y + 8.0,
            r.right() - curve.right() - 22.0,
            side,
        );
        (curve, spec)
    }
}

impl Face for SaturatorFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(780.0, 460.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 48.0);
        let s = kit::sections(deck, &[("DRIVE", 5.4), ("TONE", 4.0), ("OUTPUT", 3.8)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.56, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::DRIVE), "DRIVE", k[0]));
        c.push(
            Ctl::small(
                pid(id::BIAS),
                "BIAS",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        let side = Rect::new(r.x + r.w * 0.56 + 4.0, r.y + 4.0, r.w * 0.44 - 10.0, 38.0);
        c.push(Ctl::choice(pid(id::TYPE), "TYPE", side));
        c.push(Ctl::segments(
            pid(id::OVERSAMPLING),
            "OVERSAMPLING",
            Rect::new(r.x + 8.0, r.y + KNOB.1 + 10.0, r.w - 16.0, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y + 14.0, r.w, KNOB.1),
            &[SMALL.0, KNOB.0, SMALL.0],
        );
        c.push(
            Ctl::small(
                pid(id::LOW_CUT),
                "LOW CUT",
                kit::at(k[0], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        c.push(Ctl::knob(pid(id::TONE), "TONE", k[1]).bipolar());
        c.push(
            Ctl::small(
                pid(id::HIGH_CUT),
                "HIGH CUT",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[0]));
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[1]).bipolar());
        c.push(Ctl::toggle(
            pid(id::AUTO_GAIN),
            "Auto Gain",
            Rect::new(
                r.x + (r.w - 130.0) / 2.0,
                r.bottom() - SWITCH_H,
                130.0,
                SWITCH_H,
            ),
        ));
        let w = (meter.w - 4.0) / 2.0;
        let meters = vec![
            Meter {
                rect: Rect::new(meter.x, meter.y, w, meter.h),
                kind: MeterKind::Input,
                label: "IN",
            },
            Meter {
                rect: Rect::new(meter.x + w + 4.0, meter.y, w, meter.h),
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
        let (curve, spec) = Self::split(r);
        // The curve: input −1…1 across, output −1…1 up.
        p.fill_rounded(curve, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let cxm = curve.x + curve.w / 2.0;
        let cym = curve.y + curve.h / 2.0;
        for f in [-0.5f32, 0.5] {
            p.vline(
                cxm + f * curve.w / 2.0,
                curve.y,
                curve.bottom(),
                th.device.grid,
            );
            p.hline(
                curve.x,
                curve.right(),
                cym + f * curve.h / 2.0,
                th.device.grid,
            );
        }
        p.vline(cxm, curve.y, curve.bottom(), th.device.grid_strong);
        p.hline(curve.x, curve.right(), cym, th.device.grid_strong);
        p.line(
            Point::new(curve.x, curve.bottom()),
            Point::new(curve.right(), curve.y),
            1.0,
            th.device.grid_strong,
        );
        let kind = cx.value(pid(id::TYPE)).round().clamp(0.0, 5.0) as usize;
        let drive = 10f64.powf(cx.value(pid(id::DRIVE)) / 20.0);
        let bias = 0.5 * cx.value(pid(id::BIAS));
        let makeup =
            10f64.powf((f64::from(cx.published(value::AUTO)) + cx.value(pid(id::OUTPUT))) / 20.0);
        let mix = cx.value(pid(id::MIX));
        let at = |x: f64| {
            let wet = sat::shape(kind, drive * x, bias) * makeup;
            x + mix * (wet - x)
        };
        let to = |x: f64, y: f64| {
            Point::new(
                cxm + (x as f32) * curve.w / 2.0,
                (cym - (y.clamp(-1.05, 1.05) as f32) * curve.h / 2.0)
                    .clamp(curve.y, curve.bottom()),
            )
        };
        let pts: Vec<Point> = (0..=200)
            .map(|i| {
                let x = -1.0 + f64::from(i) / 100.0;
                to(x, at(x))
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.2, self.accent);
        // Where the input's peaks reach.
        let peak = cx.tap.take_value(value::IN_PEAK);
        self.reach = peak.max(self.reach * (1.0 - 1.5 * cx.dt).max(0.0));
        if self.reach > 0.001 {
            let x = f64::from(self.reach.min(1.0));
            let band = Rect::new(
                cxm - (x as f32) * curve.w / 2.0,
                curve.y,
                (x as f32) * curve.w,
                curve.h,
            );
            p.fill(band, self.accent.with_alpha(0.07));
            for s in [-1.0, 1.0] {
                let pt = to(s * x, at(s * x));
                p.circle(pt, 6.0, self.accent.with_alpha(0.25));
                p.circle(pt, 3.5, th.ui.text);
            }
        }
        p.text(
            sat::TYPES[kind],
            Rect::new(curve.x + 8.0, curve.y + 6.0, curve.w - 16.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        p.text(
            &format!("+{:.1} dB", cx.value(pid(id::DRIVE))),
            Rect::new(curve.x + 8.0, curve.bottom() - 20.0, curve.w - 16.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, self.accent)
                .bold()
                .right(),
        );
        // The spectrum: harmonics show as new peaks over the input.
        self.spectrum.paint(p, spec, cx, -110.0, true);
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        sat::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::TYPE => {
                "Soft (round), Tape (soft knee, head bump), Tube (even harmonics), Transistor (hard knee), Fold (folds back), Clip (hard)"
            }
            id::BIAS => "Asymmetry: more even harmonics",
            id::TONE => "Tilt the result darker or brighter round 1 kHz",
            id::OVERSAMPLING => "Run the curve faster to keep aliasing out (adds latency)",
            id::AUTO_GAIN => "Keep the level as the drive changes",
            _ => return None,
        })
    }
}
