//! Clip envelopes on the arranger's audio clips, and the Strip Silence
//! preview.
//!
//! While a lane is shown (`Session::clip_envelope_shown`: Edit → Clip
//! Envelopes, or the clip menu) audio clips draw it over their waveform
//! — volume on a fader law with 0 dB at three quarters of the height, pan
//! with the centre in the middle — and their body edits it: a click adds a
//! point (and drags it), a drag moves one (snapped to the grid in time;
//! Alt free, Shift keeps its time, Ctrl fine), a double-click removes it, a
//! right-click on one sets its segment's shape. Edges still trim and the
//! name strip still moves the clip. Each drag is one undo step
//! (`Action::SetClipEnvelope` inside a gesture).

use super::*;
use crate::clip_edit::EditDrag;
use faderframe_automation::CurveShape;
use faderframe_project::clip_envelope::{EnvelopeLane, EnvelopePoint};

/// Pick radius of a point (pixels).
const POINT_PICK: f32 = 6.0;
/// Drawn size of a point.
const POINT_SIZE: f32 = 6.0;

/// A point of a clip's envelope being dragged (or just added).
#[derive(Clone, Debug)]
pub(crate) struct EnvelopeDrag {
    pub clip: ClipId,
    pub lane: EnvelopeLane,
    pub index: usize,
    /// The lane's points as the press found them (the drag is absolute
    /// from these).
    pub points: Vec<EnvelopePoint>,
    pub origin: Point,
    /// The clip's content area at the press (values from y).
    pub content: Rect,
    /// Past the drag threshold (or a point added: already an edit).
    pub moved: bool,
}

impl ArrangerView {
    /// The shown envelope lane's points of an audio clip (empty: none).
    fn envelope_points(model: &Session, clip: ClipId, lane: EnvelopeLane) -> Vec<EnvelopePoint> {
        model
            .clip_envelope(clip)
            .map(|e| e.lane(lane).to_vec())
            .unwrap_or_default()
    }

    /// Does pressing `clip` edit its envelope (a lane shown, an audio
    /// clip)?
    pub(crate) fn edits_envelope(model: &Session, clip: &Clip) -> Option<EnvelopeLane> {
        let lane = model.clip_envelope_shown()?;
        matches!(clip.content, ClipContent::Audio(_)).then_some(lane)
    }

    /// y of a value in the clip's content area.
    fn envelope_y(content: Rect, lane: EnvelopeLane, v: f64) -> f32 {
        content.bottom() - lane.to_unit(v) as f32 * content.h
    }

    /// The value at y.
    fn envelope_value(content: Rect, lane: EnvelopeLane, y: f32) -> f64 {
        let u = ((content.bottom() - y) / content.h.max(1.0)) as f64;
        lane.from_unit(u)
    }

