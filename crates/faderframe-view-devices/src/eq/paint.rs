//! Painting the display: grid and scales, the analyser, the bands' curves
//! and the overall response, nodes and their values, the piano or the
//! frequency scale, the output meter.

use super::analyser::{self, Analyser, FLOOR, Line};
use super::geometry::{
    Layout, NODE_R, analyser_y, freq_of, is_black, note_label, note_name, note_of, sweep,
};
use super::{Drag, EqView, Hit, NodeButton, Settings, Source, ValueField};
use faderframe_plugin_host::eq::design::{
    self, AnalogBand, BandShape, BandType, Coefs, MAX_SECTIONS,
};
use faderframe_plugin_host::eq::{BandParams, PhaseMode, SPECTRAL_POINTS, format_hz, value};
use faderframe_plugin_host::tap::{AnalysisTap, db, db_power};
use faderframe_session::Session;
use faderframe_ui_canvas::{Color, FontWeight, Paint, Painter, Path, Point, Rect, Size, TextStyle};
use std::time::Instant;

/// How long the pointer rests on the spectrum before it can be grabbed.
const GRAB_AFTER: f64 = 0.9;

/// The response of one band across the display.
pub(crate) struct BandCurve {
    pub band: usize,
    pub params: BandParams,
    /// dB at the display's frequencies, as it is now.
    pub db: Vec<f64>,
    /// A dynamic band at rest and fully moved.
    pub range: Option<(Vec<f64>, Vec<f64>)>,
    /// The live dynamic gain (dB).
    pub moved: f64,
}

/// The digital response of a shape at frequencies whose `sin²(w/2)` are
/// given.
fn digital_db(shape: &BandShape, rate: f64, sin2: &[f64]) -> Vec<f64> {
    let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
    let n = design::design(shape, rate, &mut s);
    sin2.iter()
        .map(|x| {
            let m: f64 = s[..n].iter().map(|c| c.magnitude2_sin2(*x)).product();
            10.0 * m.max(1e-30).log10()
        })
        .collect()
}

fn analog_db(shape: &BandShape, freqs: &[f64]) -> Vec<f64> {
    let a = AnalogBand::new(shape);
    freqs.iter().map(|f| a.db(*f)).collect()
}

/// A spectral band's live movement at `freq` (from the published points).
fn spectral_at(tap: &AnalysisTap, band: usize, freq: f64) -> f64 {
    let t = (freq / 10.0).ln() / 3_000.0f64.ln() * (SPECTRAL_POINTS - 1) as f64;
    let i = (t.floor().max(0.0) as usize).min(SPECTRAL_POINTS - 2);
    let frac = (t - i as f64).clamp(0.0, 1.0);
    let v = |k: usize| f64::from(tap.value(value::SPECTRAL + band * SPECTRAL_POINTS + k));
    v(i) + (v(i + 1) - v(i)) * frac
}

impl EqView {
    /// Every band's curve at `freqs`.
    pub(crate) fn curves(
        &self,
        tap: &AnalysisTap,
        bands: &[(usize, BandParams)],
        freqs: &[f64],
        rate: f64,
    ) -> Vec<BandCurve> {
        let mode = Self::mode(tap);
        let scale = Self::scale(tap);
        let interact = Self::interact(tap);
        let sin2: Vec<f64> = freqs
            .iter()
            .map(|f| {
                (std::f64::consts::PI * f.min(0.5 * rate) / rate)
                    .sin()
                    .powi(2)
            })
            .collect();
        bands
            .iter()
            .map(|&(b, p)| {
                let moved = f64::from(tap.value(value::DYN + b));
                // Linear phase and natural phase play the analog curves;
                // dynamic bands in linear phase and everything in zero
                // latency the digital sections.
                let exact = match mode {
                    PhaseMode::ZeroLatency => false,
                    PhaseMode::Natural => true,
                    PhaseMode::Linear => !p.dynamic(),
                } || p.is_spectral();
                let curve = |gain: f64| {
                    let shape = p.shape_with(gain, interact);
                    if exact {
                        analog_db(&shape, freqs)
                    } else {
                        digital_db(&shape, rate, &sin2)
                    }
                };
                let gain = p.gain * scale;
                let db = if p.is_spectral() {
                    let base = curve(gain);
                    base.iter()
                        .zip(freqs)
                        .map(|(d, f)| d + spectral_at(tap, b, *f))
                        .collect()
                } else if p.dynamic() && moved.abs() > 1e-3 {
                    curve(gain + moved)
                } else {
                    curve(gain)
                };
                let range = (p.dynamic() && !p.is_spectral())
                    .then(|| (curve(gain), curve(gain + p.range * scale)));
                BandCurve {
                    band: b,
                    params: p,
                    db,
                    range,
                    moved,
                }
            })
            .collect()
    }

