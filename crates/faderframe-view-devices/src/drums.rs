//! The Drum Sampler's face: sixteen pads (each its sample's name and
//! note; lit when it sounds; click to pick and play one, double-click to
//! load its sample, several files fill the pads from there), the picked
//! pad's waveform with its start and end (drag their markers) and buttons
//! to load or clear it, and that pad's controls.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use crate::values::note_name;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::drums::{self as drm, PADS, id, value};
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle, ViewEvent};

pub(crate) struct DrumsFace {
    accent: Color,
    selected: usize,
    glow: [f32; PADS],
    /// The marker being dragged (`id::START` or `id::END`).
    drag: Option<u32>,
    /// A pad is being played (its key is held while the button is).
    playing: bool,
}

/// How close the start and the end may come (of the sample's length).
const GAP: f64 = 0.01;

impl DrumsFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Sampler),
            selected: 0,
            glow: [0.0; PADS],
            drag: None,
            playing: false,
        }
    }

    fn pid(&self, field: u32) -> ParameterId {
        ParameterId(id::pad(self.selected) + field)
    }

    /// The pads (4 × 4, the first bottom left as on a drum machine), the
    /// waveform, its buttons.
    fn split(r: Rect) -> ([Rect; PADS], Rect, Rect, Rect) {
        let side = (r.h - 16.0).min(r.w * 0.42);
        let grid = Rect::new(r.x + 8.0, r.y + 8.0, side, side);
        let cell = (side - 3.0 * 6.0) / 4.0;
        let pads = std::array::from_fn(|p| {
            let (col, row) = (p % 4, 3 - p / 4);
            Rect::new(
                grid.x + col as f32 * (cell + 6.0),
                grid.y + row as f32 * (cell + 6.0),
                cell,
                cell,
            )
        });
        let right = Rect::new(
            grid.right() + 14.0,
            r.y + 8.0,
            r.right() - grid.right() - 22.0,
            r.h - 16.0,
        );
        let load = Rect::new(right.right() - 196.0, right.y, 96.0, 24.0);
        let clear = Rect::new(right.right() - 92.0, right.y, 92.0, 24.0);
        let wave = Rect::new(right.x, right.y + 52.0, right.w, right.h - 52.0);
        (pads, wave, load, clear)
    }

    fn contents(cx: &Ctx<'_>) -> Option<std::sync::Arc<Contents>> {
        cx.tap.assets::<Contents>()
    }

    /// Move the picked pad's start or end to `f` of the sample, keeping
    /// them apart and in their ranges.
    fn move_marker(&self, which: u32, f: f64, cx: &Ctx<'_>, edit: &mut Edit<'_, '_>) {
        let v = if which == id::START {
            let end = cx.value(self.pid(id::END));
            f.clamp(0.0, 0.95).min(end - GAP).max(0.0)
        } else {
            let start = cx.value(self.pid(id::START));
            f.clamp(0.05, 1.0).max(start + GAP).min(1.0)
        };
        edit.set(self.pid(which), v);
    }
}

