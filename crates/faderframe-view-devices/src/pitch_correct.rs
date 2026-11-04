//! Pitch Correct's face: the last four seconds of the pitch sung (dim) and
//! the pitch played (the accent) over the semitones, those the target
//! allows now shaded and named; the target note, how far the voice is
//! from it and how much is corrected; the controls.

use crate::kit::{self, Ctl, Ctx, Face, Meter, MeterKind, Panel, SMALL, SWITCH_H};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::pitch_correct::{self as pc, NOTE_LABELS, id, value};
use faderframe_ui_canvas::{Accent, Color, Painter, Point, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// Seconds shown and samples a second.
const SECONDS: f32 = 4.0;
const RATE: f32 = 60.0;
/// Semitones shown either side of the centre.
const SPAN: f64 = 6.5;

pub(crate) struct PitchCorrectFace {
    accent: Color,
    /// (sung, played, target) per sample, oldest first; −1 = none.
    history: Vec<[f32; 3]>,
    head: usize,
    since: f32,
    /// The note the graph is centred on (follows the voice slowly).
    centre: f64,
}

impl PitchCorrectFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Modulation),
            history: vec![[-1.0; 3]; (SECONDS * RATE) as usize],
            head: 0,
            since: 0.0,
            centre: 60.0,
        }
    }

    /// The graph and the readouts' column.
    pub(crate) fn split(r: Rect) -> (Rect, Rect) {
        let side_w = 200.0;
        let graph = Rect::new(r.x + 8.0, r.y + 8.0, r.w - side_w - 24.0, r.h - 16.0);
        let side = Rect::new(graph.right() + 12.0, r.y + 8.0, side_w, r.h - 16.0);
        (graph, side)
    }

    fn record(&mut self, cx: &Ctx<'_>) {
        self.since += cx.dt;
        let now = [
            cx.published(value::INPUT),
            cx.published(value::OUTPUT),
            cx.published(value::TARGET),
        ];
        let step = 1.0 / RATE;
        while self.since >= step {
            self.since -= step;
            self.history[self.head] = now;
            self.head = (self.head + 1) % self.history.len();
        }
        // Centred where the voice is, gliding there.
        let at = [now[2], now[0]].into_iter().find(|v| *v > 0.0);
        if let Some(v) = at {
            let v = f64::from(v);
            if (v - self.centre).abs() > SPAN {
                self.centre = v.round();
            } else {
                self.centre += 0.1 * (v - self.centre);
            }
        }
    }
}

