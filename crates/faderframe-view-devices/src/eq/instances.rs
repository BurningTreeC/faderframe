//! The instance list: every FaderFrame EQ in the project, track by track,
//! each with its output spectrum and its curve, where it collides with this
//! EQ's output. From here another EQ becomes the external spectrum (and
//! the collision reference), opens in its own editor, or becomes the
//! reference of an EQ Match.

use super::analyser::{self, Line};
use super::geometry::{FreqAxis, Layout, sweep};
use super::{Settings, Source};
use faderframe_core::{PluginInstanceId, builtin};
use faderframe_plugin_host::eq::design::AnalogBand;
use faderframe_plugin_host::eq::{BANDS, BandParams, global};
use faderframe_plugin_host::tap::RING_FRAMES;
use faderframe_project::TrackColor;
use faderframe_session::Session;
use faderframe_ui_canvas::{
    Color, FontWeight, Paint, Painter, Path, Point, Rect, TextStyle, Theme,
};
use std::collections::HashMap;

/// Every EQ in the project: its instance, a label (its track, numbered
/// when a track has several) and its track's colour.
pub(crate) fn instances(model: &Session) -> Vec<(PluginInstanceId, String, TrackColor)> {
    let mut out = Vec::new();
    for t in &model.project().tracks {
        let eqs: Vec<_> = t
            .inserts
            .iter()
            .filter(|s| s.plugin.id == builtin::EQ)
            .collect();
        for (i, s) in eqs.iter().enumerate() {
            let label = if eqs.len() > 1 {
                format!("{} · EQ {}", t.name, i + 1)
            } else {
                t.name.clone()
            };
            out.push((s.id, label, t.color));
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hit {
    /// Use as the external spectrum.
    Reference(PluginInstanceId),
    Open(PluginInstanceId),
    Match(PluginInstanceId),
    Close,
    Body,
}

const ROW_H: f32 = 70.0;

/// A row's button: what it does to an instance, its label, where it is.
type Button = (fn(PluginInstanceId) -> Hit, &'static str, Rect);

/// The open list and the spectra it shows.
pub(crate) struct List {
    lines: HashMap<PluginInstanceId, Line>,
    scratch: [Vec<f32>; 2],
    pub scroll: f32,
}

impl List {
    pub fn new() -> Self {
        Self {
            lines: HashMap::new(),
            scratch: [vec![0.0; RING_FRAMES], vec![0.0; RING_FRAMES]],
            scroll: 0.0,
        }
    }

    pub fn rect(l: &Layout) -> Rect {
        l.graph.inset(6.0)
    }

    fn rows(
        &self,
        l: &Layout,
        model: &Session,
    ) -> Vec<(PluginInstanceId, String, TrackColor, Rect)> {
        let r = Self::rect(l);
        instances(model)
            .into_iter()
            .enumerate()
            .map(|(i, (id, label, color))| {
                let y = r.y + 34.0 + i as f32 * (ROW_H + 6.0) - self.scroll;
                (id, label, color, Rect::new(r.x + 8.0, y, r.w - 16.0, ROW_H))
            })
            .collect()
    }

    fn buttons(row: &Rect) -> [Button; 3] {
        let x = row.right() - 92.0;
        [
            (
                Hit::Reference,
                "Reference",
                Rect::new(x, row.y + 6.0, 84.0, 18.0),
            ),
            (Hit::Open, "Open", Rect::new(x, row.y + 26.0, 84.0, 18.0)),
            (
                Hit::Match,
                "EQ Match",
                Rect::new(x, row.y + 46.0, 84.0, 18.0),
            ),
        ]
    }

    pub fn close_rect(l: &Layout) -> Rect {
        let r = Self::rect(l);
        Rect::new(r.right() - 70.0, r.y + 6.0, 62.0, 20.0)
    }

    pub fn hit(&self, pos: Point, l: &Layout, model: &Session) -> Option<Hit> {
        let r = Self::rect(l);
        if !r.contains(pos) {
            return None;
        }
        if Self::close_rect(l).contains(pos) {
            return Some(Hit::Close);
        }
        for (id, _, _, row) in self.rows(l, model) {
            for (make, _, br) in Self::buttons(&row) {
                if br.contains(pos) {
                    return Some(make(id));
                }
            }
            if row.contains(pos) {
                return Some(Hit::Open(id));
            }
        }
        Some(Hit::Body)
    }

    /// Scroll the rows (positive: down).
    pub fn scroll_by(&mut self, dy: f32, l: &Layout, model: &Session) {
        let n = instances(model).len() as f32;
        let r = Self::rect(l);
        let max = (34.0 + n * (ROW_H + 6.0) - r.h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        th: &Theme,
        this: PluginInstanceId,
        s: &Settings,
    ) {
        let r = Self::rect(l);
        p.shadow(r, 8.0, Color::rgba(0.0, 0.0, 0.0, 0.5), 0.0, 4.0, 18.0);
        p.fill_rounded(r, 8.0, &Paint::Solid(th.eq.panel.with_alpha(0.98)));
        p.stroke_rounded(r, 8.0, 1.0, th.eq.panel_edge);
        p.text(
            "INSTANCES",
            Rect::new(r.x + 14.0, r.y + 8.0, 200.0, 18.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim)
                .bold()
                .tracking(0.8),
        );
        let close = Self::close_rect(l);
        p.fill_rounded(close, 4.0, &Paint::Solid(th.ui.surface_alt));
        p.text(
            "Close",
            close,
            &TextStyle::new(th.fonts.small, th.ui.text).center(),
        );
        let rows = self.rows(l, model);
        if rows.is_empty() {
            p.text(
                "No EQs in the project",
                r,
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        }
        let rate = model.sample_rate().max(8_000);
        // This EQ's output: what the others collide with.
        let here_tap = model.plugin_tap(this);
        let axis = FreqAxis {
            lo: 20.0,
            hi: 20_000.0f64.min(0.5 * f64::from(rate) * 0.999),
        };
        p.push_clip(Rect::new(r.x, r.y + 30.0, r.w, r.h - 30.0));
        for (id, label, color, row) in rows {
            if row.bottom() < r.y + 30.0 || row.y > r.bottom() {
                continue;
            }
            let Some(tap) = model.plugin_tap(id) else {
                continue;
            };
            tap.watch();
            let line = self
                .lines
                .entry(id)
                .or_insert_with(|| Line::new(2048, f64::from(rate)));
            line.feed(&tap.output, &mut self.scratch, 40.0, false);
            let here = id == this;
            let referenced = s.external && s.source == Source::Instance(id);
            p.fill_rounded(row, 6.0, &Paint::Solid(th.eq.display));
            p.stroke_rounded(
                row,
                6.0,
                1.0,
                if here {
                    th.eq.curve.with_alpha(0.7)
                } else {
                    th.eq.panel_edge
                },
            );
            let swatch = Rect::new(row.x + 10.0, row.y + 10.0, 10.0, 10.0);
            p.fill_rounded(
                swatch,
                5.0,
                &Paint::Solid(Color::rgb8(color.r, color.g, color.b)),
            );
            p.text(
                &label,
                Rect::new(row.x + 26.0, row.y + 6.0, 150.0, 18.0),
                &TextStyle::new(th.fonts.small, th.ui.text).weight(FontWeight::Bold),
            );
            if here {
                p.text(
                    "this EQ",
                    Rect::new(row.x + 26.0, row.y + 24.0, 150.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, th.eq.curve),
                );
            } else if referenced {
                p.text(
                    "external spectrum",
                    Rect::new(row.x + 26.0, row.y + 24.0, 150.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, th.eq.external),
                );
            }
            // Its spectrum and curve.
            let g = Rect::new(row.x + 180.0, row.y + 6.0, row.w - 290.0, row.h - 12.0);
            p.fill_rounded(g, 4.0, &Paint::Solid(th.eq.display.darken(0.15)));
            let freqs = sweep(&axis, (g.w / 3.0).max(24.0) as usize);
            let levels: Vec<f32> = line
                .curve(&freqs)
                .iter()
                .zip(&freqs)
                .map(|(d, f)| d + s.tilt * (f / 1000.0).log2() as f32)
                .collect();
            let ys = |d: f32| g.y + g.h * (-d / s.range).clamp(0.0, 1.0);
            let pts: Vec<Point> = freqs
                .iter()
                .zip(&levels)
                .map(|(f, d)| Point::new(axis.x(&g, *f), ys(*d)))
                .collect();
            if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
                let mut fill = Path::polyline(&pts);
                fill.line_to(Point::new(last.x, g.bottom()))
                    .line_to(Point::new(first.x, g.bottom()))
                    .close();
                p.fill_path(&fill, th.eq.post.with_alpha(0.18));
            }
            // Collisions with this EQ's output.
            if !here && let Some(here_tap) = &here_tap {
                let mine = self.lines.get(&this).map(|l| l.curve(&freqs));
                if let Some(mine) = mine {
                    let mine: Vec<f32> = mine
                        .iter()
                        .zip(&freqs)
                        .map(|(d, f)| d + s.tilt * (f / 1000.0).log2() as f32)
                        .collect();
                    let c = analyser::collisions(&levels, &mine, 30.0);
                    for (i, k) in c.iter().enumerate() {
                        if *k > 0.15 {
                            let x = pts[i].x;
                            p.fill(
                                Rect::new(x - 1.5, pts[i].y, 3.0, g.bottom() - pts[i].y),
                                th.eq.collision.with_alpha(0.5 * k),
                            );
                        }
                    }
                }
                here_tap.watch();
            }
            // Its EQ curve (analog, at rest).
            let scale = f64::from(tap.params.get(global::GAIN_SCALE));
            let interact = tap.params.get(global::GAIN_Q) >= 0.5;
            let mut total = vec![0.0f64; freqs.len()];
            for b in 0..BANDS {
                let bp = BandParams::read(&tap.params, b);
                if !bp.enabled {
                    continue;
                }
                let a = AnalogBand::new(&bp.shape(scale, interact));
                for (t, f) in total.iter_mut().zip(&freqs) {
                    *t += a.db(*f);
                }
            }
            let mid = g.y + g.h / 2.0;
            let curve: Vec<Point> = freqs
                .iter()
                .zip(&total)
                .map(|(f, d)| {
                    Point::new(
                        axis.x(&g, *f),
                        (mid - (*d as f32) / 15.0 * g.h * 0.45).clamp(g.y, g.bottom()),
                    )
                })
                .collect();
            p.stroke_path(&Path::polyline(&curve), 1.6, th.eq.curve.with_alpha(0.9));
            for (make, name, br) in Self::buttons(&row) {
                let on = matches!(make(id), Hit::Reference(_)) && referenced;
                if here && !matches!(make(id), Hit::Open(_)) {
                    continue;
                }
                p.fill_rounded(
                    br,
                    3.0,
                    &Paint::Solid(if on {
                        th.eq.external.with_alpha(0.35)
                    } else {
                        th.ui.surface_alt
                    }),
                );
                p.text(
                    name,
                    br,
                    &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
                );
            }
        }
        p.pop_clip();
    }
}
