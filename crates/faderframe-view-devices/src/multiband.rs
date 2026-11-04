//! The Multiband Dynamics' face: the spectrum going in and coming out with
//! the bands over it — each tinted, its gain change now as a bar from the
//! middle (cut down, lift up), the response the bands make together as a
//! curve — the crossovers as handles to drag, tabs to pick a band (a click
//! in a band picks it too), the picked band's controls on the deck with
//! the split's and the global ones, and meters.

use crate::kit::{
    self, Ctl, Ctx, Edit, Face, KNOB, Meter, MeterKind, Panel, SMALL, SWITCH_H, Scale,
};
use faderframe_core::ParameterId;
use faderframe_plugin_host::devices::multiband::{self as mb, BANDS, id, value};
use faderframe_ui_canvas::{
    Accent, Color, Paint, Painter, Path, Point, Rect, Size, TextStyle, Theme, ViewEvent,
};

fn pid(i: u32) -> ParameterId {
    ParameterId(i)
}

/// The deck's band section, titled by the band picked.
const TITLES: [&str; BANDS] = ["BAND 1", "BAND 2", "BAND 3", "BAND 4", "BAND 5", "BAND 6"];

/// Each band's colour (the device families' accents: the theme's).
pub(crate) fn band_colour(theme: &Theme, b: usize) -> Color {
    let a = [
        Accent::Dynamics,
        Accent::Saturation,
        Accent::Time,
        Accent::Space,
        Accent::Modulation,
        Accent::Midi,
    ];
    theme.device.accent(a[b % BANDS])
}

/// The range of the gain bars (dB each way).
const RANGE_DB: f32 = 24.0;

pub(crate) struct MultibandFace {
    accent: Color,
    colours: [Color; BANDS],
    spectrum: kit::Spectrum,
    /// The band whose controls the deck shows.
    pub(crate) selected: usize,
    /// The crossover being dragged.
    drag: Option<usize>,
}

impl MultibandFace {
    pub fn new(theme: &Theme) -> Self {
        Self {
            accent: theme.device.accent(Accent::Dynamics),
            colours: std::array::from_fn(|b| band_colour(theme, b)),
            spectrum: kit::Spectrum::new(),
            selected: 0,
            drag: None,
        }
    }

    /// The tabs row and the spectrum under it.
    pub(crate) fn layout(r: Rect) -> (Rect, Rect) {
        let tabs = Rect::new(r.x + 8.0, r.y + 6.0, r.w - 16.0, 20.0);
        let spec = Rect::new(r.x + 8.0, r.y + 32.0, r.w - 16.0, r.h - 40.0);
        (tabs, spec)
    }

    /// Tab `b` of `n`.
    pub(crate) fn tab(tabs: Rect, b: usize) -> Rect {
        let w = 74.0;
        Rect::new(tabs.x + b as f32 * (w + 4.0), tabs.y, w, tabs.h)
    }

    fn count(cx: &Ctx<'_>) -> usize {
        (cx.value(pid(id::BANDS)).round() as usize).clamp(1, BANDS)
    }

    fn crossovers(cx: &Ctx<'_>) -> [f64; BANDS - 1] {
        let rate = f64::from(cx.model.sample_rate().max(8_000));
        mb::crossovers(&cx.tap.params, rate)
    }

