//! De-click's face: a history of the level going in and coming out with a
//! mark where clicks were taken (taller for more in a moment), the clicks
//! per second and in all, the controls and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SWITCH_H};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::declick::{self as dc, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Rect, Size};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// History marks per second.
const MARK_RATE: f32 = 40.0;

pub(crate) struct DeclickFace {
    accent: Color,
    history: kit::History,
    /// Clicks taken per history step, oldest first from `head`.
    marks: Vec<f32>,
    head: usize,
    since: f32,
    /// The running count last seen.
    seen: f32,
}

impl DeclickFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Gate),
            history: kit::History::new(6.0, MARK_RATE),
            marks: vec![0.0; (6.0 * MARK_RATE) as usize],
            head: 0,
            since: 0.0,
            seen: 0.0,
        }
    }

    pub(crate) fn split(r: Rect) -> (Rect, Rect, Rect) {
        let hist = Rect::new(r.x + 8.0, r.y + 8.0, r.w - 170.0, r.h - 16.0);
        let rate = Rect::new(
            hist.right() + 12.0,
            r.y + 8.0,
            r.right() - hist.right() - 20.0,
            58.0,
        );
        let total = Rect::new(rate.x, rate.bottom() + 8.0, rate.w, 58.0);
        (hist, rate, total)
    }

    fn record_marks(&mut self, cx: &Ctx<'_>) {
        let total = cx.published(value::TOTAL);
        let new = (total - self.seen).max(0.0);
        self.seen = total;
        self.since += cx.dt;
        let step = 1.0 / MARK_RATE;
        let mut pending = new;
        while self.since >= step {
            self.since -= step;
            self.marks[self.head] = pending;
            pending = 0.0;
            self.head = (self.head + 1) % self.marks.len();
        }
        if pending > 0.0 {
            let last = (self.head + self.marks.len() - 1) % self.marks.len();
            self.marks[last] += pending;
        }
    }
}

impl Face for DeclickFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(760.0, 460.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 46.0);
        let s = kit::sections(deck, &[("DETECTION", 5.0), ("OUTPUT", 2.6)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y + 6.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::SENSITIVITY), "SENSITIVITY", k[0]));
        c.push(Ctl::knob(pid(id::WIDTH), "CLICK WIDTH", k[1]));
        c.push(Ctl::knob(pid(id::CRACKLE), "CRACKLE", k[2]));
        let r = kit::inside(&s[1]);
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
        self.record_marks(cx);
        let th = cx.theme;
        let (hist, rate, total) = Self::split(r);
        self.history.paint(p, hist, cx, -60.0, 0.0, false);
        // A mark per step that took clicks, taller for more.
        let n = self.marks.len();
        for i in 0..n {
            let m = self.marks[(self.head + i) % n];
            if m <= 0.0 {
                continue;
            }
            let x = hist.x + hist.w * i as f32 / (n - 1).max(1) as f32;
            let h = hist.h * (0.12 + 0.08 * m).min(0.6);
            p.vline(x, hist.y, hist.y + h, self.accent);
        }
        kit::readout(
            p,
            rate,
            "CLICKS / S",
            &format!("{:.0}", cx.published(value::RATE)),
            self.accent,
            th,
        );
        kit::readout(
            p,
            total,
            "TAKEN",
            &format!("{:.0}", cx.published(value::TOTAL)),
            th.ui.text,
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        dc::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::SENSITIVITY => "More clicks found (and more risk of touching the music)",
            id::WIDTH => "The widest click repaired: longer ones are left (transients)",
            id::CRACKLE => "Dense, tiny clicks taken too",
            id::LINK => "A click found on one channel is repaired on both",
            id::LISTEN => "Hear the result, or only the clicks taken out",
            _ => return None,
        })
    }
}
