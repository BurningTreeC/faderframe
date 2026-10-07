//! The Channel Strip's face: the equaliser's curve (with the filters) and
//! the history of what goes in and out with the compressor's reduction,
//! over a deck laid out like a console channel — input and filters, gate,
//! compressor, the four bands, drive and output — and meters for the input,
//! the gate, the compressor and the output.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::channel_strip::{self as strip, id, value};
use faderframe_plugin_host::dsp::filter::Filter;
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct ChannelStripFace {
    accent: Color,
    history: kit::History,
}

/// The curve's range (dB either side of 0).
const RANGE: f64 = 18.0;

impl ChannelStripFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Dynamics),
            history: kit::History::new(6.0, 40.0),
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let curve = Rect::new(r.x + 30.0, r.y + 8.0, (r.w - 30.0) * 0.62, r.h - 24.0);
        let hist = Rect::new(
            curve.right() + 16.0,
            r.y + 8.0,
            r.right() - curve.right() - 20.0,
            r.h - 24.0,
        );
        (curve, hist)
    }

    /// The whole response (dB) at `freqs`: filters and bands as designed.
    fn response(cx: &Ctx<'_>, freqs: &[f64]) -> Vec<f64> {
        let filters: Vec<Filter<1>> = strip::bands(&cx.tap.params)
            .iter()
            .flatten()
            .map(|s| Filter::new(*s, 96_000.0))
            .collect();
        freqs
            .iter()
            .map(|&f| filters.iter().map(|flt| flt.db_at(f)).sum())
            .collect()
    }
}

