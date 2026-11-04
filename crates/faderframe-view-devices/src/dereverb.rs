//! De-reverb's face: the spectrum going in and coming out, the room's decay
//! as the device models it (the early part left, the tail taken, its length
//! estimated or set), the tail length and the reduction now, the controls
//! and meters.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::dereverb::{self as dr, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct DereverbFace {
    accent: Color,
    spectrum: kit::Spectrum,
}

impl DereverbFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Space),
            spectrum: kit::Spectrum::new(),
        }
    }

    pub(crate) fn split(r: Rect) -> (Rect, Rect, Rect, Rect) {
        let spec = Rect::new(r.x + 8.0, r.y + 8.0, r.w * 0.58, r.h - 16.0);
        let decay = Rect::new(
            spec.right() + 12.0,
            r.y + 8.0,
            r.right() - spec.right() - 150.0,
            r.h - 16.0,
        );
        let tail = Rect::new(
            decay.right() + 10.0,
            r.y + 8.0,
            r.right() - decay.right() - 18.0,
            58.0,
        );
        let red = Rect::new(tail.x, tail.bottom() + 8.0, tail.w, 58.0);
        (spec, decay, tail, red)
    }
}

impl Face for DereverbFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(920.0, 480.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 70.0);
        let s = kit::sections(deck, &[("ROOM", 6.0), ("PROCESSING", 3.6), ("OUTPUT", 2.6)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, 100.0],
        );
        c.push(Ctl::knob(pid(id::TAIL), "TAIL", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::EARLY), "EARLY", k[1]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::DAMPING), "DAMPING", k[2]));
        c.push(Ctl::toggle(
            pid(id::AUTO_TAIL),
            "Auto Tail",
            Rect::new(k[3].x, k[3].y + 30.0, k[3].w, SWITCH_H),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::AMOUNT), "AMOUNT", k[0]));
        c.push(Ctl::knob(pid(id::SMOOTHING), "SMOOTHING", k[1]));
        let r = kit::inside(&s[2]);
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
                label: "RED",
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
        let (spec, decay, tail, red) = Self::split(r);
        self.spectrum.paint(p, spec, cx, -110.0, true);
        // The decay over twice the tail (60 dB down at the tail length).
        let tail_ms = f64::from(cx.published(value::TAIL)).max(50.0);
        let early = cx.value(pid(id::EARLY));
        let amount = cx.value(pid(id::AMOUNT));
        p.fill_rounded(
            decay,
            3.0,
            &faderframe_ui_canvas::Paint::Solid(th.device.display.darken(0.12)),
        );
        let span = 1.6 * tail_ms;
        let x = |ms: f64| decay.x + decay.w * (ms / span).clamp(0.0, 1.0) as f32;
        let y = |db: f64| decay.y + decay.h * (-db / 80.0).clamp(0.0, 1.0) as f32;
        let n = 64;
        let before: Vec<Point> = (0..=n)
            .map(|i| {
                let ms = span * i as f64 / n as f64;
                Point::new(x(ms), y(-60.0 * ms / tail_ms))
            })
            .collect();
        p.stroke_path(&Path::polyline(&before), 1.4, th.ui.text_dim);
        // After: the late part taken down by up to 30 dB × amount.
        let after: Vec<Point> = (0..=n)
            .map(|i| {
                let ms = span * i as f64 / n as f64;
                let taken = if ms > early { 30.0 * amount } else { 0.0 };
                Point::new(x(ms), y(-60.0 * ms / tail_ms - taken))
            })
            .collect();
        p.stroke_path(&Path::polyline(&after), 2.0, self.accent);
        let xe = x(early);
        p.vline(xe, decay.y, decay.bottom(), self.accent.with_alpha(0.5));
        p.text(
            "early",
            Rect::new(xe + 4.0, decay.y + 4.0, 60.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
        );
        let estimated = cx.on(pid(id::AUTO_TAIL)) && cx.published(value::ESTIMATED) >= 0.5;
        let caption = if !cx.on(pid(id::AUTO_TAIL)) {
            "TAIL"
        } else if estimated {
            "TAIL (HEARD)"
        } else {
            "TAIL (LISTENING)"
        };
        kit::readout(
            p,
            tail,
            caption,
            &if tail_ms >= 1_000.0 {
                format!("{:.2} s", tail_ms / 1_000.0)
            } else {
                format!("{tail_ms:.0} ms")
            },
            self.accent,
            th,
        );
        kit::readout(
            p,
            red,
            "TAKEN",
            &format!("{:.1} dB", cx.published(value::REDUCTION)),
            th.ui.text,
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        dr::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::AMOUNT => "How much of the room is taken (up to 30 dB of the tail)",
            id::TAIL => "The room's RT60 (used unless Auto Tail hears it)",
            id::AUTO_TAIL => "Estimate the tail from the decays heard",
            id::EARLY => "The sound and its first reflections, left alone",
            id::DAMPING => "The highs die away sooner in most rooms",
            id::SMOOTHING => "Steadier gains, or quicker ones",
            id::LISTEN => "Hear the result, or only the reverb taken out",
            _ => return None,
        })
    }
}
