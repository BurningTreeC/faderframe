//! The Compressor's face: its transfer curve with the level moving on it
//! (drag across the curve to set the threshold), a scrolling history of
//! what goes in, what comes out and how much is taken, the controls and
//! the meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale, at,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::compressor::{self as comp, id, value};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct CompressorFace {
    accent: Color,
    history: kit::History,
    /// Dragging the threshold: where it started and the value then.
    drag: Option<(f32, f64)>,
}

impl CompressorFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Dynamics),
            history: kit::History::new(6.0, 40.0),
            drag: None,
        }
    }

    /// The display's two parts: the transfer curve (square) and the
    /// history.
    fn split(r: Rect) -> (Rect, Rect) {
        let side = (r.h - 16.0).min(r.w * 0.42);
        let curve = Rect::new(r.x + 34.0, r.y + 8.0, side, side);
        let hist = Rect::new(
            curve.right() + 16.0,
            r.y + 8.0,
            r.right() - curve.right() - 50.0,
            side,
        );
        (curve, hist)
    }
}

impl Face for CompressorFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(820.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 70.0);
        let s = kit::sections(
            deck,
            &[
                ("COMPRESSION", 5.0),
                ("TIME", 4.0),
                ("CHARACTER", 3.0),
                ("SIDECHAIN", 2.5),
                ("OUTPUT", 2.6),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(pid(id::RATIO), "RATIO", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(
            pid(id::KNEE),
            "KNEE",
            at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::small(
            pid(id::RANGE),
            "RANGE",
            at(k[3], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let row2 = Rect::new(r.x, r.y + KNOB.1 + 10.0, r.w, SWITCH_H + 26.0);
        c.push(Ctl::segments(
            pid(id::DETECTOR),
            "DETECTOR",
            Rect::new(row2.x + 8.0, row2.y, 132.0, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::ATTACK), "ATTACK", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::RELEASE), "RELEASE", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(
            pid(id::LOOKAHEAD),
            "LOOKAHEAD",
            at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            pid(id::AUTO_RELEASE),
            "Auto Release",
            Rect::new(r.x + 8.0, r.y + KNOB.1 + 18.0, 120.0, SWITCH_H),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::choice(
            pid(id::STYLE),
            "STYLE",
            Rect::new(r.x + 6.0, r.y + 2.0, r.w - 12.0, 38.0),
        ));
        let k = kit::spread(Rect::new(r.x, r.y + 46.0, r.w, SMALL.1), 2, SMALL.0);
        c.push(Ctl::small(pid(id::COLOR), "COLOR", k[0]));
        c.push(Ctl::small(pid(id::LINK), "LINK", k[1]));
        let r = kit::inside(&s[3]);
        c.push(Ctl::toggle(
            pid(id::EXTERNAL),
            "External",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, SWITCH_H),
        ));
        let k = kit::spread(Rect::new(r.x, r.y + 30.0, r.w, SMALL.1), 2, SMALL.0 - 4.0);
        c.push(Ctl::small(pid(id::SC_LOW), "LOW CUT", k[0]));
        c.push(Ctl::small(pid(id::SC_HIGH), "HIGH CUT", k[1]));
        c.push(Ctl::toggle(
            pid(id::LISTEN),
            "Listen",
            Rect::new(r.x + 4.0, r.bottom() - SWITCH_H, r.w - 8.0, SWITCH_H),
        ));
        let r = kit::inside(&s[4]);
        let k = kit::spread(Rect::new(r.x, r.y, r.w, KNOB.1), 2, KNOB.0 - 6.0);
        c.push(Ctl::knob(pid(id::MAKEUP), "MAKEUP", k[0]).bipolar());
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[1]));
        c.push(Ctl::toggle(
            pid(id::AUTO_MAKEUP),
            "Auto Makeup",
            Rect::new(r.x + 4.0, r.y + KNOB.1 + 18.0, r.w - 8.0, SWITCH_H),
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
        let (curve, hist) = Self::split(r);
        let grid = th.device.grid;
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        // The transfer curve: −60 to 0 dB both ways.
        let to_x = |d: f64| curve.x + curve.w * ((d + 60.0) / 60.0).clamp(0.0, 1.0) as f32;
        let to_y = |d: f64| curve.bottom() - curve.h * ((d + 60.0) / 60.0).clamp(0.0, 1.0) as f32;
        for d in [-48.0, -36.0, -24.0, -12.0] {
            p.vline(to_x(d), curve.y, curve.bottom(), grid);
            p.hline(curve.x, curve.right(), to_y(d), grid);
            p.text(
                &format!("{d:.0}").replace('-', "−"),
                Rect::new(curve.x - 32.0, to_y(d) - 7.0, 28.0, 14.0),
                &label.right(),
            );
        }
        p.stroke_rounded(curve, 2.0, 1.0, th.device.grid_strong);
        p.line(
            Point::new(curve.x, curve.bottom()),
            Point::new(curve.right(), curve.y),
            1.0,
            th.device.grid_strong,
        );
        let threshold = cx.value(pid(id::THRESHOLD));
        let ratio = cx.value(pid(id::RATIO));
        let knee = cx.value(pid(id::KNEE));
        let range = cx.value(pid(id::RANGE));
        let range = if range >= 59.5 { f64::INFINITY } else { range };
        let pts: Vec<Point> = (0..=120)
            .map(|i| {
                let x = -60.0 + f64::from(i) * 0.5;
                let y = x - comp::reduction(x, threshold, ratio, knee).min(range);
                Point::new(to_x(x), to_y(y))
            })
            .collect();
        let mut fill = Path::polyline(&pts);
        fill.line_to(Point::new(curve.right(), curve.bottom()))
            .line_to(Point::new(curve.x, curve.bottom()))
            .close();
        p.fill_path_paint(
            &fill,
            &Paint::vertical(
                curve,
                self.accent.with_alpha(0.22),
                self.accent.with_alpha(0.03),
            ),
        );
        p.stroke_path(&Path::polyline(&pts), 2.0, self.accent);
        // The threshold, dashed.
        let tx = to_x(threshold);
        let mut y = curve.y;
        while y < curve.bottom() {
            p.vline(
                tx,
                y,
                (y + 4.0).min(curve.bottom()),
                self.accent.with_alpha(0.55),
            );
            y += 8.0;
        }
        // Where the signal is now.
        let level = f64::from(cx.published(value::LEVEL));
        if level > -60.0 {
            let gr = f64::from(cx.published(value::REDUCTION));
            let at = Point::new(to_x(level), to_y(level - gr));
            p.circle(at, 7.0, self.accent.with_alpha(0.25));
            p.circle(at, 4.0, th.ui.text);
        }
        p.text(
            &format!(
                "{} · {}",
                crate::values::db_text(threshold),
                comp::format(pid(id::RATIO), ratio).unwrap_or_default()
            ),
            Rect::new(curve.x + 6.0, curve.y + 4.0, curve.w - 12.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        // The history: input filled, output a line, reduction from the top.
        self.history.paint(p, hist, cx, -60.0, 24.0, false);
        p.hline(
            hist.x,
            hist.right(),
            kit::History::level_y(hist, threshold as f32, -60.0),
            self.accent.with_alpha(0.45),
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (curve, _) = Self::split(r);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if curve.contains(pos) => {
                self.drag = Some((pos.x, cx.value(pid(id::THRESHOLD))));
                edit.begin("Threshold");
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } => {
                let Some((x0, from)) = self.drag else {
                    return false;
                };
                let db = from + f64::from((pos.x - x0) / curve.w * 60.0);
                edit.set(pid(id::THRESHOLD), db.clamp(-60.0, 0.0));
                true
            }
            ViewEvent::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    edit.end();
                }
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        comp::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::STYLE => {
                "Clean (transparent), Punch (peaks, snappy), Opto (smooth, programme dependent), Vintage (fast, with bite), Bus (glue)"
            }
            id::KNEE => "How gradually compression starts round the threshold",
            id::RANGE => "The most it may turn down",
            id::LOOKAHEAD => "See peaks coming (adds as much latency)",
            id::COLOR => "Harmonic saturation",
            id::LINK => "How much both channels follow the louder one",
            id::EXTERNAL => "Key from the sidechain when one is routed in",
            id::LISTEN => "Hear the key through its filters",
            _ => return None,
        })
    }
}
