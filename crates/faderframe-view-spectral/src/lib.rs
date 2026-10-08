//! The spectral editor: an audio clip's spectrogram (its source over the
//! clip's region, a logarithmic frequency axis), regions drawn on it — a
//! rectangle, a time range, a frequency band, a lasso, a brush stroke —
//! and turned into edits with Attenuate (down to the sound around it),
//! Heal (replaced by it), Remove or Gain. Edits show as outlines: a click
//! selects one, a drag moves it, Delete removes it; the buttons change the
//! selected one's operation, the Soft values its feather. "Original" shows
//! the unedited audio. Ctrl+wheel zooms in time, the wheel scrolls.
//!
//! Everything goes through `Action::EditSpectral`; the session renders the
//! edits (`session::spectral`) and the picture comes from
//! `Session::spectrogram`.

#![forbid(unsafe_code)]

use faderframe_core::{AudioSourceId, ClipId};
use faderframe_project::spectral::{SpectralEdit, SpectralOp, SpectralShape};
use faderframe_session::spectral::{PictureKey, SpectralChange};
use faderframe_session::{Action, Session, TransportAction};
use faderframe_spectral::Spectrogram;
use faderframe_spectral::spectrogram::LOW_HZ;
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, FontFamily, HostRequest, Key, MenuItem, Painter, Path,
    Pixels, Point, PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const BAR_H: f32 = 34.0;
const RULER_H: f32 = 20.0;
const AXIS_W: f32 = 52.0;
const EDITS_H: f32 = 26.0;
/// How long the view repaints waiting for a picture.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Pixels a press may move and still be a click.
const CLICK_PX: f32 = 3.0;

/// How a region is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Time × frequency.
    Rect,
    /// A time range, every frequency.
    Time,
    /// A frequency band, the whole clip.
    Band,
    /// A freehand outline.
    Lasso,
    /// A stroke as wide as the brush.
    Brush,
}

impl Tool {
    pub const ALL: [Tool; 5] = [Tool::Rect, Tool::Time, Tool::Band, Tool::Lasso, Tool::Brush];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Rect => "Rect",
            Tool::Time => "Time",
            Tool::Band => "Band",
            Tool::Lasso => "Lasso",
            Tool::Brush => "Brush",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tool::Rect => "Draw a region in time and frequency",
            Tool::Time => "Draw a time range, every frequency",
            Tool::Band => "Draw a frequency band over the whole clip",
            Tool::Lasso => "Draw around a sound freehand",
            Tool::Brush => "Paint over a sound (the brush's size is set beside Soft)",
        }
    }
}

/// The operations the buttons apply (Gain at its value).
const OPS: [&str; 4] = ["Attenuate", "Heal", "Remove", "Gain"];

/// A number set by dragging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Gain,
    FeatherMs,
    FeatherSt,
    Brush,
    Range,
}

enum Drag {
    /// Drawing a region (source frames, Hz).
    Shape {
        tool: Tool,
        from: (f64, f64),
        now: (f64, f64),
        points: Vec<(i64, f32)>,
        start: Point,
        last: Point,
    },
    /// Moving an edit.
    Move {
        index: usize,
        edit: SpectralEdit,
        from: Point,
        now: Point,
    },
    Value {
        which: Value,
        y0: f32,
        base: f32,
    },
}

/// The picture shown, ready to draw.
struct Picture {
    key: PictureKey,
    /// The range it is coloured for.
    range_db: f32,
    id: u64,
    columns: u32,
    rows: u32,
    rgba: Vec<u8>,
}

/// The clip as the editor maps it.
#[derive(Clone, Copy, Debug)]
struct Shown {
    clip: ClipId,
    /// The source drawn (edited or the original).
    source: AudioSourceId,
    rate: f64,
    /// The clip's region of the source.
    region: (f64, f64),
}

struct Layout {
    bar: Rect,
    ruler: Rect,
    axis: Rect,
    area: Rect,
    edits: Rect,
    tools: [Rect; 5],
    ops: [Rect; 4],
    gain: Rect,
    feather_ms: Rect,
    feather_st: Rect,
    brush: Rect,
    range: Rect,
    play: Rect,
    original: Rect,
}

pub struct SpectralView {
    theme: Theme,
    /// Visible source frames.
    view: Option<(f64, f64)>,
    clip: Option<ClipId>,
    pub tool: Tool,
    pub gain_db: f32,
    pub feather_ms: f32,
    pub feather_st: f32,
    /// The brush's radius in pixels.
    pub brush_px: f32,
    /// Show the unedited audio.
    pub original: bool,
    /// The region drawn, not yet an edit.
    pub selection: Option<SpectralShape>,
    /// The edit selected.
    pub selected: Option<usize>,
    drag: Option<Drag>,
    picture: Option<Picture>,
    hover: Option<Point>,
    next_id: u64,
    /// The region's start, frames per pixel and the span shown when last
    /// painted (for the scroll bar).
    scroll_map: Option<(f64, f64, f64)>,
    /// Since when the picture shown is not the one asked for (painting
    /// goes on until it is — it may arrive between two frames — for at
    /// most [`WAIT`]).
    waiting: Option<std::time::Instant>,
    /// The levels shown: this many dB below full scale.
    pub range_db: f32,
}