impl Face for ChannelStripFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1360.0, 560.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 236.0, 128.0);
        let s = kit::sections(
            deck,
            &[
                ("INPUT", 3.0),
                ("GATE", 4.0),
                ("COMPRESSOR", 6.6),
                ("EQUALISER", 9.6),
                ("OUTPUT", 2.6),
            ],
        );
        let mut c = Vec::new();
        let low = |r: Rect| Rect::new(r.x, r.y + KNOB.1 + 2.0, r.w, SMALL.1);
        let bottom = |r: Rect| Rect::new(r.x, r.bottom() - SWITCH_H, r.w, SWITCH_H);
        // Input and filters.
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::INPUT), "INPUT", k[0]).bipolar());
        c.push(Ctl::knob(pid(id::HPF), "HIGH PASS", k[1]).scaled(Scale::Log));
        let k = kit::row(low(r), &[SMALL.0, SMALL.0]);
        c.push(Ctl::small(pid(id::LPF), "LOW PASS", k[0]).scaled(Scale::Log));
        c.push(Ctl::choice(
            pid(id::HPF_SLOPE),
            "SLOPE",
            kit::at(k[1], 0.0, 18.0, SMALL.0, SWITCH_H),
        ));
        c.push(Ctl::toggle(pid(id::FILTERS_TO_SC), "To Key", bottom(r)));
        // Gate.
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::GATE_THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(pid(id::GATE_RANGE), "RANGE", k[1]));
        let k = kit::row(low(r), &[SMALL.0, SMALL.0, SMALL.0]);
        c.push(Ctl::small(pid(id::GATE_ATTACK), "ATTACK", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::GATE_HOLD), "HOLD", k[1]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::small(pid(id::GATE_RELEASE), "RELEASE", k[2]).scaled(Scale::Log));
        let b = kit::row(bottom(r), &[60.0, (r.w - 72.0).max(60.0)]);
        c.push(Ctl::toggle(pid(id::GATE), "On", b[0]));
        c.push(Ctl::segments(pid(id::GATE_MODE), "", b[1]));
        // Compressor.
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::COMP_THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(pid(id::COMP_RATIO), "RATIO", k[1]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::COMP_ATTACK), "ATTACK", k[2]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::COMP_RELEASE), "RELEASE", k[3]).scaled(Scale::Log));
        let k = kit::row(low(r), &[SMALL.0, SMALL.0, SMALL.0]);
        c.push(Ctl::small(pid(id::COMP_KNEE), "KNEE", k[0]));
        c.push(Ctl::small(pid(id::COMP_MAKEUP), "MAKE-UP", k[1]));
        c.push(Ctl::small(pid(id::COMP_MIX), "MIX", k[2]));
        let b = kit::row(bottom(r), &[60.0, 70.0, 100.0]);
        c.push(Ctl::toggle(pid(id::COMP), "On", b[0]));
        c.push(Ctl::toggle(pid(id::COMP_PEAK), "Peak", b[1]));
        c.push(Ctl::toggle(pid(id::EXTERNAL), "Sidechain", b[2]));
        // Equaliser: four columns.
        let r = kit::inside(&s[3]);
        let cols = kit::spread(
            Rect::new(r.x, r.y, r.w, r.h - SWITCH_H - 4.0),
            4,
            (r.w / 4.0).min(2.0 * SMALL.0 + 4.0),
        );
        let bands = [
            (id::LF_GAIN, id::LF_FREQ, id::LF_BELL, "LF"),
            (id::LMF_GAIN, id::LMF_FREQ, id::LMF_Q, "LMF"),
            (id::HMF_GAIN, id::HMF_FREQ, id::HMF_Q, "HMF"),
            (id::HF_GAIN, id::HF_FREQ, id::HF_BELL, "HF"),
        ];
        for (col, (g, f, third, name)) in cols.iter().zip(bands) {
            let top = kit::row(Rect::new(col.x, col.y, col.w, KNOB.1), &[KNOB.0]);
            c.push(Ctl::knob(pid(g), name, top[0]).bipolar());
            let k = kit::row(low(*col), &[SMALL.0, SMALL.0]);
            c.push(Ctl::small(pid(f), "FREQ", k[0]).scaled(Scale::Log));
            if third == id::LF_BELL || third == id::HF_BELL {
                c.push(Ctl::toggle(
                    pid(third),
                    "Bell",
                    kit::at(k[1], 0.0, 22.0, SMALL.0, SWITCH_H),
                ));
            } else {
                c.push(Ctl::small(pid(third), "Q", k[1]).scaled(Scale::Log));
            }
        }
        let b = kit::row(bottom(r), &[60.0, 150.0]);
        c.push(Ctl::toggle(pid(id::EQ), "On", b[0]));
        c.push(Ctl::segments(pid(id::EQ_TYPE), "", b[1]));
        // Output.
        let r = kit::inside(&s[4]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::DRIVE), "DRIVE", k[0]));
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[1]).bipolar());
        c.push(Ctl::choice(
            pid(id::ORDER),
            "ORDER",
            Rect::new(r.x, r.y + KNOB.1 + 20.0, r.w, SWITCH_H),
        ));
        let w = (meter.w - 12.0) / 4.0;
        let at = |i: f32| Rect::new(meter.x + i * (w + 4.0), meter.y, w, meter.h);
        let meters = vec![
            Meter {
                rect: at(0.0),
                kind: MeterKind::Input,
                label: "IN",
            },
            Meter {
                rect: at(1.0),
                kind: MeterKind::Reduction(value::GATE_GR),
                label: "GATE",
            },
            Meter {
                rect: at(2.0),
                kind: MeterKind::Reduction(value::COMP_GR),
                label: "COMP",
            },
            Meter {
                rect: at(3.0),
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
            Some(value::COMP_GR_PEAK),
        );
        let th = cx.theme;
        let (curve, hist) = Self::split(r);
        p.fill_rounded(curve, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let x_of = |f: f64| kit::Spectrum::x_of(curve, f);
        let y_of =
            |db: f64| curve.y + curve.h * (0.5 - (db / (2.0 * RANGE)).clamp(-0.5, 0.5) as f32);
        for f in [100.0, 1_000.0, 10_000.0] {
            p.vline(x_of(f), curve.y, curve.bottom(), th.device.grid);
            let label = if f >= 1_000.0 {
                format!("{}k", f / 1_000.0)
            } else {
                format!("{f}")
            };
            p.text(
                &label,
                Rect::new(x_of(f) + 3.0, curve.bottom() - 14.0, 40.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
        }
        for d in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            p.hline(
                curve.x,
                curve.right(),
                y_of(d),
                if d == 0.0 {
                    th.device.grid_strong
                } else {
                    th.device.grid
                },
            );
            p.text(
                &format!("{d:+.0}").replace('-', "−"),
                Rect::new(curve.x - 30.0, y_of(d) - 7.0, 26.0, 14.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
            );
        }
        let freqs: Vec<f64> = (0..=240)
            .map(|i| 20.0 * 1000f64.powf(f64::from(i) / 240.0))
            .collect();
        let db = Self::response(cx, &freqs);
        let pts: Vec<Point> = freqs
            .iter()
            .zip(&db)
            .map(|(&f, &d)| Point::new(x_of(f), y_of(d)))
            .collect();
        let mut fill = Path::polyline(&pts);
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            fill.line_to(Point::new(last.x, y_of(0.0)));
            fill.line_to(Point::new(first.x, y_of(0.0)));
            fill.close();
        }
        p.fill_path(&fill, self.accent.with_alpha(0.16));
        p.stroke_path(&Path::polyline(&pts), 2.0, self.accent);
        p.stroke_rounded(curve, 3.0, 1.0, th.device.grid_strong);
        let order = strip::ORDERS[usize::from(cx.value(pid(id::ORDER)) >= 0.5)];
        p.text(
            order,
            Rect::new(curve.x + 8.0, curve.y + 6.0, curve.w - 16.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim),
        );
        self.history.paint(p, hist, cx, -60.0, 30.0, false);
        if cx.on(pid(id::GATE)) && cx.published(value::OPEN) < 0.5 {
            p.text(
                "GATE CLOSED",
                Rect::new(hist.x + 8.0, hist.y + 6.0, hist.w - 16.0, 14.0),
                &TextStyle::new(th.fonts.tiny + 0.5, self.accent),
            );
        }
    }

    fn display_event(
        &mut self,
        _ev: &ViewEvent,
        _r: Rect,
        _cx: &Ctx<'_>,
        _edit: &mut Edit<'_, '_>,
    ) -> bool {
        false
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        strip::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::FILTERS_TO_SC => "The filters shape only what the dynamics hear, not the sound",
            id::EXTERNAL => "Key the gate and compressor from the sidechain when one is routed in",
            id::EQ_TYPE => "Brown keeps the bands as set; Black narrows them as their gain grows",
            id::ORDER => "Equaliser before the dynamics, or after them",
            id::COMP_MIX => "Parallel compression: the dry signal mixed back in",
            id::DRIVE => "Console saturation: rounds the peaks, leaves quiet signals alone",
            id::GATE_MODE => {
                "Gate (down by the range when closed) or Expander (2:1 under the threshold)"
            }
            _ => return None,
        })
    }
}
