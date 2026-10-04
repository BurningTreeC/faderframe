//! The Synth's face: one cycle of each oscillator as set, the filter's
//! response (its reach under the envelope shaded, where the newest voice
//! has it now marked) and both envelopes with their levels now; below,
//! two rows of sections for the oscillators, mix and voicing, filter,
//! envelopes, LFO and output.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Meter, MeterKind, Panel, SMALL, Scale, Section};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::synth::{self as syn, id, value};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct SynthFace {
    accent: Color,
}

impl SynthFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Instrument),
        }
    }
}

/// Small knobs in a row across `r` (from the top).
fn smalls(c: &mut Vec<Ctl>, r: Rect, items: &[(u32, &'static str, Option<Scale>, bool)]) {
    let widths: Vec<f32> = items.iter().map(|_| SMALL.0).collect();
    for (k, &(i, label, scale, bipolar)) in kit::row(Rect::new(r.x, r.y, r.w, SMALL.1), &widths)
        .iter()
        .zip(items)
    {
        let mut ctl = Ctl::small(pid(i), label, *k);
        if let Some(s) = scale {
            ctl = ctl.scaled(s);
        }
        if bipolar {
            ctl = ctl.bipolar();
        }
        c.push(ctl);
    }
}

/// An ADSR's shape across `r` (attack, decay, a fixed sustain stretch,
/// release; times on a square-root scale so short ones show).
fn adsr(
    p: &mut dyn Painter,
    r: Rect,
    times: [f64; 3],
    sustain: f64,
    color: Color,
    now: Option<f32>,
) {
    let w = |ms: f64| (ms.max(0.5) / 10_000.0).sqrt() as f32;
    let (a, d, rel) = (w(times[0]), w(times[1]), w(times[2]));
    let hold = 0.25;
    let total = a + d + hold + rel;
    let x = |t: f32| r.x + r.w * t / total;
    let y = |v: f64| r.bottom() - r.h * v.clamp(0.0, 1.0) as f32;
    let mut pts = vec![Point::new(r.x, r.bottom()), Point::new(x(a), r.y)];
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        let v = sustain + (1.0 - sustain) * f64::from((-6.9 * t).exp());
        pts.push(Point::new(x(a + d * t), y(v)));
    }
    pts.push(Point::new(x(a + d + hold), y(sustain)));
    for i in 1..=16 {
        let t = i as f32 / 16.0;
        pts.push(Point::new(
            x(a + d + hold + rel * t),
            y(sustain * f64::from((-6.9 * t).exp())),
        ));
    }
    let mut fill = Path::polyline(&pts);
    fill.line_to(Point::new(r.right(), r.bottom())).close();
    p.fill_path(&fill, color.with_alpha(0.12));
    p.stroke_path(&Path::polyline(&pts), 1.8, color);
    if let Some(v) = now.filter(|v| *v > 0.001) {
        let yy = y(f64::from(v));
        p.hline(r.x, r.right(), yy, color.with_alpha(0.35));
        p.circle(Point::new(r.x + 4.0, yy), 3.5, color);
    }
}

impl Face for SynthFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1240.0, 640.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let deck_h = 2.0 * 178.0 + 8.0;
        let (display, deck, meter) = kit::frame(size, deck_h, 30.0);
        let top = Rect::new(deck.x, deck.y, deck.w, 178.0);
        let bottom = Rect::new(deck.x, deck.y + 186.0, deck.w, 178.0);
        let mut s: Vec<Section> = kit::sections(
            top,
            &[("OSC 1", 5.0), ("OSC 2", 5.0), ("MIX", 6.6), ("VOICE", 4.4)],
        );
        s.extend(kit::sections(
            bottom,
            &[
                ("FILTER", 7.6),
                ("FILTER ENVELOPE", 4.6),
                ("AMP ENVELOPE", 4.4),
                ("LFO", 5.8),
                ("OUT", 2.2),
            ],
        ));
        let mut c = Vec::new();
        let seg = |r: Rect| Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0);
        let below = |r: Rect| Rect::new(r.x, r.y + 34.0, r.w, r.h - 34.0);
        // Oscillators.
        let r = kit::inside(&s[0]);
        c.push(Ctl::segments(pid(id::OSC1_WAVE), "", seg(r)));
        smalls(
            &mut c,
            below(r),
            &[
                (id::OSC1_OCTAVE, "OCTAVE", None, true),
                (id::OSC1_PW, "PW", None, false),
                (id::OSC1_LEVEL, "LEVEL", None, false),
            ],
        );
        let r = kit::inside(&s[1]);
        c.push(Ctl::segments(pid(id::OSC2_WAVE), "", seg(r)));
        smalls(
            &mut c,
            below(r),
            &[
                (id::OSC2_OCTAVE, "OCTAVE", None, true),
                (id::OSC2_SEMI, "SEMI", None, true),
                (id::OSC2_LEVEL, "LEVEL", None, false),
            ],
        );
        let r = kit::inside(&s[2]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 20.0, r.w, r.h),
            &[
                (id::SUB, "SUB", None, false),
                (id::NOISE, "NOISE", None, false),
                (id::DETUNE, "DETUNE", None, false),
                (id::UNISON, "UNISON", None, false),
                (id::UNISON_SPREAD, "SPREAD", None, false),
                (id::WIDTH, "WIDTH", None, false),
            ],
        );
        let r = kit::inside(&s[3]);
        c.push(Ctl::segments(pid(id::VOICE_MODE), "", seg(r)));
        smalls(
            &mut c,
            below(r),
            &[
                (id::GLIDE, "GLIDE", Some(Scale::Skew(2.5)), false),
                (id::POLYPHONY, "VOICES", None, false),
                (id::VELOCITY_AMP, "VELOCITY", None, false),
            ],
        );
        // Filter.
        let r = kit::inside(&s[4]);
        c.push(Ctl::segments(pid(id::FILTER_TYPE), "", seg(r)));
        let k = kit::row(
            Rect::new(r.x, r.y + 30.0, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::CUTOFF), "CUTOFF", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::RESONANCE), "RESO", k[1]));
        let small_at = |r: Rect| kit::at(r, 0.0, 6.0, SMALL.0, SMALL.1);
        c.push(Ctl::small(pid(id::ENV_AMOUNT), "ENV", small_at(k[2])).bipolar());
        c.push(Ctl::small(pid(id::DRIVE), "DRIVE", small_at(k[3])));
        c.push(Ctl::small(pid(id::KEY_TRACK), "KEY", small_at(k[4])));
        c.push(Ctl::small(pid(id::VELOCITY_CUTOFF), "VEL", small_at(k[5])));
        // Envelopes.
        let r = kit::inside(&s[5]);
        c.push(Ctl::segments(pid(id::FILTER_ENV), "", seg(r)));
        let ms = Some(Scale::Log);
        smalls(
            &mut c,
            below(r),
            &[
                (id::F_ATTACK, "A", ms, false),
                (id::F_DECAY, "D", ms, false),
                (id::F_SUSTAIN, "S", None, false),
                (id::F_RELEASE, "R", ms, false),
            ],
        );
        let r = kit::inside(&s[6]);
        smalls(
            &mut c,
            below(r),
            &[
                (id::ATTACK, "A", ms, false),
                (id::DECAY, "D", ms, false),
                (id::SUSTAIN, "S", None, false),
                (id::RELEASE, "R", ms, false),
            ],
        );
        // LFO.
        let r = kit::inside(&s[7]);
        let third = (r.w - 16.0) / 3.0;
        c.push(Ctl::choice(
            pid(id::LFO_SHAPE),
            "SHAPE",
            Rect::new(r.x + 4.0, r.y - 6.0, third, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::LFO_SYNC),
            "Sync",
            Rect::new(r.x + 8.0 + third, r.y + 8.0, third, 24.0),
        ));
        c.push(Ctl::choice(
            pid(id::LFO_DIVISION),
            "NOTE",
            Rect::new(r.x + 12.0 + 2.0 * third, r.y - 6.0, third, 38.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, r.h - 40.0),
            &[
                (id::LFO_RATE, "RATE", Some(Scale::Log), false),
                (id::LFO_PITCH, "PITCH", None, false),
                (id::LFO_CUTOFF, "CUTOFF", None, false),
                (id::LFO_AMP, "LEVEL", None, false),
                (id::LFO_PW, "PW", None, false),
            ],
        );
        let r = kit::inside(&s[8]);
        c.push(Ctl::knob(
            pid(id::VOLUME),
            "VOLUME",
            Rect::new(r.x + (r.w - KNOB.0) / 2.0, r.y + 30.0, KNOB.0, KNOB.1),
        ));
        let meters = vec![Meter {
            rect: meter,
            kind: MeterKind::Output,
            label: "OUT",
        }];
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters,
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        let w = (r.w - 32.0) / 3.0;
        let boxes = [0, 1, 2]
            .map(|i| Rect::new(r.x + 8.0 + i as f32 * (w + 8.0), r.y + 8.0, w, r.h - 16.0));
        let title = TextStyle::new(th.fonts.tiny, th.ui.text_dim)
            .bold()
            .tracking(0.6);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        for (b, t) in boxes.iter().zip(["OSCILLATORS", "FILTER", "ENVELOPES"]) {
            p.fill_rounded(*b, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
            p.text(t, Rect::new(b.x + 8.0, b.y + 6.0, b.w, 12.0), &title);
        }
        let second = th.device.accent(Accent::Modulation);
        // One cycle of each oscillator (osc 2 at its pitch relative to 1).
        let plot = Rect::new(
            boxes[0].x + 10.0,
            boxes[0].y + 26.0,
            boxes[0].w - 20.0,
            boxes[0].h - 40.0,
        );
        let mid = plot.y + plot.h / 2.0;
        p.hline(plot.x, plot.right(), mid, th.device.grid_strong);
        let ratio = 2f64.powf(
            cx.value(pid(id::OSC2_OCTAVE)).round() - cx.value(pid(id::OSC1_OCTAVE)).round()
                + cx.value(pid(id::OSC2_SEMI)).round() / 12.0,
        );
        let pw = cx.value(pid(id::OSC1_PW));
        let shape = |wave: f64, t: f64| -> f64 {
            let t = t.rem_euclid(1.0);
            match wave.round() as i64 {
                1 => {
                    if t < pw {
                        1.0
                    } else {
                        -1.0
                    }
                }
                2 => 1.0 - 4.0 * (t - 0.5).abs(),
                3 => (std::f64::consts::TAU * t).sin(),
                _ => 2.0 * t - 1.0,
            }
        };
        for (o, cycles, color) in [(0, 2.0, self.accent), (1, 2.0 * ratio, second)] {
            let level = cx.value(pid(if o == 0 {
                id::OSC1_LEVEL
            } else {
                id::OSC2_LEVEL
            }));
            let wave = cx.value(pid(if o == 0 { id::OSC1_WAVE } else { id::OSC2_WAVE }));
            if level <= 0.001 {
                continue;
            }
            let pts: Vec<Point> = (0..=240)
                .map(|i| {
                    let t = f64::from(i) / 240.0;
                    let v = shape(wave, t * cycles) * level;
                    Point::new(plot.x + plot.w * t as f32, mid - (v * 0.42) as f32 * plot.h)
                })
                .collect();
            p.stroke_path(
                &Path::polyline(&pts),
                if o == 0 { 2.0 } else { 1.4 },
                color.with_alpha(if o == 0 { 1.0 } else { 0.8 }),
            );
        }
        let unison = cx.value(pid(id::UNISON)).round();
        let voices = cx.published(value::VOICES);
        p.text(
            &format!(
                "{}{}",
                if unison > 1.0 {
                    format!("{unison:.0}× unison  ")
                } else {
                    String::new()
                },
                if voices > 0.0 {
                    format!(
                        "{voices:.0} playing · {}",
                        note_name(cx.published(value::NOTE) as i32)
                    )
                } else {
                    String::new()
                }
            ),
            Rect::new(
                boxes[0].x + 8.0,
                boxes[0].bottom() - 18.0,
                boxes[0].w - 16.0,
                12.0,
            ),
            &label.right(),
        );
        // The filter: its response, and its reach under the envelope.
        let fp = Rect::new(
            boxes[1].x + 10.0,
            boxes[1].y + 26.0,
            boxes[1].w - 20.0,
            boxes[1].h - 40.0,
        );
        let kind = cx.value(pid(id::FILTER_TYPE)).round() as i64;
        let k = 2.0 - 1.9 * cx.value(pid(id::RESONANCE));
        let cutoff = cx.value(pid(id::CUTOFF));
        let amount = cx.value(pid(id::ENV_AMOUNT)) * 6.0;
        let response = |f: f64, fc: f64| -> f64 {
            let x = f / fc;
            let den = |kk: f64| ((1.0 - x * x).powi(2) + (kk * x).powi(2)).sqrt();
            let m = match kind {
                1 => 1.0 / den(std::f64::consts::SQRT_2) / den(k),
                2 => k * x / den(k),
                3 => x * x / den(k),
                _ => 1.0 / den(k),
            };
            20.0 * m.max(1e-6).log10()
        };
        let to_y = |d: f64| fp.y + fp.h * ((18.0 - d) / 66.0).clamp(0.0, 1.0) as f32;
        p.hline(fp.x, fp.right(), to_y(0.0), th.device.grid_strong);
        let curve = |fc: f64| -> Vec<Point> {
            (0..=120)
                .map(|i| {
                    let x = fp.x + fp.w * i as f32 / 120.0;
                    Point::new(x, to_y(response(kit::Spectrum::freq_at(fp, x), fc)))
                })
                .collect()
        };
        let reach = (cutoff * 2f64.powf(amount)).clamp(20.0, 20_000.0);
        if (reach - cutoff).abs() > 1.0 {
            let (a, b) = (
                kit::Spectrum::x_of(fp, cutoff.min(reach)),
                kit::Spectrum::x_of(fp, cutoff.max(reach)),
            );
            p.fill(
                Rect::new(a, fp.y, b - a, fp.h),
                self.accent.with_alpha(0.08),
            );
            p.stroke_path(
                &Path::polyline(&curve(reach)),
                1.0,
                self.accent.with_alpha(0.35),
            );
        }
        let main = curve(cutoff);
        let mut fill = Path::polyline(&main);
        fill.line_to(Point::new(fp.right(), fp.bottom()))
            .line_to(Point::new(fp.x, fp.bottom()))
            .close();
        p.fill_path(&fill, self.accent.with_alpha(0.12));
        p.stroke_path(&Path::polyline(&main), 2.0, self.accent);
        if voices > 0.0 {
            let now = f64::from(cx.published(value::CUTOFF));
            if now > 20.0 {
                let x = kit::Spectrum::x_of(fp, now);
                p.vline(x, fp.y, fp.bottom(), th.ui.text.with_alpha(0.5));
                p.circle(Point::new(x, to_y(response(now, now))), 4.0, th.ui.text);
            }
        }
        p.text(
            &format!(
                "{} · {}",
                syn::FILTERS[kind.clamp(0, 3) as usize],
                faderframe_plugin_host::eq::format_hz(cutoff)
            ),
            Rect::new(boxes[1].x + 8.0, boxes[1].y + 6.0, boxes[1].w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, self.accent).bold().right(),
        );
        // The envelopes.
        let ep = Rect::new(
            boxes[2].x + 10.0,
            boxes[2].y + 26.0,
            boxes[2].w - 20.0,
            boxes[2].h - 40.0,
        );
        let own = cx.value(pid(id::FILTER_ENV)) >= 0.5;
        if own {
            adsr(
                p,
                ep,
                [
                    cx.value(pid(id::F_ATTACK)),
                    cx.value(pid(id::F_DECAY)),
                    cx.value(pid(id::F_RELEASE)),
                ],
                cx.value(pid(id::F_SUSTAIN)),
                second,
                Some(cx.published(value::FILTER_ENV)),
            );
        }
        adsr(
            p,
            ep,
            [
                cx.value(pid(id::ATTACK)),
                cx.value(pid(id::DECAY)),
                cx.value(pid(id::RELEASE)),
            ],
            cx.value(pid(id::SUSTAIN)),
            self.accent,
            Some(cx.published(value::AMP_ENV)),
        );
        p.text(
            if own {
                "amp · filter"
            } else {
                "amp (and filter)"
            },
            Rect::new(boxes[2].x + 8.0, boxes[2].y + 6.0, boxes[2].w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        syn::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::DETUNE => "How far the two oscillators are apart",
            id::UNISON => "Copies of the oscillators per voice",
            id::UNISON_SPREAD => "How far the unison copies are detuned",
            id::WIDTH => "How far the oscillators and copies spread across the sides",
            id::FILTER_ENV => "The filter follows the amp envelope, or its own",
            id::KEY_TRACK => "The cutoff follows the keys",
            id::VELOCITY_CUTOFF => "How much velocity opens the filter envelope",
            id::VELOCITY_AMP => "How much velocity sets the level",
            id::VOICE_MODE => "Poly, Mono (always retriggers), Legato (slides between held keys)",
            id::GLIDE => "Slide from the last note",
            _ => return None,
        })
    }
}