    /// Where a band's node sits: its gain for bells, shelves and tilts
    /// (half of it for tilts), its level at its frequency otherwise.
    pub(crate) fn node_at(
        &self,
        model: &Session,
        l: &Layout,
        tap: &AnalysisTap,
        p: &BandParams,
    ) -> Point {
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let g = &l.graph;
        let x = axis.x(g, p.freq);
        let scale = Self::scale(tap);
        let db = if p.kind.has_gain() {
            let gain = p.gain * scale;
            if matches!(p.kind, BandType::TiltShelf | BandType::FlatTilt) {
                gain * 0.5
            } else {
                gain
            }
        } else if p.kind == BandType::AllPass {
            0.0
        } else {
            design::band_db(
                &p.shape(scale, Self::interact(tap)),
                Self::rate(model),
                p.freq.min(Self::top(model)),
            )
        } as f32;
        let r = gains.range;
        Point::new(x, gains.y(g, db.clamp(-r, r)))
    }

    /// A dynamic band's range handle (where the node goes when fully
    /// moved).
    pub(crate) fn range_handle(
        &self,
        model: &Session,
        l: &Layout,
        tap: &AnalysisTap,
        p: &BandParams,
    ) -> Option<Point> {
        if !p.dynamic() {
            return None;
        }
        let at = self.node_at(model, l, tap, p);
        let gains = self.gain_axis(model);
        let range = (p.range * Self::scale(tap)) as f32;
        let r = gains.range;
        let db = (gains.db(&l.graph, at.y) + range).clamp(-r, r);
        Some(Point::new(at.x, gains.y(&l.graph, db)))
    }

    /// The box of values next to a band's node, and its parts.
    pub(crate) fn value_box(
        &self,
        l: &Layout,
        at: Point,
        p: &BandParams,
    ) -> (Rect, Vec<(Hit, Rect)>, usize) {
        let mut fields = vec![ValueField::Freq];
        if p.kind.has_gain() {
            fields.push(ValueField::Gain);
        }
        if p.kind.has_q(p.slope) {
            fields.push(ValueField::Q);
        }
        if p.kind.is_cut() {
            fields.push(ValueField::Slope);
        }
        let w = 18.0 + 64.0 * fields.len() as f32 + 4.0 * 20.0;
        let h = 22.0;
        let g = &l.graph;
        let mut r = Rect::new(at.x + 14.0, at.y - 34.0, w, h);
        if r.right() > g.right() - 2.0 {
            r.x = at.x - 14.0 - w;
        }
        if r.y < g.y + 2.0 {
            r.y = at.y + 14.0;
        }
        r.x = r.x.max(g.x + 2.0);
        let mut parts = Vec::new();
        let mut x = r.x + 6.0;
        for f in &fields {
            parts.push((Hit::Value(usize::MAX, *f), Rect::new(x, r.y, 64.0, h)));
            x += 64.0;
        }
        x += 4.0;
        for b in [
            NodeButton::Bypass,
            NodeButton::Solo,
            NodeButton::Delete,
            NodeButton::Menu,
        ] {
            parts.push((
                Hit::Button(usize::MAX, b),
                Rect::new(x, r.y + 2.0, 18.0, h - 4.0),
            ));
            x += 20.0;
        }
        (r, parts, fields.len())
    }

    pub(crate) fn paint_all(&mut self, p: &mut dyn Painter, size: Size, model: &Session) {
        let th = self.theme.clone();
        let l = self.layout(size, model);
        let s = self.settings(model);
        p.fill(Rect::from_size(size), th.ui.background);
        let Some(tap) = self.device.tap(model) else {
            p.text(
                "The EQ is not loaded",
                l.graph,
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        };
        tap.watch();
        let now = Instant::now();
        let dt = self
            .last_paint
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64())
            .min(0.25);
        self.last_paint = Some(now);
        let rate = Self::rate(model);
        // Feed the analyser.
        let size_wanted = analyser::RESOLUTIONS[s.resolution];
        if self
            .analyser
            .as_ref()
            .is_none_or(|a| a.rate != model.sample_rate() || a.size != size_wanted)
        {
            self.analyser = Some(Analyser::new(model.sample_rate(), size_wanted));
        }
        let external = match s.source {
            Source::Sidechain => Some(Ok(tap.clone())),
            Source::Instance(id) => model.plugin_tap(id).map(Err),
        };
        if let Some(a) = self.analyser.as_mut() {
            let fall = analyser::SPEEDS[s.speed].1;
            let frozen = s.freeze || self.grab.is_some();
            a.update(&tap, external.as_ref(), s.external, fall, frozen);
        }
        if let Some(m) = self.matching.as_mut() {
            m.update(model, &tap, self.device.plugin);
        }
        self.update_grab(model, &l, &s);
        // The display.
        let g = l.graph;
        p.fill(g, th.device.display);
        self.paint_grid(p, &l, model, &s);
        p.push_clip(g);
        self.paint_analyser(p, &l, model, &s);
        let freqs = sweep(&self.axis(model), (g.w as usize / 2).clamp(64, 600));
        let bands = self.used(model).map(|(_, b)| b).unwrap_or_default();
        let curves = self.curves(&tap, &bands, &freqs, rate);
        self.paint_curves(p, &l, model, &freqs, &curves);
        self.paint_previews(p, &l, model, &tap, &freqs);
        p.pop_clip();
        self.paint_nodes(p, &l, model, &tap, &curves);
        self.paint_value_box(p, &l, model, &tap);
        self.paint_peaks(p, &l, model);
        self.paint_panel(p, &l, model, &tap);
        if s.piano {
            self.paint_piano(p, &l, model, &tap, &bands);
        } else {
            self.paint_axis(p, &l, model);
        }
        let mono = model
            .plugin_slot(self.device.plugin)
            .is_some_and(|(track, _)| track.layout == faderframe_core::ChannelLayout::Mono);
        self.paint_meter(p, &l, &tap, dt as f32, s.range, mono);
        self.paint_top(p, &l, model);
        self.paint_bottom(p, &l, model, &tap);
        self.paint_output(p, &l, &tap);
        if let Some(list) = self.instances.as_mut() {
            list.paint(p, &l, model, &th, self.device.plugin, &s);
        }
        if let Some(m) = &self.matching {
            m.paint_panel(p, &l, model, &th, self.device.plugin);
        }
    }

