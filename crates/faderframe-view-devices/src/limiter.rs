//! The Limiter's face: a large history of the level going in, coming out
//! (in the limiter's colour) and the reduction (from the top), the
//! ceiling over it, readouts of the reduction and the output's true peak,
//! the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::limiter::{self as lim, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Rect, Size};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct LimiterFace {
    accent: Color,
    history: kit::History,
}

impl LimiterFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Limiter),
            history: kit::History::new(8.0, 40.0),
        }
    }
}

impl Face for LimiterFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(760.0, 440.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 170.0, 70.0);
        let s = kit::sections(
            deck,
            &[("LIMITING", 4.0), ("RELEASE", 3.0), ("OPTIONS", 3.0)],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.62, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::GAIN), "GAIN", k[0]));
        c.push(Ctl::knob(pid(id::CEILING), "CEILING", k[1]));
        c.push(Ctl::choice(
            pid(id::STYLE),
            "STYLE",
            Rect::new(r.x + r.w * 0.62 + 4.0, r.y + 10.0, r.w * 0.38 - 8.0, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::RELEASE), "RELEASE", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(
            pid(id::LOOKAHEAD),
            "LOOKAHEAD",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            pid(id::AUTO_RELEASE),
            "Auto Release",
            Rect::new(r.x + 8.0, r.bottom() - SWITCH_H, 130.0, SWITCH_H),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::toggle(
            pid(id::TRUE_PEAK),
            "True Peak",
            Rect::new(r.x + 6.0, r.y + 4.0, r.w * 0.5 - 10.0, SWITCH_H),
        ));
        c.push(Ctl::toggle(
            pid(id::UNITY),
            "Unity Gain",
            Rect::new(r.x + 6.0, r.y + 34.0, r.w * 0.5 - 10.0, SWITCH_H),
        ));
        c.push(Ctl::small(
            pid(id::LINK),
            "LINK",
            Rect::new(
                r.x + r.w * 0.5 + (r.w * 0.5 - SMALL.0) / 2.0,
                r.y,
                SMALL.0,
                SMALL.1,
            ),
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
                label: "GR",
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
        self.history.record(
            cx,
            value::IN_PEAK,
            value::OUT_PEAK,
            Some(value::REDUCTION_PEAK),
        );
        let th = cx.theme;
        let hist = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 150.0, r.h - 16.0);
        self.history.paint(p, hist, cx, -36.0, 12.0, true);
        // The ceiling, dashed.
        let ceiling = cx.value(pid(id::CEILING)) as f32;
        let y = kit::History::level_y(hist, ceiling, -36.0);
        let mut x = hist.x;
        while x < hist.right() {
            p.hline(
                x,
                (x + 6.0).min(hist.right()),
                y,
                self.accent.with_alpha(0.8),
            );
            x += 10.0;
        }
        // Readouts: the reduction and the output's true peak, held.
        let gr = self.history.recent_max(2, 2.0).max(0.0);
        let tp = self.history.recent_max(1, 3.0);
        let side = Rect::new(r.right() - 132.0, r.y + 12.0, 120.0, 46.0);
        kit::readout(
            p,
            side,
            "REDUCTION",
            &if gr > 0.05 {
                format!("−{gr:.1} dB")
            } else {
                "0.0 dB".into()
            },
            th.device.reduction,
            th,
        );
        let unit = if cx.on(pid(id::TRUE_PEAK)) {
            "dBTP"
        } else {
            "dB"
        };
        kit::readout(
            p,
            Rect::new(side.x, side.bottom() + 10.0, side.w, side.h),
            "OUTPUT PEAK",
            &if tp > -100.0 {
                format!("{tp:.1} {unit}").replace('-', "−")
            } else {
                "−∞".into()
            },
            if tp > ceiling + 0.05 {
                th.tools.level_over
            } else {
                self.accent
            },
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        lim::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::GAIN => "Drive into the limiter",
            id::CEILING => "Nothing comes out louder",
            id::STYLE => "Transparent (smooth), Punchy (keeps transients), Aggressive (loud)",
            id::LOOKAHEAD => "How far ahead peaks are seen (adds as much latency)",
            id::TRUE_PEAK => "Keep the peaks between samples under the ceiling too",
            id::UNITY => "Take the gain back off to hear what limiting does",
            id::AUTO_RELEASE => "Release slower while limiting goes on",
            _ => return None,
        })
    }
}
