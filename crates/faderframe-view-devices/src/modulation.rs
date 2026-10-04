//! The Modulation face: the LFO's cycle with where each side is on it,
//! and what the mode does right now — the voices moving on the delay
//! (chorus, ensemble, vibrato) or the comb or notches the flanger or the
//! phaser makes at this moment, over the range they sweep.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::modulation::{self as md, id, value};
use faderframe_plugin_host::dsp::lfo::{Lfo, Shape};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};
use std::f64::consts::TAU;

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct ModulationFace {
    accent: Color,
}

impl ModulationFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Modulation),
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let lfo = Rect::new(r.x + 8.0, r.y + 8.0, (r.w * 0.36).min(320.0), r.h - 16.0);
        let what = Rect::new(
            lfo.right() + 12.0,
            r.y + 8.0,
            r.right() - lfo.right() - 20.0,
            r.h - 16.0,
        );
        (lfo, what)
    }
}

/// `(re, im)` products and sums for the responses.
type C = (f64, f64);
fn mul(a: C, b: C) -> C {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
fn div(a: C, b: C) -> C {
    let d = b.0 * b.0 + b.1 * b.1;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
fn mag_db(a: C) -> f64 {
    10.0 * (a.0 * a.0 + a.1 * a.1).max(1e-12).log10()
}

impl Face for ModulationFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(900.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("MODULATION", 6.0),
                ("VOICE", 4.6),
                ("PHASER", 2.6),
                ("OUTPUT", 4.0),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::segments(
            pid(id::MODE),
            "",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 24.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 32.0, r.w * 0.68, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::RATE), "RATE", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::DEPTH), "DEPTH", k[1]));
        c.push(Ctl::small(
            pid(id::SPREAD),
            "SPREAD",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let side = Rect::new(r.x + r.w * 0.68 + 2.0, r.y + 32.0, r.w * 0.32 - 8.0, KNOB.1);
        c.push(Ctl::toggle(
            pid(id::SYNC),
            "Sync",
            Rect::new(side.x, side.y, side.w, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::DIVISION),
            "NOTE",
            Rect::new(side.x, side.y + 28.0, side.w, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::SHAPE),
            "SHAPE",
            Rect::new(side.x, side.y + 74.0, side.w, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::DELAY), "DELAY", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::FEEDBACK), "FEEDBACK", k[1]).bipolar());
        c.push(Ctl::small(
            pid(id::VOICES),
            "VOICES",
            Rect::new(
                r.x + (r.w - SMALL.0) / 2.0,
                r.y + KNOB.1 + 2.0,
                SMALL.0,
                SMALL.1,
            ),
        ));
        let r = kit::inside(&s[2]);
        c.push(Ctl::small(
            pid(id::STAGES),
            "STAGES",
            Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y, SMALL.0, SMALL.1),
        ));
        c.push(
            Ctl::small(
                pid(id::CENTER),
                "CENTRE",
                Rect::new(
                    r.x + (r.w - SMALL.0) / 2.0,
                    r.y + SMALL.1 + 6.0,
                    SMALL.0,
                    SMALL.1,
                ),
            )
            .scaled(Scale::Log),
        );
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[0]));
        c.push(Ctl::small(
            pid(id::WIDTH),
            "WIDTH",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(
            Ctl::small(
                pid(id::HIGH_CUT),
                "HIGH CUT",
                Rect::new(
                    r.x + (r.w - SMALL.0) / 2.0,
                    r.y + KNOB.1 + 2.0,
                    SMALL.0,
                    SMALL.1,
                ),
            )
            .scaled(Scale::Log),
        );
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
        let (lr, what) = Self::split(r);
        let mode = cx.value(pid(id::MODE)).round().clamp(0.0, 4.0) as usize;
        let shape_i = cx.value(pid(id::SHAPE)).round().max(0.0) as usize;
        let shape = Shape::from_index(shape_i);
        let phase = f64::from(cx.published(value::PHASE));
        let spread = cx.value(pid(id::SPREAD)) / 360.0;
        let depth = cx.value(pid(id::DEPTH));
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        let title = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .tracking(0.6);
        // The LFO over a cycle; random shapes drawn as their idea.
        p.fill_rounded(lr, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        p.text("LFO", Rect::new(lr.x + 8.0, lr.y + 6.0, 60.0, 12.0), &title);
        let rate_text = if cx.on(pid(id::SYNC)) {
            md::format(pid(id::DIVISION), cx.value(pid(id::DIVISION))).unwrap_or_default()
        } else {
            faderframe_plugin_host::eq::format_hz(cx.value(pid(id::RATE)))
        };
        p.text(
            &format!("{} · {rate_text}", md::SHAPES[shape_i.min(5)]),
            Rect::new(lr.x + 8.0, lr.y + 6.0, lr.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, self.accent).bold().right(),
        );
        let plot = Rect::new(lr.x + 10.0, lr.y + 26.0, lr.w - 20.0, lr.h - 40.0);
        let mid = plot.y + plot.h / 2.0;
        p.hline(plot.x, plot.right(), mid, th.device.grid_strong);
        let mut demo = Lfo::new(7);
        let pts: Vec<Point> = (0..=120)
            .map(|i| {
                let t = f64::from(i) / 120.0;
                let v = match shape {
                    Shape::SampleHold | Shape::Drift => {
                        demo.phase = 0.0;
                        let k = ((t * 4.0).floor() * 7.3).sin();
                        if shape == Shape::SampleHold {
                            k
                        } else {
                            let next = (((t * 4.0).floor() + 1.0) * 7.3).sin();
                            let u = 0.5 - 0.5 * (std::f64::consts::PI * (t * 4.0).fract()).cos();
                            k + (next - k) * u
                        }
                    }
                    _ => {
                        demo.phase = t;
                        demo.value(shape, 0.0)
                    }
                };
                Point::new(
                    plot.x + plot.w * t as f32,
                    mid - (v * depth.max(0.15)) as f32 * plot.h * 0.45,
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.0, self.accent.with_alpha(0.85));
        for (c, name) in [(0usize, "L"), (1, "R")] {
            let t = (phase + c as f64 * spread).rem_euclid(1.0);
            let i = (t * 120.0).round() as usize;
            let pt = pts[i.min(120)];
            p.vline(
                pt.x,
                plot.y,
                plot.bottom(),
                self.accent.with_alpha(if c == 0 { 0.4 } else { 0.2 }),
            );
            p.circle(pt, 5.0, if c == 0 { th.ui.text } else { self.accent });
            p.text(
                name,
                Rect::new(pt.x + 6.0, plot.bottom() - 12.0, 12.0, 12.0),
                &label,
            );
        }
        // What the mode does now.
        p.fill_rounded(what, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        p.text(
            md::MODES[mode],
            Rect::new(what.x + 8.0, what.y + 6.0, 120.0, 12.0),
            &title,
        );
        let plot = Rect::new(what.x + 12.0, what.y + 26.0, what.w - 24.0, what.h - 44.0);
        let lfo_now = |c: usize| {
            let mut l = Lfo::new(0);
            l.phase = (phase + c as f64 * spread).rem_euclid(1.0);
            l.value(
                if matches!(shape, Shape::SampleHold | Shape::Drift) {
                    Shape::Sine
                } else {
                    shape
                },
                0.0,
            )
        };
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        let mix = if mode == 4 {
            1.0
        } else {
            cx.value(pid(id::MIX))
        };
        let feedback = cx.value(pid(id::FEEDBACK));
        let delay_ms = cx.value(pid(id::DELAY));
        match mode {
            2 | 3 => {
                // The response now (left side) and its sweep's extremes.
                let response = |v: f64, f: f64| -> f64 {
                    let w = TAU * f / rate;
                    let wet: C = if mode == 2 {
                        let d = (1.0
                            + (delay_ms * 0.001 * rate - 1.0).max(0.0)
                                * (1.0 - depth + depth * (0.5 + 0.5 * v)))
                            .max(1.0);
                        let z = ((-w * d).cos(), (-w * d).sin());
                        div(z, (1.0 - feedback * z.0, -feedback * z.1))
                    } else {
                        let stages =
                            md::STAGES[cx.value(pid(id::STAGES)).round().clamp(0.0, 5.0) as usize];
                        let a = md::coefficient(
                            cx.value(pid(id::CENTER)) * 2f64.powf(3.0 * depth * v),
                            rate,
                        );
                        let z1 = (w.cos(), -w.sin());
                        let one = div((a + z1.0, z1.1), (1.0 + a * z1.0, a * z1.1));
                        let mut all = (1.0, 0.0);
                        for _ in 0..stages {
                            all = mul(all, one);
                        }
                        div(all, (1.0 - feedback * all.0, -feedback * all.1))
                    };
                    mag_db(((1.0 - mix) + mix * wet.0, mix * wet.1))
                };
                let to_y = |d: f64| plot.y + plot.h * ((12.0 - d) / 48.0).clamp(0.0, 1.0) as f32;
                for d in [0.0, -12.0, -24.0] {
                    p.hline(plot.x, plot.right(), to_y(d), th.device.grid);
                    p.text(
                        &format!("{d:.0}").replace('-', "−"),
                        Rect::new(plot.x - 2.0, to_y(d) - 13.0, 30.0, 12.0),
                        &label,
                    );
                }
                let n = 180;
                let curve = |v: f64| -> Vec<Point> {
                    (0..=n)
                        .map(|i| {
                            let x = plot.x + plot.w * i as f32 / n as f32;
                            Point::new(x, to_y(response(v, kit::Spectrum::freq_at(plot, x))))
                        })
                        .collect()
                };
                for v in [-1.0, 1.0] {
                    p.stroke_path(
                        &Path::polyline(&curve(v)),
                        1.0,
                        self.accent.with_alpha(0.25),
                    );
                }
                let now = curve(lfo_now(0));
                let mut fill = Path::polyline(&now);
                fill.line_to(Point::new(plot.right(), plot.bottom()))
                    .line_to(Point::new(plot.x, plot.bottom()))
                    .close();
                p.fill_path(&fill, self.accent.with_alpha(0.14));
                p.stroke_path(&Path::polyline(&now), 2.0, self.accent);
                for (f, t) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
                    let x = kit::Spectrum::x_of(plot, f);
                    p.vline(x, plot.y, plot.bottom(), th.device.grid);
                    p.text(
                        t,
                        Rect::new(x - 15.0, plot.bottom() + 3.0, 30.0, 12.0),
                        &label.center(),
                    );
                }
            }
            _ => {
                // The voices on the delay: 0 to twice the delay time.
                let span = delay_ms * 2.0;
                let x_of = |ms: f64| plot.x + plot.w * (ms / span).clamp(0.0, 1.0) as f32;
                let mut t = 0.0;
                let step = if span > 20.0 {
                    5.0
                } else if span > 8.0 {
                    2.0
                } else {
                    1.0
                };
                while t <= span {
                    p.vline(x_of(t), plot.y, plot.bottom(), th.device.grid);
                    p.text(
                        &format!("{t:.0} ms"),
                        Rect::new(x_of(t) - 20.0, plot.bottom() + 3.0, 40.0, 12.0),
                        &label.center(),
                    );
                    t += step;
                }
                let voices = match mode {
                    1 => 3,
                    4 => 1,
                    _ => cx.value(pid(id::VOICES)).round().clamp(0.0, 3.0) as usize + 1,
                };
                let swing = if mode == 4 { 0.9 } else { 0.8 };
                for c in 0..2 {
                    let lane = Rect::new(
                        plot.x,
                        plot.y + plot.h * (0.1 + 0.45 * c as f32),
                        plot.w,
                        plot.h * 0.35,
                    );
                    p.fill_rounded(lane, 3.0, &Paint::Solid(th.device.display));
                    p.text(
                        if c == 0 { "L" } else { "R" },
                        Rect::new(lane.x + 4.0, lane.y + 2.0, 12.0, 12.0),
                        &label,
                    );
                    // The reach of the sweep.
                    let (a, b) = (
                        x_of(delay_ms * (1.0 - swing * depth)),
                        x_of(delay_ms * (1.0 + swing * depth)),
                    );
                    p.fill(
                        Rect::new(a, lane.y + lane.h * 0.42, (b - a).max(1.0), lane.h * 0.16),
                        self.accent.with_alpha(0.18),
                    );
                    for v in 0..voices {
                        let mut l = Lfo::new(0);
                        l.phase =
                            (phase + c as f64 * spread + v as f64 / voices as f64).rem_euclid(1.0);
                        let s = if mode == 1 { Shape::Sine } else { shape };
                        let d = delay_ms * (1.0 + swing * depth * l.value(s, 0.0));
                        let x = x_of(d);
                        p.fill_rounded(
                            Rect::new(x - 3.0, lane.y + 6.0, 6.0, lane.h - 12.0),
                            3.0,
                            &Paint::Solid(self.accent.with_alpha(0.9)),
                        );
                    }
                }
            }
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        md::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => "Chorus, Ensemble (string machine), Flanger, Phaser, Vibrato (all wet)",
            id::DELAY => "The delay the voices move round (the flanger: the longest)",
            id::FEEDBACK => "Flanger and phaser: feed back for resonance (negative: hollow)",
            id::VOICES => "Chorus voices a side",
            id::STAGES => "Phaser stages: two per notch",
            id::CENTER => "Where the phaser's notches sweep round",
            id::SPREAD => "How far ahead the right side runs",
            _ => return None,
        })
    }
}