    fn paint_grid(&self, p: &mut dyn Painter, l: &Layout, model: &Session, s: &Settings) {
        let th = &self.theme;
        let g = &l.graph;
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let line = th.device.grid;
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        // Vertical lines at 1-2-5 steps (and the decades between, faint).
        let mut decade = 1.0;
        while decade < axis.hi {
            for m in 1..10 {
                let f = decade * f64::from(m);
                if f < axis.lo || f > axis.hi {
                    continue;
                }
                let x = axis.x(g, f);
                let strong = m == 1;
                p.vline(
                    x,
                    g.y,
                    g.bottom(),
                    if strong {
                        line.with_alpha(line.a * 2.0)
                    } else {
                        line
                    },
                );
            }
            decade *= 10.0;
        }
        let step = match gains.range as i32 {
            3 => 1.0,
            6 => 2.0,
            12 => 3.0,
            _ => 6.0,
        };
        let mut v = -gains.range;
        while v <= gains.range + 0.01 {
            let y = gains.y(g, v);
            p.hline(
                g.x,
                g.right(),
                y,
                if v == 0.0 {
                    th.device.grid_strong
                } else {
                    line
                },
            );
            let text = if v > 0.0 {
                format!("+{v:.0}")
            } else {
                format!("{v:.0}")
            };
            p.text(
                &text.replace('-', "−"),
                Rect::new(2.0, y - 7.0, g.x - 6.0, 14.0),
                &label.right().color(th.device.curve.with_alpha(0.8)),
            );
            v += step;
        }
        // The analyser's own scale on the right.
        if s.pre || s.post || s.external {
            let every = if s.range > 90.0 { 24.0 } else { 12.0 };
            let mut dbfs = -every;
            while dbfs > -s.range + 1.0 {
                let y = analyser_y(g, dbfs, s.range);
                p.text(
                    &format!("{dbfs:.0}").replace('-', "−"),
                    Rect::new(g.right() + 2.0, y - 7.0, 26.0, 14.0),
                    &label,
                );
                dbfs -= every;
            }
        }
    }

