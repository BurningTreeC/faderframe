//! The Reverb's face: the decay as set — pre-delay (synced or in ms), the
//! early reflections (the springs' bounces for the Spring), the tail of
//! the bass, the middle and the highs falling over their times, the
//! shimmer's climb (drag across it to set the decay) — and the decay time
//! across the spectrum with the wet cuts and shelves, the controls and
//! meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::reverb::{self as rev, id, value};
use faderframe_plugin_host::eq::design::{BandShape, BandType, band_db};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// The early reflections' times (fractions of the span) and levels, as
/// the processor has them (for the picture).
const EARLY: [(f64, f64); 12] = [
    (0.043, 0.84),
    (0.071, 0.79),
    (0.118, 0.72),
    (0.162, 0.66),
    (0.231, 0.58),
    (0.297, 0.55),
    (0.364, 0.47),
    (0.452, 0.43),
    (0.538, 0.37),
    (0.649, 0.31),
    (0.781, 0.26),
    (0.921, 0.21),
];

pub(crate) struct ReverbFace {
    accent: Color,
    wet: f32,
    drag: Option<(f32, f64)>,
}

impl ReverbFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Space),
            wet: 0.0,
            drag: None,
        }
    }

    fn split(r: Rect) -> (Rect, Rect) {
        let freq = Rect::new(r.right() - 288.0, r.y + 8.0, 280.0, r.h - 16.0);
        let time = Rect::new(r.x + 8.0, r.y + 8.0, freq.x - r.x - 20.0, r.h - 16.0);
        (time, freq)
    }

    /// Seconds across the decay picture.
    fn span(cx: &Ctx<'_>) -> f64 {
        let rt = rev::decay_times(&cx.tap.params)
            .iter()
            .copied()
            .fold(0.0, f64::max);
        (Self::pre(cx) + rt * 1.1).clamp(0.3, 25.0)
    }

    /// The pre-delay (seconds) at the song's tempo.
    fn pre(cx: &Ctx<'_>) -> f64 {
        let tempo = cx
            .model
            .project()
            .timeline
            .tempo
            .bpm_at(faderframe_timeline::MusicalTime::ZERO);
        rev::pre_delay_seconds(&cx.tap.params, tempo)
    }
}

