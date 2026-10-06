//! The Sampler's face: what is loaded (a sample or an SFZ instrument, with
//! a button to load another), the waveform with the start and the loop
//! (drag their markers) and where the newest voice plays, a keyboard with
//! the zones over it, and the controls.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use crate::values::{is_black, note_name};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::sampler::{self as smp, id, value};
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_ui_canvas::{Accent, Color, Paint, Painter, Rect, Size, TextStyle, ViewEvent};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// What a drag in the waveform moves.
#[derive(Clone, Copy, PartialEq)]
enum Marker {
    Start,
    LoopStart,
    LoopEnd,
}

pub(crate) struct SamplerFace {
    accent: Color,
    drag: Option<Marker>,
}

impl SamplerFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Sampler),
            drag: None,
        }
    }

    /// Header (name, button), waveform, keyboard.
    fn split(r: Rect) -> (Rect, Rect, Rect, Rect) {
        let head = Rect::new(r.x + 8.0, r.y + 6.0, r.w - 16.0, 26.0);
        let button = Rect::new(head.right() - 120.0, head.y, 120.0, 24.0);
        let keys = Rect::new(r.x + 8.0, r.bottom() - 46.0, r.w - 16.0, 38.0);
        let wave = Rect::new(
            r.x + 8.0,
            head.bottom() + 6.0,
            r.w - 16.0,
            keys.y - head.bottom() - 14.0,
        );
        (head, button, wave, keys)
    }

    fn contents(cx: &Ctx<'_>) -> Option<std::sync::Arc<Contents>> {
        cx.tap.assets::<Contents>()
    }
}

