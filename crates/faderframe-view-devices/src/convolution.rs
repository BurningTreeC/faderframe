//! The Convolution Reverb's face: the response playing — its waveform
//! after the pre-delay, the energy still to come (dB) over it, its name,
//! length, channels and decay — with a button to load an impulse file
//! (files dropped on the display load too) and one to clear it; the wet
//! cuts' response beside it; the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::convolution::{self as conv, ImpulseView, id, value};
use faderframe_plugin_host::eq::design::{BandShape, BandType, band_db};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct ConvolutionFace {
    accent: Color,
    wet: f32,
}

impl ConvolutionFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Space),
            wet: -150.0,
        }
    }

    /// The response's picture, the cuts' response, the Load and Clear
    /// buttons.
    pub(crate) fn layout(r: Rect) -> (Rect, Rect, Rect, Rect) {
        let freq = Rect::new(r.right() - 248.0, r.y + 8.0, 240.0, r.h - 16.0);
        let wave = Rect::new(r.x + 8.0, r.y + 30.0, freq.x - r.x - 20.0, r.h - 38.0);
        let load = Rect::new(wave.right() - 172.0, r.y + 6.0, 80.0, 20.0);
        let clear = Rect::new(wave.right() - 84.0, r.y + 6.0, 84.0, 20.0);
        (wave, freq, load, clear)
    }

    fn view(cx: &Ctx<'_>) -> Option<std::sync::Arc<ImpulseView>> {
        cx.tap.assets::<ImpulseView>()
    }

    fn cut(kind: BandType, freq: f64) -> BandShape {
        BandShape {
            kind,
            freq,
            gain: 0.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            slope: 12.0,
        }
    }
}

