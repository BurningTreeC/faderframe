//! The Granular's face: the sample (the one loaded, or its own pad) with
//! where the grains start now, the spray either side and the grains
//! sounding as dots (drag in it to move the position; Load… or a dropped
//! file replaces the sample), and the grain window's shape; on the deck
//! the grains, their pitch, the scan and the field, then the shared rows
//! (`poly`).

use crate::kit::{self, Ctl, Ctx, Edit, Face, Panel, SMALL, Scale, Section};
use crate::poly::{self, Page, pid};
use crate::synth::smalls;
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::granular::{self as gr, id, value};
use faderframe_plugin_host::devices::samples::Contents;
use faderframe_ui_canvas::{
    Accent, Color, Painter, Path, Point, PointerButton, Rect, Size, TextStyle, ViewEvent,
};

pub(crate) struct GranularFace {
    accent: Color,
    pub(crate) page: Page,
    dragging: bool,
}

impl GranularFace {
    pub fn new(theme: &faderframe_ui_canvas::Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Sampler),
            page: Page::Sound,
            dragging: false,
        }
    }

    /// The display's parts: the sample (wide) and the window.
    fn boxes(display: Rect) -> (Rect, Rect) {
        let r = poly::content(display);
        let w = (r.w - 8.0) * 0.75;
        (
            Rect::new(r.x, r.y, w, r.h),
            Rect::new(r.x + w + 8.0, r.y, r.w - w - 8.0, r.h),
        )
    }

    fn wave(b: Rect) -> Rect {
        Rect::new(b.x + 10.0, b.y + 30.0, b.w - 20.0, b.h - 40.0)
    }

    fn load_button(b: Rect) -> Rect {
        Rect::new(b.right() - 78.0, b.y + 4.0, 70.0, 20.0)
    }

    fn sound(f: &poly::Frame) -> Panel {
        let (mut s, mut c): (Vec<Section>, Vec<Ctl>) = (Vec::new(), Vec::new());
        s.extend(kit::sections(f.rows[0], &[("GRAINS", 9.0), ("PITCH", 6.0)]));
        s.extend(kit::sections(
            f.rows[1],
            &[
                ("SCAN", 4.0),
                ("SPACE", 5.6),
                ("MOD ENVELOPE", 4.8),
                ("LFO 2", 5.6),
            ],
        ));
        let r = kit::inside(&s[0]);
        c.push(Ctl::choice(
            pid(id::WINDOW),
            "WINDOW",
            Rect::new(r.x + 4.0, r.y - 6.0, 120.0, 38.0),
        ));
        let ms = Some(Scale::Log);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[
                (id::POSITION, "POSITION", None, false),
                (id::SPRAY, "SPRAY", Some(Scale::Skew(2.5)), false),
                (id::SIZE, "SIZE", ms, false),
                (id::DENSITY, "DENSITY", ms, false),
                (id::JITTER, "JITTER", None, false),
            ],
        );
        let r = kit::inside(&s[1]);
        c.push(Ctl::toggle(
            pid(id::FOLLOW_KEYS),
            "Follow Keys",
            Rect::new(r.x + 4.0, r.y + 2.0, 120.0, 24.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[
                (id::PITCH, "PITCH", None, true),
                (id::PITCH_RANDOM, "RANDOM", None, false),
                (id::ROOT, "ROOT", None, false),
            ],
        );
        let r = kit::inside(&s[2]);
        c.push(Ctl::toggle(
            pid(id::FREEZE),
            "Freeze",
            Rect::new(r.x + 4.0, r.y + 2.0, 100.0, 24.0),
        ));
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 40.0, r.w, SMALL.1),
            &[(id::SCAN, "SCAN", None, true)],
        );
        let r = kit::inside(&s[3]);
        smalls(
            &mut c,
            Rect::new(r.x, r.y + 20.0, r.w, SMALL.1),
            &[
                (id::PAN_SPREAD, "PAN", None, false),
                (id::REVERSE, "REVERSE", None, false),
                (id::LEVEL_RANDOM, "LEVEL RND", None, false),
            ],
        );
        poly::mod_envelope(&mut c, &s[4]);
        poly::lfo2(&mut c, &s[5]);
        poly::shape_row(f.rows[2], &mut s, &mut c);
        poly::panel(f, s, c)
    }
}