    /// The point under `pos` (index), if any.
    fn envelope_point_at(
        &self,
        model: &Session,
        clip: &Clip,
        content: Rect,
        lane: EnvelopeLane,
        points: &[EnvelopePoint],
        pos: Point,
    ) -> Option<usize> {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let at = Point::new(
                    self.x_of_frame(model, clip, p.at),
                    Self::envelope_y(content, lane, p.value),
                );
                (i, at.distance(pos))
            })
            .filter(|(_, d)| *d <= POINT_PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Clip frame at x, on the grid unless Alt (or snapping is off).
    fn envelope_frame(&self, model: &Session, clip: &Clip, x: f32, mods: Modifiers) -> i64 {
        let p = model.project();
        let rate = f64::from(p.sample_rate);
        let t = self.snap(self.time_at(x), model, mods);
        p.timeline.to_samples(t, rate) - p.timeline.to_samples(clip.start, rate)
    }

    /// A press on an audio clip's body while its envelope is shown.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn press_envelope(
        &mut self,
        c: &Clip,
        rect: Rect,
        lane: EnvelopeLane,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let content = self.content_rect(rect);
        let mut points = Self::envelope_points(model, c.id, lane);
        let hit = self.envelope_point_at(model, c, content, lane, &points, pos);
        if clicks >= 2 {
            if let Some(i) = hit {
                points.remove(i);
                cx.emit(Action::SetClipEnvelope {
                    clip: c.id,
                    lane,
                    points,
                });
            }
            return true;
        }
        if !model.selection.clips.contains(&c.id) {
            cx.emit(Action::SelectClips {
                clips: vec![c.id],
                mode: SelectMode::Replace,
            });
            cx.emit(Action::SelectTracks {
                tracks: vec![c.track],
                mode: SelectMode::Replace,
            });
        }
        let (index, moved) = match hit {
            Some(i) => (i, false),
            None => {
                // A new point on the line where it was pressed.
                let len = c.as_audio().map_or(0, |a| a.length);
                let at = self.envelope_frame(model, c, pos.x, mods).clamp(0, len);
                let value = Self::envelope_value(content, lane, pos.y);
                let i = points.partition_point(|p| p.at <= at);
                let shape = i
                    .checked_sub(1)
                    .and_then(|i| points.get(i))
                    .map_or(CurveShape::Linear, |p| p.shape);
                points.insert(i, EnvelopePoint { at, value, shape });
                cx.emit(Action::BeginGesture("Clip Envelope".into()));
                cx.emit(Action::SetClipEnvelope {
                    clip: c.id,
                    lane,
                    points: points.clone(),
                });
                (i, true)
            }
        };
        self.edit_drag = Some(EditDrag::Envelope(EnvelopeDrag {
            clip: c.id,
            lane,
            index,
            points,
            origin: pos,
            content,
            moved,
        }));
        cx.set_cursor(Cursor::Crosshair);
        true
    }

    /// The dragged point follows the pointer, kept between its
    /// neighbours.
    pub(crate) fn envelope_drag_move(
        &self,
        d: &mut EnvelopeDrag,
        pos: Point,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        if !d.moved {
            if pos.distance(d.origin) < DRAG_THRESHOLD {
                return;
            }
            cx.emit(Action::BeginGesture("Clip Envelope".into()));
            d.moved = true;
        }
        let Some(c) = model.project().clip(d.clip) else {
            return;
        };
        let Some(old) = d.points.get(d.index).copied() else {
            return;
        };
        let content = d.content;
        let lo = d.index.checked_sub(1).map_or(i64::MIN, |i| d.points[i].at);
        let hi = d.points.get(d.index + 1).map_or(i64::MAX, |p| p.at);
        let at = if mods.shift {
            old.at
        } else {
            self.envelope_frame(model, c, pos.x, mods).clamp(lo, hi)
        };
        let mut value = Self::envelope_value(content, d.lane, pos.y);
        if mods.ctrl {
            // Ctrl: fine, a tenth of the move from where it was.
            let was = Self::envelope_value(content, d.lane, d.origin.y);
            value = old.value + (value - was) * 0.1;
        }
        let mut points = d.points.clone();
        points[d.index] = EnvelopePoint {
            at,
            value: d.lane.clamp(value),
            shape: old.shape,
        };
        cx.emit(Action::SetClipEnvelope {
            clip: d.clip,
            lane: d.lane,
            points,
        });
    }

    /// Menu of a point: its segment's shape, remove it.
    pub(crate) fn envelope_point_menu(
        &self,
        model: &Session,
        size: Size,
        clip: &Clip,
        pos: Point,
    ) -> Option<HostRequest<Action>> {
        let lane = Self::edits_envelope(model, clip)?;
        let rect = self.clip_view_rect(model, size, clip.id)?;
        let content = self.content_rect(rect);
        let points = Self::envelope_points(model, clip.id, lane);
        let i = self.envelope_point_at(model, clip, content, lane, &points, pos)?;
        let with = |f: &dyn Fn(&mut Vec<EnvelopePoint>)| {
            let mut p = points.clone();
            f(&mut p);
            Action::SetClipEnvelope {
                clip: clip.id,
                lane,
                points: p,
            }
        };
        let mut items: Vec<MenuItem<Action>> = [
            (CurveShape::Linear, "Linear"),
            (CurveShape::Smooth, "Smooth"),
            (CurveShape::Exponential, "Exponential"),
            (CurveShape::Step, "Step"),
        ]
        .into_iter()
        .map(|(shape, label)| {
            MenuItem::new(
                format!("{label} to the Next Point"),
                with(&|p| p[i].shape = shape),
            )
            .checked(points[i].shape == shape)
        })
        .collect();
        items.insert(
            0,
            MenuItem::disabled(format!(
                "{} {}",
                lane.label(),
                lane.value_text(points[i].value)
            )),
        );
        items.push(
            MenuItem::new(
                "Remove Point",
                with(&|p| {
                    p.remove(i);
                }),
            )
            .separated(),
        );
        Some(HostRequest::ContextMenu { at: pos, items })
    }

    /// The clip menu's "Clip Envelope" submenu for `targets`.
    pub(crate) fn clip_envelope_items(
        model: &Session,
        targets: &[ClipId],
    ) -> Vec<MenuItem<Action>> {
        let shown = model.clip_envelope_shown();
        let mut items = vec![
            MenuItem::new(
                "Show Volume",
                Action::ShowClipEnvelope(Some(EnvelopeLane::Volume)),
            )
            .checked(shown == Some(EnvelopeLane::Volume)),
            MenuItem::new(
                "Show Pan",
                Action::ShowClipEnvelope(Some(EnvelopeLane::Pan)),
            )
            .checked(shown == Some(EnvelopeLane::Pan)),
            MenuItem::new("Hide", Action::ShowClipEnvelope(None)).checked(shown.is_none()),
        ];
        let has = |lane: Option<EnvelopeLane>| {
            targets.iter().any(|c| {
                model
                    .clip_envelope(*c)
                    .is_some_and(|e| lane.is_none_or(|l| !e.lane(l).is_empty()))
            })
        };
        for (lane, label) in [
            (Some(EnvelopeLane::Volume), "Clear Volume Envelope"),
            (Some(EnvelopeLane::Pan), "Clear Pan Envelope"),
            (None, "Clear All Envelopes"),
        ] {
            let item = if has(lane) {
                MenuItem::new(
                    label,
                    Action::ClearClipEnvelope {
                        clips: targets.to_vec(),
                        lane,
                    },
                )
            } else {
                MenuItem::disabled(label)
            };
            items.push(if lane == Some(EnvelopeLane::Volume) {
                item.separated()
            } else {
                item
            });
        }
        items
    }

    /// The shown lane over an audio clip: the line, and its points while
    /// the clip is selected or hovered (or the envelope differs from
    /// neutral, so a ride is seen at a glance).
    pub(crate) fn paint_envelope(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        rect: Rect,
    ) {
        let Some(lane) = Self::edits_envelope(model, clip) else {
            return;
        };
        let content = self.content_rect(rect);
        if content.h < 6.0 || content.w < 2.0 {
            return;
        }
        let points = Self::envelope_points(model, clip.id, lane);
        let color = self.theme.arranger.automation;
        let y_of = |v: f64| Self::envelope_y(content, lane, v);
        let (x0, x1) = (content.x, content.right());
        let mut line = Path::new();
        match points.first() {
            None => {
                let y = y_of(lane.neutral());
                line.move_to(Point::new(x0, y));
                line.line_to(Point::new(x1, y));
            }
            Some(first) => {
                line.move_to(Point::new(x0, y_of(first.value)));
                for (i, a) in points.iter().enumerate() {
                    let xa = self.x_of_frame(model, clip, a.at);
                    line.line_to(Point::new(xa.clamp(x0, x1), y_of(a.value)));
                    let Some(b) = points.get(i + 1) else {
                        continue;
                    };
                    let xb = self.x_of_frame(model, clip, b.at);
                    if xb < x0 || xa > x1 {
                        continue;
                    }
                    // Curved segments drawn in steps of a few pixels.
                    let steps = match a.shape {
                        CurveShape::Linear => 1,
                        _ => (((xb - xa) / 4.0) as usize).clamp(1, 64),
                    };
                    for k in 1..steps {
                        let t = k as f64 / steps as f64;
                        let v = faderframe_automation::interpolate(a.value, b.value, t, a.shape);
                        let x = xa + (xb - xa) * t as f32;
                        line.line_to(Point::new(x.clamp(x0, x1), y_of(v)));
                    }
                    if a.shape == CurveShape::Step {
                        line.line_to(Point::new(xb.clamp(x0, x1), y_of(a.value)));
                    }
                }
                if let Some(last) = points.last() {
                    line.line_to(Point::new(x1, y_of(last.value)));
                }
            }
        }
        p.stroke_path(&line, 1.5, color);
        for a in &points {
            let x = self.x_of_frame(model, clip, a.at);
            if x < x0 - POINT_SIZE || x > x1 + POINT_SIZE {
                continue;
            }
            let c = Point::new(x, y_of(a.value));
            let r = Rect::new(
                c.x - POINT_SIZE * 0.5,
                c.y - POINT_SIZE * 0.5,
                POINT_SIZE,
                POINT_SIZE,
            );
            p.fill(r, color);
            p.stroke_rounded(r, 0.0, 1.0, self.theme.arranger.background);
        }
    }

    /// While the Strip Silence dialog is open: its clips' silences shaded
    /// and the regions it keeps outlined.
    pub(crate) fn paint_strip_preview(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        rect: Rect,
    ) {
        let Some(preview) = model.strip_silence_preview() else {
            return;
        };
        let Some(regions) = preview.regions.get(&clip.id) else {
            return;
        };
        let Some(a) = clip.as_audio() else {
            return;
        };
        let content = self.content_rect(rect);
        let x = |f: i64| self.x_of_frame(model, clip, f).clamp(rect.x, rect.right());
        let shade = self.theme.arranger.background.with_alpha(0.6);
        // The silences: between and around the regions.
        let mut from = 0;
        let bounds = regions
            .iter()
            .map(|r| (r.start, r.end))
            .chain([(a.length, a.length)]);
        for (start, end) in bounds {
            let (x0, x1) = (x(from), x(start));
            if x1 > x0 {
                p.fill(Rect::new(x0, content.y, x1 - x0, content.h), shade);
            }
            from = from.max(end);
        }
        let outline = self.theme.ui.selection;
        for r in regions {
            let (x0, x1) = (x(r.start), x(r.end));
            if x1 > x0 {
                p.stroke_rounded(
                    Rect::new(x0, content.y, x1 - x0, content.h),
                    2.0,
                    1.5,
                    outline,
                );
            }
        }
    }
}
