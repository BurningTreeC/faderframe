//! The Scale's face: the key in use on one octave of keys — its notes lit,
//! the root marked, the degrees numbered — and where the last note went.

use crate::keys;
use crate::kit::{self, Ctl, Ctx, Face, KNOB, Panel, SMALL, SWITCH_H};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::scale::{self as sc, id, value};
use faderframe_project::harmony::{Key, Scale};
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

pub(crate) struct ScaleFace {
    accent: Color,
}

impl ScaleFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Midi),
        }
    }
}

impl Face for ScaleFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(780.0, 460.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, _) = kit::frame(size, 170.0, 0.0);
        let s = kit::sections(
            deck,
            &[("KEY", 4.0), ("OUT OF KEY", 2.4), ("TRANSPOSE", 3.6)],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        c.push(Ctl::toggle(
            pid(id::FOLLOW_KEY),
            "Follow the Key Track",
            Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, SWITCH_H),
        ));
        c.push(Ctl::choice(
            pid(id::ROOT),
            "KEY",
            Rect::new(r.x + 4.0, r.y + 34.0, r.w * 0.36, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::SCALE),
            "SCALE",
            Rect::new(r.x + r.w * 0.36 + 8.0, r.y + 34.0, r.w * 0.64 - 12.0, 38.0),
        ));
        let r = kit::inside(&s[1]);
        c.push(Ctl::choice(
            pid(id::MODE),
            "A NOTE OUTSIDE",
            Rect::new(r.x + 4.0, r.y + 16.0, r.w - 8.0, 38.0),
        ));
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(pid(id::DEGREES), "DEGREES", k[0]).bipolar());
        c.push(
            Ctl::small(
                pid(id::OCTAVE),
                "OCTAVE",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
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
        // The key in use (as the device last reported it, or set here).
        let follow = cx.on(pid(id::FOLLOW_KEY));
        let key = match cx.model.project().key_at(cx.model.playhead()) {
            Some(k) if follow => k,
            _ => Key::new(
                (cx.value(pid(id::ROOT)).round().max(0.0) as u8) % 12,
                Scale::ALL[(cx.value(pid(id::SCALE)).round().max(0.0) as usize)
                    .min(Scale::ALL.len() - 1)],
            ),
        };
        let side_w = 210.0;
        let side = Rect::new(r.right() - side_w - 8.0, r.y + 8.0, side_w, r.h - 16.0);
        kit::readout(
            p,
            Rect::new(side.x, side.y, side.w, 56.0),
            if follow && !cx.model.project().keys.is_empty() {
                "KEY TRACK"
            } else {
                "KEY"
            },
            &key.name(),
            self.accent,
            th,
        );
        let (inp, out) = (cx.tap.value(value::IN), cx.tap.value(value::OUT));
        let moved = if inp < 0.0 {
            "—".to_string()
        } else if out < 0.0 {
            format!("{} → not played", note_name(inp as i32))
        } else {
            format!("{} → {}", note_name(inp as i32), note_name(out as i32))
        };
        kit::readout(
            p,
            Rect::new(side.x, side.y + 64.0, side.w, 46.0),
            "LAST NOTE",
            &moved,
            th.ui.text,
            th,
        );
        let mode =
            sc::MODES[(cx.value(pid(id::MODE)).round().max(0.0) as usize).min(sc::MODES.len() - 1)];
        let degrees = cx.value(pid(id::DEGREES)).round() as i32;
        let octave = cx.value(pid(id::OCTAVE)).round() as i32;
        let mut what = format!("outside the key: {}", mode.to_lowercase());
        if degrees != 0 {
            what += &format!(", {degrees:+} degrees").replace('-', "−");
        }
        if octave != 0 {
            what += &format!(", {octave:+} oct").replace('-', "−");
        }
        p.text(
            &what,
            Rect::new(side.x, side.y + 118.0, side.w, 14.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
        );
        // One octave from C, the key's notes lit, the root strongest; the
        // degree numbers under them.
        let kb = Rect::new(r.x + 12.0, r.y + 14.0, side.x - r.x - 26.0, r.h - 46.0);
        let accent = self.accent;
        let last_out = (out >= 0.0).then_some(out as u8 % 12);
        keys::keyboard(
            p,
            kb,
            60,
            72,
            &|k| {
                let n = i32::from(k);
                if !key.contains(n) {
                    return None;
                }
                Some(if Some(k % 12) == last_out && k < 72 {
                    accent.lighten(0.25)
                } else if k % 12 == key.root {
                    accent
                } else {
                    accent.mix(th.ui.text, 0.45)
                })
            },
            th,
        );
        let whites = 8.0;
        let w = kb.w / whites;
        let white_index = |pc: u8| [0, 0, 1, 1, 2, 3, 3, 4, 4, 5, 5, 6][pc as usize] as f32;
        for (d, iv) in key.scale.intervals().iter().enumerate() {
            let pc = (key.root + iv) % 12;
            let black = matches!(pc, 1 | 3 | 6 | 8 | 10);
            let x = kb.x + white_index(pc) * w + if black { w } else { w / 2.0 };
            p.text(
                &format!("{}", d + 1),
                Rect::new(x - 10.0, kb.bottom() + 6.0, 20.0, 14.0),
                &TextStyle::new(th.fonts.small, th.ui.text).bold().center(),
            );
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        sc::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::FOLLOW_KEY => "Use the key track's key where each note is (else the key set here)",
            id::MODE => "A note outside the key: to the nearest, up, down, or not played",
            id::DEGREES => "Move every note by scale steps, staying in the key",
            id::OCTAVE => "Move every note by octaves",
            _ => return None,
        })
    }
}
