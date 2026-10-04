//! The EQ's editor: a frequency response display with a live analyser, the
//! bands as nodes on it, a strip of global settings above and the selected
//! band's controls below.
//!
//! * The analyser shows the spectrum going into the EQ (a faint line) and
//!   coming out (filled), tilted by 4.5 dB per octave round 1 kHz so pink
//!   noise reads flat.
//! * Every band draws its own curve in its colour (the track palette); a
//!   dynamic band shows how far it has moved right now. The bold curve is
//!   the whole EQ. The curves come from the same filter design the audio
//!   uses.
//! * Double-click the display to add a band there (a low cut near the
//!   bottom, a high cut near the top, else a bell). Drag a node: frequency
//!   and gain (Shift: finer, Alt: the Q instead); scroll on it for the Q;
//!   double-click it to bypass it; hold the middle button on it to hear it
//!   on its own; right-click for its type, slope, placement and dynamics;
//!   Delete removes the selected band.
//! * Every change is a parameter edit of the plugin slot, so it is undoable
//!   and automatable like any other.

use crate::common::Device;
use faderframe_analysis::Spectrum;
use faderframe_core::{ParameterId, PluginInstanceId};
use faderframe_plugin_host::eq::design::{self, BandType, Coefs, MAX_SECTIONS, SLOPES};
use faderframe_plugin_host::eq::{
    BANDS, BandParams, Field, Placement, band_id, band_index, format_hz, global, global_id,
    parameters,
};
use faderframe_plugin_host::tap::{AnalysisTap, RING_FRAMES};
use faderframe_project::{Command, TrackColor};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::controls::{self, KnobLook};
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FontWeight, HostRequest, Key, MenuItem, Paint, Painter,
    Path, Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
use std::sync::Arc;

const TOP_H: f32 = 32.0;
const PANEL_H: f32 = 96.0;
const LEFT: f32 = 34.0;
const RIGHT: f32 = 38.0;
const BOTTOM_AXIS: f32 = 18.0;
const NODE_R: f32 = 7.5;
const F_LO: f64 = 10.0;
const F_HI: f64 = 30_000.0;
/// Analyser: dBFS at the bottom of the display (the top is 0 dBFS).
const ANALYSER_FLOOR: f32 = -96.0;
/// Analyser tilt, dB per octave round 1 kHz.
const TILT: f32 = 4.5;
const CURVE_POINTS: usize = 360;
const RANGES: [f32; 4] = [3.0, 6.0, 12.0, 30.0];

/// The bands in use, with their settings.
type UsedBands = Vec<(usize, BandParams)>;

fn band_color(band: usize) -> Color {
    let c = TrackColor::palette(band);
    Color::rgb8(c.r, c.g, c.b)
}

/// The view's layout.
struct Layout {
    top: Rect,
    graph: Rect,
    panel: Rect,
}

/// What the top bar holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopItem {
    Phase,
    AutoGain,
    Scale,
    Output,
    Sidechain,
    Analyser,
    Range,
}

/// The selected band's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelItem {
    Type,
    Slope,
    Placement,
    Knob(Field),
    Listen,
    Bypass,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Node(usize),
    Graph(Point),
    Top(TopItem),
    Panel(PanelItem),
}

enum Drag {
    Node {
        band: usize,
        start: Point,
        freq: f64,
        gain: f64,
        q: f64,
    },
    Knob {
        id: ParameterId,
        index: usize,
        normalized: f32,
        y: f32,
    },
}

/// Which parts of the analyser are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Analyser {
    Off,
    Post,
    PreAndPost,
}

struct Spectra {
    rate: u32,
    pre: Spectrum,
    post: Spectrum,
    seen_pre: u64,
    seen_post: u64,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Spectra {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            pre: Spectrum::new(rate, 8192),
            post: Spectrum::new(rate, 8192),
            seen_pre: 0,
            seen_post: 0,
            left: vec![0.0; RING_FRAMES],
            right: vec![0.0; RING_FRAMES],
        }
    }

    /// Feed what the rings got since the last frame.
    fn update(&mut self, tap: &AnalysisTap) {
        for pre in [true, false] {
            let ring = if pre { &tap.input } else { &tap.output };
            let seen = if pre {
                &mut self.seen_pre
            } else {
                &mut self.seen_post
            };
            let written = ring.written();
            let new = (written.saturating_sub(*seen) as usize).min(RING_FRAMES);
            *seen = written;
            if new == 0 {
                continue;
            }
            ring.latest(&mut self.left[..new], &mut self.right[..new]);
            let s = if pre { &mut self.pre } else { &mut self.post };
            s.process(&self.left[..new], &self.right[..new]);
        }
    }
}

pub struct EqView {
    device: Device,
    theme: Theme,
    selected: Option<usize>,
    hover: Option<usize>,
    drag: Option<Drag>,
    spectra: Option<Spectra>,
    analyser: Analyser,
    range: f32,
    listening: bool,
}

impl EqView {
    pub fn new(plugin: PluginInstanceId, theme: &Theme) -> Self {
        Self {
            device: Device::new(plugin),
            theme: theme.clone(),
            selected: None,
            hover: None,
            drag: None,
            spectra: None,
            analyser: Analyser::PreAndPost,
            range: 12.0,
            listening: false,
        }
    }

    fn layout(size: Size) -> Layout {
        let top = Rect::new(0.0, 0.0, size.w, TOP_H);
        let panel = Rect::new(0.0, size.h - PANEL_H, size.w, PANEL_H);
        let graph = Rect::new(
            LEFT,
            TOP_H + 6.0,
            (size.w - LEFT - RIGHT).max(10.0),
            (size.h - TOP_H - PANEL_H - BOTTOM_AXIS - 6.0).max(10.0),
        );
        Layout { top, graph, panel }
    }

    fn rate(model: &Session) -> f64 {
        f64::from(model.sample_rate().max(8_000))
    }

    fn f_hi(model: &Session) -> f64 {
        F_HI.min(0.5 * Self::rate(model) * 0.999)
    }

    fn x_of(&self, g: &Rect, f: f64, f_hi: f64) -> f32 {
        g.x + g.w * ((f / F_LO).ln() / (f_hi / F_LO).ln()) as f32
    }

    fn f_of(&self, g: &Rect, x: f32, f_hi: f64) -> f64 {
        let t = f64::from(((x - g.x) / g.w).clamp(0.0, 1.0));
        F_LO * (f_hi / F_LO).powf(t)
    }

