//! The Automation view for screen readers and the keyboard: the toolbar's
//! buttons ("Selected tracks only" a toggle; the lane tools unavailable
//! without a lane), the lane list and the editor of the lane chosen.
//!
//! The list's rows as shown: a track (Enter: the menu that adds a lane)
//! and its lanes, each read as its parameter, track, mode and point count
//! — Enter edits it, its menu is the mode's; inside, its mode (Enter: the
//! mode menu) and whether the arranger shows it (a toggle). The editor:
//! "Add point at the playhead" (its menu: the segment's shape there,
//! Write Value) and each point as a slider — the arrows move its value by
//! a hundredth of the lane, Enter puts the playhead on it, its menu sets
//! its shape, moves it by a sixteenth or deletes it.

use crate::{AutomationView, Button, Layout, ROW_H, Row, RowPart, mode_label};
use faderframe_automation::{AutomationLane, AutomationPoint};
use faderframe_core::TrackId;
use faderframe_session::automation::ParamKind;
use faderframe_session::{Action, AutomationParam, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, CanvasView, EventCx, HostRequest, Point, Rect, Size,
    access_id,
};

/// Kinds of node (with track, lane ids, a button's place, a point's
/// index).
const TOOLBAR: u64 = 0xA070;
const BUTTON: u64 = 0xA071;
const LIST: u64 = 0xA072;
const TRACK: u64 = 0xA073;
const LANE: u64 = 0xA074;
const MODE: u64 = 0xA075;
const EYE: u64 = 0xA076;
const EDITOR: u64 = 0xA077;
const PLAYHEAD: u64 = 0xA078;
const POINT: u64 = 0xA079;

/// A point's step: a hundredth of the lane's height.
const STEP: f64 = 0.01;
/// The size of a point's node.
const POINT_BOX: f32 = 12.0;

fn button_id(b: Button) -> u64 {
    access_id(&[BUTTON, b as u64])
}

fn point_id(track: TrackId, lane: &AutomationLane, i: usize) -> u64 {
    access_id(&[POINT, track.0, lane.id.0, i as u64])
}

/// What a node id names.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Button(Button),
    Row(usize, Row, Option<RowPart>),
    Playhead,
    Point(usize),
}

/// A point's value a step up (`up`) or down: a hundredth of the lane, a
/// switch from off to on.
fn stepped(param: &AutomationParam, v: f64, up: bool) -> f64 {
    if param.kind == ParamKind::Toggle {
        return if up { param.max } else { param.min };
    }
    let n = param.to_normal(v);
    param.from_normal(if up { n + STEP } else { n - STEP })
}