impl Face for ConvolutionFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(900.0, 480.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 206.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("RESPONSE", 5.0),
                ("SHAPE", 4.0),
                ("TONE", 4.4),
                ("OUTPUT", 3.0),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::choice(
            pid(id::SPACE),
            "SPACE",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 46.0, r.w, KNOB.1),
            &[SMALL.0, KNOB.0, SMALL.0],
        );
        c.push(
            Ctl::small(
                pid(id::START),
                "START",
                kit::at(k[0], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Skew(2.5)),
        );
        c.push(Ctl::knob(pid(id::LENGTH), "LENGTH", k[1]).scaled(Scale::Skew(0.6)));
        c.push(
            Ctl::small(
                pid(id::STRETCH),
                "STRETCH",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::FADE_IN), "FADE IN", k[0]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::knob(pid(id::FADE_OUT), "FADE OUT", k[1]));
        let half = (r.w - 12.0) / 2.0;
        let y = r.bottom() - SWITCH_H - 4.0;
        c.push(Ctl::toggle(
            pid(id::REVERSE),
            "Reverse",
            Rect::new(r.x + 4.0, y, half, SWITCH_H),
        ));
        c.push(Ctl::toggle(
            pid(id::NORMALISE),
            "Normalise",
            Rect::new(r.x + 8.0 + half, y, half, SWITCH_H),
        ));
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::PRE_DELAY), "PRE-DELAY", k[0]).scaled(Scale::Skew(2.5)));
        c.push(
            Ctl::small(
                pid(id::LOW_CUT),
                "LOW CUT",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        c.push(
            Ctl::small(
                pid(id::HIGH_CUT),
                "HIGH CUT",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[0]));
        c.push(Ctl::small(
            pid(id::OUTPUT),
            "OUTPUT",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::small(
            pid(id::WIDTH),
            "WIDTH",
            Rect::new(
                r.x + (r.w - SMALL.0) / 2.0,
                r.y + KNOB.1 + 2.0,
                SMALL.0,
                SMALL.1,
            ),
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
        let th = cx.theme;
        let (wave, freq, load, clear) = Self::layout(r);
        let view = Self::view(cx);
        // The heading: what plays.
        let title = TextStyle::new(th.fonts.normal, th.ui.text).bold();
        let small = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim);
        let name = view.as_ref().map_or("…", |v| v.name.as_str());
        p.text(name, Rect::new(wave.x, r.y + 6.0, 260.0, 18.0), &title);
        let facts = match &view {
            Some(v) if v.problem.is_none() => {
                let kind = match v.channels {
                    4 => "True stereo",
                    2 => "Stereo",
                    _ => "Mono",
                };
                let decay = v
                    .rt60
                    .map_or_else(String::new, |t| format!(" · RT60 {t:.2} s"));
                format!("{kind} · {:.2} s{decay}", v.seconds)
            }
            Some(v) => v.problem.clone().unwrap_or_default(),
            None => "Making the response…".into(),
        };
        p.text(
            &facts,
            Rect::new(
                wave.x + 268.0,
                r.y + 8.0,
                (load.x - wave.x - 276.0).max(0.0),
                16.0,
            ),
            &small,
        );
        if cx.loading || cx.published(value::BUILDING) >= 0.5 {
            p.text(
                "Loading…",
                Rect::new(load.x - 74.0, r.y + 8.0, 70.0, 16.0),
                &TextStyle::new(th.fonts.tiny + 0.5, self.accent),
            );
        }
        kit::display_button(p, load, "LOAD…", self.accent, th);
        if cx.value(pid(id::SPACE)) < 0.5 {
            kit::display_button(p, clear, "CLEAR FILE", self.accent, th);
        } else {
            kit::display_button(p, clear, "USE FILE", self.accent, th);
        }
        // The response, after its pre-delay.
        p.fill_rounded(wave, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let pre = cx.value(pid(id::PRE_DELAY)) * 1e-3;
        if let Some(v) = view.as_ref().filter(|v| !v.envelope.is_empty()) {
            let span = (pre + v.seconds).max(0.05);
            let x0 = wave.x + wave.w * (pre / span) as f32;
            let w = wave.right() - x0;
            let mid = wave.center().y;
            let n = v.envelope.len();
            let mut top = Vec::with_capacity(n);
            let mut bottom = Vec::with_capacity(n);
            for (i, (lo, hi)) in v.envelope.iter().enumerate() {
                let x = x0 + w * i as f32 / (n - 1).max(1) as f32;
                top.push(Point::new(x, mid - hi * wave.h * 0.45));
                bottom.push(Point::new(x, mid - lo * wave.h * 0.45));
            }
            let mut path = Path::polyline(&top);
            for q in bottom.iter().rev() {
                path.line_to(*q);
            }
            path.close();
            p.fill_path(&path, th.device.wave.with_alpha(0.45));
            // The energy still to come, 0 to −60 dB down the display.
            let decay: Vec<Point> = v
                .decay_db
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let x = x0 + w * i as f32 / (n - 1).max(1) as f32;
                    Point::new(x, wave.y + wave.h * (-d / 60.0).clamp(0.0, 1.0))
                })
                .collect();
            p.stroke_path(&Path::polyline(&decay), 1.6, self.accent);
            if pre > 0.0 {
                p.vline(x0, wave.y, wave.bottom(), self.accent.with_alpha(0.5));
            }
            // Seconds along the bottom.
            let step = if span > 4.0 {
                1.0
            } else if span > 1.0 {
                0.5
            } else {
                0.1
            };
            let mut t = step;
            while t < span {
                let x = wave.x + wave.w * (t / span) as f32;
                p.vline(x, wave.bottom() - 4.0, wave.bottom(), th.device.grid_strong);
                p.text(
                    &format!("{t:.1} s"),
                    Rect::new(x + 2.0, wave.bottom() - 14.0, 40.0, 12.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
                );
                t += step;
            }
        }
        if cx.drop.is_some_and(|d| wave.contains(d)) {
            p.stroke_rounded(wave, 3.0, 2.0, self.accent);
            p.text(
                "Drop an impulse response",
                wave,
                &TextStyle::new(th.fonts.normal, self.accent).center(),
            );
        }
        // The wet signal's cuts.
        p.fill_rounded(freq, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        let low = cx.value(pid(id::LOW_CUT));
        let high = cx.value(pid(id::HIGH_CUT));
        let n = (freq.w / 2.0).max(2.0) as usize;
        let pts: Vec<Point> = (0..n)
            .map(|i| {
                let x = freq.x + freq.w * i as f32 / (n - 1) as f32;
                let f = kit::Spectrum::freq_at(freq, x);
                let mut d = 0.0;
                if low > 20.5 {
                    d += band_db(&Self::cut(BandType::LowCut, low), rate, f);
                }
                if high < 19_500.0 {
                    d += band_db(&Self::cut(BandType::HighCut, high), rate, f);
                }
                Point::new(
                    x,
                    freq.y + 14.0 + (freq.h - 28.0) * (-d as f32 / 30.0).clamp(0.0, 1.0),
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 1.8, self.accent);
        // The wet level, falling at 24 dB/s.
        let peak = faderframe_plugin_host::tap::db(cx.tap.take_value(value::WET_PEAK));
        self.wet = if peak > self.wet {
            peak
        } else {
            (self.wet - 24.0 * cx.dt).max(-150.0)
        };
        let lvl = self.wet;
        p.text(
            &format!("WET {}", faderframe_plugin_host::tap::readout(lvl)),
            Rect::new(freq.x + 6.0, freq.y + 4.0, freq.w - 12.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (_, _, load, clear) = Self::layout(r);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if load.contains(pos) => {
                edit.choose_samples(0, "Load an Impulse Response", false);
                true
            }
            ViewEvent::PointerDown { pos, .. } if clear.contains(pos) => {
                if cx.value(pid(id::SPACE)) < 0.5 {
                    edit.clear_sample(0);
                } else {
                    edit.set_once(pid(id::SPACE), 0.0);
                }
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        conv::format(id, v)
    }

    fn drop_slot(&self, pos: Point, display: Rect) -> Option<(usize, bool)> {
        display.contains(pos).then_some((0, false))
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::SPACE => "A built-in space, or the impulse file loaded (Load…, or drop one here)",
            id::START => "Where the response starts (cuts its beginning)",
            id::LENGTH => "How much of the response plays",
            id::STRETCH => "The space scaled: longer and darker, or shorter and brighter",
            id::FADE_OUT => "A fade over the end of the response (a share of its length)",
            id::REVERSE => "Play the response backwards: the swell before each sound",
            id::NORMALISE => "Every response about as loud (off: as recorded)",
            id::PRE_DELAY => "Time before the reverb starts",
            id::WIDTH => "The wet signal's width",
            id::OUTPUT => "The wet signal's level",
            _ => return None,
        })
    }
}