    fn y_of(&self, g: &Rect, db: f32) -> f32 {
        g.y + g.h * 0.5 - db / self.range * g.h * 0.46
    }

    fn db_of(&self, g: &Rect, y: f32) -> f32 {
        (g.y + g.h * 0.5 - y) / (g.h * 0.46) * self.range
    }

    fn analyser_y(g: &Rect, dbfs: f32) -> f32 {
        g.y + g.h * (dbfs / ANALYSER_FLOOR).clamp(0.0, 1.0)
    }

    fn bands(&self, model: &Session) -> Option<(Arc<AnalysisTap>, UsedBands)> {
        let tap = self.device.tap(model)?;
        let bands = (0..BANDS)
            .map(|b| (b, BandParams::read(&tap.params, b)))
            .filter(|(_, p)| p.used)
            .collect();
        Some((tap, bands))
    }

    fn scale(tap: &AnalysisTap) -> f64 {
        f64::from(tap.params.get(global::GAIN_SCALE))
    }

    /// A band's sections with its live dynamic gain.
    fn sections(
        p: &BandParams,
        scale: f64,
        dynamic: f64,
        rate: f64,
    ) -> ([Coefs; MAX_SECTIONS], usize) {
        let mut shape = p.shape(scale);
        shape.gain += dynamic;
        let mut s = [Coefs::IDENTITY; MAX_SECTIONS];
        let n = design::design(&shape, rate, &mut s);
        (s, n)
    }

    /// Where a band's node sits: its gain for bells and shelves, its level
    /// at its frequency otherwise.
    fn node(&self, g: &Rect, p: &BandParams, scale: f64, rate: f64, f_hi: f64) -> Point {
        let x = self.x_of(g, p.freq, f_hi);
        let db = if p.kind.has_gain() {
            let gain = p.gain * scale;
            if p.kind == BandType::TiltShelf {
                gain * 0.5
            } else {
                gain
            }
        } else {
            design::band_db(&p.shape(scale), rate, p.freq.min(f_hi))
        } as f32;
        let r = self.range;
        Point::new(x, self.y_of(g, db.clamp(-r, r)))
    }

    fn hit(&self, pos: Point, size: Size, model: &Session) -> Option<Hit> {
        let l = Self::layout(size);
        if l.top.contains(pos) {
            return self
                .top_items(&l.top)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(i, _)| Hit::Top(i));
        }
        if l.panel.contains(pos) {
            return self
                .panel_items(&l.panel, model)
                .into_iter()
                .find(|(_, r)| r.contains(pos))
                .map(|(i, _)| Hit::Panel(i));
        }
        let (tap, bands) = self.bands(model)?;
        let (rate, f_hi, scale) = (Self::rate(model), Self::f_hi(model), Self::scale(&tap));
        // The nearest node within reach, the selected one first.
        let near = bands
            .iter()
            .map(|(b, p)| (*b, self.node(&l.graph, p, scale, rate, f_hi).distance(pos)))
            .filter(|(_, d)| *d <= NODE_R + 4.0)
            .min_by(|a, b| {
                let ka = (Some(a.0) != self.selected, a.1);
                let kb = (Some(b.0) != self.selected, b.1);
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some((b, _)) = near {
            return Some(Hit::Node(b));
        }
        let g = l.graph.inset_xy(-4.0, -4.0);
        g.contains(pos).then_some(Hit::Graph(pos))
    }

    fn top_items(&self, top: &Rect) -> Vec<(TopItem, Rect)> {
        let y = top.y + 5.0;
        let h = top.h - 10.0;
        let mut x = top.x + 8.0;
        let mut out = Vec::new();
        for (item, w) in [
            (TopItem::Phase, 112.0),
            (TopItem::AutoGain, 78.0),
            (TopItem::Scale, 96.0),
            (TopItem::Output, 104.0),
            (TopItem::Sidechain, 82.0),
        ] {
            out.push((item, Rect::new(x, y, w, h)));
            x += w + 6.0;
        }
        let mut rx = top.right() - 8.0;
        for (item, w) in [(TopItem::Range, 78.0), (TopItem::Analyser, 128.0)] {
            rx -= w;
            out.push((item, Rect::new(rx.max(x), y, w, h)));
            rx -= 6.0;
        }
        out
    }

    fn panel_items(&self, panel: &Rect, model: &Session) -> Vec<(PanelItem, Rect)> {
        let Some(band) = self.selected else {
            return Vec::new();
        };
        let Some(tap) = self.device.tap(model) else {
            return Vec::new();
        };
        let p = BandParams::read(&tap.params, band);
        let mut out = Vec::new();
        let y = panel.y + 30.0;
        let mut x = panel.x + 104.0;
        for (item, w) in [
            (PanelItem::Type, 96.0),
            (PanelItem::Slope, 86.0),
            (PanelItem::Placement, 74.0),
        ] {
            if item == PanelItem::Slope && !p.kind.has_slope() {
                continue;
            }
            out.push((item, Rect::new(x, y, w, 22.0)));
            x += w + 6.0;
        }
        x = x.max(panel.x + 380.0);
        let mut knobs = vec![Field::Freq];
        if p.kind.has_gain() {
            knobs.push(Field::Gain);
        }
        knobs.push(Field::Q);
        if p.kind.has_gain() {
            knobs.push(Field::Range);
            knobs.push(Field::Threshold);
        }
        for f in knobs {
            out.push((PanelItem::Knob(f), Rect::new(x, panel.y + 10.0, 64.0, 76.0)));
            x += 70.0;
        }
        let mut rx = panel.right() - 10.0;
        for (item, w) in [
            (PanelItem::Remove, 70.0),
            (PanelItem::Bypass, 70.0),
            (PanelItem::Listen, 70.0),
        ] {
            rx -= w;
            out.push((item, Rect::new(rx.max(x), y, w, 22.0)));
            rx -= 6.0;
        }
        out
    }

    // --- edits -----------------------------------------------------------------

    fn set(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: ParameterId, v: f64) {
        self.device.set(model, cx, id, v);
    }

    fn set_once(&self, model: &Session, cx: &mut EventCx<'_, Action>, id: ParameterId, v: f64) {
        self.device.begin(cx, "EQ");
        self.set(model, cx, id, v);
        self.device.end(cx);
    }

    /// An action setting a parameter (for menus).
    fn action(&self, model: &Session, id: ParameterId, value: f64) -> Option<Action> {
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        Some(Action::Edit(Command::SetPluginParameter {
            track,
            plugin: self.device.plugin,
            parameter: id,
            value: Some(value),
        }))
    }

    fn item(
        &self,
        model: &Session,
        label: impl Into<String>,
        id: ParameterId,
        value: f64,
    ) -> MenuItem<Action> {
        let label = label.into();
        match self.action(model, id, value) {
            Some(a) => MenuItem::new(label, a),
            None => MenuItem::disabled(label),
        }
    }

    /// Add a band at a point of the display; returns it.
    fn add_band(
        &mut self,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
        freq: f64,
        gain: f64,
        kind: Option<BandType>,
    ) -> Option<usize> {
        let tap = self.device.tap(model)?;
        let band = (0..BANDS).find(|b| !BandParams::read(&tap.params, *b).used)?;
        let kind = kind.unwrap_or(if freq < 40.0 {
            BandType::LowCut
        } else if freq > 14_000.0 {
            BandType::HighCut
        } else {
            BandType::Bell
        });
        self.device.begin(cx, "Add EQ Band");
        let set = |cx: &mut EventCx<'_, Action>, f: Field, v: f64| {
            self.set(model, cx, band_id(band, f), v);
        };
        set(cx, Field::Type, kind.index() as f64);
        set(cx, Field::Freq, freq);
        set(cx, Field::Gain, if kind.has_gain() { gain } else { 0.0 });
        set(
            cx,
            Field::Q,
            if kind == BandType::Bell { 1.0 } else { 0.707 },
        );
        // 24 dB/oct cuts, 12 dB/oct shelves.
        set(cx, Field::Slope, if kind.is_cut() { 3.0 } else { 1.0 });
        set(cx, Field::Placement, 0.0);
        set(cx, Field::Range, 0.0);
        set(cx, Field::Enabled, 1.0);
        self.device.end(cx);
        self.selected = Some(band);
        Some(band)
    }