impl Face for DrumsFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1060.0, 600.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 186.0, 30.0);
        let s = kit::sections(
            deck,
            &[
                ("PAD", 7.4),
                ("ENVELOPE", 3.0),
                ("FILTER", 3.0),
                ("PLAY", 4.6),
                ("KIT", 3.2),
            ],
        );
        let mut c = Vec::new();
        let r = kit::inside(&s[0]);
        let k = kit::row(
            Rect::new(r.x, r.y + 20.0, r.w, KNOB.1),
            &[KNOB.0, SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(self.pid(id::LEVEL), "LEVEL", k[0]));
        c.push(
            Ctl::small(
                self.pid(id::PAN),
                "PAN",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        c.push(
            Ctl::small(
                self.pid(id::TUNE),
                "TUNE",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        c.push(Ctl::small(
            self.pid(id::START),
            "START",
            kit::at(k[3], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::small(
            self.pid(id::END),
            "END",
            kit::at(k[4], 0.0, 6.0, SMALL.0, SMALL.1),
        ));
        c.push(Ctl::toggle(
            self.pid(id::KEEP),
            "Keep Length",
            Rect::new(k[2].x - 22.0, r.bottom() - SWITCH_H, 112.0, SWITCH_H),
        ));
        let r = kit::inside(&s[1]);
        let k = kit::row(
            Rect::new(r.x, r.y + 20.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(self.pid(id::ATTACK), "ATTACK", k[0]).scaled(Scale::Skew(2.5)));
        c.push(Ctl::small(self.pid(id::DECAY), "DECAY", k[1]).scaled(Scale::Log));
        let r = kit::inside(&s[2]);
        let k = kit::row(
            Rect::new(r.x, r.y + 20.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(self.pid(id::CUTOFF), "CUTOFF", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(self.pid(id::RESONANCE), "RESO", k[1]));
        let r = kit::inside(&s[3]);
        c.push(Ctl::segments(
            self.pid(id::MODE),
            "",
            Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 32.0, r.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(self.pid(id::CHOKE), "CHOKE", k[0]));
        c.push(Ctl::small(self.pid(id::VELOCITY), "VEL", k[1]));
        c.push(Ctl::small(self.pid(id::OUTPUT), "OUTPUT", k[2]));
        c.push(Ctl::toggle(
            self.pid(id::REVERSE),
            "Reverse",
            Rect::new(r.x + 6.0, r.bottom() - SWITCH_H, r.w - 12.0, SWITCH_H),
        ));
        let r = kit::inside(&s[4]);
        let k = kit::row(Rect::new(r.x, r.y + 20.0, r.w, KNOB.1), &[KNOB.0, SMALL.0]);
        c.push(Ctl::knob(ParameterId(id::VOLUME), "VOLUME", k[0]));
        c.push(Ctl::small(
            ParameterId(id::BASE_NOTE),
            "FIRST",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
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
        let (pads, wave, load, clear) = Self::split(r);
        let contents = Self::contents(cx);
        let base = cx.value(ParameterId(id::BASE_NOTE)).round() as i32;
        for (pad, rect) in pads.iter().enumerate() {
            let hit = cx.tap.take_value(value::pad(pad)).min(1.0);
            self.glow[pad] = hit.max(self.glow[pad] * (1.0 - 5.0 * cx.dt).max(0.0));
            let loaded = contents.as_ref().and_then(|c| c.set.slot(pad));
            let fill = if loaded.is_some() {
                th.device.section.lighten(0.04)
            } else {
                th.device.display
            };
            p.fill_rounded(*rect, 6.0, &Paint::Solid(fill));
            if self.glow[pad] > 0.01 {
                p.fill_rounded(
                    *rect,
                    6.0,
                    &Paint::Solid(self.accent.with_alpha(0.75 * self.glow[pad].sqrt())),
                );
            }
            let dropping = cx.drop.is_some_and(|d| rect.contains(d));
            if dropping {
                p.fill_rounded(*rect, 6.0, &Paint::Solid(self.accent.with_alpha(0.35)));
            }
            let edge = if pad == self.selected || dropping {
                self.accent
            } else {
                th.device.section_edge
            };
            p.stroke_rounded(
                *rect,
                6.0,
                if pad == self.selected { 2.0 } else { 1.0 },
                edge,
            );
            let name = loaded.map_or_else(|| format!("Pad {}", pad + 1), |s| s.name.clone());
            p.text(
                &name,
                Rect::new(rect.x + 6.0, rect.y + 6.0, rect.w - 12.0, 14.0),
                &TextStyle::new(
                    th.fonts.tiny + 0.5,
                    if loaded.is_some() {
                        th.ui.text
                    } else {
                        th.ui.text_faint
                    },
                )
                .bold(),
            );
            p.text(
                &note_name(base + pad as i32),
                Rect::new(rect.x + 6.0, rect.bottom() - 18.0, rect.w - 12.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
            );
            let choke = cx.value(ParameterId(id::pad(pad) + id::CHOKE)).round();
            if choke > 0.0 {
                p.text(
                    &format!("⊘{choke:.0}"),
                    Rect::new(rect.x + 6.0, rect.bottom() - 18.0, rect.w - 12.0, 12.0),
                    &TextStyle::new(th.fonts.tiny, self.accent).right(),
                );
            }
        }
        // The picked pad.
        let sample = contents.as_ref().and_then(|c| c.set.slot(self.selected));
        p.text(
            &format!(
                "Pad {} · {}",
                self.selected + 1,
                note_name(base + self.selected as i32)
            ),
            Rect::new(wave.x, load.y, wave.w - 200.0, 16.0),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let detail = match (
            sample,
            contents.as_ref().and_then(|c| c.doc.file(self.selected)),
        ) {
            (Some(s), _) => format!(
                "{} · {:.2} s · {:.1} kHz",
                s.name,
                s.frames as f64 / s.rate,
                s.rate / 1000.0
            ),
            (None, Some(f)) => format!("Missing: {f}"),
            (None, None) => "Empty: drop a sample here, double-click the pad or Load…".into(),
        };
        let detail = if cx.loading {
            "Loading…".to_string()
        } else {
            detail
        };
        p.text(
            &detail,
            Rect::new(wave.x, load.y + 18.0, wave.w, 14.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
        );
        kit::display_button(p, load, "Load…", self.accent, th);
        kit::display_button(p, clear, "Clear", th.ui.text_faint, th);
        p.fill_rounded(wave, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        if let Some(s) = sample {
            kit::waveform(p, wave, s, self.accent);
            // What does not play is dimmed; the start and end markers (as
            // the Sampler's).
            let x = |f: f64| wave.x + wave.w * (f as f32).clamp(0.0, 1.0);
            let (a, b) = (
                x(cx.value(self.pid(id::START))),
                x(cx.value(self.pid(id::END))),
            );
            let shade = th.device.display.darken(0.12).with_alpha(0.7);
            p.fill(
                Rect::new(wave.x, wave.y, (a - wave.x).max(0.0), wave.h),
                shade,
            );
            p.fill(
                Rect::new(b, wave.y, (wave.right() - b).max(0.0), wave.h),
                shade,
            );
            for (xx, left) in [(a, true), (b, false)] {
                p.vline(xx, wave.y, wave.bottom(), th.ui.text);
                p.fill(
                    Rect::new(
                        if left { xx } else { xx - 8.0 },
                        wave.bottom() - 8.0,
                        8.0,
                        8.0,
                    ),
                    th.ui.text,
                );
            }
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (pads, wave, load, clear) = Self::split(r);
        match *ev {
            ViewEvent::PointerDown { pos, clicks, .. } => {
                if let Some(pad) = pads.iter().position(|p| p.contains(pos)) {
                    self.selected = pad;
                    if clicks >= 2 {
                        edit.choose_samples(pad, "Load Pad Samples", false);
                    } else {
                        // Played as a key, held while the button is (a
                        // gated pad stops when let go).
                        let base = cx.value(ParameterId(id::BASE_NOTE)).round() as i32;
                        if let Ok(key) = u8::try_from(base + pad as i32) {
                            edit.audition(key.min(127), 100);
                            self.playing = true;
                        }
                    }
                    return true;
                }
                if load.contains(pos) {
                    edit.choose_samples(self.selected, "Load Pad Samples", false);
                    return true;
                }
                if clear.contains(pos) {
                    edit.clear_sample(self.selected);
                    return true;
                }
                if wave.contains(pos) {
                    // The nearer marker (the start or the end on its own
                    // side where they meet), to where the button went down,
                    // then wherever it is dragged.
                    let f = f64::from((pos.x - wave.x) / wave.w);
                    let (start, end) = (cx.value(self.pid(id::START)), cx.value(self.pid(id::END)));
                    let which = if (end - f).abs() < (start - f).abs()
                        || ((end - f).abs() == (start - f).abs() && f > start)
                    {
                        id::END
                    } else {
                        id::START
                    };
                    self.drag = Some(which);
                    edit.begin("Pad Marker");
                    self.move_marker(which, f, cx, edit);
                    return true;
                }
                false
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } => {
                let Some(which) = self.drag else { return false };
                let f = f64::from((pos.x - wave.x) / wave.w);
                self.move_marker(which, f, cx, edit);
                true
            }
            ViewEvent::PointerUp { .. } => {
                if std::mem::take(&mut self.playing) {
                    edit.release();
                }
                if self.drag.take().is_some() {
                    edit.end();
                }
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        drm::format(id, v)
    }

    fn drop_slot(&self, pos: faderframe_ui_canvas::Point, display: Rect) -> Option<(usize, bool)> {
        let (pads, wave, _, _) = Self::split(display);
        if let Some(pad) = pads.iter().position(|p| p.contains(pos)) {
            return Some((pad, false));
        }
        wave.contains(pos).then_some((self.selected, false))
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        if pid.0 == id::BASE_NOTE {
            return Some("The note of the first pad");
        }
        Some(match pid.0.checked_sub(id::pad(0)).map(|f| f % 16)? {
            id::CHOKE => "Pads in the same group cut each other (0: none)",
            id::MODE => "One-shot plays to the end; Gate stops when the key is let go",
            id::DECAY => "How fast the pad dies away (Full: the whole sample)",
            id::START => "Where in the sample it starts (drag the left white marker)",
            id::END => {
                "Where it stops (drag the right white marker); reversed, it plays from here back to the start"
            }
            id::KEEP => "The tune changes the pad's pitch, not its length",
            id::OUTPUT => {
                "Where the pad plays: Main, or an extra output once a track takes it \
                 (insert menu → Create Output Tracks)"
            }
            _ => return None,
        })
    }
}
