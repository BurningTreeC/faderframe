//! The Arpeggiator's face: the pattern the held keys make, sixteen steps
//! as a small piano roll (the step playing lit), over a keyboard showing
//! what is held and what sounds.

use crate::keys;
use crate::kit::{self, Ctl, Ctx, Face, KNOB, Panel, SMALL, SWITCH_H};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::arpeggiator::{self as arp, id, value};
use faderframe_plugin_host::dsp::lfo::division_name;
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// Steps the display shows.
const STEPS: usize = 16;

pub(crate) struct ArpeggiatorFace {
    accent: Color,
}

impl ArpeggiatorFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Midi),
        }
    }
}

impl Face for ArpeggiatorFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(820.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, _) = kit::frame(size, 170.0, 0.0);
        let s = kit::sections(deck, &[("PATTERN", 4.4), ("TIMING", 4.4), ("PLAY", 2.8)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::choice(
            pid(id::MODE),
            "MODE",
            Rect::new(r.x + 4.0, r.y + 4.0, r.w * 0.42, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x + r.w * 0.44, r.y, r.w * 0.56, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(pid(id::OCTAVES), "OCTAVES", k[0]));
        c.push(Ctl::small(pid(id::REPEATS), "REPEATS", k[1]));
        let r = kit::inside(&s[1]);
        c.push(Ctl::choice(
            pid(id::RATE),
            "RATE",
            Rect::new(r.x + 4.0, r.y + 4.0, r.w * 0.3, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x + r.w * 0.32, r.y, r.w * 0.68, KNOB.1),
            &[KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(pid(id::GATE), "GATE", k[0]));
        c.push(Ctl::knob(pid(id::SWING), "SWING", k[1]));
        let r = kit::inside(&s[2]);
        c.push(Ctl::small(
            pid(id::VELOCITY),
            "VELOCITY",
            Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            pid(id::HOLD),
            "Hold",
            Rect::new(r.x + 8.0, r.bottom() - SWITCH_H, r.w - 16.0, SWITCH_H),
        ));
        Panel {
            display: Some(display),
            sections: s,
            controls: c,
            meters: Vec::new(),
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        p.fill_rounded(r, 4.0, &Paint::Solid(th.device.display));
        let held: Vec<u8> = (0..arp::HELD_SHOWN)
            .map(|i| cx.tap.value(value::HELD + i))
            .filter(|v| *v >= 0.0)
            .map(|v| v as u8)
            .collect();
        let mode = (cx.value(pid(id::MODE)).round().max(0.0) as usize).min(arp::MODES.len() - 1);
        let octaves = cx.value(pid(id::OCTAVES)).round().clamp(1.0, 4.0) as usize;
        let repeats = cx.value(pid(id::REPEATS)).round().clamp(1.0, 4.0) as u64;
        let gate = cx.value(pid(id::GATE)).clamp(0.05, 1.5) as f32;
        let playing = cx.tap.value(value::NOTE);
        let steps = cx.tap.value(value::STEP).max(0.0) as u64;
        let seq = arp::sequence(mode, &held, octaves);
        // Readouts on the right.
        let side_w = 150.0;
        let side = Rect::new(r.right() - side_w - 8.0, r.y + 8.0, side_w, r.h - 16.0);
        kit::readout(
            p,
            Rect::new(side.x, side.y, side.w, 46.0),
            "MODE",
            arp::MODES[mode],
            self.accent,
            th,
        );
        let rate = division_name(cx.value(pid(id::RATE)).round().max(0.0) as usize);
        kit::readout(
            p,
            Rect::new(side.x, side.y + 54.0, side.w, 46.0),
            "RATE",
            rate,
            self.accent,
            th,
        );
        kit::readout(
            p,
            Rect::new(side.x, side.y + 108.0, side.w, 46.0),
            "NOW",
            &if playing >= 0.0 {
                note_name(playing as i32)
            } else {
                "—".into()
            },
            self.accent,
            th,
        );
        // The keyboard: held keys, the note sounding.
        let kb = Rect::new(r.x + 8.0, r.bottom() - 54.0, side.x - r.x - 18.0, 46.0);
        let (low, high) = keys::span(if seq.is_empty() { &held } else { &seq }, 2);
        let accent = self.accent;
        keys::keyboard(
            p,
            kb,
            low,
            high,
            &|k| {
                if playing >= 0.0 && k == playing as u8 {
                    Some(accent)
                } else if held.contains(&k) {
                    Some(accent.with_alpha(0.45).mix(th.device.display, 0.2))
                } else if seq.contains(&k) {
                    Some(accent.with_alpha(0.18).mix(th.ui.text, 0.6))
                } else {
                    None
                }
            },
            th,
        );
        // The pattern.
        // The pitches' names in a gutter on the left.
        let gutter = 30.0;
        let grid = Rect::new(
            r.x + 8.0 + gutter,
            r.y + 8.0,
            kb.w - gutter,
            kb.y - r.y - 18.0,
        );
        p.fill_rounded(grid, 3.0, &Paint::Solid(th.device.display.darken(0.15)));
        let cell = grid.w / STEPS as f32;
        for k in 0..=STEPS {
            let x = grid.x + k as f32 * cell;
            let c = if k % 4 == 0 {
                th.device.grid_strong
            } else {
                th.device.grid
            };
            p.vline(x, grid.y, grid.bottom(), c);
        }
        if seq.is_empty() {
            p.text(
                "Hold some keys: their pattern shows here",
                grid,
                &TextStyle::new(th.fonts.normal, th.ui.text_faint).center(),
            );
            return;
        }
        let lo = *seq.iter().min().unwrap_or(&60);
        let hi = *seq.iter().max().unwrap_or(&60);
        let rows = f32::from(hi - lo + 1).max(1.0);
        let row_h = ((grid.h - 8.0) / rows).min(16.0);
        let top = grid.y + (grid.h - row_h * rows) / 2.0;
        let y_of = |k: u8| top + f32::from(hi - k) * row_h;
        let current = (playing >= 0.0).then(|| (steps.saturating_sub(1) % STEPS as u64) as usize);
        if let Some(c) = current {
            p.fill(
                Rect::new(grid.x + c as f32 * cell, grid.y, cell, grid.h),
                self.accent.with_alpha(0.12),
            );
        }
        let n_held = held.len().max(1);
        for k in 0..STEPS {
            let pos = k as u64 / repeats;
            let notes: Vec<u8> = if mode == 8 {
                // Chord: all of it, an octave up each step.
                let group = (pos as usize) % (seq.len() / n_held).max(1);
                seq.iter()
                    .skip(group * n_held)
                    .take(n_held)
                    .copied()
                    .collect()
            } else {
                match arp::order_index(mode, seq.len(), pos) {
                    Some(i) => vec![seq[i]],
                    None => Vec::new(),
                }
            };
            let x = grid.x + k as f32 * cell;
            if notes.is_empty() {
                p.text(
                    "?",
                    Rect::new(x, grid.y, cell, grid.h),
                    &TextStyle::new(th.fonts.large, th.ui.text_faint).center(),
                );
                continue;
            }
            for key in notes {
                let bar = Rect::new(
                    x + 1.5,
                    y_of(key) + 1.0,
                    (cell * gate.min(1.0) - 3.0).max(3.0),
                    (row_h - 2.0).max(2.0),
                );
                let lit = current == Some(k);
                let c = if lit {
                    self.accent
                } else {
                    self.accent.with_alpha(0.55)
                };
                p.fill_rounded(bar, 2.0, &Paint::Solid(c));
                if gate > 1.0 {
                    // Legato: the note runs into the next step.
                    p.fill(
                        Rect::new(
                            bar.right(),
                            bar.y + bar.h * 0.35,
                            cell * (gate - 1.0),
                            bar.h * 0.3,
                        ),
                        c.with_alpha(0.5),
                    );
                }
            }
        }
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_dim).right();
        p.text(
            &note_name(i32::from(hi)),
            Rect::new(grid.x - gutter, y_of(hi), gutter - 4.0, row_h.max(12.0)),
            &label,
        );
        if lo != hi {
            p.text(
                &note_name(i32::from(lo)),
                Rect::new(grid.x - gutter, y_of(lo), gutter - 4.0, row_h.max(12.0)),
                &label,
            );
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        arp::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => "The order the held keys play in (Chord: all of them each step)",
            id::RATE => "The step length, a division of the song's tempo",
            id::GATE => "How long each note lasts, a share of the step (past 100 %: legato)",
            id::OCTAVES => "Over how many octaves the pattern climbs",
            id::SWING => "Every second step later (66 %: a triplet shuffle)",
            id::VELOCITY => "A fixed velocity, or as played (all the way down)",
            id::HOLD => "Keep playing the chord after the keys are let go, until a new one",
            id::REPEATS => "Each note played this many times",
            _ => return None,
        })
    }
}