    fn remove_band(&mut self, model: &Session, cx: &mut EventCx<'_, Action>, band: usize) {
        self.set_once(model, cx, band_id(band, Field::Enabled), 0.0);
        if self.selected == Some(band) {
            self.selected = None;
        }
    }

    fn band_menu(&self, model: &Session, band: usize, at: Point) -> Option<HostRequest<Action>> {
        let tap = self.device.tap(model)?;
        let p = BandParams::read(&tap.params, band);
        let mut items = Vec::new();
        for (i, t) in BandType::ALL.iter().enumerate() {
            let item = self
                .item(model, t.name(), band_id(band, Field::Type), i as f64)
                .checked(p.kind == *t);
            items.push(item);
        }
        if p.kind.has_slope() {
            for (i, s) in SLOPES.iter().enumerate() {
                if !p.kind.is_cut() && *s > 12 {
                    continue;
                }
                let item = self
                    .item(
                        model,
                        format!("{s} dB/oct"),
                        band_id(band, Field::Slope),
                        i as f64,
                    )
                    .checked(p.slope == *s);
                items.push(if i == 0 { item.separated() } else { item });
            }
        }
        for (i, pl) in Placement::ALL.iter().enumerate() {
            let item = self
                .item(model, pl.name(), band_id(band, Field::Placement), i as f64)
                .checked(p.placement == *pl);
            items.push(if i == 0 { item.separated() } else { item });
        }
        if p.kind.has_gain() {
            let dynamic = p.dynamic();
            items.push(
                self.item(
                    model,
                    "Dynamic",
                    band_id(band, Field::Range),
                    if dynamic { 0.0 } else { -6.0 },
                )
                .checked(dynamic)
                .separated(),
            );
        }
        items.push(
            self.item(
                model,
                "Bypass",
                band_id(band, Field::Enabled),
                if p.enabled { 2.0 } else { 1.0 },
            )
            .checked(!p.enabled)
            .separated(),
        );
        items.push(self.item(model, "Delete Band", band_id(band, Field::Enabled), 0.0));
        Some(HostRequest::ContextMenu { at, items })
    }

    fn add_menu(&self, model: &Session, pos: Point, size: Size) -> Option<HostRequest<Action>> {
        let l = Self::layout(size);
        let freq = self.f_of(&l.graph, pos.x, Self::f_hi(model));
        let gain = f64::from(self.db_of(&l.graph, pos.y));
        let tap = self.device.tap(model)?;
        let band = (0..BANDS).find(|b| !BandParams::read(&tap.params, *b).used)?;
        let (track, _) = model.plugin_owner(self.device.plugin)?;
        let mut items = vec![MenuItem::disabled(format!("Add at {}", format_hz(freq)))];
        for t in BandType::ALL {
            let mut commands = Vec::new();
            for (f, v) in [
                (Field::Type, t.index() as f64),
                (Field::Freq, freq),
                (Field::Gain, if t.has_gain() { gain } else { 0.0 }),
                (Field::Q, if t == BandType::Bell { 1.0 } else { 0.707 }),
                (Field::Slope, if t.is_cut() { 3.0 } else { 1.0 }),
                (Field::Placement, 0.0),
                (Field::Range, 0.0),
                (Field::Enabled, 1.0),
            ] {
                commands.push(Command::SetPluginParameter {
                    track,
                    plugin: self.device.plugin,
                    parameter: band_id(band, f),
                    value: Some(v),
                });
            }
            items.push(MenuItem::new(
                t.name(),
                Action::Edit(Command::Batch {
                    label: "Add EQ Band".into(),
                    commands,
                }),
            ));
        }
        Some(HostRequest::ContextMenu { at: pos, items })
    }

    // --- painting --------------------------------------------------------------

