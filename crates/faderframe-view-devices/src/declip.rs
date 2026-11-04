//! De-clip's face: a history of the peaks going in and coming out with the
//! clip level as a line (rebuilt peaks rise over it), the level found, the
//! share of samples clipped and the runs rebuilt per second, the controls
//! and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SWITCH_H};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::declip::{self as dc, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Rect, Size};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct DeclipFace {
    accent: Color,
    history: kit::History,
}

impl DeclipFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Saturation),
            history: kit::History::new(6.0, 40.0),
        }
    }

    pub(crate) fn split(r: Rect) -> (Rect, [Rect; 3]) {
        let hist = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 170.0, r.h - 16.0);
        let x = hist.right() + 12.0;
        let w = r.right() - x - 8.0;
        let at = |i: f32| Rect::new(x, r.y + 8.0 + i * 64.0, w, 56.0);
        (hist, [at(0.0), at(1.0), at(2.0)])
    }
}

impl Face for DeclipFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(860.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 46.0);
        let s = kit::sections(deck, &[("DETECTION", 6.0), ("REBUILD", 4.6)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[110.0, KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::toggle(
            pid(id::AUTO),
            "Auto Level",
            Rect::new(k[0].x, k[0].y + 30.0, k[0].w, SWITCH_H),
        ));
        c.push(Ctl::knob(pid(id::LEVEL), "LEVEL", k[1]));
        c.push(Ctl::knob(pid(id::TOLERANCE), "TOLERANCE", k[2]));
        c.push(Ctl::knob(pid(id::MAX_LENGTH), "MAX LENGTH", k[3]));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[110.0, KNOB.0, 110.0]);
        c.push(Ctl::segments(
            pid(id::QUALITY),
            "QUALITY",
            Rect::new(k[0].x, k[0].y + 10.0, k[0].w, 38.0),
        ));
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[1]).bipolar());
        c.push(Ctl::segments(
            pid(id::LISTEN),
            "LISTEN",
            Rect::new(k[2].x, k[2].y + 10.0, k[2].w, 38.0),
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
        self.history
            .record(cx, value::IN_PEAK, value::OUT_PEAK, None);
        let th = cx.theme;
        let (hist, boxes) = Self::split(r);
        let floor = -36.0;
        self.history.paint(p, hist, cx, floor, 0.0, false);
        let level = cx.published(value::LEVEL);
        let y = kit::History::level_y(hist, level, floor);
        p.hline(hist.x, hist.right(), y, self.accent.with_alpha(0.8));
        kit::readout(
            p,
            boxes[0],
            "CLIP LEVEL",
            // Auto finds no level in silence.
            &if level > -150.0 {
                format!("{level:.1} dB").replace('-', "−")
            } else {
                "—".to_string()
            },
            self.accent,
            th,
        );
        kit::readout(
            p,
            boxes[1],
            "CLIPPED",
            &format!("{:.1} %", cx.published(value::CLIPPED) * 100.0),
            th.ui.text,
            th,
        );
        kit::readout(
            p,
            boxes[2],
            "REBUILT / S",
            &format!("{:.0}", cx.published(value::RATE)),
            th.ui.text,
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        dc::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::AUTO => "Find the clip level (the loudest lately), or use Level",
            id::TOLERANCE => "Samples this close under the level count as clipped",
            id::MAX_LENGTH => "Longer runs are left as they are",
            id::QUALITY => "Best fits the model twice",
            id::OUTPUT => "Rebuilt peaks go over the old ceiling: make room here",
            id::LISTEN => "Hear the result, or only what the repair adds",
            _ => return None,
        })
    }
}
