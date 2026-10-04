//! The Delay's face: the repeats on a timeline (left up, right down; their
//! height what feedback leaves of them), the times as set, what one pass
//! through the loop does to the spectrum, the controls and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::delay::{self as dly, id, value};
use faderframe_plugin_host::dsp::lfo::division_name;
use faderframe_plugin_host::eq::design::{BandShape, BandType, band_db};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct DelayFace {
    accent: Color,
    /// The repeats' level, falling (for the timeline's glow).
    wet: f32,
    /// Dragging the timeline: the time at the start and where.
    drag: Option<(f32, f64)>,
}

impl DelayFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Time),
            wet: 0.0,
            drag: None,
        }
    }

    fn split(r: Rect) -> (Rect, Rect, Rect) {
        let times = Rect::new(r.x + 8.0, r.y + 8.0, 170.0, r.h - 16.0);
        let lp = Rect::new(r.right() - 238.0, r.y + 8.0, 230.0, r.h - 16.0);
        let line = Rect::new(
            times.right() + 12.0,
            r.y + 8.0,
            lp.x - times.right() - 24.0,
            r.h - 16.0,
        );
        (times, line, lp)
    }

    /// Seconds shown across the timeline: room for eight repeats.
    fn span(left: f64, right: f64) -> f64 {
        (left.max(right) * 6.4).clamp(0.2, 12.0)
    }
}