impl SpectralView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            view: None,
            clip: None,
            tool: Tool::Rect,
            gain_db: -6.0,
            feather_ms: 10.0,
            feather_st: 1.0,
            brush_px: 12.0,
            original: false,
            selection: None,
            selected: None,
            drag: None,
            picture: None,
            hover: None,
            next_id: 1,
            scroll_map: None,
            waiting: None,
            range_db: 96.0,
        }
    }

    fn layout(size: Size) -> Layout {
        let bar = Rect::new(0.0, 0.0, size.w, BAR_H);
        let y = 6.0;
        let h = BAR_H - 12.0;
        let mut x = 100.0;
        let mut next = |w: f32, gap: f32| {
            let r = Rect::new(x, y, w, h);
            x += w + gap;
            r
        };
        let tools = [
            next(48.0, 2.0),
            next(48.0, 2.0),
            next(48.0, 2.0),
            next(52.0, 2.0),
            next(52.0, 14.0),
        ];
        let ops = [
            next(78.0, 2.0),
            next(50.0, 2.0),
            next(64.0, 2.0),
            next(46.0, 2.0),
        ];
        let gain = next(62.0, 44.0);
        let feather_ms = next(64.0, 2.0);
        let feather_st = next(58.0, 8.0);
        let brush = next(80.0, 8.0);
        let range = next(90.0, 8.0);
        let original = Rect::new(size.w - 12.0 - 72.0, y, 72.0, h);
        let play = Rect::new(original.x - 6.0 - 64.0, y, 64.0, h);
        let top = BAR_H + RULER_H;
        let area_h = (size.h - top - EDITS_H).max(0.0);
        Layout {
            bar,
            ruler: Rect::new(AXIS_W, BAR_H, (size.w - AXIS_W).max(0.0), RULER_H),
            axis: Rect::new(0.0, top, AXIS_W, area_h),
            area: Rect::new(AXIS_W, top, (size.w - AXIS_W).max(0.0), area_h),
            edits: Rect::new(0.0, top + area_h, size.w, EDITS_H),
            tools,
            ops,
            gain,
            feather_ms,
            feather_st,
            brush,
            range,
            play,
            original,
        }
    }

    /// The clip shown: the session's spectral clip.
    fn shown(&self, model: &Session) -> Option<Shown> {
        let clip = model.spectral_clip()?;
        let (now, original) = model.spectral_sources(clip)?;
        let source = if self.original { original } else { now };
        let (rate, _, _) = model.source_format(source)?;
        let a = model.project().clip(clip)?.as_audio()?;
        let from = a.source_offset as f64;
        let to = from + a.source_span().max(1) as f64;
        Some(Shown {
            clip,
            source,
            rate: f64::from(rate.max(1)),
            region: (from, to),
        })
    }

    fn visible(&self, s: &Shown) -> (f64, f64) {
        self.view.unwrap_or(s.region)
    }

    fn x_of(&self, s: &Shown, area: Rect, frame: f64) -> f32 {
        let (a, b) = self.visible(s);
        area.x + ((frame - a) / (b - a).max(1.0)) as f32 * area.w
    }

    fn frame_at(&self, s: &Shown, area: Rect, x: f32) -> f64 {
        let (a, b) = self.visible(s);
        a + f64::from((x - area.x) / area.w.max(1.0)) * (b - a)
    }

    fn high(s: &Shown) -> f64 {
        s.rate / 2.0
    }

    fn low(s: &Shown) -> f64 {
        f64::from(LOW_HZ).min(Self::high(s) / 2.0)
    }

    fn y_of(s: &Shown, area: Rect, hz: f64) -> f32 {
        let (lo, hi) = (Self::low(s), Self::high(s));
        let k = (hz.max(1.0) / lo).ln() / (hi / lo).ln();
        area.y + area.h - k as f32 * area.h
    }

    fn hz_at(s: &Shown, area: Rect, y: f32) -> f64 {
        let (lo, hi) = (Self::low(s), Self::high(s));
        let k = f64::from((area.y + area.h - y) / area.h.max(1.0)).clamp(0.0, 1.0);
        lo * (hi / lo).powf(k)
    }

    /// Pixels per semitone on the frequency axis.
    fn px_per_semitone(s: &Shown, area: Rect) -> f32 {
        let octaves = (Self::high(s) / Self::low(s)).log2();
        area.h / (12.0 * octaves as f32).max(1.0)
    }

    /// An edit's outline on screen.
    fn outline(&self, s: &Shown, area: Rect, shape: &SpectralShape) -> Path {
        let mut path = Path::new();
        let pt = |t: i64, hz: f32| {
            Point::new(
                self.x_of(s, area, t as f64),
                Self::y_of(s, area, f64::from(hz)),
            )
        };
        match shape {
            SpectralShape::Rect {
                start,
                end,
                low,
                high,
            } => {
                let top = Self::y_of(s, area, f64::from(*high)).max(area.y);
                let bottom = if *low <= 1.0 {
                    area.y + area.h
                } else {
                    Self::y_of(s, area, f64::from(*low))
                };
                let (x0, x1) = (
                    self.x_of(s, area, *start as f64),
                    self.x_of(s, area, *end as f64),
                );
                path.move_to(Point::new(x0, top))
                    .line_to(Point::new(x1, top))
                    .line_to(Point::new(x1, bottom))
                    .line_to(Point::new(x0, bottom))
                    .close();
            }
            SpectralShape::Lasso { points } => {
                for (i, &(t, hz)) in points.iter().enumerate() {
                    if i == 0 {
                        path.move_to(pt(t, hz));
                    } else {
                        path.line_to(pt(t, hz));
                    }
                }
                path.close();
            }
            SpectralShape::Brush { points, .. } => {
                for (i, &(t, hz)) in points.iter().enumerate() {
                    if i == 0 {
                        path.move_to(pt(t, hz));
                    } else {
                        path.line_to(pt(t, hz));
                    }
                }
            }
        }
        path
    }

    /// The brush stroke's width on screen.
    fn brush_width(s: &Shown, area: Rect, radius_st: f32) -> f32 {
        2.0 * radius_st * Self::px_per_semitone(s, area)
    }

    /// The edit under `p` (the topmost: the last drawn).
    fn edit_at(&self, s: &Shown, area: Rect, edits: &[SpectralEdit], p: Point) -> Option<usize> {
        edits.iter().enumerate().rev().find_map(|(i, e)| {
            let inside = match &e.shape {
                SpectralShape::Rect { .. } => {
                    let pts = self.screen_points(s, area, &e.shape);
                    let (x0, x1) = (pts[0].x.min(pts[1].x), pts[0].x.max(pts[1].x));
                    let (y0, y1) = (pts[0].y.min(pts[2].y), pts[0].y.max(pts[2].y));
                    p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1
                }
                SpectralShape::Lasso { .. } => {
                    point_in_polygon(p, &self.screen_points(s, area, &e.shape))
                }
                SpectralShape::Brush { radius_st, .. } => {
                    let pts = self.screen_points(s, area, &e.shape);
                    let r = (Self::brush_width(s, area, *radius_st) / 2.0).max(4.0);
                    distance_to_polyline(p, &pts) <= r
                }
            };
            inside.then_some(i)
        })
    }

    fn screen_points(&self, s: &Shown, area: Rect, shape: &SpectralShape) -> Vec<Point> {
        let pt = |t: f64, hz: f64| Point::new(self.x_of(s, area, t), Self::y_of(s, area, hz));
        match shape {
            SpectralShape::Rect {
                start,
                end,
                low,
                high,
            } => {
                let low = if *low <= 1.0 {
                    Self::low(s)
                } else {
                    f64::from(*low)
                };
                vec![
                    pt(*start as f64, f64::from(*high)),
                    pt(*end as f64, f64::from(*high)),
                    pt(*end as f64, low),
                    pt(*start as f64, low),
                ]
            }
            SpectralShape::Lasso { points } | SpectralShape::Brush { points, .. } => points
                .iter()
                .map(|&(t, hz)| pt(t as f64, f64::from(hz)))
                .collect(),
        }
    }

    /// The shape a finished drag draws.
    fn shape_of(&self, s: &Shown, area: Rect, drag: &Drag) -> Option<SpectralShape> {
        let Drag::Shape {
            tool,
            from,
            now,
            points,
            ..
        } = drag
        else {
            return None;
        };
        let (t0, t1) = (from.0.min(now.0), from.0.max(now.0));
        let (f0, f1) = (from.1.min(now.1) as f32, from.1.max(now.1) as f32);
        let clamp = |t: f64| t.clamp(s.region.0, s.region.1).round() as i64;
        Some(match tool {
            Tool::Rect => SpectralShape::Rect {
                start: clamp(t0),
                end: clamp(t1),
                low: f0,
                high: f1,
            },
            Tool::Time => SpectralShape::Rect {
                start: clamp(t0),
                end: clamp(t1),
                low: 0.0,
                high: Self::high(s) as f32,
            },
            Tool::Band => SpectralShape::Rect {
                start: s.region.0 as i64,
                end: s.region.1 as i64,
                low: f0,
                high: f1,
            },
            Tool::Lasso => {
                if points.len() < 3 {
                    return None;
                }
                SpectralShape::Lasso {
                    points: points.clone(),
                }
            }
            Tool::Brush => {
                let (a, b) = self.visible(s);
                let px_per_ms = area.w / ((b - a) / s.rate * 1000.0).max(1e-6) as f32;
                SpectralShape::Brush {
                    points: points.clone(),
                    radius_ms: self.brush_px / px_per_ms.max(1e-6),
                    radius_st: self.brush_px / Self::px_per_semitone(s, area).max(1e-6),
                }
            }
        })
    }

    /// The edit an operation button makes of `shape` (with the Soft values).
    fn edit_of(&self, shape: SpectralShape, op: SpectralOp) -> SpectralEdit {
        let mut e = SpectralEdit::new(shape, op);
        e.feather_ms = self.feather_ms;
        e.feather_st = self.feather_st;
        e
    }

    fn op(&self, i: usize) -> SpectralOp {
        match i {
            0 => SpectralOp::Attenuate,
            1 => SpectralOp::Heal,
            2 => SpectralOp::Remove,
            _ => SpectralOp::Gain { db: self.gain_db },
        }
    }

    /// Apply operation `op`: the region drawn becomes an edit, else the
    /// selected edit takes the operation.
    fn apply(&mut self, op: SpectralOp, model: &Session, cx: &mut EventCx<'_, Action>) {
        let Some(clip) = model.spectral_clip() else {
            return;
        };
        let edits = model.spectral_edits(clip);
        if let Some(shape) = self.selection.take() {
            cx.emit(Action::EditSpectral {
                clip,
                change: SpectralChange::Add(self.edit_of(shape, op)),
            });
            self.selected = Some(edits.len());
        } else if let Some(i) = self.selected.filter(|i| *i < edits.len()) {
            let mut e = edits[i].clone();
            e.op = op;
            cx.emit(Action::EditSpectral {
                clip,
                change: SpectralChange::Set(i, e),
            });
        }
        cx.redraw();
    }

    fn value(&self, which: Value) -> f32 {
        match which {
            Value::Gain => self.gain_db,
            Value::FeatherMs => self.feather_ms,
            Value::FeatherSt => self.feather_st,
            Value::Brush => self.brush_px,
            Value::Range => self.range_db,
        }
    }

    fn set_value(&mut self, which: Value, v: f32) {
        match which {
            Value::Gain => self.gain_db = (v * 2.0).round() / 2.0,
            Value::FeatherMs => self.feather_ms = v.round(),
            Value::FeatherSt => self.feather_st = (v * 10.0).round() / 10.0,
            Value::Brush => self.brush_px = v.round(),
            Value::Range => self.range_db = (v / 6.0).round() * 6.0,
        }
    }

    /// Range and change per pixel of a value.
    fn value_range(which: Value) -> (f32, f32, f32) {
        match which {
            Value::Gain => (-30.0, 12.0, 0.25),
            Value::FeatherMs => (0.0, 200.0, 1.0),
            Value::FeatherSt => (0.0, 12.0, 0.05),
            Value::Brush => (2.0, 80.0, 0.5),
            Value::Range => (36.0, 120.0, 1.0),
        }
    }

    fn value_text(&self, which: Value) -> String {
        match which {
            Value::Gain => format!("{:+.1} dB", self.gain_db).replace('-', "−"),
            Value::FeatherMs => format!("{:.0} ms", self.feather_ms),
            Value::FeatherSt => format!("{:.1} st", self.feather_st),
            Value::Brush => format!("Brush {:.0}", self.brush_px),
            Value::Range => format!("Range {:.0} dB", self.range_db),
        }
    }

    /// The project position of source frame `frame` of the clip (as played
    /// without warping).
    fn position_of(
        model: &Session,
        clip: ClipId,
        frame: f64,
    ) -> Option<faderframe_timeline::MusicalTime> {
        let p = model.project();
        let c = p.clip(clip)?;
        let a = c.as_audio()?;
        let rate = f64::from(p.sample_rate.max(1));
        let start = p.timeline.to_samples(c.start, rate);
        Some(p.timeline.to_musical(
            start + (frame - a.source_offset as f64).max(0.0) as i64,
            rate,
        ))
    }

    /// The source frame the playhead is at, when it is inside the clip.
    fn playhead_frame(model: &Session, clip: ClipId) -> Option<f64> {
        let p = model.project();
        let c = p.clip(clip)?;
        let a = c.as_audio()?;
        let rate = f64::from(p.sample_rate.max(1));
        let at = p.timeline.to_samples(model.playhead(), rate);
        let start = p.timeline.to_samples(c.start, rate);
        let rel = at - start;
        (rel >= 0 && rel < a.length).then(|| a.source_at(rel as f64))
    }

    fn button(&self, p: &mut dyn Painter, r: Rect, label: &str, on: bool, enabled: bool) {
        let th = &self.theme;
        if on {
            p.fill_rounded(r, 4.0, &th.ui.selection.with_alpha(0.5).into());
        }
        p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
        let ink = if !enabled {
            th.ui.text_faint
        } else if on {
            th.ui.text
        } else {
            th.ui.text_dim
        };
        p.text(label, r, &TextStyle::new(th.fonts.small, ink).center());
    }

    fn op_color(&self, op: SpectralOp) -> Color {
        let m = &self.theme.console.meter;
        match op {
            SpectralOp::Attenuate => self.theme.ui.accent,
            SpectralOp::Heal => m.green,
            SpectralOp::Remove => m.red,
            SpectralOp::Gain { .. } => m.yellow,
        }
    }

    /// The spectrogram's colours (dark to bright; data, not chrome) for a
    /// level 0…1 of the range shown.
    fn palette(x: f32) -> [u8; 4] {
        const STOPS: [(f32, [f32; 3]); 6] = [
            (0.0, [0.0, 0.0, 4.0]),
            (0.25, [40.0, 11.0, 84.0]),
            (0.5, [137.0, 34.0, 106.0]),
            (0.7, [221.0, 81.0, 58.0]),
            (0.85, [250.0, 160.0, 20.0]),
            (1.0, [252.0, 255.0, 164.0]),
        ];
        let x = x.clamp(0.0, 1.0);
        let i = STOPS.iter().rposition(|(s, _)| *s <= x).unwrap_or(0);
        let j = (i + 1).min(STOPS.len() - 1);
        let (a, b) = (STOPS[i], STOPS[j]);
        let k = if j == i { 0.0 } else { (x - a.0) / (b.0 - a.0) };
        let c = |n: usize| (a.1[n] + (b.1[n] - a.1[n]) * k).round() as u8;
        [c(0), c(1), c(2), 255]
    }

    /// Keep `picture` ready to draw (rows flipped: high frequencies on top).
    fn take_picture(&mut self, key: PictureKey, s: &Spectrogram) {
        if self
            .picture
            .as_ref()
            .is_some_and(|p| p.key == key && p.range_db == self.range_db)
        {
            return;
        }
        // Each byte's colour once (levels are bytes).
        let floor = -self.range_db;
        let colours: Vec<[u8; 4]> = (0..=255u8)
            .map(|v| {
                Self::palette((faderframe_spectral::spectrogram::level_db(v) - floor) / -floor)
            })
            .collect();
        let mut rgba = Vec::with_capacity(s.columns * s.rows * 4);
        for row in (0..s.rows).rev() {
            for col in 0..s.columns {
                rgba.extend_from_slice(&colours[usize::from(s.levels[row * s.columns + col])]);
            }
        }
        self.next_id += 1;
        self.picture = Some(Picture {
            key,
            range_db: self.range_db,
            id: self.next_id,
            columns: s.columns as u32,
            rows: s.rows as u32,
            rgba,
        });
    }

    /// Zoom by `factor` around source frame `at`.
    fn zoom(&mut self, s: &Shown, at: f64, factor: f64) {
        let (a, b) = self.visible(s);
        let span = ((b - a) * factor).clamp(s.rate * 0.02, s.region.1 - s.region.0);
        let k = (at - a) / (b - a).max(1.0);
        let start = (at - k * span).clamp(s.region.0, s.region.1 - span);
        self.view = Some((start, start + span));
    }

    fn scroll_by(&mut self, s: &Shown, frames: f64) {
        let (a, b) = self.visible(s);
        let span = b - a;
        let start = (a + frames).clamp(s.region.0, (s.region.1 - span).max(s.region.0));
        self.view = Some((start, start + span));
    }
}

