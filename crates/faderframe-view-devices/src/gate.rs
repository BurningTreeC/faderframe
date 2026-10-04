//! The Gate's face: its curve (output for input as a gate or expander, the
//! gain for the key's level as a ducker) with the open and close
//! thresholds (drag to move them), the history of the level and of the
//! attenuation with a strip lit while open, the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::gate::{self as gate, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct GateFace {
    accent: Color,
    history: kit::History,
    open: Vec<bool>,
    open_head: usize,
    drag: Option<(f32, f64)>,
}

impl GateFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Gate),
            history: kit::History::new(6.0, 40.0),
            open: vec![false; 240],
            open_head: 0,
            drag: None,
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let side = (r.h - 16.0).min(r.w * 0.40);
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

impl Face for GateFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(820.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 70.0);
        let s = kit::sections(deck, &[("GATE", 5.0), ("TIME", 4.6), ("DETECTION", 4.4)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::segments(
            pid(id::MODE),
            "",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 24.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 32.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(pid(id::RANGE), "RANGE", k[1]));
        c.push(
            Ctl::small(
                pid(id::RATIO),
                "RATIO",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y + 6.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::ATTACK), "ATTACK", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::HOLD), "HOLD", k[1]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::knob(pid(id::RELEASE), "RELEASE", k[2]).scaled(Scale::Log));
        c.push(Ctl::small(
            pid(id::LOOKAHEAD),
            "LOOKAHEAD",
            kit::at(k[3], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::HYSTERESIS), "HYSTERESIS", k[0]));
        c.push(Ctl::small(pid(id::SC_LOW), "LOW CUT", k[1]));
        c.push(Ctl::small(pid(id::SC_HIGH), "HIGH CUT", k[2]));
        let b = kit::row(
            Rect::new(r.x, r.bottom() - SWITCH_H, r.w, SWITCH_H),
            &[110.0, 90.0],
        );
        c.push(Ctl::toggle(pid(id::EXTERNAL), "Sidechain", b[0]));
        c.push(Ctl::toggle(pid(id::LISTEN), "Listen", b[1]));
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
                label: "ATT",
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
        let is_open = cx.published(value::OPEN) >= 0.5;
        // The open strip follows the history's pace closely enough.
        self.open[self.open_head] = is_open;
        self.open_head = (self.open_head + 1) % self.open.len();
        let th = cx.theme;
        let (curve, hist) = Self::split(r);
        let threshold = cx.value(pid(id::THRESHOLD));
        let range = cx.value(pid(id::RANGE));
        let ratio = cx.value(pid(id::RATIO));
        let hyst = cx.value(pid(id::HYSTERESIS));
        let mode = cx.value(pid(id::MODE)).round() as i64;
        let to_x = |d: f64| curve.x + curve.w * ((d + 80.0) / 80.0).clamp(0.0, 1.0) as f32;
        let to_y = |d: f64| curve.bottom() - curve.h * ((d + 80.0) / 80.0).clamp(0.0, 1.0) as f32;
        for d in [-60.0, -40.0, -20.0] {
            p.vline(to_x(d), curve.y, curve.bottom(), th.device.grid);
            p.hline(curve.x, curve.right(), to_y(d), th.device.grid);
            p.text(
                &format!("{d:.0}").replace('-', "−"),
                Rect::new(curve.x - 32.0, to_y(d) - 7.0, 28.0, 14.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
            );
        }
        p.stroke_rounded(curve, 2.0, 1.0, th.device.grid_strong);
        let pts: Vec<Point> = (0..=160)
            .map(|i| {
                let x = -80.0 + f64::from(i) * 0.5;
                let y = match mode {
                    // Expander: down by the ratio under the threshold.
                    1 => {
                        x + if x < threshold {
                            ((x - threshold) * (ratio - 1.0)).max(range)
                        } else {
                            0.0
                        }
                    }
                    // Ducker: the gain for the key's level (0 dB at the
                    // top of the box).
                    2 => {
                        if x > threshold {
                            range.max(-80.0)
                        } else {
                            0.0
                        }
                    }
                    _ => {
                        if x >= threshold {
                            x
                        } else {
                            x + range
                        }
                    }
                };
                Point::new(
                    to_x(x),
                    if mode == 2 {
                        curve.y + curve.h * (-y / 80.0).clamp(0.0, 1.0) as f32
                    } else {
                        to_y(y)
                    },
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.0, self.accent);
        // Open and close thresholds.
        for (t, alpha) in [(threshold, 0.7), (threshold - hyst, 0.35)] {
            let x = to_x(t);
            let mut y = curve.y;
            while y < curve.bottom() {
                p.vline(
                    x,
                    y,
                    (y + 4.0).min(curve.bottom()),
                    self.accent.with_alpha(alpha),
                );
                y += 8.0;
            }
        }
        let level = f64::from(cx.published(value::LEVEL));
        if level > -80.0 {
            p.circle(
                Point::new(to_x(level), curve.bottom() - 6.0),
                4.0,
                th.ui.text,
            );
        }
        p.text(
            gate::MODES[mode.clamp(0, 2) as usize],
            Rect::new(curve.x + 6.0, curve.y + 4.0, curve.w - 12.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        // The history and the open strip under it.
        let strip = Rect::new(hist.x, hist.bottom() - 6.0, hist.w, 6.0);
        let hist_r = Rect::new(hist.x, hist.y, hist.w, hist.h - 10.0);
        self.history.paint(p, hist_r, cx, -80.0, 60.0, false);
        let y = kit::History::level_y(hist_r, threshold as f32, -80.0);
        p.hline(hist_r.x, hist_r.right(), y, self.accent.with_alpha(0.55));
        let n = self.open.len();
        let w = strip.w / n as f32;
        for i in 0..n {
            let open = self.open[(self.open_head + i) % n];
            if open != (mode == 2) {
                p.fill(
                    Rect::new(strip.x + i as f32 * w, strip.y, w + 0.5, strip.h),
                    self.accent.with_alpha(0.75),
                );
            }
        }
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
                let db = from + f64::from((pos.x - x0) / curve.w * 80.0);
                edit.set(pid(id::THRESHOLD), db.clamp(-80.0, 0.0));
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
        gate::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => {
                "Gate (silence under the threshold), Expander (down by the ratio), Ducker (down while the key plays)"
            }
            id::RANGE => "How far down when closed",
            id::HYSTERESIS => "It closes this much under the threshold: no chatter",
            id::HOLD => "Stay open this long after the key falls",
            id::EXTERNAL => "Key from the sidechain when one is routed in",
            _ => return None,
        })
    }
}
