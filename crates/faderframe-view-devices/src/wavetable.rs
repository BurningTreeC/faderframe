//! The Wavetable's face: each oscillator's table as a stack of its frames
//! (the frame the newest voice reads now drawn bold, a user table's name
//! and size; Load… puts a file into the oscillator's slot and switches it
//! to its own table, right-click chooses the frame size or clears it),
//! and the envelopes; the deck's oscillators, sub and noise, unison per
//! oscillator and the shared rows (`poly`).

use crate::kit::{self, Ctl, Ctx, Edit, Face, Panel, SMALL, Section};
use crate::poly::{self, Page, pid};
use crate::synth::{adsr, smalls};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_plugin_host::devices::wavetable::{self as wt, id, table, value};
use faderframe_project::Command;
use faderframe_session::Action;
use faderframe_ui_canvas::{
    Accent, Color, FileChoice, HostRequest, MenuItem, Painter, Point, PointerButton, Rect, Size,
    TextStyle, ViewEvent,
};

/// The frame sizes a user table can be read with (0: from the file).
const CYCLES: [u32; 7] = [0, 256, 512, 600, 1024, 2048, 4096];

pub(crate) struct WavetableFace {
    accent: Color,
    pub(crate) page: Page,
}

impl WavetableFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Instrument),
            page: Page::Sound,
        }
    }

    /// The display's boxes: oscillator 1, oscillator 2, the envelopes.
    fn boxes(display: Rect) -> [Rect; 3] {
        let r = poly::content(display);
        let w = (r.w - 16.0) / 3.0;
        std::array::from_fn(|i| Rect::new(r.x + i as f32 * (w + 8.0), r.y, w, r.h))
    }

    /// An oscillator box's Load… button.
    fn load_button(b: Rect) -> Rect {
        Rect::new(b.right() - 78.0, b.y + 4.0, 70.0, 20.0)
    }

    fn sound(f: &poly::Frame) -> Panel {
        let (mut s, mut c): (Vec<Section>, Vec<Ctl>) = (Vec::new(), Vec::new());
        s.extend(kit::sections(
            f.rows[0],
            &[("OSC 1", 10.0), ("OSC 2", 10.0), ("SUB · NOISE", 5.0)],
        ));
        s.extend(kit::sections(
            f.rows[1],
            &[("UNISON 1", 5.0), ("UNISON 2", 5.0)],
        ));
        for k in 0..2 {
            let r = kit::inside(&s[k]);
            let w = r.w - 12.0;
            c.push(Ctl::choice(
                pid(id::osc(k, id::TABLE)),
                "TABLE",
                Rect::new(r.x + 4.0, r.y - 6.0, w * 0.55, 38.0),
            ));
            c.push(Ctl::choice(
                pid(id::osc(k, id::WARP)),
                "WARP",
                Rect::new(r.x + 8.0 + w * 0.55, r.y - 6.0, w * 0.45, 38.0),
            ));
            smalls(
                &mut c,
                Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
                &[
                    (id::osc(k, id::POSITION), "POSITION", None, false),
                    (id::osc(k, id::WARP_AMOUNT), "WARP", None, false),
                    (id::osc(k, id::OCTAVE), "OCTAVE", None, true),
                    (id::osc(k, id::SEMI), "SEMI", None, true),
                    (id::osc(k, id::FINE), "FINE", None, true),
                    (id::osc(k, id::LEVEL), "LEVEL", None, false),
                    (id::osc(k, id::PAN), "PAN", None, true),
                ],
            );
            let r = kit::inside(&s[3 + k]);
            smalls(
                &mut c,
                Rect::new(r.x, r.y + 20.0, r.w, SMALL.1),
                &[
                    (id::osc(k, id::UNISON), "VOICES", None, false),
                    (id::osc(k, id::DETUNE), "DETUNE", None, false),
                    (id::osc(k, id::SPREAD), "SPREAD", None, false),
                    (id::osc(k, id::PHASE), "PHASE", None, false),
                    (id::osc(k, id::RANDOM), "RANDOM", None, false),
                ],
            );
        }
        let r = kit::inside(&s[2]);
        let half = (r.w - 12.0) / 2.0;
        c.push(Ctl::choice(
            pid(id::SUB_WAVE),
            "SUB",
            Rect::new(r.x + 4.0, r.y - 6.0, half, 38.0),
        ));
        c.push(Ctl::choice(
            pid(id::NOISE_COLOUR),
            "NOISE",
            Rect::new(r.x + 8.0 + half, r.y - 6.0, half, 38.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[
                (id::SUB, "SUB", None, false),
                (id::SUB_OCTAVE, "OCTAVE", None, false),
                (id::NOISE, "NOISE", None, false),
            ],
        );
        poly::shape_row(f.rows[2], &mut s, &mut c);
        poly::panel(f, s, c)
    }

    fn contents(cx: &Ctx<'_>) -> Option<std::sync::Arc<Contents>> {
        cx.tap.assets::<Contents>()
    }

    /// The table oscillator `k` reads.
    fn table_of(cx: &Ctx<'_>, k: usize) -> Option<std::sync::Arc<table::Table>> {
        let i = cx.value(pid(id::osc(k, id::TABLE))).round().max(0.0) as usize;
        if i < wt::USER {
            return table::factory().get(i).cloned();
        }
        Self::contents(cx).and_then(|c| c.set.tables.get(k).cloned().flatten())
    }

    fn paint_oscillator(&self, p: &mut dyn Painter, b: Rect, k: usize, cx: &Ctx<'_>) {
        let th = cx.theme;
        poly::display_box(p, b, ["OSCILLATOR 1", "OSCILLATOR 2"][k], th);
        kit::display_button(p, Self::load_button(b), "Load…", self.accent, th);
        let level = cx.value(pid(id::osc(k, id::LEVEL)));
        let user = cx.value(pid(id::osc(k, id::TABLE))).round() as usize >= wt::USER;
        let loaded = Self::contents(cx).and_then(|c| c.set.tables.get(k).cloned().flatten());
        let name = match (&loaded, user) {
            (Some(t), true) => format!("{} · {} frames", t.name, t.frames),
            (None, true) => "No table loaded".into(),
            _ => wt::table_name(cx.value(pid(id::osc(k, id::TABLE))).round() as usize).into(),
        };
        p.text(
            &name,
            Rect::new(b.x + 8.0, b.y + 20.0, b.w - 96.0, 12.0),
            &TextStyle::new(
                th.fonts.tiny,
                if user && loaded.is_none() {
                    th.ui.text_faint
                } else {
                    self.accent
                },
            ),
        );
        let plot = Rect::new(b.x + 10.0, b.y + 38.0, b.w - 20.0, b.h - 48.0);
        if cx.drop.is_some_and(|d| b.contains(d)) {
            p.stroke_rounded(b, 4.0, 2.0, self.accent);
        }
        let Some(t) = Self::table_of(cx, k) else {
            return;
        };
        // Up to twelve frames stacked back to front, the one playing (or
        // set) bold in front.
        let shown = t.frames.min(12);
        let depth = 3.5;
        let w = plot.w - depth * shown as f32;
        let h = plot.h - depth * shown as f32;
        let faint = if level > 0.001 { 0.25 } else { 0.12 };
        for i in (0..shown).rev() {
            let f = if shown > 1 {
                i * (t.frames - 1) / (shown - 1)
            } else {
                0
            };
            let r = Rect::new(
                plot.x + depth * i as f32,
                plot.y + depth * (shown - 1 - i) as f32,
                w,
                h,
            );
            poly::cycle(p, r, t.cycle(f), 96, 1.0, self.accent.with_alpha(faint));
        }
        let at = if cx.published(value::VOICES) > 0.0 {
            f64::from(cx.published(value::POSITION[k]))
        } else {
            cx.value(pid(id::osc(k, id::POSITION)))
        };
        let frame = t.frames_at(at as f32);
        let x = (frame.f0 as f32 + frame.mix) / (t.frames.max(2) - 1) as f32;
        let slot = x * (shown.max(2) - 1) as f32;
        let r = Rect::new(
            plot.x + depth * slot,
            plot.y + depth * (shown as f32 - 1.0 - slot),
            w,
            h,
        );
        let now = if frame.mix > 0.5 { frame.f1 } else { frame.f0 };
        poly::cycle(
            p,
            r,
            t.cycle(now),
            192,
            2.0,
            self.accent
                .with_alpha(if level > 0.001 { 1.0 } else { 0.4 }),
        );
        p.text(
            &format!("frame {} / {}", now + 1, t.frames),
            Rect::new(b.x + 8.0, b.bottom() - 16.0, b.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
        );
    }

    /// Load a file into oscillator `k`'s slot and switch it to its own
    /// table.
    fn choose_table(k: usize, cx: &Ctx<'_>, edit: &mut Edit<'_, '_>) {
        let plugin = cx.plugin;
        let Some((track, _)) = cx.model.plugin_owner(plugin) else {
            return;
        };
        let mut patterns: Vec<String> = faderframe_audio_files::decode::SUPPORTED_EXTENSIONS
            .iter()
            .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_ascii_uppercase())])
            .collect();
        patterns.sort();
        edit.cx.request(HostRequest::ChooseFiles {
            choice: FileChoice::Open {
                title: format!("Load a Wavetable into Oscillator {}", k + 1),
                filters: vec![
                    ("Wavetables and samples".into(), patterns),
                    ("All files".into(), vec!["*".into()]),
                ],
            },
            commit: Box::new(move |files| {
                let file = files.into_iter().next()?;
                Some(Action::Several(vec![
                    Action::LoadDeviceSamples {
                        plugin,
                        slot: k,
                        files: vec![file],
                    },
                    Action::Edit(Command::SetPluginParameter {
                        track,
                        plugin,
                        parameter: pid(id::osc(k, id::TABLE)),
                        value: Some(wt::USER as f64),
                    }),
                ]))
            }),
        });
    }

    /// The oscillator box's menu: frame sizes, load, clear.
    fn menu(k: usize, at: Point, cx: &Ctx<'_>, edit: &mut Edit<'_, '_>) {
        let plugin = cx.plugin;
        let now = cx.model.wavetable_cycle(plugin, k);
        let mut items: Vec<MenuItem<Action>> = CYCLES
            .iter()
            .map(|&n| {
                let label = if n == 0 {
                    "Frame Size From the File".to_string()
                } else {
                    format!("Frames of {n} Samples")
                };
                MenuItem::new(
                    label,
                    Action::SetWavetableCycle {
                        plugin,
                        slot: k,
                        samples: n,
                    },
                )
                .checked(now == n)
            })
            .collect();
        items.push(
            MenuItem::new(
                "Clear the User Table",
                Action::LoadDeviceSamples {
                    plugin,
                    slot: k,
                    files: Vec::new(),
                },
            )
            .separated(),
        );
        edit.cx.request(HostRequest::ContextMenu { at, items });
    }
}