impl Face for GranularFace {
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
        poly::paint_tabs(p, r, "GRAINS", self.page, self.accent, th);
        let (b, w) = Self::boxes(r);
        poly::display_box(p, b, "SAMPLE", th);
        kit::display_button(p, Self::load_button(b), "Load…", self.accent, th);
        let contents = cx.tap.assets::<Contents>();
        let loaded = contents.as_ref().and_then(|c| c.set.slot(0));
        let sample = loaded.unwrap_or_else(|| gr::builtin_source());
        p.text(
            &if loaded.is_some() {
                format!(
                    "{} · {:.1} s",
                    sample.name,
                    sample.frames as f64 / sample.rate.max(1.0)
                )
            } else {
                "The built-in pad (load a sample, or drop one here)".into()
            },
            Rect::new(b.x + 80.0, b.y + 6.0, b.w - 170.0, 12.0),
            &TextStyle::new(
                th.fonts.tiny,
                if loaded.is_some() {
                    self.accent
                } else {
                    th.ui.text_faint
                },
            ),
        );
        let wave = Self::wave(b);
        if cx.drop.is_some_and(|d| b.contains(d)) {
            p.stroke_rounded(b, 4.0, 2.0, self.accent);
        }
        kit::waveform(p, wave, sample, th.device.wave);
        // Where the grains start, and the spray either side.
        let seconds = sample.frames as f64 / sample.rate.max(1.0);
        let spray = cx.value(pid(id::SPRAY)) * 0.001 / seconds.max(1e-3);
        let at = if cx.published(value::VOICES) > 0.0 {
            f64::from(cx.published(value::POSITION))
        } else {
            cx.value(pid(id::POSITION))
        };
        let x = |f: f64| wave.x + wave.w * f.rem_euclid(1.0) as f32;
        let (a, z) = (x(at - spray).min(x(at)), x(at + spray).max(x(at)));
        if spray > 0.0 {
            p.fill(
                Rect::new(a, wave.y, (z - a).max(1.0), wave.h),
                self.accent.with_alpha(0.12),
            );
        }
        p.vline(x(at), wave.y, wave.bottom(), self.accent);
        for k in 0..gr::SHOWN_GRAINS {
            let g = cx.published(value::GRAIN + k);
            if g >= 0.0 {
                let y = wave.y + wave.h * (0.2 + 0.6 * (k as f32 / gr::SHOWN_GRAINS as f32));
                p.circle(Point::new(x(f64::from(g)), y), 3.0, th.ui.text);
            }
        }
        if cx.published(value::VOICES) > 0.0 {
            p.text(
                &format!("{:.0} grains", cx.published(value::GRAINS)),
                Rect::new(b.x + 8.0, b.bottom() - 16.0, b.w - 16.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).right(),
            );
        }
        // The window.
        poly::display_box(p, w, "WINDOW", th);
        let shape =
            (cx.value(pid(id::WINDOW)).round().max(0.0) as usize).min(gr::WINDOWS.len() - 1);
        let plot = Rect::new(w.x + 12.0, w.y + 30.0, w.w - 24.0, w.h - 46.0);
        let pts: Vec<Point> = (0..=64)
            .map(|i| {
                let t = i as f32 / 64.0;
                Point::new(
                    plot.x + plot.w * t,
                    plot.bottom() - plot.h * gr::window_at(shape, t),
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 1.8, self.accent);
        p.text(
            &format!(
                "{} · {}",
                gr::WINDOWS[shape],
                crate::values::ms_text(cx.value(pid(id::SIZE)))
            ),
            Rect::new(w.x + 8.0, w.bottom() - 16.0, w.w - 16.0, 12.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim).right(),
        );
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (b, _) = Self::boxes(r);
        let wave = Self::wave(b);
        let at = |pos: Point| f64::from((pos.x - wave.x) / wave.w).clamp(0.0, 1.0);
        match *ev {
            ViewEvent::PointerDown { pos, button, .. } => {
                if let Some(page) = poly::tab_at(r, pos) {
                    self.page = page;
                    return true;
                }
                if Self::load_button(b).contains(pos) {
                    edit.choose_samples(0, "Load a Sample to Granulate", false);
                    return true;
                }
                if button == PointerButton::Primary && wave.contains(pos) {
                    self.dragging = true;
                    edit.begin("Grain Position");
                    edit.set(pid(id::POSITION), at(pos));
                    return true;
                }
                let _ = cx;
                false
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.dragging => {
                edit.set(pid(id::POSITION), at(pos));
                true
            }
            ViewEvent::PointerUp { .. } if self.dragging => {
                self.dragging = false;
                edit.end();
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        gr::format(id, v)
    }

    fn drop_slot(&self, pos: Point, display: Rect) -> Option<(usize, bool)> {
        Self::boxes(display).0.contains(pos).then_some((0, false))
    }

    fn tip(&self, p: ParameterId) -> Option<&'static str> {
        if p.0 < faderframe_plugin_host::devices::poly::COMMON {
            return poly::tip(p);
        }
        Some(match p.0 {
            id::POSITION => "Where in the sample grains start (drag in the display)",
            id::SPRAY => "How far either side of it each grain may start, at random",
            id::SIZE => "Each grain's length",
            id::DENSITY => "Grains a second",
            id::JITTER => "How much each grain's start time is left to chance",
            id::WINDOW => "Each grain's shape",
            id::PITCH => "The grains' transpose",
            id::PITCH_RANDOM => "Each grain's pitch spread at random either side",
            id::FOLLOW_KEYS => {
                "The grains follow the keys from the root key (off: the sample's pitch)"
            }
            id::ROOT => "The key that plays the sample as recorded",
            id::FREEZE => "The position stays where it is set",
            id::SCAN => {
                "How fast the position moves through the sample (1: as recorded; negative: backwards)"
            }
            id::PAN_SPREAD => "How far the grains spread across the stereo field",
            id::REVERSE => "The chance a grain plays backwards",
            id::LEVEL_RANDOM => "How much quieter a grain may be, at random",
            _ => return None,
        })
    }
}
