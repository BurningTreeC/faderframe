//! The Automation view (bottom dock): every automation lane of the project
//! at a glance and a large editor for one of them.
//!
//! * The lane list on the left groups lanes under their tracks (every
//!   track that can be automated is listed; "+" adds a lane). A lane row
//!   shows the parameter, its mode (click: Off/Read/Touch/Latch/Write), its
//!   point count and whether the arranger shows it (the eye). "Selected
//!   tracks" limits the list to the arranger's track selection; "All Read"
//!   and "All Off" set every listed lane at once.
//! * The editor on the right draws the selected lane over the whole song
//!   with a value scale, bar lines, the loop range, the edit selection and
//!   the playhead. Click to add a point and drag it, drag points (snapped
//!   to 1/16 when snapping is on; Alt: free), double-click a point to delete
//!   it, right-click for curve shapes. Ctrl+wheel zooms, the wheel scrolls.
//! * Lane tools: Write Value (the control's value at the playhead, or over
//!   the edit selection), Thin, Clear, Delete Lane.
//!
//! Edits go through `Command::SetAutomationLane` like the arranger's lanes
//! (drags are one gesture, one undo step).

use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, CurveShape, thin,
};
use faderframe_core::{AutomationLaneId, TrackId};
use faderframe_project::{Command, Track};
use faderframe_session::automation::has_automation;
use faderframe_session::{Action, AutomationParam, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    CanvasView, Color, Cursor, EventCx, HostRequest, MenuItem, Modifiers, Paint, Painter, Path,
    Point, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};

#[cfg(test)]
mod tests;

const TOOLBAR_H: f32 = 30.0;
const LIST_W: f32 = 290.0;
const ROW_H: f32 = 24.0;
const RULER_H: f32 = 18.0;
const SCALE_W: f32 = 58.0;
const POINT_HIT: f32 = 6.0;
const DRAG_THRESHOLD: f32 = 3.0;
/// Snap step when snapping is on (a sixteenth note).
const SNAP_QUARTERS: f64 = 0.25;

const MODES: [AutomationMode; 5] = [
    AutomationMode::Off,
    AutomationMode::Read,
    AutomationMode::Touch,
    AutomationMode::Latch,
    AutomationMode::Write,
];
const SHAPES: [(CurveShape, &str); 4] = [
    (CurveShape::Linear, "Linear"),
    (CurveShape::Smooth, "Smooth (S-curve)"),
    (CurveShape::Exponential, "Exponential"),
    (CurveShape::Step, "Step (hold)"),
];

pub fn mode_label(m: AutomationMode) -> &'static str {
    match m {
        AutomationMode::Off => "Off",
        AutomationMode::Read => "Read",
        AutomationMode::Touch => "Touch",
        AutomationMode::Latch => "Latch",
        AutomationMode::Write => "Write",
    }
}

fn mode_color(m: AutomationMode) -> Color {
    match m {
        AutomationMode::Off => Color::hex(0x6d6f75),
        AutomationMode::Read => Color::hex(0x5fc27a),
        AutomationMode::Touch => Color::hex(0xe5c04b),
        AutomationMode::Latch => Color::hex(0xee9a42),
        AutomationMode::Write => Color::hex(0xff5a52),
    }
}

/// A row of the lane list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Track(TrackId),
    Lane(TrackId, AutomationLaneId),
}

/// Toolbar buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    SelectedOnly,
    AllRead,
    AllOff,
    WriteValue,
    Thin,
    Clear,
    Delete,
    Fit,
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Button(Button),
    /// A list row; for lanes, `Some` part.
    Row(Row, Option<RowPart>),
    Editor,
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPart {
    Name,
    Mode,
    Eye,
    Add,
}

struct Layout {
    toolbar: Rect,
    list: Rect,
    ruler: Rect,
    scale: Rect,
    plot: Rect,
    buttons: Vec<(Button, Rect)>,
}

struct Drag {
    track: TrackId,
    base: AutomationLane,
    index: usize,
    origin: Point,
    moved: bool,
}

pub struct AutomationView {
    theme: Theme,
    selected: Option<(TrackId, AutomationLaneId)>,
    selected_only: bool,
    list_scroll: f32,
    /// Pixels per quarter (0: fit the song).
    zoom: f64,
    /// First visible quarter.
    scroll_q: f64,
    drag: Option<Drag>,
}