impl Face for ReverbFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1_000.0, 490.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 206.0, 48.0);
        let s = kit::sections(
            deck,
            &[
                ("SPACE", 6.0),
                ("TONE", 5.6),
                ("CHARACTER", 4.6),
                ("OUTPUT", 3.4),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::segments(
            pid(id::TYPE),
            "",
            Rect::new(r.x + 4.0, r.y, r.w - 8.0, 24.0),
        ));
        let side_w = 66.0;
        let k = kit::row(
            Rect::new(r.x, r.y + 32.0, r.w - side_w - 6.0, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::DECAY), "DECAY", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::SIZE), "SIZE", k[1]));
        c.push(
            Ctl::small(
                pid(id::PRE_DELAY),
                "PRE-DELAY",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Skew(2.5)),
        );
        // The pre-delay synced: a switch and the note.
        let side = Rect::new(r.right() - side_w, r.y + 32.0, side_w, KNOB.1);
        c.push(Ctl::toggle(
            pid(id::PRE_SYNC),
            "Sync",
            Rect::new(side.x, side.y + 4.0, side.w, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::PRE_DIVISION),
            "NOTE",
            Rect::new(side.x, side.y + 34.0, side.w, 38.0),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w, KNOB.1),
            &[KNOB.0, KNOB.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::DAMPING), "DAMPING", k[0]).scaled(Scale::Log));
        c.push(Ctl::knob(pid(id::BASS), "BASS", k[1]).scaled(Scale::Log));
        c.push(
            Ctl::small(
                pid(id::HIGH_DECAY),
                "HIGH DECAY",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        c.push(
            Ctl::small(
                pid(id::BASS_FREQ),
                "CROSSOVER",
                kit::at(k[3], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .scaled(Scale::Log),
        );
        let k = kit::row(
            Rect::new(r.x, r.y + KNOB.1 + 2.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::LOW_CUT), "LOW CUT", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::HIGH_CUT), "HIGH CUT", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::LOW_SHELF), "LOW SHELF", k[2]).bipolar());
        c.push(Ctl::small(pid(id::HIGH_SHELF), "HIGH SHELF", k[3]).bipolar());
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::DIFFUSION), "DIFFUSION", k[0]));
        c.push(Ctl::knob(pid(id::BALANCE), "EARLY / LATE", k[1]).bipolar());
        c.push(Ctl::small(
            pid(id::SHIMMER),
            "SHIMMER",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + KNOB.1 + 2.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, 70.0],
        );
        c.push(Ctl::small(pid(id::MODULATION), "MOD", k[0]));
        c.push(Ctl::small(pid(id::MOD_RATE), "RATE", k[1]).scaled(Scale::Log));
        c.push(Ctl::choice(
            pid(id::SHIMMER_PITCH),
            "PITCH",
            Rect::new(k[2].x, k[2].y + 12.0, 70.0, 38.0),
        ));
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[0]));
        c.push(Ctl::small(
            pid(id::WIDTH),
            "WIDTH",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + KNOB.1 + 2.0, r.w, SMALL.1),
            &[SMALL.0, 96.0],
        );
        c.push(Ctl::small(pid(id::DUCKING), "DUCKING", k[0]));
        c.push(Ctl::toggle(
            pid(id::FREEZE),
            "Freeze",
            Rect::new(k[1].x, k[1].y + 24.0, 96.0, SWITCH_H),
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
        let (time, freq) = Self::split(r);
        let rts = rev::decay_times(&cx.tap.params);
        let pre = Self::pre(cx);
        let kind = (cx.value(pid(id::TYPE)).round().max(0.0) as usize).min(rev::TYPES.len() - 1);
        let voicing = &rev::VOICINGS[kind];
        let shimmer = cx.value(pid(id::SHIMMER));
        let scale = rev::size_scale(cx.value(pid(id::SIZE)));
        let balance = cx.value(pid(id::BALANCE));
        let frozen = cx.on(pid(id::FREEZE));
        let span = Self::span(cx);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        // The decay picture: 0 to −60 dB down, time across.
        p.fill_rounded(time, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        let plot = Rect::new(time.x + 30.0, time.y + 10.0, time.w - 40.0, time.h - 30.0);
        let x_of = |t: f64| plot.x + plot.w * (t / span).clamp(0.0, 1.0) as f32;
        let y_of = |d: f64| plot.y + plot.h * (-d / 60.0).clamp(0.0, 1.0) as f32;
        for d in [-12.0, -24.0, -36.0, -48.0] {
            p.hline(plot.x, plot.right(), y_of(d), th.device.grid);
            p.text(
                &format!("{d:.0}").replace('-', "−"),
                Rect::new(time.x + 2.0, y_of(d) - 7.0, 24.0, 14.0),
                &label.right(),
            );
        }
        let step = [0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0]
            .into_iter()
            .find(|s| span / s <= 10.0)
            .unwrap_or(5.0);
        let mut t = step;
        while t < span {
            p.vline(x_of(t), plot.y, plot.bottom(), th.device.grid);
            p.text(
                &if step < 1.0 {
                    format!("{:.0} ms", t * 1000.0)
                } else {
                    format!("{t:.0} s")
                },
                Rect::new(x_of(t) - 30.0, plot.bottom() + 3.0, 60.0, 12.0),
                &label.center(),
            );
            t += step;
        }
        // Pre-delay.
        p.fill(
            Rect::new(plot.x, plot.y, x_of(pre) - plot.x, plot.h),
            th.device.grid.with_alpha(0.25),
        );
        p.vline(plot.x, plot.y, plot.bottom(), th.ui.text_faint);
        // Early reflections (the springs' bounces for the Spring).
        let early = voicing.early.1 * (2.0 * (1.0 - balance)).min(1.0);
        if kind == rev::SPRING && early > 0.0 {
            for (k, ms) in rev::SPRING_MS.into_iter().enumerate() {
                let len = ms * 0.001 * scale;
                let g = 10f64.powf(-3.0 * len / rts[1]);
                for n in 1..8 {
                    let t = pre + len * f64::from(n);
                    let d = 20.0 * (early * 0.7 * g.powi(n - 1)).log10();
                    if d < -60.0 || t > span {
                        break;
                    }
                    let x = x_of(t);
                    let h = plot.bottom() - y_of(d);
                    p.fill(
                        Rect::new(x - 1.0, y_of(d), 2.0 + 1.5 * k as f32, h),
                        self.accent.with_alpha(0.7),
                    );
                }
            }
        } else if early > 0.0 {
            for (f, g) in EARLY {
                let t = pre + f * voicing.early.0 * 0.001 * scale;
                let d = 20.0 * (g * early).log10();
                let x = x_of(t);
                p.fill(
                    Rect::new(x - 1.0, y_of(d), 2.0, plot.bottom() - y_of(d)),
                    self.accent.with_alpha(0.75),
                );
            }
        }
        // The tails: bass, middle (filled) and highs.
        let late = (2.0 * balance).min(1.0);
        let start = pre + voicing.lines.0 * 0.001 * scale;
        let top = if late > 0.0 {
            20.0 * late.log10() - 3.0
        } else {
            -90.0
        };
        let tail = |rt: f64| -> Vec<Point> {
            (0..=60)
                .map(|i| {
                    let t = start + (span - start) * f64::from(i) / 60.0;
                    let d = if frozen {
                        top
                    } else {
                        top - 60.0 * (t - start) / rt
                    };
                    Point::new(x_of(t), y_of(d))
                })
                .collect()
        };
        let mid = tail(rts[1]);
        let mut fill = Path::polyline(&mid);
        fill.line_to(Point::new(x_of(span), plot.bottom()))
            .line_to(Point::new(x_of(start), plot.bottom()))
            .close();
        p.fill_path_paint(
            &fill,
            &Paint::vertical(
                plot,
                self.accent.with_alpha(0.35),
                self.accent.with_alpha(0.04),
            ),
        );
        p.stroke_path(&Path::polyline(&mid), 2.2, self.accent);
        p.stroke_path(
            &Path::polyline(&tail(rts[0])),
            1.4,
            th.device.wave.with_alpha(0.8),
        );
        p.stroke_path(
            &Path::polyline(&tail(rts[2])),
            1.4,
            th.device.reduction.with_alpha(0.8),
        );
        // The shimmer: what climbs an interval with every pass, rising
        // out of the tail as it feeds back.
        if shimmer > 0.0 && !frozen && late > 0.0 {
            let pts: Vec<Point> = (0..=60)
                .map(|i| {
                    let t = start + (span - start) * f64::from(i) / 60.0;
                    let fed = (shimmer * 0.5 * (t - start) / rts[1].max(0.05)).min(1.0);
                    let d = top - 60.0 * (t - start) / rts[1] + 20.0 * fed.max(1e-3).log10();
                    Point::new(x_of(t), y_of(d))
                })
                .collect();
            p.stroke_path(&Path::polyline(&pts), 1.4, th.ui.text.with_alpha(0.6));
        }
        // The live level (a glow at the top left).
        let peak = cx.tap.take_value(value::WET_PEAK);
        self.wet = peak.max(self.wet * (1.0 - 2.0 * cx.dt).max(0.0));
        let glow = Rect::new(plot.right() - 120.0, plot.y + 4.0, 110.0, 6.0);
        p.fill_rounded(glow, 3.0, &Paint::Solid(th.device.display));
        let lvl = ((20.0 * self.wet.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
        p.fill_rounded(
            Rect::new(glow.x, glow.y, glow.w * lvl, glow.h),
            3.0,
            &Paint::Solid(self.accent),
        );
        p.text(
            &format!(
                "{} · {}{}{}",
                rev::TYPES[kind],
                rev::format(pid(id::DECAY), rts[1]).unwrap_or_default(),
                if shimmer > 0.0 {
                    format!(
                        " · SHIMMER {}",
                        rev::format(pid(id::SHIMMER_PITCH), cx.value(pid(id::SHIMMER_PITCH)))
                            .unwrap_or_default()
                    )
                } else {
                    String::new()
                },
                if frozen { " · FROZEN" } else { "" }
            ),
            Rect::new(plot.x + 8.0, plot.y + 2.0, plot.w - 140.0, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).bold(),
        );
        // The decay time across the spectrum.
        p.fill_rounded(freq, 4.0, &Paint::Solid(th.device.display.darken(0.12)));
        p.text(
            "DECAY OVER FREQUENCY",
            Rect::new(freq.x + 8.0, freq.y + 6.0, freq.w, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                .bold()
                .tracking(0.6),
        );
        let fp = Rect::new(freq.x + 8.0, freq.y + 26.0, freq.w - 16.0, freq.h - 44.0);
        let max_rt = rts.iter().copied().fold(0.0, f64::max).max(0.2) * 1.3;
        let rt_y = |rt: f64| fp.bottom() - fp.h * (rt / max_rt).clamp(0.0, 1.0) as f32;
        let damping = cx.value(pid(id::DAMPING));
        let bass_hz = cx.value(pid(id::BASS_FREQ)).max(20.0);
        let low_cut = cx.value(pid(id::LOW_CUT));
        let high_cut = cx.value(pid(id::HIGH_CUT));
        let n = 90;
        let pts: Vec<Point> = (0..=n)
            .map(|i| {
                let x = fp.x + fp.w * i as f32 / n as f32;
                let f = kit::Spectrum::freq_at(fp, x);
                // The one-pole splits' weights.
                let wl = 1.0 / (1.0 + (f / bass_hz).powi(2));
                let wh = if damping >= 19_500.0 {
                    0.0
                } else {
                    1.0 / (1.0 + (damping / f).powi(2))
                };
                let wm = (1.0 - wl - wh).max(0.0);
                let rt = 1.0 / (wl / rts[0] + wm / rts[1] + wh / rts[2]).max(1e-6);
                Point::new(x, rt_y(rt))
            })
            .collect();
        // The cuts shade what the wet signal loses.
        if low_cut > 20.5 {
            let x = kit::Spectrum::x_of(fp, low_cut);
            p.fill(
                Rect::new(fp.x, fp.y, x - fp.x, fp.h),
                th.ui.text.with_alpha(0.05),
            );
        }
        if high_cut < 19_500.0 {
            let x = kit::Spectrum::x_of(fp, high_cut);
            p.fill(
                Rect::new(x, fp.y, fp.right() - x, fp.h),
                th.ui.text.with_alpha(0.05),
            );
        }
        let mut fill = Path::polyline(&pts);
        fill.line_to(Point::new(fp.right(), fp.bottom()))
            .line_to(Point::new(fp.x, fp.bottom()))
            .close();
        p.fill_path(&fill, self.accent.with_alpha(0.14));
        p.stroke_path(&Path::polyline(&pts), 2.0, self.accent);
        // The shelves: the wet signal's level across the spectrum (±12 dB
        // over the middle of the plot).
        let (low_shelf, high_shelf) = (cx.value(pid(id::LOW_SHELF)), cx.value(pid(id::HIGH_SHELF)));
        if low_shelf.abs() >= 0.05 || high_shelf.abs() >= 0.05 {
            let rate = f64::from(cx.model.sample_rate().max(8_000));
            let pts: Vec<Point> = (0..=n)
                .map(|i| {
                    let x = fp.x + fp.w * i as f32 / n as f32;
                    let f = kit::Spectrum::freq_at(fp, x);
                    let mut d = 0.0;
                    if low_shelf.abs() >= 0.05 {
                        d += band_db(
                            &shelf(BandType::LowShelf, rev::LOW_SHELF_HZ, low_shelf),
                            rate,
                            f,
                        );
                    }
                    if high_shelf.abs() >= 0.05 {
                        d += band_db(
                            &shelf(BandType::HighShelf, rev::HIGH_SHELF_HZ, high_shelf),
                            rate,
                            f,
                        );
                    }
                    Point::new(x, fp.y + fp.h * (0.5 - d / 24.0).clamp(0.0, 1.0) as f32)
                })
                .collect();
            p.stroke_path(&Path::polyline(&pts), 1.4, th.ui.text.with_alpha(0.55));
        }
        for (f, t) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
            let x = kit::Spectrum::x_of(fp, f);
            p.vline(x, fp.y, fp.bottom(), th.device.grid);
            p.text(
                t,
                Rect::new(x - 15.0, fp.bottom() + 3.0, 30.0, 12.0),
                &label.center(),
            );
        }
        p.text(
            &format!("{:.1} s", max_rt / 1.3),
            Rect::new(fp.x + 2.0, rt_y(max_rt / 1.3) - 14.0, 60.0, 12.0),
            &label,
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (time, _) = Self::split(r);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if time.contains(pos) => {
                self.drag = Some((pos.x, cx.value(pid(id::DECAY))));
                edit.begin("Decay");
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
                let rt = from * 2f64.powf(f64::from(pos.x - x0) / 160.0);
                edit.set(pid(id::DECAY), rt.clamp(0.1, 20.0));
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
        rev::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::TYPE => {
                "Room, Hall, Plate (dense, no early reflections), Chamber, Ambience (short, close), Spring (two springs' chirp), Cathedral (the longest)"
            }
            id::PRE_SYNC => "The pre-delay a note value of the song's tempo",
            id::PRE_DIVISION => "The synced pre-delay's note value",
            id::HIGH_DECAY => "How long the highs ring, as a multiple of the decay",
            id::BASS_FREQ => "Below this the bass multiplier takes over",
            id::LOW_SHELF => "The tail's lows, round 250 Hz",
            id::HIGH_SHELF => "The tail's highs, above 4 kHz",
            id::SHIMMER => "The tail fed back pitch shifted: it climbs with every pass",
            id::SHIMMER_PITCH => "The shimmer's interval",
            id::SIZE => "How large the space is (its reflections and lines)",
            id::DAMPING => "Above this the reverb dies in a third of the time",
            id::BASS => "How much longer (or shorter) the bass rings",
            id::DIFFUSION => "How fast the reflections smear into a tail",
            id::MODULATION => "Slow movement inside the tail: no metallic ringing",
            id::BALANCE => "Early reflections against the tail",
            id::FREEZE => "Hold the tail for ever, let nothing new in",
            id::DUCKING => "The reverb steps back while you play",
            _ => return None,
        })
    }
}

fn shelf(kind: BandType, freq: f64, gain: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain,
        q: std::f64::consts::FRAC_1_SQRT_2,
        slope: 12.0,
    }
}