/// Is `p` inside the polygon `pts`?
fn point_in_polygon(p: Point, pts: &[Point]) -> bool {
    let mut inside = false;
    let n = pts.len();
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

fn distance_to_polyline(p: Point, pts: &[Point]) -> f32 {
    let seg = |a: Point, b: Point| {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx * dx + dy * dy;
        let t = if len > 0.0 {
            (((p.x - a.x) * dx + (p.y - a.y) * dy) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ((a.x + t * dx - p.x).powi(2) + (a.y + t * dy - p.y).powi(2)).sqrt()
    };
    match pts {
        [] => f32::INFINITY,
        [a] => seg(*a, *a),
        _ => pts
            .windows(2)
            .map(|w| seg(w[0], w[1]))
            .fold(f32::INFINITY, f32::min),
    }
}

fn hz_label(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{hz:.0}")
    }
}

impl CanvasView<Session, Action> for SpectralView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        self.theme = theme.clone();
        let th = theme;
        let l = Self::layout(size);
        p.fill(Rect::from_size(size), th.ui.background);
        p.fill(l.bar, th.ui.surface);
        p.hline(0.0, size.w, BAR_H - 0.5, th.ui.border);
        p.text(
            "Spectral",
            Rect::new(12.0, 0.0, 84.0, BAR_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        let shown = self.shown(model);
        if shown.map(|s| s.clip) != self.clip {
            self.clip = shown.map(|s| s.clip);
            self.view = None;
            self.selection = None;
            self.selected = None;
            self.picture = None;
        }
        for (r, t) in l.tools.iter().zip(Tool::ALL) {
            self.button(p, *r, t.label(), self.tool == t, shown.is_some());
        }
        let can_apply = self.selection.is_some() || self.selected.is_some();
        for (i, r) in l.ops.iter().enumerate() {
            self.button(p, *r, OPS[i], false, can_apply);
        }
        for (r, which) in [
            (l.gain, Value::Gain),
            (l.feather_ms, Value::FeatherMs),
            (l.feather_st, Value::FeatherSt),
            (l.brush, Value::Brush),
            (l.range, Value::Range),
        ] {
            p.fill_rounded(r, 4.0, &th.ui.lcd_bg.into());
            p.text(
                &self.value_text(which),
                r,
                &TextStyle::new(th.fonts.small, th.ui.lcd_text)
                    .family(FontFamily::Mono)
                    .center(),
            );
        }
        p.text(
            "Soft",
            Rect::new(l.feather_ms.x - 36.0, 0.0, 32.0, BAR_H),
            &TextStyle::new(th.fonts.small, th.ui.text_faint).right(),
        );
        self.button(p, l.play, "▶ Region", false, shown.is_some());
        self.button(p, l.original, "Original", self.original, shown.is_some());
        let Some(s) = shown else {
            p.text(
                "No audio clip: an audio clip's menu → Spectral Editor",
                Rect::new(0.0, BAR_H, size.w, size.h - BAR_H),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
            return;
        };
        // Busy: the render's progress.
        if let Some(f) = model.spectral_busy(s.clip) {
            let text = if f > 0.0 {
                format!("Rendering {:.0} %", f * 100.0)
            } else {
                "Rendering…".to_string()
            };
            p.text(
                &text,
                Rect::new(l.play.x - 130.0, 0.0, 120.0, BAR_H),
                &TextStyle::new(th.fonts.small, th.ui.accent).right(),
            );
        }
        // The picture: asked for at this view's size.
        let area = l.area;
        let (va, vb) = self.visible(&s);
        self.scroll_map = Some((s.region.0, (vb - va) / f64::from(area.w.max(1.0)), vb - va));
        let scale = p.scale_factor().max(1.0);
        let key = PictureKey {
            source: s.source,
            from: va.floor() as i64,
            to: vb.ceil() as i64,
            columns: ((area.w * scale) as usize).clamp(16, 2048),
            rows: ((area.h * scale) as usize).clamp(16, 768),
        };
        let got = model.spectrogram(key);
        if got.as_ref().is_none_or(|(k, _)| *k != key) {
            self.waiting.get_or_insert_with(std::time::Instant::now);
        } else {
            self.waiting = None;
        }
        if let Some((k, picture)) = got {
            self.take_picture(k, &picture);
        }
        p.fill(area, Color::rgb8(0, 0, 4));
        p.push_clip(area);
        if let Some(pic) = self.picture.as_ref().filter(|pc| pc.key.source == s.source) {
            let (x0, x1) = (
                self.x_of(&s, area, pic.key.from as f64),
                self.x_of(&s, area, pic.key.to as f64),
            );
            p.pixels(
                &Pixels {
                    key: pic.id,
                    width: pic.columns,
                    height: pic.rows,
                    rgba: &pic.rgba,
                },
                Rect::new(x0, area.y, x1 - x0, area.h),
            );
        } else {
            p.text(
                "Computing the spectrogram…",
                area,
                &TextStyle::new(th.fonts.small, th.ui.text_dim).center(),
            );
        }
        // The clip's region (outside it, shaded).
        let (r0, r1) = (
            self.x_of(&s, area, s.region.0),
            self.x_of(&s, area, s.region.1),
        );
        let shade = th.ui.background.with_alpha(0.6);
        if r0 > area.x {
            p.fill(Rect::new(area.x, area.y, r0 - area.x, area.h), shade);
        }
        if r1 < area.x + area.w {
            p.fill(Rect::new(r1, area.y, area.x + area.w - r1, area.h), shade);
        }
        // The edits.
        let edits = model.spectral_edits(s.clip);
        for (i, e) in edits.iter().enumerate() {
            let shape = match &self.drag {
                Some(Drag::Move {
                    index,
                    edit,
                    from,
                    now,
                }) if *index == i => self.moved(&s, area, edit, *from, *now).shape,
                _ => e.shape.clone(),
            };
            let color = self.op_color(e.op);
            let path = self.outline(&s, area, &shape);
            let picked = self.selected == Some(i);
            let alpha = if self.original { 0.3 } else { 1.0 };
            match &shape {
                SpectralShape::Brush { radius_st, .. } => {
                    let w = Self::brush_width(&s, area, *radius_st).max(2.0);
                    p.stroke_path(&path, w, color.with_alpha(0.25 * alpha));
                    p.stroke_path(&path, 1.0, color.with_alpha(0.9 * alpha));
                }
                _ => {
                    p.fill_path(&path, color.with_alpha(0.15 * alpha));
                    p.stroke_path(
                        &path,
                        if picked { 2.0 } else { 1.0 },
                        color.with_alpha(alpha),
                    );
                }
            }
            if picked {
                p.stroke_path(&path, 2.0, th.ui.text.with_alpha(0.9));
            }
            let pts = self.screen_points(&s, area, &shape);
            if let Some(top_left) = pts
                .iter()
                .copied()
                .reduce(|a, b| Point::new(a.x.min(b.x), a.y.min(b.y)))
            {
                p.text(
                    &format!("{} {}", i + 1, e.op.label()),
                    Rect::new(top_left.x + 3.0, top_left.y + 1.0, 140.0, 14.0),
                    &TextStyle::new(th.fonts.small, color.with_alpha(alpha)),
                );
            }
        }
        // The region being drawn, or the one drawn.
        let drawing = self
            .drag
            .as_ref()
            .and_then(|d| self.shape_of(&s, area, d))
            .or_else(|| self.selection.clone());
        if let Some(shape) = drawing {
            let path = self.outline(&s, area, &shape);
            match &shape {
                SpectralShape::Brush { radius_st, .. } => {
                    let w = Self::brush_width(&s, area, *radius_st).max(2.0);
                    p.stroke_path(&path, w, th.ui.selection.with_alpha(0.35));
                }
                _ => {
                    p.fill_path(&path, th.ui.selection.with_alpha(0.2));
                    p.stroke_path(&path, 1.5, th.ui.text.with_alpha(0.85));
                }
            }
        }
        // The playhead.
        if let Some(f) = Self::playhead_frame(model, s.clip) {
            let x = self.x_of(&s, area, f);
            p.vline(x, area.y, area.y + area.h, th.ui.accent);
        }
        // The readout at the pointer.
        if let Some(h) = self.hover.filter(|h| area.contains(*h)) {
            let frame = self.frame_at(&s, area, h.x);
            let hz = Self::hz_at(&s, area, h.y);
            let secs = (frame - s.region.0) / s.rate;
            let level = model
                .spectrogram(key)
                .filter(|(k, _)| *k == key)
                .map(|(_, pic)| {
                    let col = ((frame - pic.from as f64) / (pic.to - pic.from).max(1) as f64
                        * pic.columns as f64)
                        .clamp(0.0, (pic.columns - 1) as f64)
                        as usize;
                    let row = (pic.row_at(hz as f32) as usize).min(pic.rows - 1);
                    format!(" · {:.0} dB", pic.db(row, col)).replace('-', "−")
                })
                .unwrap_or_default();
            let text = format!("{secs:.3} s · {} Hz{level}", hz_label(hz));
            let r = Rect::new(area.x + area.w - 230.0, area.y + area.h - 22.0, 224.0, 18.0);
            p.fill_rounded(r, 3.0, &th.ui.background.with_alpha(0.75).into());
            p.text(
                &text,
                r,
                &TextStyle::new(th.fonts.small, th.ui.text)
                    .family(FontFamily::Mono)
                    .center(),
            );
        }
        p.pop_clip();
        // The frequency axis.
        p.fill(l.axis, th.ui.surface);
        for hz in [
            20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
        ] {
            if hz > Self::high(&s) || hz < Self::low(&s) {
                continue;
            }
            let y = Self::y_of(&s, area, hz);
            p.hline(AXIS_W - 6.0, AXIS_W, y, th.ui.text_faint);
            p.text(
                &hz_label(hz),
                Rect::new(2.0, y - 7.0, AXIS_W - 10.0, 14.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim).right(),
            );
        }
        // The time ruler: seconds from the clip's start.
        p.fill(l.ruler, th.ui.surface.with_alpha(0.7));
        p.hline(
            l.ruler.x,
            l.ruler.x + l.ruler.w,
            l.ruler.y + l.ruler.h - 0.5,
            th.ui.border,
        );
        let span = (vb - va) / s.rate;
        let step = [
            0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0,
        ]
        .into_iter()
        .find(|st| span / st <= f64::from(area.w / 70.0).max(1.0))
        .unwrap_or(120.0);
        let first = ((va - s.region.0) / s.rate / step).ceil() as i64;
        let last = ((vb - s.region.0) / s.rate / step).floor() as i64;
        for k in first..=last.min(first + 400) {
            let secs = k as f64 * step;
            let x = self.x_of(&s, area, s.region.0 + secs * s.rate);
            p.vline(
                x,
                l.ruler.y + l.ruler.h - 6.0,
                l.ruler.y + l.ruler.h,
                th.ui.text_faint,
            );
            let label = if step < 0.1 {
                format!("{secs:.3}")
            } else if step < 1.0 {
                format!("{secs:.2}")
            } else {
                format!("{secs:.0} s")
            };
            p.text(
                &label,
                Rect::new(x + 3.0, l.ruler.y, 70.0, l.ruler.h),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        }
        // The edits' list.
        p.fill(l.edits, th.ui.surface);
        p.hline(0.0, size.w, l.edits.y + 0.5, th.ui.border);
        let hint = if edits.is_empty() {
            "Draw a region with a tool, then Attenuate, Heal, Remove or Gain it".to_string()
        } else {
            let picked = self
                .selected
                .filter(|i| *i < edits.len())
                .map(|i| format!(" · {} selected: Delete removes it", i + 1))
                .unwrap_or_default();
            format!(
                "{} edit{}{picked}",
                edits.len(),
                if edits.len() == 1 { "" } else { "s" }
            )
        };
        p.text(
            &hint,
            l.edits.inset_xy(12.0, 0.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = Self::layout(size);
        let shown = self.shown(model);
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                ..
            } => {
                cx.request(HostRequest::GrabFocus);
                if let Some(i) = l.tools.iter().position(|r| r.contains(pos)) {
                    self.tool = Tool::ALL[i];
                    cx.redraw();
                    return true;
                }
                if let Some(i) = l.ops.iter().position(|r| r.contains(pos)) {
                    self.apply(self.op(i), model, cx);
                    return true;
                }
                for (r, which) in [
                    (l.gain, Value::Gain),
                    (l.feather_ms, Value::FeatherMs),
                    (l.feather_st, Value::FeatherSt),
                    (l.brush, Value::Brush),
                    (l.range, Value::Range),
                ] {
                    if r.contains(pos) {
                        self.drag = Some(Drag::Value {
                            which,
                            y0: pos.y,
                            base: self.value(which),
                        });
                        return true;
                    }
                }
                if l.original.contains(pos) {
                    self.original = !self.original;
                    cx.redraw();
                    return true;
                }
                let Some(s) = shown else {
                    return false;
                };
                if l.play.contains(pos) {
                    let from = self
                        .selection
                        .as_ref()
                        .map(|sh| sh.frames().0)
                        .or_else(|| {
                            let edits = model.spectral_edits(s.clip);
                            self.selected
                                .and_then(|i| edits.get(i))
                                .map(|e| e.shape.frames().0)
                        })
                        .map_or(s.region.0, |f| f as f64);
                    if let Some(at) = Self::position_of(model, s.clip, from) {
                        cx.emit(Action::Transport(TransportAction::Locate(at)));
                        cx.emit(Action::Transport(TransportAction::Play));
                    }
                    return true;
                }
                if !l.area.contains(pos) {
                    return false;
                }
                // On a selected edit: move it; else draw.
                let edits = model.spectral_edits(s.clip);
                if let Some(i) = self
                    .edit_at(&s, l.area, &edits, pos)
                    .filter(|i| self.selected == Some(*i))
                {
                    self.drag = Some(Drag::Move {
                        index: i,
                        edit: edits[i].clone(),
                        from: pos,
                        now: pos,
                    });
                    return true;
                }
                let at = (
                    self.frame_at(&s, l.area, pos.x),
                    Self::hz_at(&s, l.area, pos.y),
                );
                self.drag = Some(Drag::Shape {
                    tool: self.tool,
                    from: at,
                    now: at,
                    points: vec![(at.0.round() as i64, at.1 as f32)],
                    start: pos,
                    last: pos,
                });
                cx.redraw();
                true
            }
            ViewEvent::PointerMove { pos, dragging, .. } => {
                self.hover = Some(pos);
                let Some(s) = shown else {
                    return false;
                };
                match (&mut self.drag, dragging) {
                    (
                        Some(Drag::Shape {
                            tool,
                            now,
                            points,
                            last,
                            ..
                        }),
                        true,
                    ) => {
                        let x = pos.x.clamp(l.area.x, l.area.x + l.area.w);
                        let y = pos.y.clamp(l.area.y, l.area.y + l.area.h);
                        let (a, b) = self.view.unwrap_or(s.region);
                        let frame = a + f64::from((x - l.area.x) / l.area.w.max(1.0)) * (b - a);
                        let hz = Self::hz_at(&s, l.area, y);
                        *now = (frame, hz);
                        let step = if *tool == Tool::Brush { 2.0 } else { 3.0 };
                        if (pos.x - last.x).hypot(pos.y - last.y) >= step {
                            points.push((frame.round() as i64, hz as f32));
                            *last = pos;
                        }
                        cx.set_cursor(Cursor::Crosshair);
                        cx.redraw();
                        true
                    }
                    (Some(Drag::Move { now, .. }), true) => {
                        *now = pos;
                        cx.set_cursor(Cursor::Grabbing);
                        cx.redraw();
                        true
                    }
                    (Some(Drag::Value { which, y0, base }), true) => {
                        let (lo, hi, per) = Self::value_range(*which);
                        let v = (*base + (*y0 - pos.y) * per).clamp(lo, hi);
                        let which = *which;
                        self.set_value(which, v);
                        cx.set_cursor(Cursor::ResizeVertical);
                        cx.redraw();
                        true
                    }
                    _ => {
                        if l.area.contains(pos) {
                            let edits = model.spectral_edits(s.clip);
                            let over = self.edit_at(&s, l.area, &edits, pos);
                            cx.set_cursor(if over.is_some() && over == self.selected {
                                Cursor::Grab
                            } else {
                                Cursor::Crosshair
                            });
                        } else if [l.gain, l.feather_ms, l.feather_st, l.brush, l.range]
                            .iter()
                            .any(|r| r.contains(pos))
                        {
                            cx.set_cursor(Cursor::ResizeVertical);
                        } else {
                            cx.set_cursor(Cursor::Default);
                        }
                        cx.redraw();
                        false
                    }
                }
            }
            ViewEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => {
                let Some(drag) = self.drag.take() else {
                    return false;
                };
                let Some(s) = shown else {
                    return true;
                };
                let edits = model.spectral_edits(s.clip);
                match &drag {
                    Drag::Shape { start, last, .. }
                        if (last.x - start.x).hypot(last.y - start.y) < CLICK_PX =>
                    {
                        // A click: select the edit there, or nothing.
                        self.selected = self.edit_at(&s, l.area, &edits, *start);
                        self.selection = None;
                    }
                    Drag::Shape { .. } => {
                        self.selection = self.shape_of(&s, l.area, &drag);
                        self.selected = None;
                    }
                    Drag::Move {
                        index,
                        edit,
                        from,
                        now,
                    } => {
                        if (now.x - from.x).hypot(now.y - from.y) >= CLICK_PX {
                            let moved = self.moved(&s, l.area, edit, *from, *now);
                            cx.emit(Action::EditSpectral {
                                clip: s.clip,
                                change: SpectralChange::Set(*index, moved),
                            });
                        }
                    }
                    Drag::Value { which, .. } => {
                        // The selected edit takes the new value.
                        if let Some(i) = self.selected.filter(|i| *i < edits.len()) {
                            let mut e = edits[i].clone();
                            match which {
                                Value::Gain => {
                                    if let SpectralOp::Gain { .. } = e.op {
                                        e.op = SpectralOp::Gain { db: self.gain_db };
                                    }
                                }
                                Value::FeatherMs => e.feather_ms = self.feather_ms,
                                Value::FeatherSt => e.feather_st = self.feather_st,
                                Value::Brush | Value::Range => {}
                            }
                            if e != edits[i] {
                                cx.emit(Action::EditSpectral {
                                    clip: s.clip,
                                    change: SpectralChange::Set(i, e),
                                });
                            }
                        }
                    }
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(s) = shown else {
                    return false;
                };
                if !l.area.contains(pos) {
                    return false;
                }
                let edits = model.spectral_edits(s.clip);
                let mut items: Vec<MenuItem<Action>> = Vec::new();
                if let Some(i) = self.edit_at(&s, l.area, &edits, pos) {
                    self.selected = Some(i);
                    for (k, label) in OPS.iter().enumerate() {
                        let mut e = edits[i].clone();
                        e.op = self.op(k);
                        let same =
                            std::mem::discriminant(&e.op) == std::mem::discriminant(&edits[i].op);
                        let label = if k == 3 {
                            format!("Gain {:+.1} dB", self.gain_db).replace('-', "−")
                        } else {
                            (*label).to_string()
                        };
                        items.push(
                            MenuItem::new(
                                label,
                                Action::EditSpectral {
                                    clip: s.clip,
                                    change: SpectralChange::Set(i, e),
                                },
                            )
                            .checked(same),
                        );
                    }
                    items.push(
                        MenuItem::new(
                            format!("Delete Edit {}", i + 1),
                            Action::EditSpectral {
                                clip: s.clip,
                                change: SpectralChange::Remove(i),
                            },
                        )
                        .separated(),
                    );
                }
                if let Some(at) = Self::position_of(model, s.clip, self.frame_at(&s, l.area, pos.x))
                {
                    items.push(
                        MenuItem::new(
                            "Play From Here",
                            Action::Transport(TransportAction::Locate(at)),
                        )
                        .separated(),
                    );
                }
                if !edits.is_empty() {
                    items.push(
                        MenuItem::new(
                            "Remove Every Edit",
                            Action::EditSpectral {
                                clip: s.clip,
                                change: SpectralChange::Clear,
                            },
                        )
                        .separated(),
                    );
                }
                cx.request(HostRequest::ContextMenu { at: pos, items });
                true
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                let Some(s) = shown else {
                    return false;
                };
                if !l.area.contains(pos) && !l.ruler.contains(pos) {
                    return false;
                }
                if modifiers.ctrl {
                    let steps = if precise { dy / 40.0 } else { dy };
                    let at = self.frame_at(&s, l.area, pos.x);
                    self.zoom(&s, at, 1.25f64.powf(f64::from(steps)));
                } else {
                    let (a, b) = self.visible(&s);
                    let px = if precise {
                        if dx != 0.0 { dx } else { dy }
                    } else {
                        (if dx != 0.0 { dx } else { dy }) * l.area.w * 0.1
                    };
                    self.scroll_by(&s, f64::from(px / l.area.w.max(1.0)) * (b - a));
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerLeave => {
                self.hover = None;
                cx.redraw();
                false
            }
            ViewEvent::Key { key, .. } => {
                let Some(s) = shown else {
                    return false;
                };
                match key {
                    Key::Delete | Key::Backspace => {
                        let edits = model.spectral_edits(s.clip);
                        if let Some(i) = self.selected.take().filter(|i| *i < edits.len()) {
                            cx.emit(Action::EditSpectral {
                                clip: s.clip,
                                change: SpectralChange::Remove(i),
                            });
                        } else if self.selection.take().is_none() {
                            return false;
                        }
                        cx.redraw();
                        true
                    }
                    Key::Escape => {
                        self.selection = None;
                        self.selected = None;
                        cx.redraw();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, _model: &Session) -> Option<String> {
        let l = Self::layout(size);
        if let Some(i) = l.tools.iter().position(|r| r.contains(pos)) {
            return Some(Tool::ALL[i].tip().into());
        }
        let tips = [
            "Bring the region down to the sound around it in time, only where it stands out (coughs, clicks, squeaks)",
            "Replace the region by the sound around it in time (dropouts, longer noises)",
            "Silence the region",
            "Make the region louder or quieter by the value beside it",
        ];
        if let Some(i) = l.ops.iter().position(|r| r.contains(pos)) {
            return Some(format!(
                "{} — on the region drawn, or the selected edit",
                tips[i]
            ));
        }
        for (r, tip) in [
            (l.gain, "Gain for the Gain button: drag up or down"),
            (
                l.feather_ms,
                "Soft edges in time: drag up or down (applies to the selected edit too)",
            ),
            (
                l.feather_st,
                "Soft edges in frequency, in semitones: drag up or down",
            ),
            (l.brush, "The brush's radius in pixels: drag up or down"),
            (
                l.range,
                "The levels shown, down from full scale: drag up or down",
            ),
            (l.play, "Play from the region's start"),
            (l.original, "Show the clip's audio without the edits"),
        ] {
            if r.contains(pos) {
                return Some(tip.into());
            }
        }
        None
    }

    fn wants_frames(&self, model: &Session) -> bool {
        self.drag.is_some()
            || self.waiting.is_some_and(|t| t.elapsed() < WAIT)
            || model.spectrogram_pending()
            || model
                .spectral_clip()
                .is_some_and(|c| model.spectral_busy(c).is_some())
            || model.transport().playing
    }

    fn min_size(&self) -> Size {
        Size::new(760.0, 240.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Horizontal {
            return None;
        }
        let s = self.shown(model)?;
        let l = Self::layout(size);
        let (a, b) = self.visible(&s);
        let per_px = (b - a) / f64::from(l.area.w.max(1.0));
        Some(ScrollInfo {
            content: ((s.region.1 - s.region.0) / per_px) as f32,
            viewport: l.area.w,
            offset: ((a - s.region.0) / per_px) as f32,
            start: l.area.x,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis != ScrollAxis::Horizontal {
            return;
        }
        // Offsets are pixels at the zoom last painted.
        if let Some((start, per_px, span)) = self.scroll_map {
            let a = start + f64::from(offset.max(0.0)) * per_px;
            self.view = Some((a, a + span));
        }
    }
}

impl SpectralView {
    /// `edit` moved by the drag from `from` to `now`.
    fn moved(
        &self,
        s: &Shown,
        area: Rect,
        edit: &SpectralEdit,
        from: Point,
        now: Point,
    ) -> SpectralEdit {
        let frames =
            (self.frame_at(s, area, now.x) - self.frame_at(s, area, from.x)).round() as i64;
        let semitones = -(now.y - from.y) / Self::px_per_semitone(s, area).max(1e-6);
        let mut e = edit.clone();
        e.shape = edit.shape.moved(frames, semitones);
        e
    }
}

#[cfg(test)]
mod tests;