impl AutomationView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            selected: None,
            selected_only: false,
            list_scroll: 0.0,
            zoom: 0.0,
            scroll_q: 0.0,
            drag: None,
        }
    }

    /// The lane being edited (view state).
    pub fn selected(&self) -> Option<(TrackId, AutomationLaneId)> {
        self.selected
    }

    pub fn select(&mut self, track: TrackId, lane: AutomationLaneId) {
        self.selected = Some((track, lane));
    }

    // --- model ---------------------------------------------------------------------

    fn tracks<'a>(&self, model: &'a Session) -> Vec<&'a Track> {
        model
            .project()
            .tracks
            .iter()
            .filter(|t| has_automation(t.kind))
            .filter(|t| !self.selected_only || model.selection.tracks.contains(&t.id))
            .collect()
    }

    /// The list's rows, top to bottom.
    pub fn rows(&self, model: &Session) -> Vec<Row> {
        let mut out = Vec::new();
        for t in self.tracks(model) {
            out.push(Row::Track(t.id));
            out.extend(t.automation.lanes.iter().map(|l| Row::Lane(t.id, l.id)));
        }
        out
    }

    fn lane(model: &Session, track: TrackId, id: AutomationLaneId) -> Option<&AutomationLane> {
        model
            .project()
            .track(track)?
            .automation
            .lanes
            .iter()
            .find(|l| l.id == id)
    }

    /// The selected lane if it still exists, else the first listed one.
    fn current(&self, model: &Session) -> Option<(TrackId, AutomationLane)> {
        if let Some((t, id)) = self.selected
            && let Some(l) = Self::lane(model, t, id)
        {
            return Some((t, l.clone()));
        }
        self.rows(model).into_iter().find_map(|r| match r {
            Row::Lane(t, id) => Self::lane(model, t, id).map(|l| (t, l.clone())),
            Row::Track(_) => None,
        })
    }

    fn listed_lanes(&self, model: &Session) -> Vec<(TrackId, AutomationLaneId)> {
        self.rows(model)
            .into_iter()
            .filter_map(|r| match r {
                Row::Lane(t, l) => Some((t, l)),
                Row::Track(_) => None,
            })
            .collect()
    }

    fn edit(track: TrackId, lane: AutomationLane) -> Action {
        Action::Edit(Command::SetAutomationLane {
            track,
            lane: Box::new(lane),
        })
    }

    // --- geometry ------------------------------------------------------------------

    fn layout(&self, size: Size) -> Layout {
        let toolbar = Rect::new(0.0, 0.0, size.w, TOOLBAR_H);
        let list = Rect::new(
            0.0,
            TOOLBAR_H,
            LIST_W.min(size.w * 0.45),
            size.h - TOOLBAR_H,
        );
        let right = Rect::new(
            list.right(),
            TOOLBAR_H,
            size.w - list.right(),
            size.h - TOOLBAR_H,
        );
        let ruler = Rect::new(right.x + SCALE_W, right.y, right.w - SCALE_W, RULER_H);
        let scale = Rect::new(right.x, right.y + RULER_H, SCALE_W, right.h - RULER_H);
        let plot = Rect::new(
            right.x + SCALE_W,
            right.y + RULER_H + 6.0,
            (right.w - SCALE_W - 10.0).max(10.0),
            (right.h - RULER_H - 14.0).max(10.0),
        );
        // Buttons: list tools on the left, lane tools after the list.
        let mut buttons = Vec::new();
        let mut x = 8.0;
        for (b, w) in [
            (Button::SelectedOnly, 104.0),
            (Button::AllRead, 64.0),
            (Button::AllOff, 58.0),
        ] {
            buttons.push((b, Rect::new(x, 4.0, w, TOOLBAR_H - 8.0)));
            x += w + 6.0;
        }
        let mut x = list.right() + 8.0;
        for (b, w) in [
            (Button::WriteValue, 92.0),
            (Button::Thin, 48.0),
            (Button::Clear, 52.0),
            (Button::Delete, 92.0),
            (Button::Fit, 40.0),
        ] {
            if x + w > size.w - 4.0 {
                break;
            }
            buttons.push((b, Rect::new(x, 4.0, w, TOOLBAR_H - 8.0)));
            x += w + 6.0;
        }
        Layout {
            toolbar,
            list,
            ruler,
            scale,
            plot,
            buttons,
        }
    }

    fn row_rect(&self, list: Rect, i: usize) -> Rect {
        Rect::new(
            list.x,
            list.y + i as f32 * ROW_H - self.list_scroll,
            list.w,
            ROW_H,
        )
    }

    fn row_parts(r: Rect, row: Row) -> Vec<(RowPart, Rect)> {
        match row {
            Row::Track(_) => vec![(
                RowPart::Add,
                Rect::new(r.right() - 26.0, r.y + 3.0, 20.0, r.h - 6.0),
            )],
            Row::Lane(..) => vec![
                (
                    RowPart::Eye,
                    Rect::new(r.right() - 26.0, r.y + 3.0, 20.0, r.h - 6.0),
                ),
                (
                    RowPart::Mode,
                    Rect::new(r.right() - 78.0, r.y + 4.0, 48.0, r.h - 8.0),
                ),
                (RowPart::Name, Rect::new(r.x, r.y, r.w - 80.0, r.h)),
            ],
        }
    }

    fn zoom_of(&self, model: &Session, plot: Rect) -> f64 {
        if self.zoom > 0.0 {
            return self.zoom;
        }
        let end = model.project().content_end().quarters().max(8.0);
        plot.w as f64 / (end * 1.05)
    }

    fn x_of(&self, model: &Session, plot: Rect, t: MusicalTime) -> f32 {
        plot.x + ((t.quarters() - self.scroll_q) * self.zoom_of(model, plot)) as f32
    }

    fn time_at(&self, model: &Session, plot: Rect, x: f32) -> MusicalTime {
        let q = self.scroll_q + (x - plot.x) as f64 / self.zoom_of(model, plot);
        MusicalTime::from_quarters(q.max(0.0))
    }

    fn snapped(&self, model: &Session, t: MusicalTime, mods: Modifiers) -> MusicalTime {
        if !model.editor.snap || mods.alt {
            return t;
        }
        let q = (t.quarters() / SNAP_QUARTERS).round() * SNAP_QUARTERS;
        MusicalTime::from_quarters(q.max(0.0))
    }

    fn value_y(param: &AutomationParam, plot: Rect, v: f64) -> f32 {
        plot.bottom() - plot.h * param.to_normal(v).clamp(0.0, 1.0) as f32
    }

    fn y_value(param: &AutomationParam, plot: Rect, y: f32) -> f64 {
        let n = ((plot.bottom() - y) / plot.h).clamp(0.0, 1.0) as f64;
        param.from_normal(n)
    }

    fn point_at(
        &self,
        model: &Session,
        plot: Rect,
        lane: &AutomationLane,
        param: &AutomationParam,
        pos: Point,
    ) -> Option<usize> {
        lane.curve
            .points()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let c = Point::new(
                    self.x_of(model, plot, p.time),
                    Self::value_y(param, plot, p.value),
                );
                (i, c.distance(pos))
            })
            .filter(|(_, d)| *d <= POINT_HIT)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    pub fn hit(&self, pos: Point, size: Size, model: &Session) -> Hit {
        let l = self.layout(size);
        if let Some((b, _)) = l.buttons.iter().find(|(_, r)| r.contains(pos)) {
            return Hit::Button(*b);
        }
        if l.list.contains(pos) {
            for (i, row) in self.rows(model).into_iter().enumerate() {
                let r = self.row_rect(l.list, i);
                if r.contains(pos) {
                    let part = Self::row_parts(r, row)
                        .into_iter()
                        .find(|(_, pr)| pr.contains(pos))
                        .map(|(p, _)| p);
                    return Hit::Row(row, part);
                }
            }
            return Hit::Nothing;
        }
        if pos.x >= l.plot.x - 6.0 && pos.y >= l.ruler.y {
            return Hit::Editor;
        }
        Hit::Nothing
    }

    // --- lane tools ----------------------------------------------------------------

    /// The control's value at the playhead, or over the edit selection.
    fn write_value(
        model: &Session,
        track: TrackId,
        lane: &AutomationLane,
    ) -> Option<AutomationLane> {
        let param = model.automation_param(track, lane.target)?;
        let v = model
            .static_value(track, lane.target)
            .or_else(|| model.display_value(track, lane.target))?;
        let mut l = lane.clone();
        let point = |time, value| AutomationPoint {
            time,
            value,
            shape: param.shape(),
        };
        match model.selection.range.filter(|r| r.end > r.start) {
            Some(r) => {
                let before = l.curve.value_at(r.start);
                let after = l.curve.value_at(r.end);
                let kept: Vec<AutomationPoint> = l
                    .curve
                    .points()
                    .iter()
                    .filter(|p| p.time < r.start || p.time > r.end)
                    .copied()
                    .collect();
                let mut c = AutomationCurve::from_points(kept);
                if let Some(b) = before {
                    c.insert(point(r.start, b));
                }
                c.insert(point(r.start, v));
                c.insert(point(r.end, v));
                if let Some(a) = after {
                    c.insert(point(r.end, a));
                }
                l.curve = c;
            }
            None => {
                let at = model.playhead();
                let keep: Vec<AutomationPoint> = l
                    .curve
                    .points()
                    .iter()
                    .filter(|p| p.time != at)
                    .copied()
                    .collect();
                l.curve = AutomationCurve::from_points(keep);
                if l.curve.is_empty() && at > MusicalTime::ZERO {
                    l.curve.insert(point(MusicalTime::ZERO, v));
                }
                l.curve.insert(point(at, v));
            }
        }
        Some(l)
    }

    /// Fewer points for the same curve (within half a percent of the
    /// lane's height).
    fn thinned(model: &Session, track: TrackId, lane: &AutomationLane) -> Option<AutomationLane> {
        let param = model.automation_param(track, lane.target)?;
        let pts = lane.curve.points();
        let normal: Vec<AutomationPoint> = pts
            .iter()
            .map(|p| AutomationPoint {
                value: param.to_normal(p.value),
                ..*p
            })
            .collect();
        let kept = thin(&normal, 0.005);
        // Keep the original points the thinned list kept (same order).
        let mut out = Vec::new();
        let mut k = kept.iter().peekable();
        for (p, n) in pts.iter().zip(&normal) {
            if k.peek()
                .is_some_and(|q| q.time == n.time && q.value == n.value)
            {
                out.push(*p);
                k.next();
            }
        }
        let mut l = lane.clone();
        l.curve = AutomationCurve::from_points(out);
        Some(l)
    }

    fn press_button(&mut self, b: Button, model: &Session, cx: &mut EventCx<'_, Action>) {
        let current = self.current(model);
        match b {
            Button::SelectedOnly => {
                self.selected_only = !self.selected_only;
                self.list_scroll = 0.0;
                cx.redraw();
            }
            Button::AllRead | Button::AllOff => cx.emit(Action::SetAutomationModes {
                lanes: self.listed_lanes(model),
                mode: if b == Button::AllRead {
                    AutomationMode::Read
                } else {
                    AutomationMode::Off
                },
            }),
            Button::WriteValue => {
                if let Some((t, l)) = current
                    && let Some(l) = Self::write_value(model, t, &l)
                {
                    cx.emit(Self::edit(t, l));
                }
            }
            Button::Thin => {
                if let Some((t, l)) = current
                    && let Some(l) = Self::thinned(model, t, &l)
                {
                    cx.emit(Self::edit(t, l));
                }
            }
            Button::Clear => {
                if let Some((t, mut l)) = current {
                    l.curve = AutomationCurve::new();
                    cx.emit(Self::edit(t, l));
                }
            }
            Button::Delete => {
                if let Some((t, l)) = current {
                    cx.emit(Action::Edit(Command::RemoveAutomationLane {
                        track: t,
                        lane: l.id,
                    }));
                    self.selected = None;
                }
            }
            Button::Fit => {
                self.zoom = 0.0;
                self.scroll_q = 0.0;
                cx.redraw();
            }
        }
    }

    fn add_menu(model: &Session, track: TrackId, at: Point) -> HostRequest<Action> {
        let existing: Vec<_> = model
            .project()
            .track(track)
            .map(|t| t.automation.lanes.iter().map(|l| l.target).collect())
            .unwrap_or_default();
        let mut items: Vec<MenuItem<Action>> = model
            .automatable_parameters(track)
            .into_iter()
            .filter(|p| !existing.contains(&p.target))
            .map(|p| {
                MenuItem::new(
                    p.name.clone(),
                    Action::ShowAutomation {
                        track,
                        target: p.target,
                    },
                )
            })
            .collect();
        if items.is_empty() {
            items.push(MenuItem::disabled("Every parameter has a lane"));
        }
        HostRequest::ContextMenu { at, items }
    }

    fn mode_menu(track: TrackId, lane: &AutomationLane, at: Point) -> HostRequest<Action> {
        HostRequest::ContextMenu {
            at,
            items: MODES
                .iter()
                .map(|m| {
                    MenuItem::new(
                        mode_label(*m),
                        Action::SetAutomationMode {
                            track,
                            lane: lane.id,
                            mode: *m,
                        },
                    )
                    .checked(lane.mode == *m)
                })
                .collect(),
        }
    }

    fn point_menu(&self, model: &Session, size: Size, pos: Point) -> Option<HostRequest<Action>> {
        let (track, lane) = self.current(model)?;
        let param = model.automation_param(track, lane.target)?;
        let plot = self.layout(size).plot;
        let at = self.time_at(model, plot, pos.x);
        let point = self.point_at(model, plot, &lane, &param, pos);
        let index = point.or_else(|| {
            lane.curve
                .points()
                .partition_point(|p| p.time <= at)
                .checked_sub(1)
        });
        let mut items = Vec::new();
        if let Some(i) = index
            && let Some(p) = lane.curve.points().get(i).copied()
        {
            for (shape, label) in SHAPES {
                let mut l = lane.clone();
                l.curve.update(i, AutomationPoint { shape, ..p });
                items.push(MenuItem::new(label, Self::edit(track, l)).checked(p.shape == shape));
            }
        }
        if let Some(i) = point {
            let mut l = lane.clone();
            l.curve.remove(i);
            items.push(MenuItem::new("Delete Point", Self::edit(track, l)).separated());
        }
        if let Some(l) = Self::write_value(model, track, &lane) {
            items.push(
                MenuItem::new(
                    if model.selection.range.is_some_and(|r| r.end > r.start) {
                        "Write Value over the Selection"
                    } else {
                        "Write Value at the Playhead"
                    },
                    Self::edit(track, l),
                )
                .separated(),
            );
        }
        Some(HostRequest::ContextMenu { at: pos, items })
    }

    // --- painting ------------------------------------------------------------------

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

    fn paint_toolbar(&self, p: &mut dyn Painter, l: &Layout, has_lane: bool) {
        let th = &self.theme;
        p.fill(l.toolbar, th.ui.surface);
        p.hline(
            0.0,
            l.toolbar.right(),
            l.toolbar.bottom() - 0.5,
            th.ui.border,
        );
        for (b, r) in &l.buttons {
            let label = match b {
                Button::SelectedOnly => "Selected Tracks",
                Button::AllRead => "All Read",
                Button::AllOff => "All Off",
                Button::WriteValue => "Write Value",
                Button::Thin => "Thin",
                Button::Clear => "Clear",
                Button::Delete => "Delete Lane",
                Button::Fit => "Fit",
            };
            let on = *b == Button::SelectedOnly && self.selected_only;
            if !has_lane
                && matches!(
                    b,
                    Button::WriteValue | Button::Thin | Button::Clear | Button::Delete
                )
            {
                p.fill_rounded(*r, 4.0, &Paint::Solid(th.ui.surface_alt.with_alpha(0.4)));
                p.text(
                    label,
                    *r,
                    &TextStyle::new(th.fonts.small, th.ui.text_faint).center(),
                );
                continue;
            }
            self.button(p, *r, label, on);
        }
    }

    fn paint_list(
        &self,
        p: &mut dyn Painter,
        l: &Layout,
        model: &Session,
        current: Option<(TrackId, AutomationLaneId)>,
    ) {
        let th = &self.theme;
        p.fill(l.list, th.ui.background);
        p.push_clip(l.list);
        let rows = self.rows(model);
        if rows.is_empty() {
            p.text(
                if self.selected_only {
                    "No selected track can be automated."
                } else {
                    "No track can be automated."
                },
                l.list.inset(12.0),
                &TextStyle::new(th.fonts.small, th.ui.text_dim),
            );
        }
        for (i, row) in rows.into_iter().enumerate() {
            let r = self.row_rect(l.list, i);
            if r.bottom() < l.list.y || r.y > l.list.bottom() {
                continue;
            }
            match row {
                Row::Track(id) => {
                    let Some(t) = model.project().track(id) else {
                        continue;
                    };
                    p.fill(r, th.ui.surface);
                    let c = Color::rgb8(t.color.r, t.color.g, t.color.b);
                    p.fill(Rect::new(r.x, r.y, 4.0, r.h), c);
                    p.text(
                        &t.name,
                        Rect::new(r.x + 10.0, r.y, r.w - 44.0, r.h),
                        &TextStyle::new(th.fonts.small, th.ui.text).bold(),
                    );
                    let add = Self::row_parts(r, row)[0].1;
                    p.text(
                        "+",
                        add,
                        &TextStyle::new(th.fonts.normal, th.ui.accent)
                            .bold()
                            .center(),
                    );
                }
                Row::Lane(t, id) => {
                    let Some(lane) = Self::lane(model, t, id) else {
                        continue;
                    };
                    if current == Some((t, id)) {
                        p.fill(r, th.ui.selection.with_alpha(0.28));
                    }
                    let name = model
                        .automation_param(t, lane.target)
                        .map_or_else(|| "?".to_string(), |p| p.name);
                    let parts = Self::row_parts(r, row);
                    let name_r = parts[2].1;
                    p.text(
                        &name,
                        Rect::new(r.x + 22.0, r.y, name_r.w - 92.0, r.h),
                        &TextStyle::new(th.fonts.small, th.ui.text),
                    );
                    let count = lane.curve.points().len();
                    p.text(
                        &match count {
                            0 => "no points".to_string(),
                            1 => "1 point".to_string(),
                            n => format!("{n} points"),
                        },
                        Rect::new(name_r.right() - 70.0, r.y, 64.0, r.h),
                        &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                            .align(faderframe_ui_canvas::Align::End),
                    );
                    let mode = parts[1].1;
                    let mc = mode_color(lane.mode);
                    p.fill_rounded(mode, 3.0, &Paint::Solid(mc.with_alpha(0.25)));
                    p.stroke_rounded(mode, 3.0, 1.0, mc);
                    p.text(
                        mode_label(lane.mode),
                        mode,
                        &TextStyle::new(th.fonts.tiny, th.ui.text).bold().center(),
                    );
                    let shown = model.shown_lanes(t).iter().any(|l| l.id == id);
                    let eye = parts[0].1;
                    let c = eye.center();
                    p.circle(
                        c,
                        5.0,
                        if shown {
                            th.ui.accent
                        } else {
                            th.ui.text_faint.with_alpha(0.5)
                        },
                    );
                    p.circle(c, 2.2, th.ui.background);
                }
            }
            p.hline(
                r.x,
                r.right(),
                r.bottom() - 0.5,
                th.ui.border.with_alpha(0.6),
            );
        }
        p.pop_clip();
        p.vline(
            l.list.right() - 0.5,
            l.list.y,
            l.list.bottom(),
            th.ui.border,
        );
    }

    fn paint_editor(&self, p: &mut dyn Painter, l: &Layout, size: Size, model: &Session) {
        let th = &self.theme;
        let right = Rect::new(l.list.right(), l.list.y, size.w - l.list.right(), l.list.h);
        p.fill(right, th.arranger.lane_a);
        let Some((track, lane)) = self.current(model) else {
            p.text(
                "No automation lane yet: add one with “+” next to a track.",
                right.inset(16.0),
                &TextStyle::new(th.fonts.normal, th.ui.text_dim),
            );
            return;
        };
        let Some(param) = model.automation_param(track, lane.target) else {
            return;
        };
        let plot = l.plot;
        let tl = &model.project().timeline;
        // Ruler with bars.
        p.fill(l.ruler, th.arranger.ruler_bg);
        let zoom = self.zoom_of(model, plot);
        let first = tl
            .meter
            .bar_at(MusicalTime::from_quarters(self.scroll_q.max(0.0)));
        let mut bar = first;
        loop {
            let t = tl.meter.bar_start(bar);
            let x = self.x_of(model, plot, t);
            if x > plot.right() {
                break;
            }
            let bar_px = tl.meter.signature_of_bar(bar).bar_length().quarters() * zoom;
            let every = (40.0 / bar_px.max(1.0)).ceil().max(1.0) as i32;
            if x >= plot.x && (bar - first) % every == 0 {
                p.vline(
                    x,
                    l.ruler.y + 6.0,
                    l.ruler.bottom(),
                    th.arranger.ruler_text.with_alpha(0.6),
                );
                p.text(
                    &format!("{}", bar + 1),
                    Rect::new(x + 3.0, l.ruler.y, 40.0, l.ruler.h),
                    &TextStyle::new(th.fonts.tiny, th.arranger.ruler_text),
                );
                p.vline(x, plot.y, plot.bottom(), th.ui.text.with_alpha(0.07));
            }
            bar += 1;
            if bar > first + 100_000 {
                break;
            }
        }
        // Value scale and grid.
        p.fill(l.scale, th.ui.surface);
        for n in [0.0f64, 0.25, 0.5, 0.75, 1.0] {
            let y = plot.bottom() - plot.h * n as f32;
            p.hline(plot.x, plot.right(), y, th.ui.text.with_alpha(0.08));
            p.text(
                &param.format(param.from_normal(n)),
                Rect::new(l.scale.x + 4.0, y - 8.0, SCALE_W - 8.0, 16.0),
                &TextStyle::new(th.fonts.tiny, th.ui.text_dim)
                    .align(faderframe_ui_canvas::Align::End),
            );
        }
        p.push_clip(Rect::new(
            plot.x - 6.0,
            plot.y - 6.0,
            plot.w + 12.0,
            plot.h + 12.0,
        ));
        // Loop and edit selection.
        let project = model.project();
        if let Some(r) = project.loop_range.filter(|_| project.loop_enabled) {
            let (a, b) = (
                self.x_of(model, plot, r.start),
                self.x_of(model, plot, r.end),
            );
            p.fill(
                Rect::new(a, plot.y, b - a, plot.h),
                th.ui.accent.with_alpha(0.06),
            );
        }
        if let Some(r) = model.selection.range.filter(|r| r.end > r.start) {
            let (a, b) = (
                self.x_of(model, plot, r.start),
                self.x_of(model, plot, r.end),
            );
            p.fill(
                Rect::new(a, plot.y, b - a, plot.h),
                th.ui.selection.with_alpha(0.16),
            );
        }
        // The curve: the static value without points.
        let tc = model
            .project()
            .track(track)
            .map(|t| Color::rgb8(t.color.r, t.color.g, t.color.b))
            .unwrap_or(th.ui.accent);
        let line = if lane.mode == AutomationMode::Off {
            th.ui.text_faint
        } else {
            tc.lighten(0.15)
        };
        let value_at = |x: f32| {
            let t = self.time_at(model, plot, x);
            lane.curve
                .value_at(t)
                .or_else(|| model.static_value(track, lane.target))
                .unwrap_or(param.default)
        };
        let mut top = Path::new();
        let mut fill = Path::new();
        let mut x = plot.x;
        let step = 2.0;
        fill.move_to(Point::new(plot.x, plot.bottom()));
        while x <= plot.right() + step {
            let pt = Point::new(
                x.min(plot.right()),
                Self::value_y(&param, plot, value_at(x)),
            );
            if x == plot.x {
                top.move_to(pt);
            } else {
                top.line_to(pt);
            }
            fill.line_to(pt);
            x += step;
        }
        fill.line_to(Point::new(plot.right(), plot.bottom()))
            .close();
        p.fill_path(&fill, line.with_alpha(0.14));
        p.stroke_path(&top, 2.0, line);
        for pt in lane.curve.points() {
            let x = self.x_of(model, plot, pt.time);
            if x < plot.x - 6.0 || x > plot.right() + 6.0 {
                continue;
            }
            let c = Point::new(x, Self::value_y(&param, plot, pt.value));
            p.circle(c, 5.0, Color::rgba(0.0, 0.0, 0.0, 0.55));
            p.circle(c, 3.8, line.lighten(0.3));
        }
        // Playhead.
        let ph = self.x_of(model, plot, model.playhead());
        if ph >= plot.x && ph <= plot.right() {
            p.vline(ph, plot.y - 6.0, plot.bottom() + 6.0, th.arranger.playhead);
        }
        p.pop_clip();
        // Lane title and mode.
        let title = format!(
            "{} · {}  —  {}",
            model.project().track(track).map_or("", |t| t.name.as_str()),
            param.name,
            mode_label(lane.mode)
        );
        p.text(
            &title,
            Rect::new(plot.x + 8.0, plot.y + 2.0, plot.w - 16.0, 16.0),
            &TextStyle::new(th.fonts.small, th.ui.text_dim).bold(),
        );
    }
}

