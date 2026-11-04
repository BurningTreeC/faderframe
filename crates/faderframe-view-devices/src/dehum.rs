//! De-hum's face: the spectrum going in and coming out up to 2 kHz with a
//! mark at each notch (its width shaded), the fundamental the notches
//! follow now and the mains found, the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::dehum::{self as dh, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct DehumFace {
    accent: Color,
    spectrum: kit::Spectrum,
}

impl DehumFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Limiter),
            spectrum: kit::Spectrum::new(),
        }
    }

    pub(crate) fn split(r: Rect) -> (Rect, Rect, Rect) {
        let spec = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 170.0, r.h - 16.0);
        let freq = Rect::new(
            spec.right() + 12.0,
            r.y + 8.0,
            r.right() - spec.right() - 20.0,
            58.0,
        );
        let mains = Rect::new(freq.x, freq.bottom() + 8.0, freq.w, 58.0);
        (spec, freq, mains)
    }
}

impl Face for DehumFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(860.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 46.0);
        let s = kit::sections(deck, &[("MAINS", 5.4), ("NOTCHES", 4.4), ("OUTPUT", 2.4)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::segments(
            pid(id::FUNDAMENTAL),
            "",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 24.0),
        ));
        let k = kit::row(Rect::new(r.x, r.y + 32.0, r.w, SMALL.1), &[SMALL.0, 110.0]);
        c.push(Ctl::small(pid(id::FREQUENCY), "CUSTOM", k[0]).scaled(Scale::Log));
        c.push(Ctl::toggle(
            pid(id::TRACK),
            "Track Drift",
            Rect::new(k[1].x, k[1].y + 24.0, k[1].w, SWITCH_H),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y + 6.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::HARMONICS), "HARMONICS", k[0]));
        c.push(Ctl::knob(pid(id::WIDTH), "WIDTH", k[1]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::DEPTH), "DEPTH", k[2]));
        let r = kit::inside(&s[2]);
        c.push(Ctl::segments(
            pid(id::LISTEN),
            "LISTEN",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, 38.0),
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
        let (spec, freq, mains) = Self::split(r);
        self.spectrum.paint(p, spec, cx, -110.0, true);
        let f0 = f64::from(cx.published(value::FREQUENCY)).max(1.0);
        let harmonics = cx.value(pid(id::HARMONICS)).round().max(1.0) as usize;
        let width = cx.value(pid(id::WIDTH));
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        for h in 1..=harmonics {
            let f = f0 * h as f64;
            if f >= 0.45 * rate {
                break;
            }
            let (a, b) = (
                kit::Spectrum::x_of(spec, (f - width / 2.0).max(1.0)),
                kit::Spectrum::x_of(spec, f + width / 2.0),
            );
            p.fill(
                Rect::new(a, spec.y, (b - a).max(1.0), spec.h),
                self.accent.with_alpha(0.18),
            );
            let x = kit::Spectrum::x_of(spec, f);
            p.vline(x, spec.y, spec.y + spec.h * 0.12, self.accent);
        }
        kit::readout(
            p,
            freq,
            "FUNDAMENTAL",
            &format!("{f0:.2} Hz"),
            self.accent,
            th,
        );
        let nominal = cx.published(value::NOMINAL);
        let auto = cx.value(pid(id::FUNDAMENTAL)) < 0.5;
        kit::readout(
            p,
            mains,
            if auto { "MAINS FOUND" } else { "NOMINAL" },
            &format!("{nominal:.0} Hz"),
            th.ui.text,
            th,
        );
        let drift = (f0 - f64::from(nominal)) / f64::from(nominal.max(1.0)) * 100.0;
        p.text(
            &format!("drift {drift:+.2} %"),
            Rect::new(mains.x, mains.bottom() + 6.0, mains.w, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).center(),
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        dh::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::FUNDAMENTAL => "Auto finds 50 or 60 Hz; Custom uses the frequency below",
            id::TRACK => "The notches follow the mains as it drifts (within 3 %)",
            id::HARMONICS => "Notches at the fundamental and its harmonics",
            id::WIDTH => "Each notch's width in Hz (the same for every harmonic)",
            id::DEPTH => "How much of the hum is taken",
            id::LISTEN => "Hear the result, or only the hum taken out",
            _ => return None,
        })
    }
}
