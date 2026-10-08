//! The Hardware Insert's face: the way out and back (interface output →
//! the gear → interface input), the round trip and a Ping that measures
//! it, and the send, return and mix controls.

use crate::kit::{self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SWITCH_H};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::hardware_insert::{self as hw, id};
use faderframe_ui_canvas::{
    Color, Paint, Painter, Point, PointerButton, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct HardwareInsertFace {
    accent: Color,
}

impl HardwareInsertFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.wave,
        }
    }

    /// The Ping button in the display.
    fn ping_button(r: Rect) -> Rect {
        Rect::new(r.right() - 132.0, r.bottom() - 46.0, 116.0, 32.0)
    }
}

impl Face for HardwareInsertFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(640.0, 420.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 170.0, 48.0);
        let s = kit::sections(deck, &[("SEND", 3.0), ("RETURN", 4.0), ("MIX", 2.0)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, r.w - KNOB.0 - 8.0],
        );
        c.push(Ctl::knob(pid(id::SEND), "LEVEL", k[0]));
        c.push(Ctl::choice(
            pid(id::SEND_CHANNEL),
            "OUTPUT",
            Rect::new(k[1].x, k[1].y + 12.0, k[1].w, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, r.w - KNOB.0 - 8.0],
        );
        c.push(Ctl::knob(pid(id::RETURN), "LEVEL", k[0]));
        c.push(Ctl::choice(
            pid(id::RETURN_CHANNEL),
            "INPUT",
            Rect::new(k[1].x, k[1].y + 12.0, k[1].w, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::INVERT),
            "Ø Invert",
            Rect::new(k[1].x, k[1].y + 58.0, k[1].w, SWITCH_H),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::knob(
            pid(id::MIX),
            "WET",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y, KNOB.0, KNOB.1),
        ));
        let w = (meter.w - 4.0) / 2.0;
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters: vec![
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
            ],
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        cx.tap.watch();
        let th = cx.theme;
        let channels = |v: f64, word: &str| format!("{word} {}", v.round() as i64 + 1);
        let out = channels(cx.value(pid(id::SEND_CHANNEL)), "OUT");
        let inp = channels(cx.value(pid(id::RETURN_CHANNEL)), "IN");
        // The way: DAW → interface output → gear → interface input → DAW.
        let y = r.y + r.h * 0.38;
        let boxes = [
            ("FADERFRAME", "send"),
            (out.as_str(), "interface"),
            ("YOUR GEAR", "outboard"),
            (inp.as_str(), "interface"),
            ("FADERFRAME", "return"),
        ];
        let n = boxes.len() as f32;
        let gap = 18.0;
        let bw = ((r.w - 32.0 - gap * (n - 1.0)) / n).max(40.0);
        let label = TextStyle::new(th.fonts.small, th.ui.text).bold().center();
        let small = TextStyle::new(th.fonts.tiny, th.ui.text_dim).center();
        for (i, (title, what)) in boxes.iter().enumerate() {
            let x = r.x + 16.0 + i as f32 * (bw + gap);
            let b = Rect::new(x, y - 22.0, bw, 44.0);
            let gear = i == 2;
            p.fill_rounded(
                b,
                5.0,
                &Paint::Solid(if gear {
                    self.accent.with_alpha(0.22)
                } else {
                    th.device.display.lighten(0.05)
                }),
            );
            p.stroke_rounded(
                b,
                5.0,
                1.0,
                self.accent.with_alpha(if gear { 0.9 } else { 0.4 }),
            );
            p.text(title, Rect::new(b.x, b.y + 6.0, b.w, 16.0), &label);
            p.text(what, Rect::new(b.x, b.y + 24.0, b.w, 14.0), &small);
            if i + 1 < boxes.len() {
                let a = Point::new(b.right() + 3.0, y);
                let e = Point::new(b.right() + gap - 3.0, y);
                p.line(a, e, 1.5, self.accent);
                p.line(e, Point::new(e.x - 5.0, y - 4.0), 1.5, self.accent);
                p.line(e, Point::new(e.x - 5.0, y + 4.0), 1.5, self.accent);
            }
        }
        // The round trip and the ping.
        let trip = cx.value(pid(id::ROUND_TRIP));
        let rate = f64::from(cx.model.sample_rate().max(1));
        let text = if trip < 0.5 {
            "Round trip not measured yet — Ping it (the gear in, levels down: it is a click)"
                .to_string()
        } else {
            format!(
                "Round trip {:.0} samples ({:.2} ms), compensated",
                trip,
                trip * 1000.0 / rate
            )
        };
        p.text(
            &text,
            Rect::new(r.x + 16.0, r.bottom() - 42.0, r.w - 170.0, 24.0),
            &TextStyle::new(
                th.fonts.small,
                if trip < 0.5 {
                    th.ui.text_dim
                } else {
                    th.ui.text
                },
            ),
        );
        let b = Self::ping_button(r);
        let busy = cx.model.pinging();
        kit::readout(
            p,
            b,
            "",
            if busy { "MEASURING…" } else { "PING" },
            self.accent,
            th,
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        _cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        if let ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            ..
        } = *ev
            && Self::ping_button(r).contains(pos)
        {
            edit.ping();
            return true;
        }
        false
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        hw::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::SEND_CHANNEL => "The interface output the gear's input is patched to",
            id::RETURN_CHANNEL => "The interface input the gear's output is patched to",
            id::SEND => "Level into the gear",
            id::RETURN => "Level of what comes back",
            id::MIX => "How much of the gear (100 %: the gear alone)",
            id::INVERT => "Flip the return's polarity (gear that inverts)",
            _ => return None,
        })
    }
}
