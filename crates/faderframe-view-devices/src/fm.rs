//! The FM's face: the algorithm drawn (operators as boxes, modulators over
//! what they modulate, the feedback loop, the carriers going out; click
//! one to show it on the deck) — or, for the free matrix, its grid (drag a
//! cell up or down); the operator's envelope with its level now, tabs to
//! pick an operator; and the DX7 bank (Import DX7… reads a SysEx file,
//! a click on a voice loads it as one undo step). On the deck the chosen
//! operator, its envelope, every operator's level, the algorithm, pitch
//! and LFO depths, then the shared rows (`poly`).

use crate::kit::{self, Ctl, Ctx, Edit, Face, Panel, SMALL, Scale, Section};
use crate::poly::{self, Page, pid};
use crate::synth::smalls;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::fm::{self as fmd, algorithms, id, value};
use faderframe_session::Action;
use faderframe_ui_canvas::{
    Accent, Color, FileChoice, HostRequest, Paint, Painter, Path, Point, Rect, Size, TextStyle,
    ViewEvent,
};

const OPERATORS: [&str; 6] = [
    "OPERATOR 1",
    "OPERATOR 2",
    "OPERATOR 3",
    "OPERATOR 4",
    "OPERATOR 5",
    "OPERATOR 6",
];
const ENVELOPES: [&str; 6] = [
    "OP 1 ENVELOPE",
    "OP 2 ENVELOPE",
    "OP 3 ENVELOPE",
    "OP 4 ENVELOPE",
    "OP 5 ENVELOPE",
    "OP 6 ENVELOPE",
];

/// A matrix cell being dragged: from, to (`to` 6: the output row) and
/// where the drag started.
#[derive(Clone, Copy)]
struct CellDrag {
    from: usize,
    to: usize,
    y: f32,
    value: f64,
}

pub(crate) struct FmFace {
    accent: Color,
    pub(crate) page: Page,
    /// The operator the deck shows (0–5).
    pub(crate) op: usize,
    drag: Option<CellDrag>,
}