    fn paint_analyser(&self, p: &mut dyn Painter, l: &Layout, model: &Session, s: &Settings) {
        let Some(a) = &self.analyser else {
            return;
        };
        let th = &self.theme;
        let g = &l.graph;
        let axis = self.axis(model);
        let points = (g.w / 2.0).max(32.0) as usize;
        let freqs = sweep(&axis, points);
        // Each line tilted.
        let levels = |line: &Line| -> Vec<f32> {
            line.curve(&freqs)
                .iter()
                .zip(&freqs)
                .map(|(d, f)| d + s.tilt * (f / 1000.0).log2() as f32)
                .collect()
        };
        let shape = |v: &[f32]| -> Vec<Point> {
            v.iter()
                .zip(&freqs)
                .map(|(db, f)| Point::new(axis.x(g, *f), analyser_y(g, *db, s.range)))
                .collect()
        };
        let dim = if self.grab.is_some() { 1.6 } else { 1.0 };
        if s.post {
            let post_levels = levels(&a.post);
            let post = shape(&post_levels);
            if let (Some(first), Some(last)) = (post.first(), post.last()) {
                let mut fill = Path::polyline(&post);
                fill.line_to(Point::new(last.x, g.bottom()))
                    .line_to(Point::new(first.x, g.bottom()))
                    .close();
                p.fill_path_paint(
                    &fill,
                    &Paint::vertical(
                        *g,
                        th.device.post.with_alpha(0.20 * dim),
                        th.device.post.with_alpha(0.05),
                    ),
                );
                p.stroke_path(
                    &Path::polyline(&post),
                    1.0,
                    th.device.post.with_alpha(0.45 * dim),
                );
            }
            // Collisions with the external spectrum: a red glow along the
            // output where both crowd the same frequencies.
            if s.external && s.collisions && a.ext.alive_recently() {
                let raw = analyser::collisions(&post_levels, &levels(&a.ext), 30.0);
                let n = raw.len();
                let c: Vec<f32> = (0..n)
                    .map(|i| {
                        let (lo, hi) = (i.saturating_sub(4), (i + 5).min(n));
                        raw[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
                    })
                    .collect();
                const DEPTH: f32 = 34.0;
                for i in 0..post.len().saturating_sub(1) {
                    let k = 0.5 * (c[i] + c[i + 1]);
                    if k < 0.08 {
                        continue;
                    }
                    let (a0, a1) = (post[i], post[i + 1]);
                    let mut quad = Path::new();
                    quad.move_to(a0)
                        .line_to(a1)
                        .line_to(Point::new(a1.x, (a1.y + DEPTH).min(g.bottom())))
                        .line_to(Point::new(a0.x, (a0.y + DEPTH).min(g.bottom())))
                        .close();
                    let top = a0.y.min(a1.y);
                    p.fill_path_paint(
                        &quad,
                        &Paint::vertical(
                            Rect::new(
                                a0.x,
                                top,
                                (a1.x - a0.x).max(1.0),
                                DEPTH + (a0.y - a1.y).abs(),
                            ),
                            th.device.collision.with_alpha(0.7 * k),
                            th.device.collision.with_alpha(0.0),
                        ),
                    );
                }
            }
        }
        if s.pre {
            p.stroke_path(
                &Path::polyline(&shape(&levels(&a.pre))),
                1.0,
                th.device.pre.with_alpha(0.55),
            );
        }
        if s.external {
            p.stroke_path(
                &Path::polyline(&shape(&levels(&a.ext))),
                1.4,
                th.device.external.with_alpha(0.75),
            );
        }
    }

    fn paint_curves(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        freqs: &[f64],
        curves: &[BandCurve],
    ) {
        let th = &self.theme;
        let g = &l.graph;
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let limit = gains.range * 2.2;
        let zero = gains.y(g, 0.0);
        let to_points = |db: &[f64]| -> Vec<Point> {
            freqs
                .iter()
                .zip(db)
                .map(|(f, d)| {
                    Point::new(axis.x(g, *f), gains.y(g, (*d as f32).clamp(-limit, limit)))
                })
                .collect()
        };
        let grabbing = self.grab.is_some();
        for c in curves {
            let p_ = &c.params;
            let selected = self.is_selected(c.band);
            let color = if p_.enabled {
                Self::band_color(c.band)
            } else {
                th.ui.text_faint
            };
            let dim = if grabbing { 0.35 } else { 1.0 };
            // A dynamic band's reach: between where it rests and where it
            // goes.
            if let Some((rest, full)) = &c.range {
                let a = to_points(rest);
                let mut b = to_points(full);
                b.reverse();
                let mut area = Path::polyline(&a);
                for q in &b {
                    area.line_to(*q);
                }
                area.close();
                p.fill_path(
                    &area,
                    color.with_alpha(if selected { 0.16 } else { 0.07 } * dim),
                );
            }
            let pts = to_points(&c.db);
            if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
                let mut fill = Path::polyline(&pts);
                fill.line_to(Point::new(last.x, zero))
                    .line_to(Point::new(first.x, zero))
                    .close();
                let alpha = match (p_.enabled, selected) {
                    (true, true) => 0.26,
                    (true, false) => 0.10,
                    (false, _) => 0.04,
                };
                p.fill_path(&fill, color.with_alpha(alpha * dim));
            }
            p.stroke_path(
                &Path::polyline(&pts),
                if selected { 1.6 } else { 1.0 },
                color.with_alpha(if p_.enabled { 0.85 } else { 0.35 } * dim),
            );
        }
        // The overall response, per part of the signal the bands work on.
        let mut sums: [Option<Vec<f64>>; 5] = Default::default();
        for c in curves.iter().filter(|c| c.params.enabled) {
            let k = c.params.placement.index();
            let sum = sums[k].get_or_insert_with(|| vec![0.0; freqs.len()]);
            for (s, d) in sum.iter_mut().zip(&c.db) {
                *s += d;
            }
        }
        let stereo = sums[0].clone().unwrap_or_else(|| vec![0.0; freqs.len()]);
        let with = |k: usize| -> Option<Vec<f64>> {
            sums[k]
                .as_ref()
                .map(|v| v.iter().zip(&stereo).map(|(a, b)| a + b).collect())
        };
        let colors = [
            th.device.curve,
            th.device.left,
            th.device.right,
            th.device.mid,
            th.device.side,
        ];
        let split = sums[1..].iter().any(Option::is_some);
        for (k, color) in colors.iter().enumerate().skip(1) {
            if let Some(v) = with(k) {
                p.stroke_path(&Path::polyline(&to_points(&v)), 1.8, color.with_alpha(0.9));
            }
        }
        if !split || sums[0].is_some() {
            let alpha = if grabbing { 0.4 } else { 0.95 };
            p.stroke_path(
                &Path::polyline(&to_points(&stereo)),
                2.2,
                th.device.curve.with_alpha(alpha),
            );
        }
    }

