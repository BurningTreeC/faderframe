//! Automation lanes under a track: painting, hit testing and editing.
//!
//! Each shown lane (session: `shown_lanes`) is a row below the track's main
//! lane and take lanes. Its header (in the header column) shows the
//! parameter (click: choose which parameters are shown), the mode (click:
//! Off/Read/Touch/Latch/Write) and a close button. In the lane: click to
//! add a point and drag it, drag points to move them (snapped; Alt for
//! free), double-click a point to delete it, Ctrl+drag to draw freehand,
//! right-click for curve shapes, clearing and modes.

use crate::{ArrangerView, DRAG_THRESHOLD};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, CurveShape, thin,
};
use faderframe_core::{AutomationLaneId, TrackId};
use faderframe_project::{Command, Track};
use faderframe_session::{Action, AutomationParam, Session};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    Color, EventCx, HostRequest, MenuItem, Modifiers, Paint, Painter, Path, Point, Rect, TextStyle,
};

/// Height of one automation lane.
pub const AUTO_LANE_H: f32 = 56.0;
/// Pointer distance that picks a point.
const POINT_HIT: f32 = 6.0;
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

/// Which part of a lane header was hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoHeaderPart {
    Name,
    Mode,
    Close,
    Body,
}

/// Geometry of one shown lane.
#[derive(Clone, Copy, Debug)]
pub struct LaneGeom {
    pub track: TrackId,
    pub lane: AutomationLaneId,
    /// Full row (header column + lane area).
    pub row: Rect,
}

impl LaneGeom {
    pub fn header_parts(&self, header_w: f32) -> [(Rect, AutoHeaderPart); 3] {
        let r = Rect::new(self.row.x + 14.0, self.row.y + 6.0, header_w - 24.0, 16.0);
        let close = Rect::new(r.right() - 16.0, r.y, 16.0, 16.0);
        let mode = Rect::new(close.x - 54.0, r.y, 50.0, 16.0);
        let name = Rect::new(r.x, r.y, mode.x - r.x - 6.0, 16.0);
        [
            (name, AutoHeaderPart::Name),
            (mode, AutoHeaderPart::Mode),
            (close, AutoHeaderPart::Close),
        ]
    }

    /// Value area (inset so extreme values stay visible).
    pub fn value_area(&self, lanes_x: f32, lanes_right: f32) -> Rect {
        Rect::new(
            lanes_x,
            self.row.y + 5.0,
            lanes_right - lanes_x,
            self.row.h - 10.0,
        )
    }
}

/// A drag in an automation lane (outside the `Copy` drag enum).
pub struct AutoDrag {
    pub track: TrackId,
    pub base: AutomationLane,
    pub kind: AutoDragKind,
    pub origin: Point,
    pub moved: bool,
}

pub enum AutoDragKind {
    /// Moving point `index` of `base`.
    Point(usize),
    /// Freehand drawing: points collected so far.
    Draw(Vec<AutomationPoint>),
}

impl ArrangerView {
    /// Lane rows of `track` whose row starts at `row` (main lane + takes).
    pub(crate) fn lane_geoms(&self, model: &Session, t: &Track, row: Rect) -> Vec<LaneGeom> {
        let mut y =
            row.y + self.base_h(model, t.id) + Self::open_lanes(t, model) as f32 * crate::LANE_H;
        model
            .shown_lanes(t.id)
            .iter()
            .map(|l| {
                let g = LaneGeom {
                    track: t.id,
                    lane: l.id,
                    row: Rect::new(0.0, y, row.w, AUTO_LANE_H),
                };
                y += AUTO_LANE_H;
                g
            })
            .collect()
    }

    fn lane_of(model: &Session, track: TrackId, lane: AutomationLaneId) -> Option<&AutomationLane> {
        model
            .project()
            .track(track)?
            .automation
            .lanes
            .iter()
            .find(|l| l.id == lane)
    }

    fn value_y(param: &AutomationParam, area: Rect, v: f64) -> f32 {
        area.bottom() - param.to_normal(v) as f32 * area.h
    }

    fn y_value(param: &AutomationParam, area: Rect, y: f32) -> f64 {
        param.from_normal(((area.bottom() - y) / area.h.max(1.0)) as f64)
    }

