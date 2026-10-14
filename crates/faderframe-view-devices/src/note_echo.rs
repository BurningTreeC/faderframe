//! The Note Echo's face: the repeats laid out in time — each one's
//! loudness as its height, its pitch written over it — with the delay and
//! how many repeats are waiting to play.

use crate::kit::{self, Ctl, Ctx, Face, KNOB, Panel, SMALL, SWITCH_H, Scale};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::note_echo::{self as ne, id, value};
use faderframe_plugin_host::dsp::lfo::division_name;
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct NoteEchoFace {
    accent: Color,
}

impl NoteEchoFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Midi),
        }
    }
}

impl Face for NoteEchoFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(880.0, 460.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, _) = kit::frame(size, 170.0, 0.0);
        let s = kit::sections(deck, &[("TIME", 4.2), ("REPEATS", 5.2), ("PITCH", 3.2)]);
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::toggle(
            pid(id::SYNC),
            "Sync",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w * 0.38, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::DIVISION),
            "NOTE",
            Rect::new(r.x + 4.0, r.y + 32.0, r.w * 0.38, 38.0),
        ));
        let k = kit::row(
            Rect::new(r.x + r.w * 0.4, r.y, r.w * 0.6, KNOB.1),
            &[KNOB.0],
        );
        c.push(Ctl::knob(pid(id::TIME), "TIME", k[0]).scaled(Scale::Log));
        let r = kit::inside(&s[1]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::REPEATS), "REPEATS", k[0]));
        c.push(Ctl::knob(pid(id::FEEDBACK), "FEEDBACK", k[1]));
        c.push(Ctl::small(
            pid(id::LENGTH),
            "LENGTH",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        let r = kit::inside(&s[2]);
        c.push(
            Ctl::small(
                pid(id::PITCH),
                "PITCH",
                Rect::new(r.x + 4.0, r.y, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        c.push(Ctl::toggle(
            pid(id::IN_KEY),
            "In Key",
            Rect::new(
                r.x + SMALL.0 + 12.0,
                r.y + 28.0,
                (r.w - SMALL.0 - 18.0).max(50.0),
                SWITCH_H,
            ),
        ));
        c.push(Ctl::toggle(
            pid(id::DRY),
            "Played Note",
            Rect::new(r.x + 6.0, r.bottom() - SWITCH_H, r.w - 12.0, SWITCH_H),
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
        let sync = cx.on(pid(id::SYNC));
        let repeats = cx.value(pid(id::REPEATS)).round().clamp(1.0, 16.0) as i32;
        let feedback = cx.value(pid(id::FEEDBACK)).clamp(0.0, 1.0);
        let pitch = cx.value(pid(id::PITCH)).round() as i32;
        let in_key = cx.on(pid(id::IN_KEY));
        let dry = cx.on(pid(id::DRY));
        let delay_ms = cx.tap.value(value::DELAY_MS);
        let waiting = cx.tap.value(value::ECHOES).max(0.0) as usize;
        let side_w = 170.0;
        let side = Rect::new(r.right() - side_w - 8.0, r.y + 8.0, side_w, r.h - 16.0);
        let time = if sync {
            let d = division_name(cx.value(pid(id::DIVISION)).round().max(0.0) as usize);
            if delay_ms > 0.0 {
                format!("{d} · {delay_ms:.0} ms")
            } else {
                d.to_string()
            }
        } else {
            format!("{:.0} ms", cx.value(pid(id::TIME)))
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y, side.w, 56.0),
            "DELAY",
            &time,
            self.accent,
            th,
        );
        kit::readout(
            p,
            Rect::new(side.x, side.y + 64.0, side.w, 46.0),
            "WAITING",
            &format!("{waiting}"),
            th.ui.text,
            th,
        );
        // The taps.
        let g = Rect::new(r.x + 14.0, r.y + 26.0, side.x - r.x - 30.0, r.h - 52.0);
        p.hline(g.x, g.right(), g.bottom(), th.device.grid_strong);
        let slots = (repeats + 1) as f32;
        let step = g.w / slots;
        for k in 0..=repeats {
            let x = g.x + (k as f32 + 0.5) * step;
            p.vline(x, g.y, g.bottom(), th.device.grid);
            let level = if k == 0 {
                if dry { 1.0 } else { 0.0 }
            } else {
                feedback.powi(k)
            };
            // Too soft to play (velocity under 1 of 127): not drawn.
            if level * 127.0 < 1.0 {
                continue;
            }
            let h = (g.h - 18.0) * level as f32;
            let bar = Rect::new(x - step * 0.22, g.bottom() - h, step * 0.44, h);
            let c = if k == 0 {
                th.ui.text.with_alpha(0.55)
            } else {
                self.accent.with_alpha(0.35 + 0.6 * level as f32)
            };
            p.fill_rounded(bar, 3.0, &Paint::Solid(c));
            let label = if k == 0 {
                "played".to_string()
            } else if pitch == 0 {
                format!("{}", k)
            } else {
                format!("{:+}", k * pitch).replace('-', "−")
            };
            p.text(
                &label,
                Rect::new(x - step / 2.0, bar.y - 16.0, step, 14.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
            );
        }
        p.text(
            if pitch == 0 {
                "each repeat softer by the feedback"
            } else if in_key {
                "scale steps from the played note (the key track's key), each repeat softer by the feedback"
            } else {
                "semitones above the played note, each repeat softer by the feedback"
            },
            Rect::new(g.x, r.y + 6.0, g.w, 14.0),
            &TextStyle::new(th.fonts.small, th.ui.text_faint),
        );
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        ne::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::SYNC => "Repeats a division of the song's tempo apart (else the time in ms)",
            id::REPEATS => "How many times each note comes back",
            id::FEEDBACK => "Each repeat this much of the one before as loud",
            id::PITCH => {
                "Each repeat this many semitones (In Key: scale steps) from the one before"
            }
            id::IN_KEY => {
                "Pitch steps are degrees of the key track's key where the note was played"
            }
            id::LENGTH => "Each repeat's length as a share of the played note's",
            id::DRY => "Play the note itself too (off: only its echoes)",
            _ => return None,
        })
    }
}