    /// What a click would create, a sketch being drawn, the selection
    /// rectangle, and EQ Match's target and result.
    fn paint_previews(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
        freqs: &[f64],
    ) {
        let th = &self.theme;
        let g = &l.graph;
        let axis = self.axis(model);
        let gains = self.gain_axis(model);
        let to_points = |db: &[f64]| -> Vec<Point> {
            freqs
                .iter()
                .zip(db)
                .map(|(f, d)| Point::new(axis.x(g, *f), gains.y(g, *d as f32)))
                .collect()
        };
        match &self.drag {
            Some(Drag::Lasso { start, now, .. }) => {
                let r = Rect::from_points(*start, *now);
                p.fill_rounded(r, 2.0, &Paint::Solid(th.ui.selection.with_alpha(0.12)));
                p.stroke_rounded(r, 2.0, 1.0, th.ui.selection.with_alpha(0.7));
            }
            Some(Drag::Sketch { stroke, .. }) => {
                let pts: Vec<Point> = stroke
                    .points
                    .iter()
                    .map(|(f, d)| Point::new(axis.x(g, *f), gains.y(g, *d as f32)))
                    .collect();
                dashed(p, &pts, 1.4, th.device.curve.with_alpha(0.8));
            }
            None => {
                // A faint preview of the band a click would add.
                if let (Some(Hit::Graph(at)), None) = (self.hover, &self.grab) {
                    let (kind, freq, gain) =
                        self.shape_at(model, Size::new(g.right() + 46.0, l.bottom.bottom()), at);
                    let shape = super::edit::new_shape(kind, freq, gain);
                    let db = analog_db(&shape, freqs);
                    let pts = to_points(&db);
                    if self.selected.is_empty() {
                        p.stroke_path(&Path::polyline(&pts), 1.0, th.device.curve.with_alpha(0.25));
                    } else {
                        dashed(p, &pts, 1.0, th.device.curve.with_alpha(0.25));
                    }
                }
            }
            _ => {}
        }
        if let Some(m) = &self.matching {
            m.paint_curves(p, l, &axis, &gains, th, freqs);
        }
        let _ = tap;
    }

    fn paint_nodes(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
        curves: &[BandCurve],
    ) {
        let th = &self.theme;
        let g = &l.graph;
        let gains = self.gain_axis(model);
        for c in curves {
            let b = c.band;
            let band = &c.params;
            let at = self.node_at(model, l, tap, band);
            let color = Self::band_color(b);
            let selected = self.is_selected(b);
            let r = if selected { NODE_R + 1.5 } else { NODE_R };
            // A dynamic band's range handle and its live position.
            if let Some(h) = self.range_handle(model, l, tap, band) {
                p.line(at, h, 1.0, color.with_alpha(0.5));
                let square = Rect::new(h.x - 3.5, h.y - 3.5, 7.0, 7.0);
                p.fill_rounded(square, 1.5, &Paint::Solid(th.device.dyn_range));
                if c.moved.abs() > 0.05 && !band.is_spectral() {
                    let live = gains.y(
                        g,
                        (gains.db(g, at.y) + c.moved as f32).clamp(-gains.range, gains.range),
                    );
                    p.line(
                        at,
                        Point::new(at.x, live),
                        2.4,
                        th.device.dyn_live.with_alpha(0.9),
                    );
                    p.circle(Point::new(at.x, live), 2.8, th.device.dyn_live);
                }
            }
            if matches!(self.hover, Some(Hit::Node(h)) if h == b) || selected {
                p.circle(at, r + 4.0, color.with_alpha(0.25));
            }
            if band.enabled {
                p.circle(at, r, color);
                p.circle(at, r - 1.4, color.lighten(0.15));
            } else {
                p.circle(at, r, th.device.display);
                p.stroke_path(&Path::circle(at, r - 0.6), 1.2, color.with_alpha(0.7));
            }
            let text = TextStyle::new(
                th.fonts.tiny,
                if band.enabled {
                    th.device.node_text
                } else {
                    color
                },
            )
            .weight(FontWeight::Bold)
            .center();
            p.text(
                &(b + 1).to_string(),
                Rect::new(at.x - r, at.y - r, r * 2.0, r * 2.0),
                &text,
            );
            let mut tag = band.placement.letter().to_string();
            if band.is_spectral() {
                tag.push('~');
            } else if band.dynamic() {
                tag.push('±');
            }
            if !tag.is_empty() {
                p.text(
                    &tag,
                    Rect::new(at.x + r, at.y - r - 9.0, 20.0, 12.0),
                    &TextStyle::new(th.fonts.tiny, color).weight(FontWeight::Bold),
                );
            }
        }
    }

