//! The Limiter's face: a large history of the level going in, coming out
//! (in the limiter's colour) and the reduction (from the top), the
//! ceiling over it, readouts of the reduction, the output's true peak
//! (and the highest since the readout was clicked), its short-term and
//! momentary loudness, the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::limiter::{self as lim, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Rect, Size, ViewEvent};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// A readout's height and the gap between them.
const READOUT_H: f32 = 44.0;
const READOUT_GAP: f32 = 6.0;

pub(crate) struct LimiterFace {
    accent: Color,
    history: kit::History,
    /// The output's highest (true) peak since the readout was clicked.
    peak_max: f32,
}

impl LimiterFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Limiter),
            history: kit::History::new(8.0, 40.0),
            peak_max: f32::NEG_INFINITY,
        }
    }

    /// The readout column's `n`th box.
    fn readout_rect(r: Rect, n: usize) -> Rect {
        Rect::new(
            r.right() - 132.0,
            r.y + 10.0 + n as f32 * (READOUT_H + READOUT_GAP),
            120.0,
            READOUT_H,
        )
    }
}

fn lufs_text(v: f32) -> String {
    if v > -100.0 {
        format!("{v:.1} LUFS").replace('-', "−")
    } else {
        "−∞".into()
    }
}

impl Face for LimiterFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(820.0, 460.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 170.0, 70.0);
        let s = kit::sections(
            deck,
            &[("LIMITING", 4.6), ("RELEASE", 2.6), ("OPTIONS", 2.8)],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w * 0.62, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::GAIN), "GAIN", k[0]));
        c.push(Ctl::knob(pid(id::CEILING), "CEILING", k[1]));
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[2]));
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
            Rect::new(r.x + 6.0, r.y + 4.0, r.w * 0.45 - 10.0, SWITCH_H),
        ));
        c.push(Ctl::toggle(
            pid(id::UNITY),
            "Unity Gain",
            Rect::new(r.x + 6.0, r.y + 34.0, r.w * 0.45 - 10.0, SWITCH_H),
        ));
        let k = kit::spread(
            Rect::new(r.x + r.w * 0.45, r.y, r.w * 0.55, SMALL.1),
            2,
            SMALL.0 - 4.0,
        );
        c.push(Ctl::small(pid(id::LINK), "LINK", k[0]));
        c.push(Ctl::small(pid(id::RELEASE_LINK), "REL LINK", k[1]));
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
        self.peak_max = self.peak_max.max(self.history.recent_max(1, 0.1));
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
        // Readouts: the reduction, the output's true peak (now and the
        // highest), its loudness.
        let gr = self.history.recent_max(2, 2.0).max(0.0);
        let tp = self.history.recent_max(1, 3.0);
        let output_ceiling = ceiling + cx.value(pid(id::OUTPUT)) as f32;
        kit::readout(
            p,
            Self::readout_rect(r, 0),
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
        let peak_text = |v: f32| {
            if v > -100.0 {
                format!("{v:.1} {unit}").replace('-', "−")
            } else {
                "−∞".into()
            }
        };
        let over = |v: f32| {
            if v > output_ceiling + 0.05 {
                th.tools.level_over
            } else {
                self.accent
            }
        };
        kit::readout(
            p,
            Self::readout_rect(r, 1),
            "OUTPUT PEAK",
            &peak_text(tp),
            over(tp),
            th,
        );
        kit::readout(
            p,
            Self::readout_rect(r, 2),
            "PEAK MAX · CLICK",
            &peak_text(self.peak_max),
            over(self.peak_max),
            th,
        );
        kit::readout(
            p,
            Self::readout_rect(r, 3),
            "SHORT TERM",
            &lufs_text(cx.published(value::LUFS_SHORT)),
            th.ui.text,
            th,
        );
        if Self::readout_rect(r, 4).bottom() <= r.bottom() - 4.0 {
            kit::readout(
                p,
                Self::readout_rect(r, 4),
                "MOMENTARY",
                &lufs_text(cx.published(value::LUFS_MOMENTARY)),
                th.ui.text_dim,
                th,
            );
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        _cx: &Ctx<'_>,
        _edit: &mut Edit<'_, '_>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown { pos, .. } if Self::readout_rect(r, 2).contains(pos) => {
                self.peak_max = f32::NEG_INFINITY;
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        lim::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::GAIN => "Drive into the limiter",
            id::CEILING => "Nothing comes out louder",
            id::OUTPUT => "Trim after the limiter: the output ceiling is the ceiling plus this",
            id::STYLE => {
                "Transparent (smooth), Punchy (keeps transients), Aggressive (loud), Allround, Bus (slow, programme dependent), Safe (true peaks never over), Modern (quick, dense)"
            }
            id::LOOKAHEAD => "How far ahead peaks are seen (adds as much latency)",
            id::TRUE_PEAK => "Keep the peaks between samples under the ceiling too",
            id::UNITY => "Take the gain back off to hear what limiting does",
            id::AUTO_RELEASE => "Release slower while limiting goes on",
            id::LINK => "How much both channels limit on the louder one",
            id::RELEASE_LINK => "How far each channel's reduction follows the deeper one",
            _ => return None,
        })
    }
}
