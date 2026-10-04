//! The De-esser's face: the spectrum going in and coming out with the band
//! the detector listens to (drag it to move the frequency, the wheel sets
//! the width) and the cut it makes right now, a history of the level and
//! the reduction, the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::deesser::{self as ds, id, value};
use faderframe_plugin_host::eq::design::band_db;
use faderframe_ui_canvas::{Accent, Color, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct DeesserFace {
    accent: Color,
    spectrum: kit::Spectrum,
    history: kit::History,
    drag: bool,
}

impl DeesserFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::DeEsser),
            spectrum: kit::Spectrum::new(),
            history: kit::History::new(5.0, 40.0),
            drag: false,
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let spec = Rect::new(r.x + 8.0, r.y + 8.0, r.w * 0.60, r.h - 16.0);
        let hist = Rect::new(
            spec.right() + 12.0,
            r.y + 8.0,
            r.right() - spec.right() - 46.0,
            r.h - 16.0,
        );
        (spec, hist)
    }
}

impl Face for DeesserFace {
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
            &[("DETECTION", 6.2), ("TIME", 3.8), ("PROCESSING", 3.0)],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(pid(id::RANGE), "RANGE", k[1]));
        c.push(Ctl::knob(pid(id::FREQUENCY), "FREQUENCY", k[2]).scaled(Scale::Log));
        c.push(
            Ctl::small(
                pid(id::Q),
                "WIDTH",
                kit::at(k[3], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let row2 = r.y + KNOB.1 + 10.0;
        let half = (r.w - 24.0) / 2.0;
        c.push(Ctl::segments(
            pid(id::SHAPE),
            "BAND",
            Rect::new(r.x + 8.0, row2, half, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::DETECTION),
            "DETECTION",
            Rect::new(r.x + 16.0 + half, row2, half, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::ATTACK), "ATTACK", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::RELEASE), "RELEASE", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(
            pid(id::LOOKAHEAD),
            "LOOKAHEAD",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::segments(
            pid(id::MODE),
            "MODE",
            Rect::new(r.x + 6.0, r.y + 2.0, r.w - 12.0, 38.0),
        ));
        c.push(Ctl::small(
            pid(id::LINK),
            "LINK",
            Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y + 46.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            pid(id::LISTEN),
            "Listen",
            Rect::new(r.x + 6.0, r.bottom() - SWITCH_H, r.w - 12.0, SWITCH_H),
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
        self.spectrum.update(cx);
        self.history.record(
            cx,
            value::IN_PEAK,
            value::OUT_PEAK,
            Some(value::REDUCTION_PEAK),
        );
        let th = cx.theme;
        let (spec, hist) = Self::split(r);
        self.spectrum.paint(p, spec, cx, -96.0, true);
        let freq = cx.value(pid(id::FREQUENCY));
        let q = cx.value(pid(id::Q));
        let high = cx.value(pid(id::SHAPE)) < 0.5;
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        // The detector's band: shaded where it listens.
        let det = ds::detector_shape(high, freq, q);
        let n = (spec.w / 3.0) as usize;
        let xs: Vec<f32> = (0..=n)
            .map(|i| spec.x + spec.w * i as f32 / n as f32)
            .collect();
        // One gradient across the display, its stops following the band.
        let stops: Vec<(f32, Color)> = xs
            .iter()
            .step_by(4)
            .map(|&x| {
                let d = band_db(&det, rate, kit::Spectrum::freq_at(spec, x)) as f32;
                let w = (1.0 + d / 24.0).clamp(0.0, 1.0);
                ((x - spec.x) / spec.w, self.accent.with_alpha(0.16 * w))
            })
            .collect();
        p.fill_rounded(
            spec,
            3.0,
            &faderframe_ui_canvas::Paint::Linear {
                start: Point::new(spec.x, spec.y),
                end: Point::new(spec.right(), spec.y),
                stops,
            },
        );
        // The cut it makes now (centred: 0 dB at the middle, ±24 dB).
        let gr = f64::from(cx.published(value::REDUCTION));
        let mid = spec.y + spec.h * 0.42;
        let split = cx.value(pid(id::MODE)) < 0.5;
        let pts: Vec<Point> = xs
            .iter()
            .map(|&x| {
                let d = if split {
                    if gr > 0.01 {
                        band_db(
                            &ds::cut_shape(high, freq, q, gr),
                            rate,
                            kit::Spectrum::freq_at(spec, x),
                        )
                    } else {
                        0.0
                    }
                } else {
                    -gr
                };
                Point::new(x, mid - (d as f32) / 24.0 * spec.h * 0.42)
            })
            .collect();
        p.hline(spec.x, spec.right(), mid, self.accent.with_alpha(0.25));
        p.stroke_path(&Path::polyline(&pts), 2.2, self.accent);
        let fx = kit::Spectrum::x_of(spec, freq);
        p.vline(fx, spec.y, spec.bottom(), self.accent.with_alpha(0.6));
        p.circle(Point::new(fx, mid), 5.0, self.accent);
        p.text(
            &faderframe_plugin_host::eq::format_hz(freq),
            Rect::new(fx + 8.0, spec.y + 6.0, 80.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, self.accent).bold(),
        );
        // History: levels and reduction.
        self.history.paint(p, hist, cx, -60.0, 18.0, false);
        if cx.value(pid(id::DETECTION)) >= 0.5 {
            let y = kit::History::level_y(hist, cx.value(pid(id::THRESHOLD)) as f32, -60.0);
            p.hline(hist.x, hist.right(), y, self.accent.with_alpha(0.55));
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (spec, _) = Self::split(r);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if spec.contains(pos) => {
                self.drag = true;
                edit.begin("Frequency");
                edit.set(
                    pid(id::FREQUENCY),
                    kit::Spectrum::freq_at(spec, pos.x).clamp(1_500.0, 16_000.0),
                );
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.drag => {
                edit.set(
                    pid(id::FREQUENCY),
                    kit::Spectrum::freq_at(spec, pos.x).clamp(1_500.0, 16_000.0),
                );
                true
            }
            ViewEvent::PointerUp { .. } if self.drag => {
                self.drag = false;
                edit.end();
                true
            }
            ViewEvent::Scroll { pos, dy, .. } if spec.contains(pos) => {
                let q = cx.value(pid(id::Q)) * if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
                edit.set_once(pid(id::Q), q.clamp(0.5, 4.0));
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        ds::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => "Split: only the band is turned down. Wide: the whole signal",
            id::SHAPE => "Listen above the frequency (High) or round it (Band)",
            id::DETECTION => {
                "Relative follows the take's level; Absolute compares with the threshold as it reads"
            }
            id::RANGE => "The most it takes",
            id::LISTEN => "Hear what the detector hears",
            _ => return None,
        })
    }
}