    fn paint_grid(&self, p: &mut dyn Painter, g: &Rect, f_hi: f64) {
        let th = &self.theme;
        let line = th.ui.text.with_alpha(0.06);
        let strong = th.ui.text.with_alpha(0.14);
        let label = TextStyle::new(th.fonts.tiny, th.ui.text_faint);
        for (f, text) in [
            (20.0, "20"),
            (50.0, "50"),
            (100.0, "100"),
            (200.0, "200"),
            (500.0, "500"),
            (1_000.0, "1k"),
            (2_000.0, "2k"),
            (5_000.0, "5k"),
            (10_000.0, "10k"),
            (20_000.0, "20k"),
        ] {
            if f >= f_hi {
                continue;
            }
            let x = self.x_of(g, f, f_hi);
            p.vline(x, g.y, g.bottom(), line);
            p.text(
                text,
                Rect::new(x - 20.0, g.bottom() + 2.0, 40.0, BOTTOM_AXIS - 4.0),
                &label.center(),
            );
        }
        // Minor lines at 30, 40, 60...
        let mut decade = 10.0;
        while decade < f_hi {
            for m in [3.0, 4.0, 6.0, 7.0, 8.0, 9.0] {
                let f = decade * m;
                if f < f_hi && f > F_LO {
                    p.vline(
                        self.x_of(g, f, f_hi),
                        g.y,
                        g.bottom(),
                        line.with_alpha(0.03),
                    );
                }
            }
            decade *= 10.0;
        }
        let step = match self.range as i32 {
            3 => 1.0,
            6 => 2.0,
            12 => 3.0,
            _ => 6.0,
        };
        let mut db = -self.range;
        while db <= self.range + 0.01 {
            let y = self.y_of(g, db);
            p.hline(g.x, g.right(), y, if db == 0.0 { strong } else { line });
            let text = if db > 0.0 {
                format!("+{db:.0}")
            } else {
                format!("{db:.0}")
            };
            p.text(
                &text.replace('-', "−"),
                Rect::new(2.0, y - 7.0, LEFT - 6.0, 14.0),
                &label.right(),
            );
            db += step;
        }
        // The analyser's own scale on the right.
        if self.analyser != Analyser::Off {
            for dbfs in [-12.0, -24.0, -36.0, -48.0, -60.0, -72.0, -84.0] {
                let y = Self::analyser_y(g, dbfs);
                p.text(
                    &format!("{dbfs:.0}").replace('-', "−"),
                    Rect::new(g.right() + 4.0, y - 7.0, RIGHT - 6.0, 14.0),
                    &TextStyle::new(th.fonts.tiny, th.ui.text_faint.with_alpha(0.6)),
                );
            }
        }
    }

    fn paint_analyser(
        &mut self,
        p: &mut dyn Painter,
        g: &Rect,
        tap: &AnalysisTap,
        model: &Session,
    ) {
        if self.analyser == Analyser::Off {
            return;
        }
        let rate = model.sample_rate().max(8_000);
        if self.spectra.as_ref().is_none_or(|s| s.rate != rate) {
            self.spectra = Some(Spectra::new(rate));
        }
        let Some(spectra) = self.spectra.as_mut() else {
            return;
        };
        spectra.update(tap);
        let f_hi = Self::f_hi(model);
        let points = (g.w / 2.0).max(16.0) as usize;
        let shape = |levels: &[f32]| -> Vec<Point> {
            levels
                .iter()
                .enumerate()
                .map(|(i, db)| {
                    let t = (i as f32 + 0.5) / levels.len() as f32;
                    let f = F_LO * (f_hi / F_LO).powf(f64::from(t));
                    let tilted = db + TILT * (f / 1000.0).log2() as f32;
                    Point::new(g.x + g.w * t, Self::analyser_y(g, tilted))
                })
                .collect()
        };
        let th = &self.theme;
        let (post, _) = spectra.post.curve(points, F_LO, f_hi);
        let post = shape(&post);
        if let (Some(first), Some(last)) = (post.first(), post.last()) {
            let mut fill = Path::polyline(&post);
            fill.line_to(Point::new(last.x, g.bottom()))
                .line_to(Point::new(first.x, g.bottom()))
                .close();
            p.fill_path_paint(
                &fill,
                &Paint::vertical(*g, th.ui.text.with_alpha(0.16), th.ui.text.with_alpha(0.04)),
            );
            p.stroke_path(&Path::polyline(&post), 1.0, th.ui.text.with_alpha(0.30));
        }
        if self.analyser == Analyser::PreAndPost {
            let (pre, _) = spectra.pre.curve(points, F_LO, f_hi);
            p.stroke_path(
                &Path::polyline(&shape(&pre)),
                1.0,
                th.ui.accent.with_alpha(0.35),
            );
        }
    }