impl FmFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Instrument),
            page: Page::Sound,
            op: 0,
            drag: None,
        }
    }

    /// The display's boxes: algorithm, the operator's envelope, the bank.
    fn boxes(display: Rect) -> [Rect; 3] {
        let r = poly::content(display);
        let w = (r.w - 16.0) / 3.0;
        std::array::from_fn(|i| Rect::new(r.x + i as f32 * (w + 8.0), r.y, w, r.h))
    }

    /// Where an algorithm's operators sit in box `b`.
    pub(crate) fn op_boxes(b: Rect, a: &algorithms::Algorithm) -> [Rect; 6] {
        let l = algorithms::layout(a);
        let cols = l.iter().fold(0f32, |m, (_, c)| m.max(*c)) + 1.0;
        let rows = l.iter().map(|(r, _)| *r).max().unwrap_or(0) as f32 + 1.0;
        let area = Rect::new(b.x + 12.0, b.y + 24.0, b.w - 24.0, b.h - 44.0);
        // The boxes grow with the display (a small window keeps them
        // legible at their least).
        let cw = (area.w / cols).min(84.0);
        let rh = (area.h / rows).min(52.0);
        let (w, h) = ((cw * 0.6).clamp(26.0, 46.0), (rh * 0.6).clamp(18.0, 28.0));
        let x0 = area.x + (area.w - cw * cols) / 2.0;
        std::array::from_fn(|k| {
            let (row, col) = l[k];
            Rect::new(
                x0 + col * cw + (cw - w) / 2.0,
                area.bottom() - (row as f32 + 1.0) * rh + (rh - h) / 2.0,
                w,
                h,
            )
        })
    }

    /// The free matrix's cells in box `b`: per (from, to) and the output
    /// row (to = 6).
    fn cell(b: Rect, from: usize, to: usize) -> Rect {
        let size = ((b.w - 40.0) / 6.0).min((b.h - 40.0) / 7.5);
        let x0 = b.x + 28.0;
        let y0 = b.y + 26.0;
        let y = if to == 6 {
            y0 + 6.3 * size
        } else {
            y0 + to as f32 * size
        };
        Rect::new(x0 + from as f32 * size, y, size - 2.0, size - 2.0)
    }

    /// The operator tabs over the envelope box.
    fn op_tabs(b: Rect) -> [Rect; 6] {
        let w = 22.0;
        std::array::from_fn(|k| {
            Rect::new(
                b.right() - 8.0 - (6 - k) as f32 * (w + 3.0),
                b.y + 4.0,
                w,
                18.0,
            )
        })
    }

    fn import_button(b: Rect) -> Rect {
        Rect::new(b.right() - 104.0, b.y + 4.0, 96.0, 20.0)
    }

    /// A voice's row in the bank box (two columns of sixteen).
    fn voice_rect(b: Rect, i: usize) -> Rect {
        let col = (i / 16) as f32;
        let row = (i % 16) as f32;
        let h = ((b.h - 34.0) / 16.0).max(6.0);
        let w = (b.w - 20.0) / 2.0;
        Rect::new(b.x + 8.0 + col * (w + 4.0), b.y + 28.0 + row * h, w, h)
    }

    fn sound(&self, f: &poly::Frame) -> Panel {
        let k = self.op.min(5);
        let (mut s, mut c): (Vec<Section>, Vec<Ctl>) = (Vec::new(), Vec::new());
        s.extend(kit::sections(
            f.rows[0],
            &[(OPERATORS[k], 13.0), (ENVELOPES[k], 7.0)],
        ));
        s.extend(kit::sections(
            f.rows[1],
            &[("LEVELS", 7.0), ("ALGORITHM", 5.0), ("PITCH", 9.0)],
        ));
        let op = |field| id::op(k, field);
        let r = kit::inside(&s[0]);
        let mut widths = vec![SMALL.0; 12];
        widths[2] = 64.0;
        let slots = kit::row(Rect::new(r.x, r.y + 30.0, r.w, SMALL.1), &widths);
        let items: [(u32, &'static str, Option<Scale>, bool); 12] = [
            (op(id::RATIO), "RATIO", None, false),
            (op(id::FINE), "FINE", None, false),
            (op(id::FIXED), "", None, false),
            (op(id::FREQ), "FIXED HZ", Some(Scale::Log), false),
            (op(id::DETUNE), "DETUNE", None, true),
            (op(id::LEVEL), "LEVEL", None, false),
            (op(id::VELOCITY), "VELOCITY", None, false),
            (op(id::AMP_MOD), "AMP MOD", None, false),
            (op(id::RATE_SCALE), "RATE KEY", None, false),
            (op(id::KEY_BREAK), "BREAK KEY", None, false),
            (op(id::KEY_LEFT), "LEFT", None, true),
            (op(id::KEY_RIGHT), "RIGHT", None, true),
        ];
        for (slot, (i, label, scale, bipolar)) in slots.iter().zip(items) {
            if i == op(id::FIXED) {
                c.push(Ctl::toggle(
                    pid(i),
                    "Fixed",
                    Rect::new(slot.x, slot.y + 20.0, slot.w, 24.0),
                ));
                continue;
            }
            let mut ctl = Ctl::small(pid(i), label, *slot);
            if let Some(sc) = scale {
                ctl = ctl.scaled(sc);
            }
            if bipolar {
                ctl = ctl.bipolar();
            }
            c.push(ctl);
        }
        let ms = Some(Scale::Log);
        let r = kit::inside(&s[1]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 30.0, r.w, SMALL.1),
            &[
                (op(id::OP_ATTACK), "ATTACK", ms, false),
                (op(id::PEAK), "PEAK", None, false),
                (op(id::OP_DECAY), "DECAY", ms, false),
                (op(id::BREAK), "BREAK", None, false),
                (op(id::SLOPE), "SLOPE", ms, false),
                (op(id::OP_SUSTAIN), "SUSTAIN", None, false),
                (op(id::OP_RELEASE), "RELEASE", ms, false),
            ],
        );
        // Every operator's level.
        const LEVELS: [&str; 6] = ["OP 1", "OP 2", "OP 3", "OP 4", "OP 5", "OP 6"];
        let r = kit::inside(&s[2]);
        let items: Vec<(u32, &'static str, Option<Scale>, bool)> = (0..6)
            .map(|k| (id::op(k, id::LEVEL), LEVELS[k], None, false))
            .collect();
        smalls(&mut c, Rect::new(r.x, r.y + 30.0, r.w, SMALL.1), &items);
        // The algorithm, feedback, key sync and spread.
        let r = kit::inside(&s[3]);
        let half = (r.w - 12.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::ALGORITHM),
            "ALGORITHM",
            Rect::new(r.x + 4.0, r.y - 6.0, half, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::KEY_SYNC),
            "Key Sync",
            Rect::new(r.x + 8.0 + half, r.y + 8.0, half, 24.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[
                (id::FEEDBACK, "FEEDBACK", None, false),
                (id::SPREAD, "SPREAD", None, false),
            ],
        );
        // Pitch: transpose, LFO 1 depths, the pitch envelope.
        let r = kit::inside(&s[4]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 30.0, r.w, SMALL.1),
            &[
                (id::TRANSPOSE, "TRANSPOSE", None, true),
                (id::LFO_PITCH, "LFO PITCH", None, false),
                (id::LFO_AMP, "LFO AMP", None, false),
                (id::PITCH_START, "ENV START", None, true),
                (id::PITCH_TIME, "ENV TIME", ms, false),
                (id::PITCH_END, "ENV END", None, true),
                (id::PITCH_RELEASE, "ENV REL", ms, false),
            ],
        );
        poly::shape_row(f.rows[2], &mut s, &mut c);
        poly::panel(f, s, c)
    }

    fn algorithm(cx: &Ctx<'_>) -> usize {
        (cx.value(pid(id::ALGORITHM)).round().max(0.0) as usize).min(fmd::FREE_MATRIX)
    }

    fn paint_algorithm(&self, p: &mut dyn Painter, b: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        let n = Self::algorithm(cx);
        let Some(a) = algorithms::ALGORITHMS.get(n) else {
            poly::display_box(p, b, "FREE MATRIX", th);
            self.paint_matrix(p, b, cx);
            return;
        };
        poly::display_box(p, b, &format!("ALGORITHM {}", n + 1), th);
        let ops = Self::op_boxes(b, a);
        let line = th.ui.text_dim;
        // Modulations: from the top of the modulated box to the bottom of
        // its modulator.
        for to in 0..6 {
            for from in 0..6 {
                if a.modulates(from, to) {
                    let (f, t) = (ops[from], ops[to]);
                    p.stroke_path(
                        &Path::polyline(&[
                            Point::new(f.x + f.w / 2.0, f.bottom()),
                            Point::new(t.x + t.w / 2.0, t.y),
                        ]),
                        1.2,
                        line,
                    );
                }
            }
        }
        // The carriers' outputs join a bus under them.
        let bus = b.bottom() - 12.0;
        let carriers: Vec<Rect> = (0..6)
            .filter(|k| a.is_carrier(*k))
            .map(|k| ops[k])
            .collect();
        if let (Some(first), Some(last)) = (carriers.first(), carriers.last()) {
            p.hline(first.x + first.w / 2.0, last.x + last.w / 2.0, bus, line);
            for c in &carriers {
                p.vline(c.x + c.w / 2.0, c.bottom(), bus, line);
            }
        }
        // The feedback loop at the side of its operator.
        let (from, to) = a.feedback;
        let fb = cx.value(pid(id::FEEDBACK));
        let (f, t) = (ops[from], ops[to]);
        let color = self.accent.with_alpha(if fb > 0.001 { 0.9 } else { 0.35 });
        let x = f.right().max(t.right()) + 6.0;
        p.stroke_path(
            &Path::polyline(&[
                Point::new(f.right(), f.y + f.h / 2.0),
                Point::new(x, f.y + f.h / 2.0),
                Point::new(x, t.y - 4.0),
                Point::new(t.x + t.w / 2.0, t.y - 4.0),
                Point::new(t.x + t.w / 2.0, t.y),
            ]),
            1.2,
            color,
        );
        for (k, r) in ops.iter().enumerate() {
            let level = cx.value(pid(id::op(k, id::LEVEL)));
            let env = cx.published(value::OP_ENV[k]);
            let selected = k == self.op;
            p.fill_rounded(
                *r,
                3.0,
                &Paint::Solid(if a.is_carrier(k) {
                    self.accent.with_alpha(0.25 + 0.5 * env)
                } else {
                    th.ui.surface_alt.mix(self.accent, 0.4 * env)
                }),
            );
            p.stroke_rounded(
                *r,
                3.0,
                if selected { 2.0 } else { 1.0 },
                if selected { th.ui.text } else { th.ui.border },
            );
            p.text(
                &format!("{}", k + 1),
                *r,
                &TextStyle::new(
                    th.fonts.small,
                    if level > 0.5 {
                        th.ui.text
                    } else {
                        th.ui.text_faint
                    },
                )
                .bold()
                .center(),
            );
        }
    }

    fn paint_matrix(&self, p: &mut dyn Painter, b: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_dim).center();
        for from in 0..6 {
            let top = Self::cell(b, from, 0);
            p.text(
                &format!("{}", from + 1),
                Rect::new(top.x, top.y - 14.0, top.w, 12.0),
                &label,
            );
            for to in 0..7 {
                let r = Self::cell(b, from, to);
                let v = if to == 6 {
                    cx.value(pid(id::out(from)))
                } else {
                    cx.value(pid(id::matrix(from, to)))
                } as f32;
                p.fill(r, th.ui.surface_alt.mix(self.accent, v.clamp(0.0, 1.0)));
                if from == to {
                    p.stroke_rounded(r, 1.0, 1.0, th.ui.border);
                }
            }
        }
        for to in 0..6 {
            let r = Self::cell(b, 0, to);
            p.text(
                &format!("→{}", to + 1),
                Rect::new(b.x + 2.0, r.y, 24.0, r.h),
                &label,
            );
        }
        let r = Self::cell(b, 0, 6);
        p.text("out", Rect::new(b.x + 2.0, r.y, 24.0, r.h), &label);
    }

    fn paint_envelope(&self, p: &mut dyn Painter, b: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        poly::display_box(p, b, ENVELOPES[self.op.min(5)], th);
        for (k, r) in Self::op_tabs(b).iter().enumerate() {
            let on = k == self.op;
            p.fill_rounded(
                *r,
                3.0,
                &Paint::Solid(if on {
                    self.accent.with_alpha(0.4)
                } else {
                    th.ui.surface_alt
                }),
            );
            p.text(
                &format!("{}", k + 1),
                *r,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
            );
        }
        let v = |f: u32| cx.value(pid(id::op(self.op, f)));
        let plot = Rect::new(b.x + 10.0, b.y + 30.0, b.w - 20.0, b.h - 40.0);
        let w = |ms: f64| (ms.max(0.5) / 30_000.0).sqrt() as f32;
        let (a, d, s, r) = (
            w(v(id::OP_ATTACK)),
            w(v(id::OP_DECAY)),
            w(v(id::SLOPE)),
            w(v(id::OP_RELEASE)),
        );
        let hold = 0.25;
        let total = a + d + s + hold + r;
        let x = |t: f32| plot.x + plot.w * t / total;
        let y = |level: f64| plot.bottom() - plot.h * (level / 99.0).clamp(0.0, 1.0) as f32;
        let pts = [
            Point::new(x(0.0), y(0.0)),
            Point::new(x(a), y(v(id::PEAK))),
            Point::new(x(a + d), y(v(id::BREAK))),
            Point::new(x(a + d + s), y(v(id::OP_SUSTAIN))),
            Point::new(x(a + d + s + hold), y(v(id::OP_SUSTAIN))),
            Point::new(x(total), y(0.0)),
        ];
        let mut fill = Path::polyline(&pts);
        fill.close();
        p.fill_path(&fill, self.accent.with_alpha(0.12));
        p.stroke_path(&Path::polyline(&pts), 1.8, self.accent);
        let now = cx.published(value::OP_ENV[self.op.min(5)]);
        if now > 0.001 {
            // The envelope's amplitude now, as a level (¾ dB a step).
            let level = 99.0 + 20.0 * f64::from(now).log10() / 0.75;
            let yy = y(level);
            p.hline(plot.x, plot.right(), yy, self.accent.with_alpha(0.35));
            p.circle(Point::new(plot.x + 4.0, yy), 3.5, self.accent);
        }
    }

    fn paint_bank(&self, p: &mut dyn Painter, b: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        poly::display_box(p, b, "DX7 VOICES", th);
        kit::display_button(p, Self::import_button(b), "Import DX7…", self.accent, th);
        let Some(bank) = cx.model.dx7_bank(cx.plugin) else {
            p.text(
                "Read a DX7 SysEx file (a bank of 32 voices, or one):\nclick a voice to load it.",
                Rect::new(b.x + 10.0, b.y + 34.0, b.w - 20.0, 40.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_faint),
            );
            return;
        };
        for (i, v) in bank.voices.iter().enumerate().take(32) {
            let r = Self::voice_rect(b, i);
            if bank.loaded == Some(i) {
                p.fill_rounded(r, 2.0, &Paint::Solid(self.accent.with_alpha(0.3)));
            }
            p.text(
                &format!("{:>2} {}", i + 1, v.name),
                Rect::new(r.x + 3.0, r.y, r.w - 3.0, r.h),
                &TextStyle::new(th.fonts.tiny, th.ui.text),
            );
        }
    }
}