    /// The band whose values show beside it: the one dragged, else hovered.
    pub(crate) fn boxed_band(&self) -> Option<usize> {
        match (&self.drag, self.hover) {
            (Some(Drag::Bands { anchor, .. }), _) => Some(*anchor),
            (Some(Drag::Value { anchor, .. }), _) => Some(*anchor),
            (_, Some(Hit::Node(b) | Hit::Value(b, _) | Hit::Button(b, _) | Hit::Range(b))) => {
                Some(b)
            }
            _ => None,
        }
    }

    fn paint_value_box(&self, p: &mut dyn Painter, l: &Layout, model: &Session, tap: &AnalysisTap) {
        let Some(b) = self.boxed_band() else {
            return;
        };
        if self.grab.is_some() {
            return;
        }
        let th = &self.theme;
        let band = BandParams::read(&tap.params, b);
        if !band.used {
            return;
        }
        let at = self.node_at(model, l, tap, &band);
        let (r, parts, _) = self.value_box(l, at, &band);
        let color = Self::band_color(b);
        p.shadow(r, 4.0, Color::rgba(0.0, 0.0, 0.0, 0.35), 0.0, 2.0, 8.0);
        p.fill_rounded(r, 4.0, &Paint::Solid(th.device.panel.with_alpha(0.95)));
        p.stroke_rounded(r, 4.0, 1.0, color.with_alpha(0.75));
        let scale = Self::scale(tap);
        let piano = self.settings(model).piano;
        for (hit, pr) in parts {
            let hovered = self.hover.is_some_and(|h| match (h, hit) {
                (Hit::Value(_, a), Hit::Value(_, b)) => a == b,
                (Hit::Button(_, a), Hit::Button(_, b)) => a == b,
                _ => false,
            });
            if hovered {
                p.fill_rounded(pr, 3.0, &Paint::Solid(th.ui.selection.with_alpha(0.18)));
            }
            match hit {
                Hit::Value(_, f) => {
                    let text = match f {
                        ValueField::Freq if piano => note_label(band.freq),
                        ValueField::Freq => format_hz(band.freq),
                        ValueField::Gain => super::geometry::db_text(band.gain * scale),
                        ValueField::Q => format!("Q {:.2}", band.q),
                        ValueField::Slope => design::slope_name(band.kind.snap_slope(band.slope)),
                    };
                    p.text(
                        &text,
                        pr,
                        &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
                    );
                }
                Hit::Button(_, which) => {
                    let c = pr.center();
                    match which {
                        NodeButton::Bypass => power_icon(
                            p,
                            c,
                            5.0,
                            if band.enabled {
                                th.ui.text_dim
                            } else {
                                th.device.dyn_range
                            },
                        ),
                        NodeButton::Solo => headphones_icon(
                            p,
                            c,
                            5.5,
                            if self.listening {
                                th.device.curve
                            } else {
                                th.ui.text_dim
                            },
                        ),
                        NodeButton::Delete => cross_icon(p, c, 4.0, th.ui.text_dim),
                        NodeButton::Menu => p.text(
                            "▾",
                            pr,
                            &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
                        ),
                    }
                }
                _ => {}
            }
        }
    }

    /// Spectrum Grab: comes on when the pointer rests on the spectrum.
    fn update_grab(&mut self, model: &Session, l: &Layout, s: &Settings) {
        if !s.grab || !s.post || self.drag.is_some() || self.instances.is_some() {
            if self.drag.is_none() {
                self.grab = None;
            }
            return;
        }
        let Some((at, since)) = self.rest else {
            self.grab = None;
            return;
        };
        if self.grab.is_some() || !matches!(self.hover, Some(Hit::Graph(_))) {
            return;
        }
        if since.elapsed().as_secs_f64() < GRAB_AFTER {
            return;
        }
        let Some(a) = &self.analyser else {
            return;
        };
        let g = &l.graph;
        let axis = self.axis(model);
        let freqs = sweep(&axis, (g.w / 2.0).max(32.0) as usize);
        let levels: Vec<f32> = a
            .post
            .curve(&freqs)
            .iter()
            .zip(&freqs)
            .map(|(d, f)| d + s.tilt * (f / 1000.0).log2() as f32)
            .collect();
        // Only near the spectrum itself.
        let near = freqs.iter().zip(&levels).any(|(f, d)| {
            (axis.x(g, *f) - at.x).abs() < 6.0 && (analyser_y(g, *d, s.range) - at.y).abs() < 22.0
        });
        if near {
            let peaks = analyser::peaks(&freqs, &levels, 6.0, 10);
            if !peaks.is_empty() {
                self.grab = Some(peaks);
            }
        }
    }