    fn paint_curves(&self, p: &mut dyn Painter, g: &Rect, model: &Session) {
        let Some((tap, bands)) = self.bands(model) else {
            return;
        };
        let (rate, f_hi, scale) = (Self::rate(model), Self::f_hi(model), Self::scale(&tap));
        let th = &self.theme;
        let n = CURVE_POINTS.min((g.w as usize).max(16));
        let freqs: Vec<f64> = (0..n)
            .map(|i| F_LO * (f_hi / F_LO).powf(i as f64 / (n - 1) as f64))
            .collect();
        let mut total = vec![0.0f64; n];
        let zero = self.y_of(g, 0.0);
        p.push_clip(*g);
        // Linear phase plays the analog curves exactly (dynamic bands stay
        // minimum phase).
        let linear = tap.params.get(global::PHASE) >= 0.5;
        for &(b, ref band) in &bands {
            let dynamic = f64::from(tap.value(b));
            let (s, k) = Self::sections(band, scale, dynamic, rate);
            let db: Vec<f64> = if linear && !band.dynamic() {
                let shape = band.shape(scale);
                freqs
                    .iter()
                    .map(|&f| design::analog_db(&shape, f))
                    .collect()
            } else {
                freqs
                    .iter()
                    .map(|&f| design::sections_db(&s[..k], rate, f))
                    .collect()
            };
            if band.enabled {
                for (t, d) in total.iter_mut().zip(&db) {
                    *t += d;
                }
            }
            let color = if band.enabled {
                band_color(b)
            } else {
                th.ui.text_faint
            };
            let pts: Vec<Point> = freqs
                .iter()
                .zip(&db)
                .map(|(&f, &d)| {
                    Point::new(
                        self.x_of(g, f, f_hi),
                        self.y_of(g, (d as f32).clamp(-self.range * 2.0, self.range * 2.0)),
                    )
                })
                .collect();
            let selected = self.selected == Some(b);
            if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
                let mut fill = Path::polyline(&pts);
                fill.line_to(Point::new(last.x, zero))
                    .line_to(Point::new(first.x, zero))
                    .close();
                let alpha = match (band.enabled, selected) {
                    (true, true) => 0.28,
                    (true, false) => 0.12,
                    (false, _) => 0.05,
                };
                p.fill_path(&fill, color.with_alpha(alpha));
            }
            p.stroke_path(
                &Path::polyline(&pts),
                if selected { 1.6 } else { 1.0 },
                color.with_alpha(if band.enabled { 0.8 } else { 0.35 }),
            );
            // A dynamic band: where it rests, dashed.
            if band.dynamic() && dynamic.abs() > 0.05 {
                let (s0, k0) = Self::sections(band, scale, 0.0, rate);
                let rest: Vec<Point> = freqs
                    .iter()
                    .step_by(2)
                    .map(|&f| {
                        let d = design::sections_db(&s0[..k0], rate, f) as f32;
                        Point::new(self.x_of(g, f, f_hi), self.y_of(g, d))
                    })
                    .collect();
                for seg in rest.chunks(2) {
                    p.stroke_path(&Path::polyline(seg), 1.0, color.with_alpha(0.5));
                }
            }
        }
        let pts: Vec<Point> = freqs
            .iter()
            .zip(&total)
            .map(|(&f, &d)| {
                Point::new(
                    self.x_of(g, f, f_hi),
                    self.y_of(g, (d as f32).clamp(-self.range * 2.0, self.range * 2.0)),
                )
            })
            .collect();
        p.stroke_path(&Path::polyline(&pts), 2.2, th.ui.text.with_alpha(0.92));
        p.pop_clip();
        // The nodes on top.
        for &(b, ref band) in &bands {
            let at = self.node(g, band, scale, rate, f_hi);
            let color = band_color(b);
            let selected = self.selected == Some(b);
            let r = if selected { NODE_R + 1.5 } else { NODE_R };
            if self.hover == Some(b) || selected {
                p.circle(at, r + 4.0, color.with_alpha(0.25));
            }
            if band.enabled {
                p.circle(at, r, color);
                p.circle(at, r - 1.4, color.lighten(0.15));
            } else {
                p.circle(at, r, th.ui.background);
                p.stroke_path(&Path::circle(at, r - 0.6), 1.2, color.with_alpha(0.7));
            }
            let text = TextStyle::new(
                th.fonts.tiny,
                if band.enabled {
                    Color::rgb(0.08, 0.08, 0.09)
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
            let letter = band.placement.letter();
            if !letter.is_empty() {
                p.text(
                    letter,
                    Rect::new(at.x + r, at.y - r - 9.0, 14.0, 12.0),
                    &TextStyle::new(th.fonts.tiny, color).weight(FontWeight::Bold),
                );
            }
            // A dynamic band's live movement.
            let dynamic = tap.value(b);
            if band.dynamic() && dynamic.abs() > 0.05 {
                let to = self.y_of(
                    g,
                    (self.db_of(g, at.y) + dynamic).clamp(-self.range, self.range),
                );
                p.line(
                    Point::new(at.x, at.y),
                    Point::new(at.x, to),
                    2.0,
                    color.with_alpha(0.8),
                );
                p.circle(Point::new(at.x, to), 3.0, color);
            }
        }
        // A readout by the hovered or dragged node.
        let shown = match &self.drag {
            Some(Drag::Node { band, .. }) => Some(*band),
            _ => self.hover,
        };
        if let Some((b, band)) = shown.and_then(|s| bands.iter().find(|(b, _)| *b == s)) {
            let at = self.node(g, band, scale, rate, f_hi);
            let mut text = format!("{}  ", format_hz(band.freq));
            if band.kind.has_gain() {
                text += &format!("{:+.1} dB  ", band.gain * scale).replace('-', "−");
            }
            text += &format!("Q {:.2}", band.q);
            let w = text.chars().count() as f32 * 6.2 + 14.0;
            let mut r = Rect::new(at.x + 12.0, at.y - 28.0, w, 18.0);
            if r.right() > g.right() {
                r.x = at.x - 12.0 - w;
            }
            if r.y < g.y {
                r.y = at.y + 12.0;
            }
            p.fill_rounded(r, 3.0, &Paint::Solid(th.ui.surface.with_alpha(0.92)));
            p.stroke_rounded(r, 3.0, 1.0, band_color(*b).with_alpha(0.7));
            p.text(
                &text,
                r,
                &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
            );
        }
    }

    fn top_label(&self, item: TopItem, model: &Session) -> (String, bool) {
        let v = |g: usize| self.device.value(model, g);
        match item {
            TopItem::Phase => (
                if v(global::PHASE) >= 0.5 {
                    "Linear Phase ▾".into()
                } else {
                    "Zero Latency ▾".into()
                },
                false,
            ),
            TopItem::AutoGain => ("Auto Gain".into(), v(global::AUTO_GAIN) >= 0.5),
            TopItem::Scale => (
                format!("Scale {:.0} %", v(global::GAIN_SCALE) * 100.0),
                false,
            ),
            TopItem::Output => (
                format!("Output {:+.1} dB", v(global::OUTPUT)).replace('-', "−"),
                false,
            ),
            TopItem::Sidechain => ("Sidechain".into(), v(global::SIDECHAIN) >= 0.5),
            TopItem::Analyser => (
                match self.analyser {
                    Analyser::Off => "Analyser Off ▾",
                    Analyser::Post => "Analyser Post ▾",
                    Analyser::PreAndPost => "Analyser Pre + Post ▾",
                }
                .into(),
                false,
            ),
            TopItem::Range => (format!("±{:.0} dB ▾", self.range), false),
        }
    }

    fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool) {
        let th = &self.theme;
        let bg = if on {
            th.ui.accent.with_alpha(0.35)
        } else {
            th.ui.surface_alt
        };
        p.fill_rounded(r, 4.0, &Paint::Solid(bg));
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        p.text(
            label,
            r,
            &TextStyle::new(th.fonts.small, th.ui.text).center(),
        );
    }

    fn paint_top(&self, p: &mut dyn Painter, top: &Rect, model: &Session) {
        let th = &self.theme;
        p.fill(*top, th.ui.surface);
        p.hline(0.0, top.w, top.bottom() - 0.5, th.ui.border);
        for (item, r) in self.top_items(top) {
            let (label, on) = self.top_label(item, model);
            self.button(p, r, &label, on);
        }
    }

