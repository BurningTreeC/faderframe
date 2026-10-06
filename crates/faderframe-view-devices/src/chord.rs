//! The Chord's face: the chord played last (or, before any, what C4 would
//! become) on a keyboard, its name, and what the mode builds it from —
//! the intervals, the key, or the chord track's chord now.

use crate::keys;
use crate::kit::{self, Ctl, Ctx, Face, KNOB, Panel, SMALL, SWITCH_H};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::chord::{self as ch, id, value};
use faderframe_project::harmony::{Chord, Key, Scale};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct ChordFace {
    accent: Color,
}

impl ChordFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Midi),
        }
    }

    /// The key the scale modes use now: the key track's at the playhead
    /// (following it) or the device's own.
    fn key_now(cx: &Ctx<'_>, follow: u32, root: u32, scale: u32) -> (Key, bool) {
        let project = cx.model.project();
        if cx.on(pid(follow))
            && let Some(k) = project.key_at(cx.model.playhead())
        {
            return (k, true);
        }
        let r = (cx.value(pid(root)).round().max(0.0) as u8) % 12;
        let s =
            Scale::ALL[(cx.value(pid(scale)).round().max(0.0) as usize).min(Scale::ALL.len() - 1)];
        (Key::new(r, s), false)
    }

    /// What C4 becomes (the display before a note is played).
    fn preview(cx: &Ctx<'_>) -> Vec<u8> {
        let mode = cx.value(pid(id::MODE)).round() as i32;
        let c4 = 60i32;
        let mut out: Vec<i32> = match mode {
            1 | 2 => {
                let (key, _) = Self::key_now(cx, id::FOLLOW_KEY, id::ROOT, id::SCALE);
                if key.scale.intervals().len() == 7 {
                    let base = key.snap(c4);
                    let mut v = vec![base, key.step(base, 2), key.step(base, 4)];
                    if mode == 2 {
                        v.push(key.step(base, 6));
                    }
                    v
                } else {
                    vec![c4]
                }
            }
            3 => cx
                .model
                .project()
                .chord_at(cx.model.playhead())
                .map_or(vec![c4], |c| c.chord.voicing(c4)),
            _ => std::iter::once(c4)
                .chain((0..ch::SHIFTS).filter_map(|i| {
                    let s = cx.value(pid(id::SHIFT + i as u32)).round() as i32;
                    (s != 0).then_some(c4 + s)
                }))
                .collect(),
        };
        out.sort_unstable();
        out.dedup();
        out.into_iter()
            .filter(|k| (0..=127).contains(k))
            .map(|k| k as u8)
            .collect()
    }
}