impl CanvasView<Session, Action> for AutomationView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, _theme: &Theme) {
        let l = self.layout(size);
        let current = self.current(model).map(|(t, lane)| (t, lane.id));
        p.fill(Rect::from_size(size), self.theme.ui.background);
        self.paint_editor(p, &l, size, model);
        self.paint_list(p, &l, model, current);
        self.paint_toolbar(p, &l, current.is_some());
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let l = self.layout(size);
        match ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => match self.hit(*pos, size, model) {
                Hit::Editor => {
                    if let Some(req) = self.point_menu(model, size, *pos) {
                        cx.request(req);
                    }
                    true
                }
                Hit::Row(Row::Lane(t, id), _) => {
                    if let Some(lane) = Self::lane(model, t, id) {
                        cx.request(Self::mode_menu(t, lane, *pos));
                    }
                    true
                }
                Hit::Row(Row::Track(t), _) => {
                    cx.request(Self::add_menu(model, t, *pos));
                    true
                }
                _ => false,
            },
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks,
            } => match self.hit(*pos, size, model) {
                Hit::Button(b) => {
                    self.press_button(b, model, cx);
                    true
                }
                Hit::Row(Row::Track(t), part) => {
                    if part == Some(RowPart::Add) {
                        cx.request(Self::add_menu(model, t, *pos));
                    }
                    true
                }
                Hit::Row(Row::Lane(t, id), part) => {
                    self.selected = Some((t, id));
                    let Some(lane) = Self::lane(model, t, id) else {
                        return true;
                    };
                    match part {
                        Some(RowPart::Mode) => cx.request(Self::mode_menu(t, lane, *pos)),
                        Some(RowPart::Eye) => {
                            if model.shown_lanes(t).iter().any(|x| x.id == id) {
                                cx.emit(Action::HideAutomationLane(id));
                            } else {
                                cx.emit(Action::ShowAutomation {
                                    track: t,
                                    target: lane.target,
                                });
                            }
                        }
                        _ => {}
                    }
                    cx.redraw();
                    true
                }
                Hit::Editor => {
                    let Some((track, lane)) = self.current(model) else {
                        return true;
                    };
                    self.selected = Some((track, lane.id));
                    let Some(param) = model.automation_param(track, lane.target) else {
                        return true;
                    };
                    let plot = l.plot;
                    if let Some(i) = self.point_at(model, plot, &lane, &param, *pos) {
                        if *clicks >= 2 {
                            let mut lane = lane;
                            lane.curve.remove(i);
                            cx.emit(Self::edit(track, lane));
                            return true;
                        }
                        cx.emit(Action::BeginGesture("Move Automation Point".into()));
                        self.drag = Some(Drag {
                            track,
                            base: lane,
                            index: i,
                            origin: *pos,
                            moved: false,
                        });
                        return true;
                    }
                    if !plot.contains(*pos) {
                        return true;
                    }
                    let time = self.snapped(model, self.time_at(model, plot, pos.x), *modifiers);
                    let value = Self::y_value(&param, plot, pos.y);
                    let mut lane = lane;
                    if lane.curve.is_empty()
                        && time > MusicalTime::ZERO
                        && let Some(v) = model.static_value(track, lane.target)
                    {
                        lane.curve.insert(AutomationPoint {
                            time: MusicalTime::ZERO,
                            value: v,
                            shape: param.shape(),
                        });
                    }
                    let i = lane.curve.insert(AutomationPoint {
                        time,
                        value,
                        shape: param.shape(),
                    });
                    cx.emit(Action::BeginGesture("Add Automation Point".into()));
                    cx.emit(Self::edit(track, lane.clone()));
                    self.drag = Some(Drag {
                        track,
                        base: lane,
                        index: i,
                        origin: *pos,
                        moved: false,
                    });
                    true
                }
                Hit::Nothing => false,
            },
            ViewEvent::PointerMove {
                pos,
                modifiers,
                dragging: true,
            } => {
                let plot = l.plot;
                let Some(d) = self.drag.as_ref() else {
                    return false;
                };
                if !d.moved && pos.distance(d.origin) < DRAG_THRESHOLD {
                    return true;
                }
                let Some(param) = model.automation_param(d.track, d.base.target) else {
                    return true;
                };
                let time = self.snapped(model, self.time_at(model, plot, pos.x), *modifiers);
                let value = Self::y_value(&param, plot, pos.y);
                let Some(d) = self.drag.as_mut() else {
                    return true;
                };
                d.moved = true;
                let mut lane = d.base.clone();
                if let Some(old) = lane.curve.points().get(d.index).copied() {
                    lane.curve
                        .update(d.index, AutomationPoint { time, value, ..old });
                }
                cx.emit(Self::edit(d.track, lane));
                cx.set_cursor(Cursor::Grabbing);
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let c = match self.hit(*pos, size, model) {
                    Hit::Button(_) | Hit::Row(..) => Cursor::Pointer,
                    Hit::Editor => Cursor::Crosshair,
                    Hit::Nothing => Cursor::Default,
                };
                cx.set_cursor(c);
                false
            }
            ViewEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => {
                if self.drag.take().is_some() {
                    cx.emit(Action::EndGesture);
                    return true;
                }
                false
            }
            ViewEvent::Scroll {
                pos,
                dx,
                dy,
                modifiers,
                precise,
            } => {
                let unit = if *precise { 1.0 / 40.0 } else { 1.0 };
                if l.list.contains(*pos) {
                    let rows = self.rows(model).len() as f32;
                    let max = (rows * ROW_H - l.list.h).max(0.0);
                    self.list_scroll = (self.list_scroll + dy * unit * ROW_H * 2.0).clamp(0.0, max);
                    cx.redraw();
                    return true;
                }
                let plot = l.plot;
                let zoom = self.zoom_of(model, plot);
                if modifiers.ctrl {
                    let at = self.time_at(model, plot, pos.x).quarters();
                    let factor = (1.15f64).powf(-(*dy * unit) as f64);
                    let new = (zoom * factor).clamp(0.5, 2000.0);
                    self.zoom = new;
                    self.scroll_q = (at - (pos.x - plot.x) as f64 / new).max(0.0);
                } else {
                    let d = if modifiers.shift || dx.abs() > dy.abs() {
                        if dx.abs() > 0.0 { *dx } else { *dy }
                    } else {
                        *dy
                    };
                    self.zoom = zoom;
                    self.scroll_q = (self.scroll_q + d as f64 * unit as f64 * 60.0 / zoom).max(0.0);
                }
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        match self.hit(pos, size, model) {
            Hit::Button(b) => Some(
                match b {
                    Button::SelectedOnly => "Only the tracks selected in the arranger",
                    Button::AllRead => "Every listed lane plays back (Read)",
                    Button::AllOff => "Every listed lane off",
                    Button::WriteValue => {
                        "Write the control's current value at the playhead (or over the edit selection)"
                    }
                    Button::Thin => "Remove points the curve does not need",
                    Button::Clear => "Remove every point of this lane",
                    Button::Delete => "Delete this lane",
                    Button::Fit => "Show the whole song",
                }
                .into(),
            ),
            Hit::Row(Row::Lane(..), Some(RowPart::Eye)) => Some("Show or hide the lane in the arranger".into()),
            Hit::Row(Row::Lane(..), Some(RowPart::Mode)) => Some("Automation mode".into()),
            Hit::Row(Row::Track(_), _) => Some("Add an automation lane for this track".into()),
            Hit::Editor => {
                let (track, lane) = self.current(model)?;
                let param = model.automation_param(track, lane.target)?;
                let plot = self.layout(size).plot;
                let t = self.time_at(model, plot, pos.x);
                let v = Self::y_value(&param, plot, pos.y);
                Some(format!(
                    "{} at {} · Click to add a point, drag points, double-click to delete, right-click for shapes",
                    param.format(v),
                    model.project().timeline.format_bbt(t)
                ))
            }
            _ => None,
        }
    }

    fn min_size(&self) -> Size {
        Size::new(420.0, 160.0)
    }
}