    fn paint_peaks(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let Some(peaks) = &self.grab else {
            return;
        };
        let th = &self.theme;
        let g = &l.graph;
        let s = self.settings(model);
        let axis = self.axis(model);
        for (i, (f, d)) in peaks.iter().enumerate() {
            let at = Point::new(axis.x(g, *f), analyser_y(g, *d, s.range));
            let hot = matches!(self.hover, Some(Hit::Peak(hf, _)) if (hf - f).abs() < 1e-9);
            p.circle(
                at,
                if hot { 6.0 } else { 4.5 },
                th.device.post.with_alpha(0.9),
            );
            p.circle(at, 2.0, th.device.display);
            if i < 6 || hot {
                let text = if s.piano {
                    note_label(*f)
                } else {
                    format_hz(*f)
                };
                p.text(
                    &text,
                    Rect::new(at.x - 40.0, at.y - 22.0, 80.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
                );
            }
        }
        p.text(
            "Spectrum Grab: drag a peak",
            Rect::new(g.x + 8.0, g.y + 6.0, 220.0, 14.0),
            &TextStyle::new(th.fonts.tiny, th.ui.text_dim),
        );
    }

    fn paint_axis(&self, p: &mut dyn Painter, l: &Layout, model: &Session) {
        let th = &self.theme;
        let g = &l.graph;
        let a = &l.axis;
        let axis = self.axis(model);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint).center();
        // Labels at 1-2-5 steps, as many as fit.
        let mut decade = 1.0;
        let mut last_x = f32::NEG_INFINITY;
        while decade < axis.hi * 10.0 {
            for m in [1.0, 2.0, 5.0] {
                let f = decade * m;
                if f < axis.lo || f > axis.hi {
                    continue;
                }
                let x = axis.x(g, f);
                if x - last_x < 34.0 {
                    continue;
                }
                last_x = x;
                let text = if f >= 1000.0 {
                    format!("{}k", f / 1000.0)
                } else {
                    format!("{f}")
                };
                p.text(
                    &text,
                    Rect::new(x - 22.0, a.y + 2.0, 44.0, a.h - 4.0),
                    &label,
                );
            }
            decade *= 10.0;
        }
        // The frequency under the pointer.
        if let Some(pos) = self
            .pointer
            .filter(|pos| g.contains(*pos) || a.contains(*pos))
        {
            let f = axis.f(g, pos.x);
            let r = Rect::new(pos.x - 30.0, a.y + 1.0, 60.0, a.h - 2.0);
            p.fill_rounded(r, 3.0, &Paint::Solid(th.device.panel));
            p.text(
                &format_hz(f),
                r,
                &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
            );
        }
    }

    fn paint_piano(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        tap: &AnalysisTap,
        bands: &[(usize, BandParams)],
    ) {
        let th = &self.theme;
        let g = &l.graph;
        let a = &l.axis;
        let axis = self.axis(model);
        p.fill(*a, th.device.key_black.darken(0.2));
        let lo = note_of(axis.lo).floor() as i32;
        let hi = note_of(axis.hi).ceil() as i32;
        let hover_note = self
            .pointer
            .filter(|pos| g.contains(*pos) || a.contains(*pos))
            .map(|pos| note_of(axis.f(g, pos.x)).round() as i32);
        for n in lo..=hi {
            let x0 = axis.x(g, freq_of(f64::from(n) - 0.5)).max(a.x);
            let x1 = axis.x(g, freq_of(f64::from(n) + 0.5)).min(a.right());
            if x1 <= x0 {
                continue;
            }
            // An 88-key piano is lit; the rest is dim.
            let on_piano = (21..=108).contains(&n);
            let hovered = hover_note == Some(n);
            let (color, h) = if is_black(n) {
                (th.device.key_black, a.h * 0.62)
            } else {
                (th.device.key_white, a.h)
            };
            let color = if hovered {
                th.device.curve
            } else if on_piano {
                color
            } else {
                color.mix(th.device.display, 0.6)
            };
            p.fill(Rect::new(x0 + 0.5, a.y, (x1 - x0 - 1.0).max(0.5), h), color);
            if n.rem_euclid(12) == 0 && x1 - x0 > 3.0 {
                p.text(
                    &note_name(n),
                    Rect::new(x0 - 8.0, a.y + a.h - 12.0, 28.0, 11.0),
                    &TextStyle::new(th.fonts.tiny - 1.0, th.device.key_black).center(),
                );
            }
        }
        if let (Some(n), Some(pos)) = (hover_note, self.pointer) {
            let r = Rect::new(pos.x - 22.0, a.y - 16.0, 44.0, 14.0);
            p.fill_rounded(r, 3.0, &Paint::Solid(th.device.panel));
            p.text(
                &note_name(n),
                r,
                &TextStyle::new(th.fonts.tiny, th.ui.text).center(),
            );
        }
        // A dot per band.
        for (b, band) in bands {
            let x = axis.x(g, band.freq);
            if x < a.x || x > a.right() {
                continue;
            }
            p.circle(Point::new(x, a.y + a.h * 0.78), 4.5, Self::band_color(*b));
        }
        let _ = tap;
    }