impl Face for WavetableFace {
    fn accent(&self) -> Color {
        self.accent
    }

    fn min_size(&self) -> Size {
        Size::new(poly::MIN_SIZE.0, poly::MIN_SIZE.1)
    }

    fn panel(&self, size: Size) -> Panel {
        let f = poly::frame(size);
        match self.page {
            Page::Sound => Self::sound(&f),
            Page::Modulation => poly::modulation_page(&f),
        }
    }

    fn paint_display(&mut self, p: &mut dyn Painter, r: Rect, cx: &Ctx<'_>) {
        let th = cx.theme;
        poly::paint_tabs(p, r, "OSCILLATORS", self.page, self.accent, th);
        let [a, b, e] = Self::boxes(r);
        self.paint_oscillator(p, a, 0, cx);
        self.paint_oscillator(p, b, 1, cx);
        poly::display_box(p, e, "ENVELOPES", th);
        let ep = Rect::new(e.x + 10.0, e.y + 26.0, e.w - 20.0, e.h - 36.0);
        let v = |i: u32| cx.value(pid(i));
        adsr(
            p,
            ep,
            [v(id::F_ATTACK), v(id::F_DECAY), v(id::F_RELEASE)],
            v(id::F_SUSTAIN),
            th.device.accent(Accent::Modulation),
            Some(cx.published(value::FILTER_ENV)),
        );
        adsr(
            p,
            ep,
            [v(id::ATTACK), v(id::DECAY), v(id::RELEASE)],
            v(id::SUSTAIN),
            self.accent,
            Some(cx.published(value::AMP_ENV)),
        );
        p.text(
            "amp · filter",
            Rect::new(e.x + 8.0, e.y + 6.0, e.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_faint).right(),
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let ViewEvent::PointerDown { pos, button, .. } = *ev else {
            return false;
        };
        if let Some(page) = poly::tab_at(r, pos) {
            self.page = page;
            return true;
        }
        let boxes = Self::boxes(r);
        for (k, b) in boxes.iter().take(2).enumerate() {
            if !b.contains(pos) {
                continue;
            }
            if button == PointerButton::Secondary {
                Self::menu(k, pos, cx, edit);
            } else if Self::load_button(*b).contains(pos) {
                Self::choose_table(k, cx, edit);
            }
            return true;
        }
        false
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        wt::format(id, v)
    }

    fn drop_slot(&self, pos: Point, display: Rect) -> Option<(usize, bool)> {
        let boxes = Self::boxes(display);
        (0..2).find(|k| boxes[*k].contains(pos)).map(|k| (k, false))
    }

    fn tip(&self, p: ParameterId) -> Option<&'static str> {
        if p.0 < faderframe_plugin_host::devices::poly::COMMON {
            return poly::tip(p);
        }
        if p.0 >= id::SUB {
            return Some(match p.0 {
                id::SUB_OCTAVE => "One or two octaves below oscillator 1",
                id::NOISE_COLOUR => "White, or pink (3 dB an octave less going up)",
                _ => return None,
            });
        }
        Some(
            match (p.0 - faderframe_plugin_host::devices::poly::COMMON) % id::OSC_FIELDS {
                id::TABLE => {
                    "The table read: a factory one, or User — the file loaded into the oscillator (Load… in the display)"
                }
                id::POSITION => {
                    "Where among the table's frames it reads (between two, both blended)"
                }
                id::WARP => {
                    "Sync: read faster, restarting each period; Bend: a half of the cycle squeezed; Mirror: forwards then back; FM: the other oscillator moves the phase"
                }
                id::WARP_AMOUNT => "How far the warp goes",
                id::DETUNE => "How far apart the unison voices are, lowest to highest",
                id::SPREAD => "How far the unison voices fan across the stereo field",
                id::PHASE => "Where every note starts the cycle",
                id::RANDOM => "How much each unison voice's start is left to chance",
                _ => return None,
            },
        )
    }
}