impl Face for PitchCorrectFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1060.0, 500.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 196.0, 46.0);
        let s = kit::sections(
            deck,
            &[
                ("TARGET", 3.2),
                ("NOTES", 4.4),
                ("CORRECTION", 3.0),
                ("VOICE", 4.0),
                ("OUTPUT", 1.4),
            ],
        );
        let mut c = Vec::new();
        // What the notes are corrected toward.
        let r = kit::inside(&s[0]);
        c.push(Ctl::choice(
            pid(id::TARGET),
            "TARGET",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, 38.0),
        ));
        let half = (r.w - 16.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::ROOT),
            "KEY",
            Rect::new(r.x + 4.0, r.y + 52.0, half * 0.7, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::SCALE),
            "SCALE",
            Rect::new(
                r.x + 12.0 + half * 0.7,
                r.y + 52.0,
                r.w - 16.0 - half * 0.7,
                38.0,
            ),
        ));
        // The notes of Custom, two rows of six.
        let r = kit::inside(&s[1]);
        let w = ((r.w - 10.0) / 6.0 - 4.0).min(46.0);
        for row in 0..2 {
            let line = Rect::new(r.x, r.y + 14.0 + row as f32 * 34.0, r.w, SWITCH_H);
            for (k, slot) in kit::spread(line, 6, w).into_iter().enumerate() {
                let n = (row * 6 + k) as u32;
                c.push(Ctl::toggle(pid(id::note(n)), NOTE_LABELS[n as usize], slot));
            }
        }
        // How it corrects.
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y + 6.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::SPEED), "SPEED", k[0]));
        c.push(Ctl::small(pid(id::HUMANIZE), "HUMANIZE", k[1]));
        c.push(Ctl::small(pid(id::VIBRATO), "VIBRATO", k[2]));
        // The voice: range, transpose, formants.
        let r = kit::inside(&s[3]);
        let k = kit::row(
            Rect::new(r.x, r.y, r.w * 0.58, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::TRANSPOSE), "TRANSPOSE", k[0]).bipolar());
        c.push(Ctl::small(pid(id::FORMANT), "FORMANT", k[1]).bipolar());
        let col = Rect::new(r.x + r.w * 0.6, r.y, r.w * 0.4, r.h);
        c.push(Ctl::choice(
            pid(id::RANGE),
            "RANGE",
            Rect::new(col.x, col.y + 2.0, col.w - 4.0, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::PRESERVE_FORMANTS),
            "Keep Formants",
            Rect::new(col.x, col.y + 52.0, col.w - 4.0, SWITCH_H),
        ));
        let r = kit::inside(&s[4]);
        c.push(Ctl::small(
            pid(id::MIX),
            "MIX",
            kit::at(r, (r.w - SMALL.0) / 2.0, 6.0, SMALL.0, SMALL.1),
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
        self.record(cx);
        let th = cx.theme;
        let (graph, side) = Self::split(r);
        let top = self.centre + SPAN;
        let y_of = |note: f64| graph.y + ((top - note) / (2.0 * SPAN)) as f32 * graph.h;
        let row_h = graph.h / (2.0 * SPAN) as f32;
        let allowed = cx.published(value::ALLOWED) as u16;
        p.push_clip(graph);
        // The semitones: those allowed now shaded and named.
        let lo = (self.centre - SPAN).floor() as i32;
        let hi = (self.centre + SPAN).ceil() as i32;
        for n in lo..=hi {
            let y = y_of(f64::from(n));
            let on = allowed & (1 << n.rem_euclid(12)) != 0;
            if on {
                p.fill(
                    Rect::new(graph.x, y - row_h / 2.0, graph.w, row_h),
                    self.accent.with_alpha(0.08),
                );
            }
            p.hline(
                graph.x,
                graph.right(),
                y,
                if on {
                    th.device.grid_strong
                } else {
                    th.device.grid
                },
            );
            if on || n.rem_euclid(12) == 0 {
                p.text(
                    &note_name(n),
                    Rect::new(graph.x + 4.0, y - 7.0, 40.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, if on { th.ui.text } else { th.ui.text_dim }),
                );
            }
        }
        // Sung, then played, oldest at the left.
        let n = self.history.len();
        let x_of = |i: usize| graph.x + 44.0 + (graph.w - 48.0) * i as f32 / (n - 1) as f32;
        for (field, color, width) in [
            (2usize, th.ui.text_dim.with_alpha(0.6), 1.0),
            (0, th.device.pre, 1.5),
            (1, self.accent, 2.0),
        ] {
            let mut last: Option<Point> = None;
            for i in 0..n {
                let v = self.history[(self.head + i) % n][field];
                if v <= 0.0 {
                    last = None;
                    continue;
                }
                let pt = Point::new(x_of(i), y_of(f64::from(v)));
                if let Some(a) = last
                    && (a.y - pt.y).abs() < graph.h * 0.4
                {
                    p.line(a, pt, width, color);
                }
                last = Some(pt);
            }
        }
        p.pop_clip();
        // The readouts.
        let target = cx.published(value::TARGET);
        let input = cx.published(value::INPUT);
        let note = if target > 0.0 {
            let k = f64::from(target).round();
            let cents = ((f64::from(target) - k) * 100.0).round() as i32;
            if cents == 0 {
                note_name(k as i32)
            } else {
                format!("{} {cents:+}", note_name(k as i32)).replace('-', "−")
            }
        } else {
            "—".into()
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y, side.w, 56.0),
            "TARGET NOTE",
            &note,
            self.accent,
            th,
        );
        let off = if target > 0.0 && input > 0.0 {
            cents(f64::from(input - target))
        } else {
            "—".into()
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y + 64.0, side.w, 46.0),
            "SUNG OFF BY",
            &off,
            th.ui.text,
            th,
        );
        let corrected = if input > 0.0 {
            cents(f64::from(cx.published(value::CORRECTION)))
        } else {
            "—".into()
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y + 118.0, side.w, 46.0),
            "CORRECTION",
            &corrected,
            th.ui.text,
            th,
        );
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        let latency = pc::latency(&cx.tap.params, rate);
        p.text(
            &format!("latency {:.1} ms", f64::from(latency) / rate * 1000.0),
            Rect::new(side.x, side.y + 172.0, side.w, 14.0),
            &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_dim).center(),
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        pc::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::TARGET => {
                "The notes the voice is pulled to: the key track's key, the chord \
                 track's chord, the key set here, every semitone, the notes ticked \
                 here, or the notes a MIDI track playing this track holds"
            }
            id::ROOT | id::SCALE => "The key for Scale (and where the key or chord track has none)",
            id::SPEED => {
                "How long the correction takes to get there (0: at once, the hard-tuned sound)"
            }
            id::HUMANIZE => "Notes held longer are corrected more slowly (short ones at the speed)",
            id::VIBRATO => {
                "How much of the voice's own vibrato and scoops is kept (100 %: as sung)"
            }
            id::TRANSPOSE => "Move the voice by semitones",
            id::FORMANT => "Move the formants (the voice's size) by semitones",
            id::PRESERVE_FORMANTS => "Keep the formants where they are as the pitch moves",
            id::RANGE => "The lowest note followed: a lower range takes longer (the latency)",
            id::MIX => "The corrected voice against the voice as it came (delayed alike)",
            p if (id::note(0)..id::note(12)).contains(&p) => "A note Custom pulls to",
            _ => return None,
        })
    }
}

/// Semitones as signed cents.
fn cents(semitones: f64) -> String {
    let c = (semitones * 100.0).round() as i32;
    if c == 0 {
        "0 ¢".into()
    } else {
        format!("{c:+} ¢").replace('-', "−")
    }
}