    fn paint_meter(
        &mut self,
        p: &mut dyn Painter,
        l: &Layout,
        tap: &AnalysisTap,
        dt: f32,
        range: f32,
        mono: bool,
    ) {
        let th = &self.theme;
        let r = l.meter;
        p.fill_rounded(r, 2.0, &Paint::Solid(th.device.display));
        let w = (r.w - 1.0) / 2.0;
        // Take each atomic peak once. A mono strip has one measurement;
        // both bars display that same snapshot, including RMS and decay.
        let peaks = [
            db(tap.meter_out.take_peak(0)),
            db(tap.meter_out.take_peak(1)),
        ];
        for c in 0..2 {
            let channel = if mono { 0 } else { c };
            let peak = peaks[channel];
            self.meter[c] = if mono && c == 1 {
                self.meter[0]
            } else if peak > self.meter[c] {
                peak
            } else {
                (self.meter[c] - 30.0 * dt).max(FLOOR)
            };
            let rms = db_power(tap.meter_out.mean_square(channel));
            let x = r.x + c as f32 * (w + 1.0);
            let y_of = |v: f32| analyser_y(&r, v, range);
            let rms_y = y_of(rms);
            p.fill(
                Rect::new(x, rms_y, w, r.bottom() - rms_y),
                th.tools.level_ok.with_alpha(0.75),
            );
            let py = y_of(self.meter[c]);
            let color = if self.meter[c] > -0.1 {
                th.tools.level_over
            } else if self.meter[c] > -6.0 {
                th.tools.level_warn
            } else {
                th.tools.level_ok
            };
            p.fill(Rect::new(x, py, w, 1.5), color);
        }
    }
}

/// A dashed polyline.
pub(crate) fn dashed(p: &mut dyn Painter, pts: &[Point], width: f32, color: Color) {
    let mut on = true;
    let mut acc = 0.0;
    let mut run: Vec<Point> = Vec::new();
    for w in pts.windows(2) {
        let d = w[0].distance(w[1]);
        if on {
            if run.is_empty() {
                run.push(w[0]);
            }
            run.push(w[1]);
        }
        acc += d;
        if acc > 6.0 {
            acc = 0.0;
            if on && run.len() > 1 {
                p.stroke_path(&Path::polyline(&run), width, color);
            }
            run.clear();
            on = !on;
        }
    }
    if on && run.len() > 1 {
        p.stroke_path(&Path::polyline(&run), width, color);
    }
}

pub(crate) fn power_icon(p: &mut dyn Painter, c: Point, r: f32, color: Color) {
    let mut arc = Path::new();
    arc.arc(
        c,
        r,
        -std::f32::consts::FRAC_PI_2 + 0.7,
        1.5 * std::f32::consts::PI - 0.7,
        false,
    );
    p.stroke_path(&arc, 1.4, color);
    p.line(
        Point::new(c.x, c.y - r - 1.0),
        Point::new(c.x, c.y - 1.0),
        1.4,
        color,
    );
}

pub(crate) fn cross_icon(p: &mut dyn Painter, c: Point, r: f32, color: Color) {
    p.line(
        Point::new(c.x - r, c.y - r),
        Point::new(c.x + r, c.y + r),
        1.4,
        color,
    );
    p.line(
        Point::new(c.x - r, c.y + r),
        Point::new(c.x + r, c.y - r),
        1.4,
        color,
    );
}

pub(crate) fn headphones_icon(p: &mut dyn Painter, c: Point, r: f32, color: Color) {
    let mut arc = Path::new();
    arc.arc(
        Point::new(c.x, c.y + 1.0),
        r,
        std::f32::consts::PI,
        std::f32::consts::TAU,
        false,
    );
    p.stroke_path(&arc, 1.3, color);
    p.fill(Rect::new(c.x - r - 1.0, c.y + 1.0, 2.6, 4.0), color);
    p.fill(Rect::new(c.x + r - 1.6, c.y + 1.0, 2.6, 4.0), color);
}