impl Face for SamplerFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(1180.0, 560.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 186.0, 30.0);
        let s = kit::sections(
            deck,
            &[
                ("SAMPLE", 5.6),
                ("LOOP", 4.6),
                ("AMP", 6.6),
                ("FILTER", 5.2),
                ("OUT", 4.2),
            ],
        );
        let mut c = Vec::new();
        let small = |c: &mut Vec<Ctl>, i: u32, label: &'static str, r: Rect| {
            c.push(Ctl::small(pid(i), label, r))
        };
        let r = kit::inside(&s[0]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, SMALL.1), &[SMALL.0; 4]);
        small(&mut c, id::ROOT, "ROOT", k[0]);
        c.push(Ctl::small(pid(id::TRANSPOSE), "TRANSPOSE", k[1]).bipolar());
        c.push(Ctl::small(pid(id::TUNE), "TUNE", k[2]).bipolar());
        small(&mut c, id::START, "START", k[3]);
        let b = kit::row(
            Rect::new(r.x, r.bottom() - SWITCH_H, r.w, SWITCH_H),
            &[110.0, 110.0],
        );
        c.push(Ctl::toggle(pid(id::KEY_TRACK), "Follow Keys", b[0]));
        c.push(Ctl::toggle(pid(id::REVERSE), "Reverse", b[1]));
        c.push(Ctl::toggle(
            pid(id::PITCH_MODE),
            "Keep Length",
            Rect::new(b[0].x, b[0].y - SWITCH_H - 6.0, b[0].w, SWITCH_H),
        ));
        let r = kit::inside(&s[1]);
        c.push(Ctl::segments(
            pid(id::LOOP),
            "",
            Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0),
        ));
        let k = kit::row(Rect::new(r.x, r.y + 34.0, r.w, SMALL.1), &[SMALL.0; 3]);
        small(&mut c, id::LOOP_START, "FROM", k[0]);
        small(&mut c, id::LOOP_END, "TO", k[1]);
        c.push(Ctl::small(pid(id::CROSSFADE), "X-FADE", k[2]).scaled(Scale::Skew(2.0)));
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y + 20.0, r.w, SMALL.1), &[SMALL.0; 5]);
        c.push(Ctl::small(pid(id::ATTACK), "A", k[0]).scaled(Scale::Log));
        c.push(Ctl::small(pid(id::DECAY), "D", k[1]).scaled(Scale::Log));
        small(&mut c, id::SUSTAIN, "S", k[2]);
        c.push(Ctl::small(pid(id::RELEASE), "R", k[3]).scaled(Scale::Log));
        small(&mut c, id::VELOCITY, "VEL", k[4]);
        let r = kit::inside(&s[3]);
        c.push(Ctl::segments(
            pid(id::FILTER_TYPE),
            "",
            Rect::new(r.x + 6.0, r.y, r.w - 12.0, 24.0),
        ));
        let k = kit::row(
            Rect::new(r.x, r.y + 30.0, r.w, KNOB.1),
            &[KNOB.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::CUTOFF), "CUTOFF", k[0]).scaled(Scale::Log));
        small(
            &mut c,
            id::RESONANCE,
            "RESO",
            kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
        );
        c.push(
            Ctl::small(
                pid(id::FILTER_ENV),
                "ENV",
                kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        let r = kit::inside(&s[4]);
        let k = kit::row(
            Rect::new(r.x, r.y + 20.0, r.w, KNOB.1),
            &[KNOB.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::knob(pid(id::VOLUME), "VOLUME", k[0]));
        c.push(
            Ctl::small(
                pid(id::PAN),
                "PAN",
                kit::at(k[1], 0.0, 6.0, SMALL.0, SMALL.1),
            )
            .bipolar(),
        );
        small(
            &mut c,
            id::POLYPHONY,
            "VOICES",
            kit::at(k[2], 0.0, 6.0, SMALL.0, SMALL.1),
        );
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
        let (head, button, wave, keys) = Self::split(r);
        let contents = Self::contents(cx);
        let set = contents.as_ref().map(|c| &c.set);
        let file = contents
            .as_ref()
            .and_then(|c| c.doc.file(0).map(str::to_string));
        let zones = set.map_or(0, |s| s.zones.len());
        // What is loaded.
        let name = match &file {
            Some(f) => std::path::Path::new(f)
                .file_name()
                .map_or(f.clone(), |n| n.to_string_lossy().into_owned()),
            None => "Nothing loaded".into(),
        };
        p.text(
            &name,
            Rect::new(head.x, head.y, head.w - 140.0, 16.0),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let detail = match set {
            Some(s) if !s.errors.is_empty() => {
                format!("{} could not be loaded: {}", s.errors.len(), s.errors[0])
            }
            Some(s) if zones > 0 => format!("SFZ · {zones} regions · {} samples", s.samples.len()),
            Some(s) => s
                .slot(0)
                .map(|x| {
                    format!(
                        "{:.2} s · {:.1} kHz · {}",
                        x.frames as f64 / x.rate,
                        x.rate / 1000.0,
                        if x.stereo { "stereo" } else { "mono" }
                    )
                })
                .unwrap_or_else(|| "Load a sample, or an SFZ instrument".into()),
            None => String::new(),
        };
        let detail = if cx.loading {
            "Loading…".to_string()
        } else {
            detail
        };
        let err = set.is_some_and(|s| !s.errors.is_empty());
        p.text(
            &detail,
            Rect::new(head.x, head.y + 15.0, head.w - 140.0, 14.0),
            &TextStyle::new(
                th.fonts.tiny,
                if err {
                    th.tools.level_over
                } else {
                    th.ui.text_dim
                },
            ),
        );
        kit::display_button(p, button, "Load…", self.accent, th);
        // The waveform: the single sample, or the zone playing (else the
        // first).
        p.fill_rounded(wave, 3.0, &Paint::Solid(th.device.display.darken(0.12)));
        if cx.drop.is_some() {
            p.stroke_rounded(wave, 3.0, 2.0, self.accent);
        }
        let voices = cx.published(value::VOICES);
        let zone_now = cx.published(value::ZONE);
        let shown = set.and_then(|s| {
            if s.zones.is_empty() {
                s.slot(0)
            } else {
                let z = if voices > 0.0 && zone_now >= 0.0 {
                    zone_now as usize
                } else {
                    0
                };
                s.zones
                    .get(z)
                    .and_then(|z| s.samples.get(z.sample))
                    .map(|x| x.as_ref())
            }
        });
        if let Some(sample) = shown {
            kit::waveform(p, wave, sample, self.accent);
            if zones == 0 {
                let frac = |i: u32| cx.value(pid(i)) as f32;
                let x = |f: f32| wave.x + wave.w * f.clamp(0.0, 1.0);
                if cx.value(pid(id::LOOP)) >= 0.5 {
                    let (a, b) = (x(frac(id::LOOP_START)), x(frac(id::LOOP_END)));
                    p.fill(
                        Rect::new(a, wave.y, (b - a).max(1.0), wave.h),
                        self.accent.with_alpha(0.12),
                    );
                    for (xx, t) in [(a, "L"), (b, "")] {
                        p.vline(xx, wave.y, wave.bottom(), self.accent);
                        p.fill(Rect::new(xx - 4.0, wave.y, 8.0, 8.0), self.accent);
                        if !t.is_empty() {
                            p.text(
                                t,
                                Rect::new(xx + 6.0, wave.y + 2.0, 20.0, 12.0),
                                &TextStyle::new(th.fonts.tiny, self.accent).bold(),
                            );
                        }
                    }
                }
                let s = x(frac(id::START));
                p.vline(s, wave.y, wave.bottom(), th.ui.text);
                p.fill(
                    Rect::new(s - 4.0, wave.bottom() - 8.0, 8.0, 8.0),
                    th.ui.text,
                );
            }
            if voices > 0.0 {
                let pos = cx.published(value::POSITION).clamp(0.0, 1.0);
                p.vline(
                    wave.x + wave.w * pos,
                    wave.y,
                    wave.bottom(),
                    th.ui.text.with_alpha(0.8),
                );
            }
        } else {
            p.text(
                "Drop a sample or an SFZ instrument here, or Load…",
                wave,
                &TextStyle::new(th.fonts.normal, th.ui.text_faint).center(),
            );
        }
        // The keyboard and the zones over it (C0 to C8).
        let (lo, hi) = (12i32, 108i32);
        let w = keys.w / (hi - lo + 1) as f32;
        let key_x = |k: i32| keys.x + (k - lo) as f32 * w;
        let strip = Rect::new(keys.x, keys.y, keys.w, 10.0);
        let bed = Rect::new(keys.x, keys.y + 12.0, keys.w, keys.h - 12.0);
        p.fill(bed, th.device.key_white);
        let held = |k: i32| {
            voices > 0.0 && u8::try_from(k).is_ok_and(|k| smp::key_held(|i| cx.published(i), k))
        };
        for k in lo..=hi {
            let x = key_x(k);
            if is_black(k) {
                p.fill(Rect::new(x, bed.y, w, bed.h * 0.6), th.device.display);
            }
            if k % 12 == 0 {
                p.vline(x, bed.y, bed.bottom(), th.device.display.with_alpha(0.5));
                p.text(
                    &note_name(k),
                    Rect::new(x + 2.0, bed.bottom() - 12.0, 30.0, 12.0),
                    &TextStyle::new(th.fonts.tiny - 1.0, th.device.display),
                );
            }
            if held(k) {
                p.fill(Rect::new(x, bed.y, w, bed.h), self.accent.with_alpha(0.8));
            }
        }
        match set {
            Some(s) if !s.zones.is_empty() => {
                for (i, z) in s.zones.iter().enumerate() {
                    let a = key_x(i32::from(z.lokey).max(lo));
                    let b = key_x(i32::from(z.hikey).min(hi) + 1);
                    let shade = 0.35 + 0.4 * (i % 3) as f32 / 2.0;
                    p.fill(
                        Rect::new(a, strip.y, (b - a - 1.0).max(1.0), strip.h),
                        self.accent.with_alpha(shade),
                    );
                }
            }
            Some(s) if s.slot(0).is_some() => {
                p.fill(strip, self.accent.with_alpha(0.4));
                let root = cx.value(pid(id::ROOT)).round() as i32;
                p.fill(
                    Rect::new(key_x(root), strip.y - 2.0, w.max(3.0), strip.h + 4.0),
                    th.ui.text,
                );
            }
            _ => {}
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (_, button, wave, _) = Self::split(r);
        let single =
            Self::contents(cx).is_some_and(|c| c.set.zones.is_empty() && c.set.slot(0).is_some());
        match *ev {
            ViewEvent::PointerDown { pos, .. } if button.contains(pos) => {
                edit.choose_samples(0, "Load a Sample or SFZ Instrument", true);
                true
            }
            ViewEvent::PointerDown { pos, .. } if single && wave.contains(pos) => {
                let f = f64::from((pos.x - wave.x) / wave.w);
                let near = |i: u32| (cx.value(pid(i)) - f).abs() * f64::from(wave.w);
                let looping = cx.value(pid(id::LOOP)) >= 0.5;
                let mut best = (Marker::Start, near(id::START));
                if looping {
                    for (m, i) in [
                        (Marker::LoopStart, id::LOOP_START),
                        (Marker::LoopEnd, id::LOOP_END),
                    ] {
                        if near(i) < best.1 {
                            best = (m, near(i));
                        }
                    }
                }
                self.drag = Some(best.0);
                edit.begin("Sample Marker");
                let target = match best.0 {
                    Marker::Start => id::START,
                    Marker::LoopStart => id::LOOP_START,
                    Marker::LoopEnd => id::LOOP_END,
                };
                edit.set(pid(target), f.clamp(0.0, 1.0));
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } => {
                let Some(m) = self.drag else { return false };
                let f = f64::from((pos.x - wave.x) / wave.w).clamp(0.0, 1.0);
                let target = match m {
                    Marker::Start => id::START,
                    Marker::LoopStart => id::LOOP_START,
                    Marker::LoopEnd => id::LOOP_END,
                };
                edit.set(pid(target), f);
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
        smp::format(id, v)
    }

    fn drop_slot(&self, pos: faderframe_ui_canvas::Point, display: Rect) -> Option<(usize, bool)> {
        display.contains(pos).then_some((0, true))
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        Some(match pid.0 {
            id::ROOT => "The key that plays the sample as recorded",
            id::KEY_TRACK => "The pitch follows the keys (off: every key plays it as recorded)",
            id::PITCH_MODE => {
                "Keep Length: other keys change the pitch, not the length (16 voices; off: higher plays shorter, as a tape would)"
            }
            id::START => "Where playing starts (drag the white marker)",
            id::LOOP => "Loop for ever, or while the key is held (drag the markers)",
            id::CROSSFADE => "Blend across the loop's seam",
            id::FILTER_ENV => "The amp envelope opens (or closes) the filter",
            _ => return None,
        })
    }
}