impl Face for FmFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(poly::MIN_SIZE.0, poly::MIN_SIZE.1)
    }

    fn panel(&self, size: Size) -> Panel {
        let f = poly::frame(size);
        match self.page {
            Page::Sound => self.sound(&f),
            Page::Modulation => poly::modulation_page(&f),
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        poly::paint_tabs(p, r, "OPERATORS", self.page, self.accent, cx.theme);
        let [a, e, b] = Self::boxes(r);
        self.paint_algorithm(p, a, cx);
        self.paint_envelope(p, e, cx);
        self.paint_bank(p, b, cx);
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let [a, e, b] = Self::boxes(r);
        match *ev {
            ViewEvent::PointerDown { pos, clicks, .. } => {
                if let Some(page) = poly::tab_at(r, pos) {
                    self.page = page;
                    return true;
                }
                if let Some(k) = Self::op_tabs(e).iter().position(|t| t.contains(pos)) {
                    self.op = k;
                    return true;
                }
                if a.contains(pos) {
                    let n = Self::algorithm(cx);
                    if let Some(alg) = algorithms::ALGORITHMS.get(n) {
                        if let Some(k) = Self::op_boxes(a, alg).iter().position(|o| o.contains(pos))
                        {
                            self.op = k;
                        }
                        return true;
                    }
                    for from in 0..6 {
                        for to in 0..7 {
                            if !Self::cell(a, from, to).contains(pos) {
                                continue;
                            }
                            let pid_ = if to == 6 {
                                id::out(from)
                            } else {
                                id::matrix(from, to)
                            };
                            let now = cx.value(pid(pid_));
                            if clicks >= 2 {
                                edit.set_once(pid(pid_), if now > 0.0 { 0.0 } else { 0.5 });
                                return true;
                            }
                            edit.begin("FM Matrix");
                            self.drag = Some(CellDrag {
                                from,
                                to,
                                y: pos.y,
                                value: now,
                            });
                            return true;
                        }
                    }
                    return true;
                }
                if Self::import_button(b).contains(pos) {
                    let plugin = cx.plugin;
                    edit.cx.request(HostRequest::ChooseFiles {
                        choice: FileChoice::Open {
                            title: "Import DX7 Voices".into(),
                            filters: vec![
                                ("DX7 SysEx".into(), vec!["*.syx".into(), "*.SYX".into()]),
                                ("All files".into(), vec!["*".into()]),
                            ],
                        },
                        commit: Box::new(move |files| {
                            let path = files.into_iter().next()?;
                            Some(Action::ImportDx7 { plugin, path })
                        }),
                    });
                    return true;
                }
                if let Some(bank) = cx.model.dx7_bank(cx.plugin)
                    && let Some(i) = (0..bank.voices.len().min(32))
                        .find(|i| Self::voice_rect(b, *i).contains(pos))
                {
                    edit.cx.emit(Action::LoadDx7Voice {
                        plugin: cx.plugin,
                        index: i,
                    });
                    return true;
                }
                false
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } => {
                let Some(d) = self.drag else { return false };
                let v = (d.value + f64::from(d.y - pos.y) / 120.0).clamp(0.0, 1.0);
                let pid_ = if d.to == 6 {
                    id::out(d.from)
                } else {
                    id::matrix(d.from, d.to)
                };
                edit.set(pid(pid_), v);
                true
            }
            ViewEvent::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    edit.end();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        fmd::format(id, v)
    }

    fn tip(&self, p: ParameterId) -> Option<&'static str> {
        if p.0 < faderframe_plugin_host::devices::poly::COMMON {
            return poly::tip(p);
        }
        Some(match p.0 {
            id::ALGORITHM => {
                "How the operators are wired (the classic 32), or Matrix: any into any (drag the grid's cells)"
            }
            id::FEEDBACK => {
                "The feedback loop the algorithm draws: its operator into its own phase"
            }
            id::KEY_SYNC => "Every note starts the operators from zero (off: free-running)",
            id::SPREAD => "The carriers alternately left and right",
            id::LFO_PITCH => "LFO 1's vibrato depth",
            id::LFO_AMP => "LFO 1's tremolo depth, by each operator's Amp Mod",
            id::PITCH_START => "The pitch envelope: where a note starts, gliding to the key",
            id::PITCH_END => "Where the pitch goes after the key is let go",
            x if (id::OPS..id::MATRIX).contains(&x) => match (x - id::OPS) % id::OP_FIELDS {
                id::RATIO => "The operator's frequency to the key's (½, 1–31), times 1 + Fine",
                id::FIXED => "A fixed frequency, whatever the key",
                id::LEVEL => {
                    "Output level 0–99 (¾ dB a step): a carrier's loudness, a modulator's brightness"
                }
                id::VELOCITY => "How much playing softer takes away",
                id::AMP_MOD => "How much LFO 1's tremolo reaches it",
                id::PEAK | id::BREAK | id::OP_SUSTAIN => {
                    "The envelope's levels: after the attack, after the decay, held"
                }
                id::SLOPE => "The second decay's time, from the break level to the sustain",
                id::RATE_SCALE => "Envelopes faster up the keyboard",
                id::KEY_BREAK => "Where keyboard level scaling turns",
                id::KEY_LEFT | id::KEY_RIGHT => {
                    "Level scaling either side of the break key: ±24 dB an octave at full"
                }
                _ => return None,
            },
            _ => return None,
        })
    }
}