    /// Paint the lanes of a track: the lane areas (`headers == false`, under
    /// the lanes clip) or their header parts (`headers == true`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_automation(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        t: &Track,
        row: Rect,
        lanes: Rect,
        headers: bool,
        color: Color,
    ) {
        let th = &self.theme;
        for g in self.lane_geoms(model, t, row) {
            if g.row.bottom() < lanes.y || g.row.y > lanes.bottom() {
                continue;
            }
            let Some(lane) = Self::lane_of(model, t.id, g.lane) else {
                continue;
            };
            let param = model.automation_param(t.id, lane.target);
            let writing = model.is_lane_writing(lane.id);
            if !headers {
                let bg = Rect::new(lanes.x, g.row.y, lanes.w, g.row.h);
                p.fill(bg, Color::rgba(0.0, 0.0, 0.0, 0.32));
                if writing {
                    p.fill(bg, Color::rgba(1.0, 0.25, 0.2, 0.08));
                }
                p.hline(
                    lanes.x,
                    lanes.right(),
                    g.row.bottom() - 0.5,
                    th.arranger.header_border,
                );
                let area = g.value_area(lanes.x, lanes.right());
                if let Some(param) = &param {
                    self.paint_curve(p, model, t, lane, param, area, color);
                } else {
                    p.text(
                        "Parameter not available (plugin removed?)",
                        area,
                        &TextStyle::new(th.fonts.small, th.ui.text_faint),
                    );
                }
                continue;
            }

            // Header part.
            let hdr = Rect::new(0.0, g.row.y, self.header_w(), g.row.h);
            p.fill(hdr, th.arranger.header_bg.darken(0.15));
            p.fill(Rect::new(0.0, g.row.y, 5.0, g.row.h), color.darken(0.35));
            p.hline(
                0.0,
                self.header_w(),
                g.row.bottom() - 0.5,
                th.arranger.header_border,
            );
            let [(name_r, _), (mode_r, _), (close_r, _)] = g.header_parts(self.header_w());
            let name = param.as_ref().map_or("?", |p| p.name.as_str());
            p.text(
                name,
                name_r,
                &TextStyle::new(th.fonts.small, th.ui.text).bold(),
            );
            let mc = mode_color(lane.mode);
            p.fill_rounded(mode_r, 3.0, &Paint::Solid(mc.with_alpha(0.22)));
            p.stroke_rounded(mode_r, 3.0, 1.0, mc.with_alpha(0.8));
            p.text(
                mode_label(lane.mode),
                mode_r,
                &TextStyle::new(th.fonts.small, mc).bold().center(),
            );
            p.text(
                "×",
                close_r,
                &TextStyle::new(th.fonts.normal, th.ui.text_dim).center(),
            );
            if let Some(param) = &param
                && let Some(v) = model.display_value(t.id, lane.target)
            {
                let r = Rect::new(name_r.x, g.row.y + 26.0, name_r.w + 60.0, 14.0);
                let text = if writing {
                    format!("● {}", param.format(v))
                } else {
                    param.format(v)
                };
                p.text(&text, r, &TextStyle::new(th.fonts.small, th.ui.text_dim));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_curve(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        t: &Track,
        lane: &AutomationLane,
        param: &AutomationParam,
        area: Rect,
        color: Color,
    ) {
        let th = &self.theme;
        let line = if lane.mode == AutomationMode::Off {
            Color::hex(0x8a8d94)
        } else {
            color.lighten(0.25)
        };
        if lane.curve.is_empty() {
            // No points yet: the static value as a dashed line.
            let v = model
                .static_value(t.id, lane.target)
                .unwrap_or(param.default);
            let y = Self::value_y(param, area, v);
            let mut x = area.x;
            while x < area.right() {
                p.hline(x, (x + 6.0).min(area.right()), y, line.with_alpha(0.6));
                x += 10.0;
            }
            p.text(
                "Click to add points · Ctrl+drag to draw",
                Rect::new(area.x + 8.0, area.y, 300.0, 14.0),
                &TextStyle::new(th.fonts.small, th.ui.text_faint),
            );
            return;
        }
        // The curve, sampled per pixel column.
        let step = 2.0;
        let mut top = Path::new();
        let mut fill = Path::new();
        let mut x = area.x;
        let mut first = true;
        while x <= area.right() + step {
            let v = lane
                .curve
                .value_at(self.time_at(x).max(MusicalTime::ZERO))
                .unwrap_or(param.default);
            let pt = Point::new(x, Self::value_y(param, area, v));
            if first {
                top.move_to(pt);
                fill.move_to(Point::new(x, area.bottom()));
                first = false;
            } else {
                top.line_to(pt);
            }
            fill.line_to(pt);
            x += step;
        }
        fill.line_to(Point::new(x - step, area.bottom())).close();
        p.fill_path(&fill, line.with_alpha(0.13));
        p.stroke_path(&top, 1.6, line);
        // Points.
        for pt in lane.curve.points() {
            let x = self.x_of(pt.time);
            if x < area.x - 4.0 || x > area.right() + 4.0 {
                continue;
            }
            let c = Point::new(x, Self::value_y(param, area, pt.value));
            p.circle(c, 4.0, Color::rgba(0.0, 0.0, 0.0, 0.6));
            p.circle(c, 3.0, line.lighten(0.3));
        }
    }

    // --- hit testing ------------------------------------------------------------------

    /// Lane under `pos`: `(geometry, part)` where part is `None` in the
    /// lane area.
    pub(crate) fn auto_hit(
        &self,
        model: &Session,
        t: &Track,
        row: Rect,
        pos: Point,
    ) -> Option<(LaneGeom, Option<AutoHeaderPart>)> {
        let g = self
            .lane_geoms(model, t, row)
            .into_iter()
            .find(|g| pos.y >= g.row.y && pos.y < g.row.bottom())?;
        if pos.x < self.header_w() {
            let part = g
                .header_parts(self.header_w())
                .into_iter()
                .find(|(r, _)| r.contains(pos))
                .map_or(AutoHeaderPart::Body, |(_, part)| part);
            Some((g, Some(part)))
        } else {
            Some((g, None))
        }
    }

    /// Index of the point of `lane` under `pos`.
    fn point_at(
        &self,
        lane: &AutomationLane,
        param: &AutomationParam,
        area: Rect,
        pos: Point,
    ) -> Option<usize> {
        lane.curve
            .points()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let c = Point::new(self.x_of(p.time), Self::value_y(param, area, p.value));
                (i, c.distance(pos))
            })
            .filter(|(_, d)| *d <= POINT_HIT)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    fn edit_lane(track: TrackId, lane: AutomationLane) -> Action {
        Action::Edit(Command::SetAutomationLane {
            track,
            lane: Box::new(lane),
        })
    }

    // --- interaction ------------------------------------------------------------------

    /// Primary press inside an automation lane row.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn auto_press(
        &mut self,
        model: &Session,
        g: LaneGeom,
        part: Option<AutoHeaderPart>,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        lanes_right: f32,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(lane) = Self::lane_of(model, g.track, g.lane).cloned() else {
            return false;
        };
        match part {
            Some(AutoHeaderPart::Close) => cx.emit(Action::HideAutomationLane(g.lane)),
            Some(AutoHeaderPart::Mode) => cx.request(Self::mode_menu(g.track, &lane, pos)),
            Some(AutoHeaderPart::Name) => cx.request(Self::parameter_menu(model, g.track, pos)),
            Some(AutoHeaderPart::Body) => {}
            None => {
                let Some(param) = model.automation_param(g.track, lane.target) else {
                    return true;
                };
                let area = g.value_area(self.header_w(), lanes_right);
                if mods.ctrl {
                    cx.emit(Action::BeginGesture("Draw Automation".into()));
                    self.auto_drag = Some(AutoDrag {
                        track: g.track,
                        base: lane,
                        kind: AutoDragKind::Draw(Vec::new()),
                        origin: pos,
                        moved: true,
                    });
                    self.auto_drag_move(model, pos, mods, lanes_right, cx);
                    return true;
                }
                if let Some(i) = self.point_at(&lane, &param, area, pos) {
                    if clicks >= 2 {
                        let mut l = lane;
                        l.curve.remove(i);
                        cx.emit(Self::edit_lane(g.track, l));
                        return true;
                    }
                    cx.emit(Action::BeginGesture("Move Automation Point".into()));
                    self.auto_drag = Some(AutoDrag {
                        track: g.track,
                        base: lane,
                        kind: AutoDragKind::Point(i),
                        origin: pos,
                        moved: false,
                    });
                    return true;
                }
                // Add a point here and drag it.
                let time = self.snap(self.time_at(pos.x).max(MusicalTime::ZERO), model, mods);
                let value = Self::y_value(&param, area, pos.y);
                let mut l = lane;
                // A first point keeps the static value before it.
                if l.curve.is_empty()
                    && let Some(v) = model.static_value(g.track, l.target)
                    && time > MusicalTime::ZERO
                {
                    l.curve.insert(AutomationPoint {
                        time: MusicalTime::ZERO,
                        value: v,
                        shape: param.shape(),
                    });
                }
                let i = l.curve.insert(AutomationPoint {
                    time,
                    value,
                    shape: param.shape(),
                });
                cx.emit(Action::BeginGesture("Add Automation Point".into()));
                cx.emit(Self::edit_lane(g.track, l.clone()));
                self.auto_drag = Some(AutoDrag {
                    track: g.track,
                    base: l,
                    kind: AutoDragKind::Point(i),
                    origin: pos,
                    moved: false,
                });
            }
        }
        true
    }

    /// Returns `true` when an automation drag consumed the move.
    pub(crate) fn auto_drag_move(
        &mut self,
        model: &Session,
        pos: Point,
        mods: Modifiers,
        lanes_right: f32,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let header_w = self.header_w();
        let Some(d) = self.auto_drag.as_mut() else {
            return false;
        };
        if !d.moved && pos.distance(d.origin) < DRAG_THRESHOLD {
            return true;
        }
        d.moved = true;
        let (track, lane_id, target) = (d.track, d.base.id, d.base.target);
        let Some(param) = model.automation_param(track, target) else {
            return true;
        };
        // Recompute the lane geometry (rows may have moved).
        let Some(t) = model.project().track(track) else {
            return true;
        };
        let tracks = Self::lane_tracks(model);
        let Some(i) = tracks.iter().position(|x| x.id == track) else {
            return true;
        };
        let row = self.row_rect(i, faderframe_ui_canvas::Size::new(lanes_right, 0.0));
        let Some(g) = self
            .lane_geoms(model, t, row)
            .into_iter()
            .find(|g| g.lane == lane_id)
        else {
            return true;
        };
        let area = g.value_area(header_w, lanes_right);
        let value = Self::y_value(&param, area, pos.y.clamp(area.y, area.bottom()));
        let raw_time = self.time_at(pos.x).max(MusicalTime::ZERO);
        let time = self.snap(raw_time, model, mods);
        let Some(d) = self.auto_drag.as_mut() else {
            return true;
        };
        let mut lane = d.base.clone();
        match &mut d.kind {
            AutoDragKind::Point(i) => {
                let Some(old) = lane.curve.points().get(*i).copied() else {
                    return true;
                };
                lane.curve
                    .update(*i, AutomationPoint { time, value, ..old });
            }
            AutoDragKind::Draw(points) => {
                if points.last().is_none_or(|p| raw_time > p.time) {
                    points.push(AutomationPoint {
                        time: raw_time,
                        value,
                        shape: param.shape(),
                    });
                }
                if let (Some(a), Some(b)) = (points.first(), points.last()) {
                    let (from, to) = (a.time.min(b.time), a.time.max(b.time));
                    lane.curve.replace_range(from, to, points);
                }
            }
        }
        cx.emit(Self::edit_lane(track, lane));
        true
    }

    /// Returns `true` when an automation drag ended.
    pub(crate) fn auto_release(&mut self, model: &Session, cx: &mut EventCx<'_, Action>) -> bool {
        let Some(d) = self.auto_drag.take() else {
            return false;
        };
        if let AutoDragKind::Draw(points) = &d.kind
            && let Some(param) = model.automation_param(d.track, d.base.target)
            && points.len() > 2
        {
            // Thin the drawn stroke into a tidy curve.
            let tolerance = (param.max - param.min).abs() * 0.003;
            let thinned = thin(
                points,
                if param.kind == faderframe_session::ParamKind::Gain {
                    0.2
                } else {
                    tolerance
                },
            );
            let mut lane = d.base.clone();
            if let (Some(a), Some(b)) = (thinned.first(), thinned.last()) {
                lane.curve.replace_range(a.time, b.time, &thinned);
                cx.emit(Self::edit_lane(d.track, lane));
            }
        }
        cx.emit(Action::EndGesture);
        true
    }

    // --- menus ----------------------------------------------------------------------------

    fn mode_menu(track: TrackId, lane: &AutomationLane, at: Point) -> HostRequest<Action> {
        let items = MODES
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
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    /// Every automatable parameter of the track, checked when shown.
    pub(crate) fn parameter_menu(
        model: &Session,
        track: TrackId,
        at: Point,
    ) -> HostRequest<Action> {
        let shown: Vec<_> = model
            .shown_lanes(track)
            .iter()
            .map(|l| (l.target, l.id))
            .collect();
        let items = model
            .automatable_parameters(track)
            .into_iter()
            .map(|param| {
                let lane = shown
                    .iter()
                    .find(|(t, _)| *t == param.target)
                    .map(|(_, id)| *id);
                let has_points = model
                    .project()
                    .track(track)
                    .and_then(|t| t.automation.lane(param.target))
                    .is_some_and(|l| !l.curve.is_empty());
                let label = if has_points {
                    format!("{} ●", param.name)
                } else {
                    param.name.clone()
                };
                let action = match lane {
                    Some(id) => Action::HideAutomationLane(id),
                    None => Action::ShowAutomation {
                        track,
                        target: param.target,
                    },
                };
                MenuItem::new(label, action).checked(lane.is_some())
            })
            .collect();
        HostRequest::ContextMenu { at, items }
    }

    /// Right-click in a lane: point shape, delete, clear, modes.
    pub(crate) fn lane_menu(
        &self,
        model: &Session,
        g: LaneGeom,
        pos: Point,
        lanes_right: f32,
    ) -> HostRequest<Action> {
        let Some(lane) = Self::lane_of(model, g.track, g.lane).cloned() else {
            return HostRequest::ContextMenu {
                at: pos,
                items: Vec::new(),
            };
        };
        let mut items = Vec::new();
        let point = model
            .automation_param(g.track, lane.target)
            .and_then(|param| {
                self.point_at(
                    &lane,
                    &param,
                    g.value_area(self.header_w(), lanes_right),
                    pos,
                )
            });
        // Without a point under the pointer, the shape applies to the
        // segment the pointer is over.
        let at = self.time_at(pos.x);
        let index = point.or_else(|| {
            let i = lane.curve.points().partition_point(|p| p.time <= at);
            i.checked_sub(1)
        });
        if let Some(i) = index
            && let Some(p) = lane.curve.points().get(i).copied()
        {
            for (shape, label) in SHAPES {
                let mut l = lane.clone();
                l.curve.update(i, AutomationPoint { shape, ..p });
                items.push(
                    MenuItem::new(label, Self::edit_lane(g.track, l)).checked(p.shape == shape),
                );
            }
        }
        if let Some(i) = point {
            let mut l = lane.clone();
            l.curve.remove(i);
            items.push(MenuItem::new("Delete Point", Self::edit_lane(g.track, l)).separated());
        }
        let mut cleared = lane.clone();
        cleared.curve = AutomationCurve::new();
        items.push(MenuItem::new("Clear Lane", Self::edit_lane(g.track, cleared)).separated());
        for (k, m) in MODES.iter().enumerate() {
            let mut item = MenuItem::new(
                format!("Mode: {}", mode_label(*m)),
                Action::SetAutomationMode {
                    track: g.track,
                    lane: lane.id,
                    mode: *m,
                },
            )
            .checked(lane.mode == *m);
            if k == 0 {
                item = item.separated();
            }
            items.push(item);
        }
        // A controller knob for this parameter.
        let target = faderframe_project::MappingTarget::Parameter {
            track: g.track,
            target: lane.target,
        };
        for (k, (label, action)) in model.midi_learn_menu(target).into_iter().enumerate() {
            let item = MenuItem::new(label, action);
            items.push(if k == 0 { item.separated() } else { item });
        }
        items.push(MenuItem::new("Hide Lane", Action::HideAutomationLane(lane.id)).separated());
        HostRequest::ContextMenu { at: pos, items }
    }
}