    fn knob_spec(field: Field) -> (f64, f64, bool) {
        match field {
            Field::Freq => (10.0, 30_000.0, true),
            Field::Q => (0.025, 40.0, true),
            Field::Gain | Field::Range => (-30.0, 30.0, false),
            Field::Threshold => (-80.0, 0.0, false),
            _ => (0.0, 1.0, false),
        }
    }

    fn to_normalized(field: Field, v: f64) -> f32 {
        let (lo, hi, log) = Self::knob_spec(field);
        let t = if log {
            (v.max(lo) / lo).ln() / (hi / lo).ln()
        } else {
            (v - lo) / (hi - lo)
        };
        t.clamp(0.0, 1.0) as f32
    }

    fn from_normalized(field: Field, t: f32) -> f64 {
        let (lo, hi, log) = Self::knob_spec(field);
        let t = f64::from(t.clamp(0.0, 1.0));
        if log {
            lo * (hi / lo).powf(t)
        } else {
            lo + t * (hi - lo)
        }
    }

    fn paint_panel(&self, p: &mut dyn Painter, panel: &Rect, model: &Session) {
        let th = &self.theme;
        controls::panel(p, *panel, th.console.panel_top, th.console.panel_bottom, th);
        p.hline(0.0, panel.w, panel.y + 0.5, th.ui.border);
        let Some(tap) = self.device.tap(model) else {
            return;
        };
        let Some(band) = self
            .selected
            .filter(|b| BandParams::read(&tap.params, *b).used)
        else {
            p.text(
                "Double-click the display to add a band · right-click for a choice of types",
                panel.inset(16.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
            return;
        };
        let bp = BandParams::read(&tap.params, band);
        let color = band_color(band);
        let swatch = Rect::new(panel.x + 14.0, panel.y + 30.0, 22.0, 22.0);
        p.fill_rounded(swatch, 11.0, &Paint::Solid(color));
        p.text(
            &format!("Band {}", band + 1),
            Rect::new(swatch.right() + 8.0, swatch.y, 60.0, 22.0),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        for (item, r) in self.panel_items(panel, model) {
            match item {
                PanelItem::Type => self.button(p, r, &format!("{} ▾", bp.kind.name()), false),
                PanelItem::Slope => self.button(p, r, &format!("{} dB/oct ▾", bp.slope), false),
                PanelItem::Placement => {
                    self.button(p, r, &format!("{} ▾", bp.placement.name()), false)
                }
                PanelItem::Listen => self.button(p, r, "Listen", self.listening),
                PanelItem::Bypass => self.button(p, r, "Bypass", !bp.enabled),
                PanelItem::Remove => self.button(p, r, "Remove", false),
                PanelItem::Knob(field) => {
                    let v = self.device.value(model, band_index(band, field));
                    let knob_r = Rect::new(r.x + 12.0, r.y + 14.0, 40.0, 40.0);
                    let bipolar = matches!(field, Field::Gain | Field::Range);
                    let dim = field == Field::Threshold && !bp.dynamic();
                    controls::knob(
                        p,
                        knob_r,
                        Self::to_normalized(field, v),
                        bipolar,
                        KnobLook {
                            cap: th.console.knob.cap_top,
                            ring: if dim { th.ui.text_faint } else { color },
                        },
                        th,
                    );
                    let name = match field {
                        Field::Freq => "FREQ",
                        Field::Gain => "GAIN",
                        Field::Q => "Q",
                        Field::Range => "DYN",
                        Field::Threshold => "THRESH",
                        _ => "",
                    };
                    p.text(
                        name,
                        Rect::new(r.x, r.y, r.w, 14.0),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                            .bold()
                            .center()
                            .tracking(0.6),
                    );
                    let text = match field {
                        Field::Freq => format_hz(v),
                        Field::Q => format!("{v:.2}"),
                        Field::Range if v.abs() < 0.05 => "Off".into(),
                        _ => format!("{v:+.1} dB").replace('-', "−"),
                    };
                    p.text(
                        &text,
                        Rect::new(r.x - 4.0, r.bottom() - 16.0, r.w + 8.0, 14.0),
                        &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
                    );
                }
            }
        }
    }

    fn knob_at(&self, pos: Point, size: Size, model: &Session) -> Option<Field> {
        match self.hit(pos, size, model)? {
            Hit::Panel(PanelItem::Knob(f)) => Some(f),
            _ => None,
        }
    }

    fn listen(&mut self, model: &Session, band: Option<usize>) {
        if let Some(tap) = self.device.tap(model) {
            tap.set_listen(band);
        }
        self.listening = band.is_some();
    }
}

impl CanvasView<Session, Action> for EqView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let l = Self::layout(size);
        let th = self.theme.clone();
        p.fill(Rect::from_size(size), th.ui.background.darken(0.15));
        let Some(tap) = self.device.tap(model) else {
            p.text(
                "The EQ is not loaded",
                l.graph,
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            return;
        };
        tap.watch();
        let f_hi = Self::f_hi(model);
        self.paint_grid(p, &l.graph, f_hi);
        p.push_clip(l.graph);
        self.paint_analyser(p, &l.graph, &tap, model);
        p.pop_clip();
        self.paint_curves(p, &l.graph, model);
        self.paint_top(p, &l.top, model);
        self.paint_panel(p, &l.panel, model);
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = Self::layout(size);
        match ev {
            ViewEvent::PointerDown {
                pos,
                button,
                clicks,
                ..
            } => {
                cx.request(HostRequest::GrabFocus);
                let Some(hit) = self.hit(*pos, size, model) else {
                    return false;
                };
                match (hit, button) {
                    (Hit::Node(b), PointerButton::Primary) => {
                        self.selected = Some(b);
                        let Some(tap) = self.device.tap(model) else {
                            return true;
                        };
                        let p = BandParams::read(&tap.params, b);
                        if *clicks >= 2 {
                            self.set_once(
                                model,
                                cx,
                                band_id(b, Field::Enabled),
                                if p.enabled { 2.0 } else { 1.0 },
                            );
                        } else {
                            self.device.begin(cx, "EQ Band");
                            self.drag = Some(Drag::Node {
                                band: b,
                                start: *pos,
                                freq: p.freq,
                                gain: p.gain,
                                q: p.q,
                            });
                        }
                    }
                    (Hit::Node(b), PointerButton::Middle) => {
                        self.selected = Some(b);
                        self.listen(model, Some(b));
                    }
                    (Hit::Node(b), PointerButton::Secondary) => {
                        self.selected = Some(b);
                        if let Some(req) = self.band_menu(model, b, *pos) {
                            cx.request(req);
                        }
                    }
                    (Hit::Graph(at), PointerButton::Primary) => {
                        if *clicks >= 2 {
                            let freq = self.f_of(&l.graph, at.x, Self::f_hi(model));
                            let gain = f64::from(self.db_of(&l.graph, at.y)).clamp(-30.0, 30.0);
                            self.add_band(model, cx, freq, (gain * 10.0).round() / 10.0, None);
                        } else {
                            self.selected = None;
                        }
                    }
                    (Hit::Graph(at), PointerButton::Secondary) => {
                        if let Some(req) = self.add_menu(model, at, size) {
                            cx.request(req);
                        }
                    }
                    (Hit::Top(item), PointerButton::Primary) => {
                        let v = |g| self.device.value(model, g);
                        let flip = |g| if v(g) >= 0.5 { 0.0 } else { 1.0 };
                        let at = self
                            .top_items(&l.top)
                            .iter()
                            .find(|(i, _)| *i == item)
                            .map_or(*pos, |(_, r)| Point::new(r.x, r.bottom()));
                        match item {
                            TopItem::AutoGain => self.set_once(
                                model,
                                cx,
                                global_id(global::AUTO_GAIN),
                                flip(global::AUTO_GAIN),
                            ),
                            TopItem::Sidechain => self.set_once(
                                model,
                                cx,
                                global_id(global::SIDECHAIN),
                                flip(global::SIDECHAIN),
                            ),
                            TopItem::Phase => {
                                let linear = v(global::PHASE) >= 0.5;
                                let mut items = vec![
                                    self.item(model, "Zero Latency", global_id(global::PHASE), 0.0)
                                        .checked(!linear),
                                    self.item(model, "Linear Phase", global_id(global::PHASE), 1.0)
                                        .checked(linear),
                                ];
                                let q = v(global::QUALITY).round() as usize;
                                for (i, name) in
                                    ["Low", "Medium", "High", "Maximum"].iter().enumerate()
                                {
                                    let item = self
                                        .item(
                                            model,
                                            format!("Linear Phase Quality: {name}"),
                                            global_id(global::QUALITY),
                                            i as f64,
                                        )
                                        .checked(q == i);
                                    items.push(if i == 0 { item.separated() } else { item });
                                }
                                cx.request(HostRequest::ContextMenu { at, items });
                            }
                            TopItem::Scale | TopItem::Output => {
                                let (index, field) = if item == TopItem::Scale {
                                    (global::GAIN_SCALE, 0.0)
                                } else {
                                    (global::OUTPUT, 1.0)
                                };
                                if *clicks >= 2 {
                                    let d = if field == 0.0 { 1.0 } else { 0.0 };
                                    self.set_once(model, cx, global_id(index), d);
                                } else {
                                    self.device.begin(cx, "EQ");
                                    let info = &parameters()[index];
                                    let t = ((v(index) - info.min) / (info.max - info.min)) as f32;
                                    self.drag = Some(Drag::Knob {
                                        id: global_id(index),
                                        index,
                                        normalized: t,
                                        y: pos.y,
                                    });
                                }
                            }
                            TopItem::Analyser | TopItem::Range => {
                                // View settings: cycle through them.
                                if item == TopItem::Analyser {
                                    self.analyser = match self.analyser {
                                        Analyser::PreAndPost => Analyser::Post,
                                        Analyser::Post => Analyser::Off,
                                        Analyser::Off => Analyser::PreAndPost,
                                    };
                                } else {
                                    let i =
                                        RANGES.iter().position(|r| *r == self.range).unwrap_or(2);
                                    self.range = RANGES[(i + 1) % RANGES.len()];
                                }
                                cx.redraw();
                            }
                        }
                    }
                    (Hit::Panel(item), PointerButton::Primary) => {
                        let Some(band) = self.selected else {
                            return true;
                        };
                        let r = self
                            .panel_items(&l.panel, model)
                            .iter()
                            .find(|(i, _)| *i == item)
                            .map_or(*pos, |(_, r)| Point::new(r.x, r.bottom()));
                        let Some(tap) = self.device.tap(model) else {
                            return true;
                        };
                        let bp = BandParams::read(&tap.params, band);
                        match item {
                            PanelItem::Type | PanelItem::Slope | PanelItem::Placement => {
                                if let Some(HostRequest::ContextMenu { items, .. }) =
                                    self.band_menu(model, band, r)
                                {
                                    // Just the part asked for.
                                    let (from, to) = match item {
                                        PanelItem::Type => (0, BandType::ALL.len()),
                                        PanelItem::Slope => {
                                            let n = SLOPES
                                                .iter()
                                                .filter(|s| bp.kind.is_cut() || **s <= 12)
                                                .count();
                                            (BandType::ALL.len(), BandType::ALL.len() + n)
                                        }
                                        _ => {
                                            let skip = BandType::ALL.len()
                                                + if bp.kind.has_slope() {
                                                    SLOPES
                                                        .iter()
                                                        .filter(|s| bp.kind.is_cut() || **s <= 12)
                                                        .count()
                                                } else {
                                                    0
                                                };
                                            (skip, skip + Placement::ALL.len())
                                        }
                                    };
                                    let items: Vec<_> = items
                                        .into_iter()
                                        .skip(from)
                                        .take(to - from)
                                        .map(|mut i| {
                                            i.separator_before = false;
                                            i
                                        })
                                        .collect();
                                    cx.request(HostRequest::ContextMenu { at: r, items });
                                }
                            }
                            PanelItem::Listen => self.listen(model, Some(band)),
                            PanelItem::Bypass => self.set_once(
                                model,
                                cx,
                                band_id(band, Field::Enabled),
                                if bp.enabled { 2.0 } else { 1.0 },
                            ),
                            PanelItem::Remove => self.remove_band(model, cx, band),
                            PanelItem::Knob(field) => {
                                let index = band_index(band, field);
                                if *clicks >= 2 {
                                    let d = parameters()[index].default;
                                    self.set_once(model, cx, band_id(band, field), d);
                                } else {
                                    self.device.begin(cx, "EQ Band");
                                    let v = self.device.value(model, index);
                                    self.drag = Some(Drag::Knob {
                                        id: band_id(band, field),
                                        index,
                                        normalized: Self::to_normalized(field, v),
                                        y: pos.y,
                                    });
                                }
                            }
                        }
                    }
                    _ => return false,
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging,
            } => {
                if !*dragging {
                    let hover = match self.hit(*pos, size, model) {
                        Some(Hit::Node(b)) => Some(b),
                        _ => None,
                    };
                    if hover != self.hover {
                        self.hover = hover;
                        cx.redraw();
                    }
                    cx.set_cursor(if hover.is_some() {
                        Cursor::Grab
                    } else {
                        Cursor::Default
                    });
                    return hover.is_some();
                }
                let fine = if modifiers.shift { 0.2 } else { 1.0 };
                match &mut self.drag {
                    Some(Drag::Node {
                        band,
                        start,
                        freq,
                        gain,
                        q,
                    }) => {
                        let (band, start, freq0, gain0, q0) = (*band, *start, *freq, *gain, *q);
                        let Some(tap) = self.device.tap(model) else {
                            return true;
                        };
                        let p = BandParams::read(&tap.params, band);
                        let f_hi = Self::f_hi(model);
                        let g = &l.graph;
                        let dx = (pos.x - start.x) * fine;
                        let dy = (pos.y - start.y) * fine;
                        if modifiers.alt {
                            // Up: narrower.
                            let q = (q0 * 2f64.powf(f64::from(-dy) / 60.0)).clamp(0.025, 40.0);
                            self.set(model, cx, band_id(band, Field::Q), q);
                        } else {
                            let x0 = self.x_of(g, freq0, f_hi);
                            let f = self.f_of(g, x0 + dx, f_hi).clamp(10.0, 30_000.0);
                            self.set(model, cx, band_id(band, Field::Freq), f);
                            if p.kind.has_gain() {
                                let scale = Self::scale(&tap).max(0.01);
                                let db_per_px = f64::from(self.range / (g.h * 0.46));
                                let gn =
                                    (gain0 - f64::from(dy) * db_per_px / scale).clamp(-30.0, 30.0);
                                self.set(model, cx, band_id(band, Field::Gain), gn);
                            }
                        }
                        cx.set_cursor(Cursor::Grabbing);
                        true
                    }
                    Some(Drag::Knob {
                        id,
                        index,
                        normalized,
                        y,
                    }) => {
                        let (id, index) = (*id, *index);
                        *normalized = (*normalized + (*y - pos.y) / 200.0 * fine).clamp(0.0, 1.0);
                        *y = pos.y;
                        let t = *normalized;
                        let v = if index < faderframe_plugin_host::eq::GLOBALS {
                            let info = &parameters()[index];
                            info.min + f64::from(t) * (info.max - info.min)
                        } else {
                            let field = Field::ALL[(index - faderframe_plugin_host::eq::GLOBALS)
                                % faderframe_plugin_host::eq::FIELDS];
                            Self::from_normalized(field, t)
                        };
                        self.set(model, cx, id, v);
                        true
                    }
                    None => false,
                }
            }
            ViewEvent::PointerUp { button, .. } => {
                if *button == PointerButton::Middle || self.listening {
                    self.listen(model, None);
                    cx.redraw();
                }
                if self.drag.take().is_some() {
                    self.device.end(cx);
                    cx.redraw();
                    return true;
                }
                false
            }
            ViewEvent::Scroll {
                pos, dy, modifiers, ..
            } => {
                let step: f64 = if modifiers.shift { 0.02 } else { 0.1 };
                if let Some(field) = self.knob_at(*pos, size, model) {
                    let Some(band) = self.selected else {
                        return false;
                    };
                    let index = band_index(band, field);
                    let t = Self::to_normalized(field, self.device.value(model, index));
                    let v = Self::from_normalized(field, t - dy.signum() * step as f32 * 0.2);
                    self.set_once(model, cx, band_id(band, field), v);
                    return true;
                }
                let band = match self.hit(*pos, size, model) {
                    Some(Hit::Node(b)) => Some(b),
                    Some(Hit::Graph(_)) => self.selected,
                    _ => None,
                };
                let Some(band) = band else {
                    return false;
                };
                let q = self.device.value(model, band_index(band, Field::Q));
                let q = (q * (1.0f64 + step).powf(f64::from(-dy.signum()))).clamp(0.025, 40.0);
                self.set_once(model, cx, band_id(band, Field::Q), q);
                true
            }
            ViewEvent::Key { key, .. } => match (key, self.selected) {
                (Key::Delete | Key::Backspace, Some(b)) => {
                    self.remove_band(model, cx, b);
                    true
                }
                (Key::Escape, Some(_)) => {
                    self.selected = None;
                    cx.redraw();
                    true
                }
                _ => false,
            },
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            _ => false,
        }
    }

    fn wants_frames(&self, _model: &Session) -> bool {
        self.analyser != Analyser::Off
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        Some(
            match self.hit(pos, size, model)? {
                Hit::Top(TopItem::Phase) => "Zero latency (minimum phase, the analog curves) or linear phase (no phase shift, adds latency)",
                Hit::Top(TopItem::AutoGain) => "Keep the loudness of pink noise where it was",
                Hit::Top(TopItem::Scale) => "Scale every band's gain — drag, double-click for 100 %",
                Hit::Top(TopItem::Output) => "Output gain — drag, double-click for 0 dB",
                Hit::Top(TopItem::Sidechain) => "Key the dynamic bands from the sidechain input",
                Hit::Top(TopItem::Analyser) => "What the analyser shows (click to change)",
                Hit::Top(TopItem::Range) => "The display's gain range (click to change)",
                Hit::Panel(PanelItem::Listen) => "Hold to hear this band's region on its own (also: hold the middle button on a node)",
                Hit::Panel(PanelItem::Knob(Field::Range)) => "Dynamic range: how far the band moves as its region gets louder than the threshold",
                Hit::Node(_) => "Drag: frequency and gain (Shift finer, Alt: Q) · scroll: Q · double-click: bypass · right-click: more · middle: listen",
                Hit::Graph(_) => "Double-click to add a band · right-click to choose its type",
                _ => return None,
            }
            .into(),
        )
    }

    fn min_size(&self) -> Size {
        Size::new(640.0, 300.0)
    }
}