impl Face for ChordFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(900.0, 470.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, _) = kit::frame(size, 170.0, 0.0);
        let s = kit::sections(
            deck,
            &[
                ("CHORD", 2.4),
                ("INTERVALS", 6.0),
                ("KEY", 3.0),
                ("STRUM", 3.6),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::choice(
            pid(id::MODE),
            "MODE",
            Rect::new(r.x + 4.0, r.y + 4.0, r.w - 8.0, 38.0),
        ));
        c.push(Ctl::small(
            pid(id::VELOCITY),
            "ADDED VEL",
            Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y + 48.0, SMALL.0, SMALL.1).inset_xy(4.0, 6.0),
        ));
        let r = kit::inside(&s[1]);
        let labels = ["1", "2", "3", "4", "5"];
        let k = kit::row(
            Rect::new(r.x, r.y + 16.0, r.w, SMALL.1),
            &[SMALL.0; ch::SHIFTS],
        );
        for (i, rect) in k.into_iter().enumerate() {
            c.push(Ctl::small(pid(id::SHIFT + i as u32), labels[i], rect).bipolar());
        }
        let r = kit::inside(&s[2]);
        c.push(Ctl::toggle(
            pid(id::FOLLOW_KEY),
            "Key Track",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::ROOT),
            "KEY",
            Rect::new(r.x + 4.0, r.y + 32.0, r.w * 0.4, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::SCALE),
            "SCALE",
            Rect::new(r.x + r.w * 0.4 + 8.0, r.y + 32.0, r.w * 0.6 - 12.0, 38.0),
        ));
        let r = kit::inside(&s[3]);
        let k = kit::row(Rect::new(r.x, r.y, r.w * 0.55, KNOB.1), &[KNOB.0]);
        c.push(Ctl::knob(pid(id::STRUM), "STRUM", k[0]));
        c.push(Ctl::choice(
            pid(id::DIRECTION),
            "FROM",
            Rect::new(r.x + r.w * 0.56, r.y + 16.0, r.w * 0.44 - 6.0, 38.0),
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
        let played: Vec<u8> = (0..ch::NOTES_SHOWN)
            .map(|i| cx.tap.value(value::NOTES + i))
            .filter(|v| *v >= 0.0)
            .map(|v| v as u8)
            .collect();
        let live = !played.is_empty();
        let notes = if live { played } else { Self::preview(cx) };
        let mode = (cx.value(pid(id::MODE)).round().max(0.0) as usize).min(ch::MODES.len() - 1);
        // The chord's name and where it comes from.
        let side_w = 200.0;
        let side = Rect::new(r.right() - side_w - 8.0, r.y + 8.0, side_w, r.h - 16.0);
        let as_i32: Vec<i32> = notes.iter().map(|k| i32::from(*k)).collect();
        let flats = cx.model.project().flats_at(cx.model.playhead());
        let name = Chord::recognise(&as_i32).map_or_else(
            || {
                if notes.len() == 1 {
                    "single note".to_string()
                } else {
                    "—".to_string()
                }
            },
            |c| c.name(flats),
        );
        kit::readout(
            p,
            Rect::new(side.x, side.y, side.w, 56.0),
            if live { "CHORD" } else { "C4 BECOMES" },
            &name,
            self.accent,
            th,
        );
        let source = match mode {
            1 | 2 => {
                let (key, track) = Self::key_now(cx, id::FOLLOW_KEY, id::ROOT, id::SCALE);
                format!("{}{}", key.name(), if track { " (key track)" } else { "" })
            }
            3 => cx
                .model
                .project()
                .chord_at(cx.model.playhead())
                .map_or("no chord here".into(), |c| {
                    format!("{} on the chord track", c.chord.name(flats))
                }),
            _ => {
                let shifts: Vec<String> = (0..ch::SHIFTS)
                    .filter_map(|i| {
                        let s = cx.value(pid(id::SHIFT + i as u32)).round() as i32;
                        (s != 0).then(|| format!("{s:+}").replace('-', "−"))
                    })
                    .collect();
                if shifts.is_empty() {
                    "no intervals".into()
                } else {
                    shifts.join("  ")
                }
            }
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y + 64.0, side.w, 46.0),
            &ch::MODES[mode].to_uppercase(),
            &source,
            th.ui.text,
            th,
        );
        let strum = cx.value(pid(id::STRUM));
        if strum > 0.5 {
            let dir = if cx.on(pid(id::DIRECTION)) {
                "down"
            } else {
                "up"
            };
            p.text(
                &format!("strummed {dir}, {strum:.0} ms a note"),
                Rect::new(side.x, side.y + 118.0, side.w, 14.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        // The keyboard, the chord's keys lit (the lowest the strongest).
        let kb = Rect::new(r.x + 12.0, r.y + 14.0, side.x - r.x - 26.0, r.h - 28.0);
        let (low, high) = keys::span(&notes, 3);
        let accent = self.accent;
        let lowest = notes.first().copied();
        keys::keyboard(
            p,
            kb,
            low,
            high,
            &|k| {
                notes.contains(&k).then(|| {
                    if Some(k) == lowest {
                        accent
                    } else {
                        accent.mix(th.ui.text, 0.25)
                    }
                })
            },
            th,
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        ch::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::MODE => {
                "Intervals: fixed distances; Scale Triad/Seventh: thirds in the key; Chord Track: the chord where the note is"
            }
            id::FOLLOW_KEY => "The scale chords use the key track's key (else the key set here)",
            id::STRUM => "Each note of the chord later than the one before",
            id::DIRECTION => "Strum from the lowest note or from the highest",
            id::VELOCITY => "How loud the added notes are, against the played one",
            p if (id::SHIFT..id::SHIFT + ch::SHIFTS as u32).contains(&p) => {
                "An added note, in semitones from the played one (0: none)"
            }
            _ => return None,
        })
    }
}