    /// The crossover handle near `x` (within 6 px).
    fn crossover_at(cx: &Ctx<'_>, spec: Rect, x: f32) -> Option<usize> {
        let n = Self::count(cx);
        let xs = Self::crossovers(cx);
        (0..n - 1)
            .map(|k| (k, (kit::Spectrum::x_of(spec, xs[k]) - x).abs()))
            .filter(|(_, d)| *d <= 6.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    /// The band under `x`.
    fn band_at(cx: &Ctx<'_>, spec: Rect, x: f32) -> usize {
        let n = Self::count(cx);
        let xs = Self::crossovers(cx);
        let f = kit::Spectrum::freq_at(spec, x);
        (0..n - 1).find(|&k| f < xs[k]).unwrap_or(n - 1)
    }
}

impl Face for MultibandFace {
    fn accent(&self) -> Color {
        self.colours[self.selected]
    }

    fn min_size(&self) -> Size {
        Size::new(960.0, 500.0)
    }

    fn panel(&self, size: Size) -> Panel {
        let (display, deck, meter) = kit::frame(size, 206.0, 70.0);
        let b = self.selected.min(BANDS - 1);
        let s = kit::sections(deck, &[(TITLES[b], 10.0), ("SPLIT", 4.0), ("GLOBAL", 4.4)]);
        let band = |f: u32| pid(id::band(b, f));
        let mut c = Vec::new();
        // The band: knobs on the left, its mode and switches on the right.
        let r = kit::inside(&s[0]);
        let left = Rect::new(r.x, r.y, (r.w * 0.58).max(4.0 * KNOB.0 + 12.0), r.h);
        let right = Rect::new(left.right() + 6.0, r.y, r.right() - left.right() - 6.0, r.h);
        let k = kit::row(
            Rect::new(left.x, left.y, left.w, KNOB.1),
            &[KNOB.0, KNOB.0, KNOB.0, KNOB.0],
        );
        c.push(Ctl::knob(band(id::THRESHOLD), "THRESHOLD", k[0]));
        c.push(Ctl::knob(band(id::RATIO), "RATIO", k[1]).scaled(Scale::Log));
        c.push(Ctl::knob(band(id::RANGE), "RANGE", k[2]));
        c.push(Ctl::knob(band(id::GAIN), "GAIN", k[3]).bipolar());
        let k = kit::row(
            Rect::new(left.x, left.y + KNOB.1 + 2.0, left.w, SMALL.1),
            &[SMALL.0, SMALL.0, SMALL.0, SMALL.0],
        );
        c.push(Ctl::small(band(id::KNEE), "KNEE", k[0]));
        c.push(Ctl::small(band(id::ATTACK), "ATTACK", k[1]).scaled(Scale::Log));
        c.push(Ctl::small(band(id::RELEASE), "RELEASE", k[2]).scaled(Scale::Log));
        // Its upper crossover (the last band: its lower one).
        c.push(
            Ctl::small(pid(id::CROSSOVER + b.min(BANDS - 2) as u32), "SPLIT", k[3])
                .scaled(Scale::Log),
        );
        c.push(Ctl::segments(
            band(id::MODE),
            "MODE",
            Rect::new(right.x, right.y + 2.0, right.w, 38.0),
        ));
        let w = (right.w - 8.0) / 3.0;
        let y = right.y + 54.0;
        for (i, (f, label)) in [
            (id::SOLO, "Solo"),
            (id::MUTE, "Mute"),
            (id::BYPASS, "Bypass"),
        ]
        .into_iter()
        .enumerate()
        {
            c.push(Ctl::toggle(
                band(f),
                label,
                Rect::new(right.x + i as f32 * (w + 4.0), y, w, SWITCH_H),
            ));
        }
        // The split.
        let r = kit::inside(&s[1]);
        c.push(Ctl::segments(
            pid(id::BANDS),
            "BANDS",
            Rect::new(r.x + 2.0, r.y, r.w - 4.0, 38.0),
        ));
        c.push(Ctl::segments(
            pid(id::PHASE),
            "PHASE",
            Rect::new(r.x + 2.0, r.y + 44.0, r.w - 4.0, 38.0),
        ));
        c.push(Ctl::small(
            pid(id::LOOKAHEAD),
            "LOOKAHEAD",
            Rect::new(r.x + (r.w - SMALL.0) / 2.0, r.y + 88.0, SMALL.0, SMALL.1),
        ));
        // Global.
        let r = kit::inside(&s[2]);
        let k = kit::row(Rect::new(r.x, r.y, r.w, KNOB.1), &[KNOB.0, KNOB.0]);
        c.push(Ctl::knob(pid(id::OUTPUT), "OUTPUT", k[0]).bipolar());
        c.push(Ctl::knob(pid(id::MIX), "MIX", k[1]));
        let row = Rect::new(r.x, r.y + KNOB.1 + 2.0, r.w, SMALL.1);
        c.push(Ctl::small(
            pid(id::LINK),
            "LINK",
            Rect::new(row.x, row.y, SMALL.0, SMALL.1),
        ));
        let col = Rect::new(row.x + SMALL.0 + 6.0, row.y, row.w - SMALL.0 - 6.0, row.h);
        c.push(Ctl::segments(
            pid(id::DETECTOR),
            "DETECTOR",
            Rect::new(col.x, col.y, col.w, 38.0),
        ));
        c.push(Ctl::toggle(
            pid(id::EXTERNAL),
            "Sidechain",
            Rect::new(col.x, col.bottom() - SWITCH_H, col.w, SWITCH_H),
        ));
        let w = (meter.w - 8.0) / 3.0;
        let meters = vec![
            Meter {
                rect: Rect::new(meter.x, meter.y, w, meter.h),
                kind: MeterKind::Input,
                label: "IN",
            },
            Meter {
                rect: Rect::new(meter.x + w + 4.0, meter.y, w, meter.h),
                kind: MeterKind::Reduction(value::REDUCTION),
                label: "GR",
            },
            Meter {
                rect: Rect::new(meter.x + 2.0 * (w + 4.0), meter.y, w, meter.h),
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
        self.spectrum.update(cx);
        let th = cx.theme;
        let (tabs, spec) = Self::layout(r);
        let n = Self::count(cx);
        if self.selected >= n {
            self.selected = n - 1;
        }
        let xs = Self::crossovers(cx);
        // The tabs.
        for b in 0..n {
            let t = Self::tab(tabs, b);
            let on = b == self.selected;
            let col = self.colours[b];
            p.fill_rounded(
                t,
                4.0,
                &Paint::Solid(if on {
                    col.with_alpha(0.35)
                } else {
                    th.ui.surface_alt
                }),
            );
            p.stroke_rounded(t, 4.0, 1.0, if on { col } else { th.ui.border });
            let mode = mb::MODES[(cx.value(pid(id::band(b, id::MODE))).round().max(0.0) as usize)
                .min(mb::MODES.len() - 1)];
            let flag = if cx.on(pid(id::band(b, id::SOLO))) {
                " S"
            } else if cx.on(pid(id::band(b, id::MUTE))) {
                " M"
            } else if cx.on(pid(id::band(b, id::BYPASS))) {
                " B"
            } else {
                ""
            };
            p.text(
                &format!("{} {mode}{flag}", b + 1),
                t,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
            );
        }
        // The spectrum, the bands over it.
        self.spectrum.paint(p, spec, cx, -96.0, true);
        let edges: Vec<f32> = std::iter::once(spec.x)
            .chain(xs.iter().take(n - 1).map(|f| kit::Spectrum::x_of(spec, *f)))
            .chain(std::iter::once(spec.right()))
            .collect();
        let mid = spec.y + spec.h * 0.5;
        for b in 0..n {
            let (a, z) = (edges[b], edges[b + 1]);
            let col = self.colours[b];
            let alpha = if b == self.selected { 0.16 } else { 0.07 };
            p.fill(
                Rect::new(a, spec.y, (z - a).max(0.0), spec.h),
                col.with_alpha(alpha),
            );
            // The gain change now: down from the middle (cut) or up (lift).
            let g = cx.published(value::GAIN + b).clamp(-RANGE_DB, RANGE_DB);
            let h = spec.h * 0.5 * g / RANGE_DB;
            let bar_w = ((z - a) * 0.3).clamp(4.0, 22.0);
            let bx = (a + z) / 2.0 - bar_w / 2.0;
            let (top, height) = if h < 0.0 { (mid, -h) } else { (mid - h, h) };
            p.fill_rounded(
                Rect::new(bx, top, bar_w, height.max(1.0)),
                2.0,
                &Paint::Solid(col.with_alpha(0.85)),
            );
            p.text(
                &format!("{:+.1}", g),
                Rect::new(bx - 20.0, mid + 4.0, bar_w + 40.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim).center(),
            );
        }
        p.hline(spec.x, spec.right(), mid, th.device.grid_strong);
        // What the bands make together now (their gains and makeup).
        let npts = (spec.w / 3.0).max(2.0) as usize;
        let pts: Vec<Point> = (0..npts)
            .map(|i| {
                let x = spec.x + spec.w * i as f32 / (npts - 1) as f32;
                let f = kit::Spectrum::freq_at(spec, x);
                let mut sum = 0.0;
                for b in 0..n {
                    let heard = !cx.on(pid(id::band(b, id::MUTE)));
                    if !heard {
                        continue;
                    }
                    let mut g = f64::from(cx.published(value::GAIN + b));
                    if !cx.on(pid(id::band(b, id::BYPASS))) {
                        g += cx.value(pid(id::band(b, id::GAIN)));
                    } else {
                        g = 0.0;
                    }
                    sum += mb::band_magnitude(b, n, &xs, f) * 10f64.powf(g / 20.0);
                }
                let d = 20.0 * sum.max(1e-6).log10();
                Point::new(
                    x,
                    mid - spec.h * 0.5 * (d as f32 / RANGE_DB).clamp(-1.0, 1.0),
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.0, th.device.curve);
        // The crossovers.
        for (k, f) in xs.iter().take(n - 1).enumerate() {
            let x = kit::Spectrum::x_of(spec, *f);
            let col = if self.drag == Some(k) {
                th.ui.text
            } else {
                th.ui.text_dim
            };
            p.vline(x, spec.y, spec.bottom(), col.with_alpha(0.7));
            p.fill_rounded(
                Rect::new(x - 4.0, spec.y + 4.0, 8.0, 18.0),
                3.0,
                &Paint::Solid(col),
            );
            p.text(
                &faderframe_plugin_host::eq::format_hz(*f),
                Rect::new(x + 6.0, spec.y + 6.0, 70.0, 12.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
            );
        }
        if cx.value(pid(id::PHASE)) >= 0.5 {
            p.text(
                "LINEAR PHASE",
                Rect::new(spec.right() - 110.0, spec.bottom() - 30.0, 100.0, 12.0),
                &TextStyle::new(th.fonts.tiny, self.accent).bold(),
            );
        }
    }

    fn display_event(
        &mut self,
        ev: &ViewEvent,
        r: Rect,
        cx: &Ctx<'_>,
        edit: &mut Edit<'_, '_>,
    ) -> bool {
        let (tabs, spec) = Self::layout(r);
        let n = Self::count(cx);
        match *ev {
            ViewEvent::PointerDown { pos, .. } if tabs.contains(pos) => {
                if let Some(b) = (0..n).find(|&b| Self::tab(tabs, b).contains(pos)) {
                    self.selected = b;
                }
                true
            }
            ViewEvent::PointerDown { pos, .. } if spec.contains(pos) => {
                if let Some(k) = Self::crossover_at(cx, spec, pos.x) {
                    self.drag = Some(k);
                    edit.begin("Crossover");
                } else {
                    self.selected = Self::band_at(cx, spec, pos.x);
                }
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } => {
                let Some(k) = self.drag else {
                    return false;
                };
                let f = kit::Spectrum::freq_at(spec, pos.x).clamp(20.0, 20_000.0);
                edit.set(pid(id::CROSSOVER + k as u32), f);
                true
            }
            ViewEvent::PointerUp { .. } if self.drag.is_some() => {
                self.drag = None;
                edit.end();
                true
            }
            // The wheel over a band moves its threshold.
            ViewEvent::Scroll { pos, dy, .. } if spec.contains(pos) => {
                let b = Self::band_at(cx, spec, pos.x);
                let t =
                    cx.value(pid(id::band(b, id::THRESHOLD))) + if dy < 0.0 { 1.0 } else { -1.0 };
                edit.set_once(pid(id::band(b, id::THRESHOLD)), t.clamp(-60.0, 0.0));
                true
            }
            _ => false,
        }
    }

    fn format(&self, id: ParameterId, v: f64) -> Option<String> {
        mb::format(id, v)
    }

    fn tip(&self, pid: ParameterId) -> Option<&'static str> {
        if pid.0 == id::PHASE {
            return Some(
                "Minimum: Linkwitz–Riley, no latency. Linear: no phase shift, ~43 ms of latency",
            );
        }
        if pid.0 == id::EXTERNAL {
            return Some("A sidechain routed in keys the bands (split alike)");
        }
        let (_, f) = id::field_of(pid.0)?;
        Some(match f {
            id::MODE => {
                "Compress: loud down. Expand: quiet further down. Up Comp: quiet up. Up Exp: loud further up"
            }
            id::RANGE => "The most the band's level changes",
            id::SOLO => "Hear only the soloed bands",
            id::BYPASS => "The band passes untouched",
            _ => return None,
        })
    }
}