impl AutomationView {
    fn button_name(b: Button) -> &'static str {
        match b {
            Button::SelectedOnly => "Selected tracks only",
            Button::AllRead => "All lanes read",
            Button::AllOff => "All lanes off",
            Button::WriteValue => "Write value",
            Button::Thin => "Thin",
            Button::Clear => "Clear",
            Button::Delete => "Delete lane",
            Button::Fit => "Fit",
        }
    }

    /// A lane's row as read.
    fn lane_text(model: &Session, t: TrackId, lane: &AutomationLane) -> String {
        let name = model
            .automation_param(t, lane.target)
            .map_or_else(|| "?".to_string(), |p| p.name);
        let track = model.project().track(t).map_or("", |t| t.name.as_str());
        let points = match lane.curve.points().len() {
            0 => "no points".to_string(),
            1 => "1 point".to_string(),
            n => format!("{n} points"),
        };
        format!("{name} on {track}, {}, {points}", mode_label(lane.mode))
    }

    /// The lane tools act on the lane edited: their actions.
    fn button_action(&self, b: Button, model: &Session) -> Option<Action> {
        let current = self.current(model);
        match b {
            Button::AllRead | Button::AllOff => Some(Action::SetAutomationModes {
                lanes: self.listed_lanes(model),
                mode: if b == Button::AllRead {
                    faderframe_automation::AutomationMode::Read
                } else {
                    faderframe_automation::AutomationMode::Off
                },
            }),
            Button::WriteValue => {
                let (t, l) = current?;
                Self::write_value(model, t, &l).map(|l| Self::edit(t, l))
            }
            Button::Thin => {
                let (t, l) = current?;
                Self::thinned(model, t, &l).map(|l| Self::edit(t, l))
            }
            Button::Clear => {
                let (t, mut l) = current?;
                l.curve = faderframe_automation::AutomationCurve::new();
                Some(Self::edit(t, l))
            }
            Button::Delete => {
                let (t, l) = current?;
                Some(Action::Edit(
                    faderframe_project::Command::RemoveAutomationLane {
                        track: t,
                        lane: l.id,
                    },
                ))
            }
            Button::SelectedOnly | Button::Fit => None,
        }
    }

    /// The lane with a point at the playhead, as a click there adds one
    /// (`None`: there is one).
    fn with_point_at_playhead(
        model: &Session,
        track: TrackId,
        lane: &AutomationLane,
    ) -> Option<AutomationLane> {
        let param = model.automation_param(track, lane.target)?;
        let at = model.playhead();
        if lane.curve.points().iter().any(|p| p.time == at) {
            return None;
        }
        let value = lane
            .curve
            .value_at(at)
            .or_else(|| model.static_value(track, lane.target))
            .unwrap_or(param.default);
        let mut l = lane.clone();
        if l.curve.is_empty()
            && at > MusicalTime::ZERO
            && let Some(v) = model.static_value(track, lane.target)
        {
            l.curve.insert(AutomationPoint {
                time: MusicalTime::ZERO,
                value: v,
                shape: param.shape(),
            });
        }
        l.curve.insert(AutomationPoint {
            time: at,
            value,
            shape: param.shape(),
        });
        Some(l)
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let l = self.layout(size);
        let current = self.current(model);
        let buttons = l.buttons.iter().map(|(b, r)| {
            let mut n = AccessNode::new(button_id(*b), AccessRole::Button, Self::button_name(*b))
                .at(*r)
                .fixed();
            if let Some(tip) = self.tooltip(r.center(), size, model) {
                n = n.described(tip);
            }
            match b {
                Button::SelectedOnly => {
                    n.role = AccessRole::ToggleButton;
                    n.checked(self.selected_only).on_press()
                }
                Button::Fit => n.on_press(),
                _ => match self.button_action(*b, model) {
                    Some(a) => n.on_activate(a),
                    None => n.disabled(true),
                },
            }
        });
        let mut out = vec![
            AccessNode::new(
                access_id(&[TOOLBAR]),
                AccessRole::Toolbar,
                "Automation toolbar",
            )
            .at(l.toolbar)
            .fixed()
            .with_children(buttons),
        ];
        // The list's rows as shown.
        let rows = self
            .rows(model)
            .into_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let r = self.row_rect(l.list, i);
                Some(match row {
                    Row::Track(t) => {
                        let track = model.project().track(t)?;
                        let n = track.automation.lanes.len();
                        AccessNode::new(
                            access_id(&[TRACK, t.0]),
                            AccessRole::ListItem,
                            format!("{}, {n} lane{}", track.name, if n == 1 { "" } else { "s" }),
                        )
                        .at(r)
                        .fixed_x()
                        .described("Enter: add an automation lane")
                        .with_menu()
                    }
                    Row::Lane(t, id) => {
                        let lane = Self::lane(model, t, id)?;
                        let parts = Self::row_parts(r, row);
                        let shown = model.shown_lanes(t).iter().any(|x| x.id == id);
                        let eye = AccessNode::new(
                            access_id(&[EYE, t.0, id.0]),
                            AccessRole::ToggleButton,
                            "Shown in the arranger",
                        )
                        .at(parts[0].1)
                        .fixed_x()
                        .checked(shown)
                        .on_activate(if shown {
                            Action::HideAutomationLane(id)
                        } else {
                            Action::ShowAutomation {
                                track: t,
                                target: lane.target,
                            }
                        });
                        let mode = AccessNode::new(
                            access_id(&[MODE, t.0, id.0]),
                            AccessRole::Button,
                            format!("Mode: {}", mode_label(lane.mode)),
                        )
                        .at(parts[1].1)
                        .fixed_x()
                        .with_menu();
                        AccessNode::new(
                            access_id(&[LANE, t.0, id.0]),
                            AccessRole::ListItem,
                            Self::lane_text(model, t, lane),
                        )
                        .at(r)
                        .fixed_x()
                        .selected(
                            current
                                .as_ref()
                                .is_some_and(|(ct, cl)| *ct == t && cl.id == id),
                        )
                        .described("Enter: edit this lane")
                        .on_press()
                        .with_menu()
                        .child(mode)
                        .child(eye)
                    }
                })
            });
        out.push(
            AccessNode::new(access_id(&[LIST]), AccessRole::List, "Automation lanes")
                .at(l.list)
                .fixed_x()
                .with_children(rows),
        );
        // The editor: the lane's points.
        if let Some((track, lane)) = &current
            && let Some(param) = model.automation_param(*track, lane.target)
        {
            let tl = &model.project().timeline;
            let at = model.playhead();
            let here = lane
                .curve
                .value_at(at)
                .or_else(|| model.static_value(*track, lane.target))
                .unwrap_or(param.default);
            let mut add = AccessNode::new(
                access_id(&[PLAYHEAD]),
                AccessRole::Button,
                format!(
                    "Add point at the playhead, {} at {}",
                    param.format(here),
                    tl.format_bbt(at)
                ),
            )
            .at(l.plot)
            .fixed()
            .with_menu();
            match Self::with_point_at_playhead(model, *track, lane) {
                Some(with) => add = add.on_activate(Self::edit(*track, with)),
                None => add = add.disabled(true).described("A point is at the playhead"),
            }
            let points = lane.curve.points().iter().enumerate().map(|(i, p)| {
                let c = Point::new(
                    self.x_of(model, l.plot, p.time),
                    Self::value_y(&param, l.plot, p.value),
                );
                let step = |up: bool| {
                    let mut moved = lane.clone();
                    moved.curve.update(
                        i,
                        AutomationPoint {
                            value: stepped(&param, p.value, up),
                            ..*p
                        },
                    );
                    Self::edit(*track, moved)
                };
                // A fader's bottom is silence: read from −80 dB up.
                let finite = |v: f64, or: f64| if v.is_finite() { v } else { or };
                let (lo, hi) = if param.kind == ParamKind::Gain {
                    (
                        finite(param.from_normal(0.0), -80.0).max(-80.0),
                        finite(param.from_normal(1.0), 12.0),
                    )
                } else {
                    (param.min, param.max)
                };
                AccessNode::new(
                    point_id(*track, lane, i),
                    AccessRole::Slider,
                    format!("Point {} at {}", i + 1, tl.format_bbt(p.time)),
                )
                .at(Rect::new(
                    c.x - POINT_BOX / 2.0,
                    c.y - POINT_BOX / 2.0,
                    POINT_BOX,
                    POINT_BOX,
                ))
                .fixed_y()
                .value(p.value.clamp(lo, hi), lo, hi, param.format(p.value))
                .on_step(step(true), step(false))
                .on_activate(Action::Transport(TransportAction::Locate(p.time)))
                .with_menu()
            });
            let track_name = model
                .project()
                .track(*track)
                .map_or("", |t| t.name.as_str());
            out.push(
                AccessNode::new(
                    access_id(&[EDITOR]),
                    AccessRole::Group,
                    format!("{} on {track_name}, {}", param.name, mode_label(lane.mode)),
                )
                .at(Rect::new(
                    l.list.right(),
                    l.list.y,
                    size.w - l.list.right(),
                    l.list.h,
                ))
                .child(add)
                .with_children(points),
            );
        }
        out
    }

    fn control_of(&self, id: u64, size: Size, model: &Session) -> Option<Control> {
        let l = self.layout(size);
        if let Some((b, _)) = l.buttons.iter().find(|(b, _)| button_id(*b) == id) {
            return Some(Control::Button(*b));
        }
        if id == access_id(&[PLAYHEAD]) {
            return Some(Control::Playhead);
        }
        for (i, row) in self.rows(model).into_iter().enumerate() {
            match row {
                Row::Track(t) if id == access_id(&[TRACK, t.0]) => {
                    return Some(Control::Row(i, row, Some(RowPart::Add)));
                }
                Row::Lane(t, lane) => {
                    if id == access_id(&[LANE, t.0, lane.0]) {
                        return Some(Control::Row(i, row, None));
                    }
                    if id == access_id(&[MODE, t.0, lane.0]) {
                        return Some(Control::Row(i, row, Some(RowPart::Mode)));
                    }
                }
                Row::Track(_) => {}
            }
        }
        let (track, lane) = self.current(model)?;
        (0..lane.curve.points().len())
            .find(|i| point_id(track, &lane, *i) == id)
            .map(Control::Point)
    }

    /// Scroll the list so row `i` is in view.
    fn keep_in_view(&mut self, l: &Layout, i: usize) {
        let top = i as f32 * ROW_H;
        if top < self.list_scroll {
            self.list_scroll = top;
        } else if top + ROW_H > self.list_scroll + l.list.h {
            self.list_scroll = top + ROW_H - l.list.h;
        }
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(control) = self.control_of(id, size, model) else {
            return false;
        };
        let l = self.layout(size);
        let under = |r: Rect| Point::new(r.x, r.bottom());
        match (control, press) {
            (Control::Button(b @ (Button::SelectedOnly | Button::Fit)), AccessPress::Activate) => {
                self.press_button(b, model, cx);
            }
            (Control::Row(i, row, part), _) => {
                let r = self.row_rect(l.list, i);
                match (row, part, press) {
                    (Row::Track(t), _, AccessPress::Menu) => {
                        cx.request(Self::add_menu(model, t, under(r)));
                    }
                    (Row::Lane(t, id), None, AccessPress::Activate) => {
                        self.selected = Some((t, id));
                        self.keep_in_view(&l, i);
                        cx.redraw();
                    }
                    (Row::Lane(t, id), _, AccessPress::Menu) => {
                        let Some(lane) = Self::lane(model, t, id) else {
                            return false;
                        };
                        let at = Self::row_parts(r, row)
                            .into_iter()
                            .find(|(p, _)| *p == RowPart::Mode)
                            .map_or(under(r), |(_, m)| under(m));
                        cx.request(Self::mode_menu(t, lane, at));
                    }
                    _ => return false,
                }
            }
            (Control::Playhead, AccessPress::Menu) => {
                let Some((track, lane)) = self.current(model) else {
                    return false;
                };
                let at = model.playhead();
                let x = self.x_of(model, l.plot, at).clamp(l.plot.x, l.plot.right());
                let items = Self::point_items(model, track, &lane, at, None);
                cx.request(HostRequest::ContextMenu {
                    at: Point::new(x, l.plot.bottom()),
                    items,
                });
            }
            (Control::Point(i), AccessPress::Menu) => {
                let Some((track, lane)) = self.current(model) else {
                    return false;
                };
                let Some(p) = lane.curve.points().get(i).copied() else {
                    return false;
                };
                let Some(param) = model.automation_param(track, lane.target) else {
                    return false;
                };
                let at = Point::new(
                    self.x_of(model, l.plot, p.time),
                    Self::value_y(&param, l.plot, p.value),
                );
                let items = Self::point_items(model, track, &lane, p.time, Some(i));
                cx.request(HostRequest::ContextMenu { at, items });
            }
            _ => return false,
        }
        true
    }
}