impl Face for DelayFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(940.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("TIME", 5.6),
                ("FEEDBACK", 2.4),
                ("LOOP", 4.6),
                ("WOW", 2.6),
                ("OUTPUT", 4.2),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.6, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::TIME), "TIME", k[0]).scaled(Scale::Log));
        c.push(
            Ctl::small(
                pid(id::OFFSET),
                "OFFSET",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        let side = Rect::new(r.x + r.w * 0.6 + 2.0, r.y, r.w * 0.4 - 8.0, r.h);
        c.push(Ctl::toggle(
            pid(id::SYNC),
            "Sync",
            Rect::new(side.x, side.y + 4.0, side.w, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::DIVISION),
            "NOTE",
            Rect::new(side.x, side.y + 34.0, side.w, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::MODE),
            "MODE",
            Rect::new(r.x + 8.0, r.y + KNOB.1 + 10.0, r.w - 16.0, 38.0),
        ));
        let r = kit::inside(&s[1]);
        c.push(Ctl::knob(
            pid(id::FEEDBACK),
            "FEEDBACK",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y, KNOB.0, KNOB.1),
        ));
        c.push(Ctl::toggle(
            pid(id::FREEZE),
            "Freeze",
            Rect::new(r.x + 6.0, r.bottom() - SWITCH_H, r.w - 12.0, SWITCH_H),
        ));
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::LOW_CUT), "LOW CUT", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::DAMPING), "DAMPING", k[1]));
        c.push(Ctl::small(pid(id::SATURATION), "DRIVE", k[2]));
        c.push(Ctl::segments(
            pid(id::STYLE),
            "STYLE",
            Rect::new(r.x + 8.0, r.bottom() - 40.0, r.w - 16.0, 38.0),
        ));
        let r = kit::inside(&s[3]);
        let k = kit::spread(Rect::new(r.x, r.y, r.w, SMALL.1 * 2.0 + 8.0), 1, SMALL.0);
        c.push(Ctl::small(
            pid(id::WOW),
            "DEPTH",
            Rect::new(k[0].x, r.y, SMALL.0, SMALL.1),
        ));
        c.push(
            Ctl::small(
                pid(id::WOW_RATE),
                "RATE",
                Rect::new(k[0].x, r.y + SMALL.1 + 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[4]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[0]));
        c.push(Ctl::small(
            pid(id::WIDTH),
            "WIDTH",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::small(
            pid(id::DUCKING),
            "DUCKING",
            Rect::new(
                r.x + (r.w - SMALL.0) / 2.0,
                r.y + KNOB.1 + 4.0,
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
        let (times, line, lp) = Self::split(r);
        let tl = f64::from(cx.published(value::TIME_L)).max(1.0) * 0.001;
        let tr = f64::from(cx.published(value::TIME_R)).max(1.0) * 0.001;
        let synced = cx.on(pid(id::SYNC));
        let mode = cx.value(pid(id::MODE)).round() as i64;
        let feedback = cx.value(pid(id::FEEDBACK));
        let frozen = cx.on(pid(id::FREEZE));
        // The times.
        p.fill_rounded(times, 4.0, &Paint::Solid(th.device.display.darken(0.2)));
        let big = TextStyle::new(th.fonts.large + 6.0, self.accent)
            .bold()
            .center();
        let dim = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .center()
            .tracking(0.6);
        p.text(
            dly::MODES[mode.clamp(0, 2) as usize],
            Rect::new(times.x, times.y + 10.0, times.w, 12.0),
            &dim,
        );
        let ms = |t: f64| {
            if t >= 1.0 {
                format!("{t:.2} s")
            } else {
                format!("{:.0} ms", t * 1000.0)
            }
        };
        if synced {
            p.text(
                division_name(cx.value(pid(id::DIVISION)).round().max(0.0) as usize),
                Rect::new(times.x, times.y + 34.0, times.w, 34.0),
                &big,
            );
            p.text(
                &ms(tl),
                Rect::new(times.x, times.y + 70.0, times.w, 16.0),
                &dim,
            );
        } else {
            p.text(
                &ms(tl),
                Rect::new(times.x, times.y + 34.0, times.w, 34.0),
                &big,
            );
        }
        if (tr - tl).abs() > 0.0005 {
            p.text(
                &format!("R {}", ms(tr)),
                Rect::new(times.x, times.y + 92.0, times.w, 16.0),
                &TextStyle::new(th.fonts.tiny + 1.0, th.ui.text).center(),
            );
        }
        let badge = |p: &mut dyn Painter, y: f32, text: &str, on: bool| {
            let b = Rect::new(times.x + 20.0, y, times.w - 40.0, 20.0);
            p.fill_rounded(
                b,
                10.0,
                &Paint::Solid(if on {
                    self.accent.with_alpha(0.25)
                } else {
                    th.device.display
                }),
            );
            p.text(
                text,
                b,
                &TextStyle::new(
                    th.fonts.tiny,
                    if on { self.accent } else { th.ui.text_faint },
                )
                .bold()
                .center(),
            );
        };
        badge(
            p,
            times.bottom() - 54.0,
            if synced { "SYNCED" } else { "FREE" },
            synced,
        );
        badge(p, times.bottom() - 28.0, "FROZEN", frozen);
        // The timeline.
        p.fill_rounded(line, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        let span = Self::span(tl, tr);
        let mid = line.y + line.h / 2.0;
        let x_of = |t: f64| line.x + 10.0 + (line.w - 20.0) * (t / span) as f32;
        let mut t = 0.0;
        while t < span {
            p.vline(x_of(t), line.y + 6.0, line.bottom() - 6.0, th.device.grid);
            t += if span > 4.0 {
                1.0
            } else if span > 1.0 {
                0.25
            } else {
                0.05
            };
        }
        p.hline(line.x + 6.0, line.right() - 6.0, mid, th.device.grid_strong);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        p.text(
            "L",
            Rect::new(line.x + 6.0, line.y + 6.0, 12.0, 12.0),
            &label,
        );
        p.text(
            "R",
            Rect::new(line.x + 6.0, line.bottom() - 18.0, 12.0, 12.0),
            &label,
        );
        let peak = cx.tap.take_value(value::WET_PEAK);
        self.wet = peak.max(self.wet * (1.0 - 2.0 * cx.dt).max(0.0));
        let glow = 0.5 + 0.5 * self.wet.min(1.0);
        let h = line.h / 2.0 - 14.0;
        let stem = |p: &mut dyn Painter, t: f64, level: f64, up: bool, alpha: f32| {
            let x = x_of(t);
            let len = h * level.clamp(0.0, 1.0) as f32;
            let (a, b) = if up {
                (mid - len, mid)
            } else {
                (mid, mid + len)
            };
            p.fill_rounded(
                Rect::new(x - 2.5, a, 5.0, (b - a).max(1.0)),
                2.0,
                &Paint::Solid(self.accent.with_alpha(alpha)),
            );
            p.circle(
                Point::new(x, if up { a } else { b }),
                3.5,
                self.accent.with_alpha(alpha),
            );
        };
        // The dry hit.
        p.vline(x_of(0.0), mid - h, mid + h, th.ui.text_faint);
        let fb = if frozen { 1.0 } else { feedback.min(1.0) };
        let mut at = 0.0;
        for k in 1..=12 {
            let level = fb.powi(k - 1);
            if level < 0.01 {
                break;
            }
            let a = glow * (0.4 + 0.6 * level as f32);
            match mode {
                1 => {
                    // Ping-pong: left after the left time, right after the
                    // right one more, left…
                    at += if k % 2 == 1 { tl } else { tr };
                    if at <= span {
                        stem(p, at, level, k % 2 == 1, a);
                    }
                }
                _ => {
                    if tl * f64::from(k) <= span {
                        stem(p, tl * f64::from(k), level, true, a);
                    }
                    if tr * f64::from(k) <= span {
                        stem(p, tr * f64::from(k), level, false, a);
                    }
                }
            }
        }
        p.text(
            &format!("{span:.1} s"),
            Rect::new(line.right() - 60.0, line.bottom() - 18.0, 52.0, 12.0),
            &label.right(),
        );
        // One pass through the loop.
        p.fill_rounded(lp, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        p.text(
            "ONE PASS",
            Rect::new(lp.x + 8.0, lp.y + 6.0, 100.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .tracking(0.6),
        );
        let plot = Rect::new(lp.x + 8.0, lp.y + 24.0, lp.w - 16.0, lp.h - 32.0);
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        let low = cx.value(pid(id::LOW_CUT));
        let damping = dly::damping_hz(cx.value(pid(id::DAMPING)), rate);
        let style = cx.value(pid(id::STYLE)).round() as i64;
        let to_y = |d: f64| plot.y + plot.h * ((6.0 - d) / 36.0).clamp(0.0, 1.0) as f32;
        for d in [0.0, -12.0, -24.0] {
            p.hline(plot.x, plot.right(), to_y(d), th.device.grid);
        }
        let n = 80;
        let pts: Vec<Point> = (0..=n)
            .map(|i| {
                let x = plot.x + plot.w * i as f32 / n as f32;
                let f = kit::Spectrum::freq_at(plot, x);
                let mut d = 0.0;
                if low > 20.5 {
                    d += band_db(&cut(BandType::LowCut, low), rate, f);
                }
                if let Some(hz) = damping {
                    d += -10.0 * (1.0 + (f / hz).powi(2)).log10();
                }
                match style {
                    1 => {
                        d += -10.0 * (1.0 + (f / 9_000.0).powi(2)).log10()
                            + band_db(&bump(), rate, f)
                    }
                    2 => d += 2.0 * band_db(&cut(BandType::HighCut, 4_500.0), rate, f),
                    _ => {}
                }
                Point::new(x, to_y(d))
            })
            .collect();
        let mut fill = Path::polyline(&pts);
        fill.line_to(Point::new(plot.right(), plot.bottom()))
            .line_to(Point::new(plot.x, plot.bottom()))
            .close();
        p.fill_path(&fill, self.accent.with_alpha(0.12));
        p.stroke_path(&Path::polyline(&pts), 1.8, self.accent);
        for (f, t) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
            let x = kit::Spectrum::x_of(plot, f);
            p.text(
                t,
                Rect::new(x - 15.0, plot.bottom() - 12.0, 30.0, 12.0),
                &label.center(),
            );
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (_, line, _) = Self::split(r);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if line.contains(pos) && !cx.on(pid(id::SYNC)) => {
                self.drag = Some((pos.x, cx.value(pid(id::TIME))));
                edit.begin("Time");
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
                // Drag right for longer, by a factor per 200 px.
                let t = from * 2f64.powf(f64::from(pos.x - x0) / 200.0);
                edit.set(pid(id::TIME), t.clamp(1.0, 4_000.0));
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
        dly::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => "Stereo (each side its own), Ping-Pong (repeats cross), Mono",
            id::OFFSET => "The right side's time longer or shorter",
            id::DAMPING => "Each pass loses its highs",
            id::LOW_CUT => "Each pass loses its lows",
            id::SATURATION => "Each pass is driven a little",
            id::STYLE => "Digital (clean), Tape (rounder, wobbling), Analog (dark, bucket brigade)",
            id::WOW => "Tape wow and flutter",
            id::DUCKING => "Repeats step back while you play",
            id::FREEZE => "Loop what is there for ever, let nothing new in",
            id::WIDTH => "How wide the repeats are",
            _ => return None,
        })
    }
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

fn bump() -> BandShape {
    BandShape {
        kind: BandType::Bell,
        freq: 110.0,
        gain: 1.0,
        q: 0.9,
        slope: 12.0,
    }
}
