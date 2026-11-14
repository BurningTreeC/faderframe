//! Clip envelopes on the arranger's audio clips and on the takes in the
//! lanes of an open take folder, and the Strip Silence preview.
//!
//! While a lane is shown (`Session::clip_envelope_shown`: Edit → Clip
//! Envelopes, or the clip menu) audio clips draw it over their waveform
//! — volume on a fader law with 0 dB at three quarters of the height, pan
//! with the centre in the middle — and their body edits it: a click adds a
//! point on the line, at the value the line has there, so adding never
//! moves the line (the pointer turns into a plus); a drag past the
//! threshold then moves it, and a drag on a point (lit, the pointer a move
//! cross, its value beside it) moves that one — by how far the pointer
//! went, so grabbing it a little off its centre does not make it jump
//! (snapped to the grid in time; Alt free, Shift keeps its time, Ctrl
//! fine). A double-click removes a point, a right-click on one sets its
//! segment's shape. Edges still trim and the name strip still moves the
//! clip. Each edit is one undo step (`Action::SetClipEnvelope` inside a
//! gesture). A take is a clip in its
//! folder: its lane draws and edits its own envelope the same way (comping
//! in the lanes waits until the envelopes are hidden), and the folder's
//! main lane draws the comp's (what it plays, not edited there).

use super::*;
use crate::clip_edit::EditDrag;
use faderframe_automation::CurveShape;
use faderframe_project::AudioRef;
use faderframe_project::clip_envelope::{EnvelopeLane, EnvelopePoint};

/// Pick radius of a point (pixels).
const POINT_PICK: f32 = 6.0;
/// Drawn size of a point.
const POINT_SIZE: f32 = 6.0;
/// Drawn size of the point under the pointer or being dragged.
const POINT_LIT: f32 = 9.0;
/// How near the line (pixels) a press grabs its segment.
const LINE_PICK: f32 = 4.0;

/// What of an envelope is under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EnvelopeHover {
    /// A point (its index).
    Point(usize),
    /// The line between two points, or from the clip's edge to the nearest
    /// one: the points a drag moves.
    Segment(Vec<usize>),
}

impl EnvelopeHover {
    /// The points it lights.
    fn points(&self) -> Vec<usize> {
        match self {
            EnvelopeHover::Point(i) => vec![*i],
            EnvelopeHover::Segment(v) => v.clone(),
        }
    }
}

/// Where an envelope is drawn and edited: an audio clip's body, or a
/// take's part of its lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EnvelopePlace {
    pub audio: AudioRef,
    /// Project frames from the clip's start (the folder's, for a take) to
    /// its first frame.
    pub offset: i64,
    /// Its frames.
    pub length: i64,
    /// Where it is drawn (values from y).
    pub content: Rect,
}

/// A point of an envelope being dragged (or just added).
#[derive(Clone, Debug)]
pub(crate) struct EnvelopeDrag {
    /// The clip or take, as the press found it.
    pub place: EnvelopePlace,
    pub lane: EnvelopeLane,
    pub index: usize,
    /// The lane's points as the press found them (the drag is absolute
    /// from these).
    pub points: Vec<EnvelopePoint>,
    pub origin: Point,
    /// Past the drag threshold.
    pub moved: bool,
    /// The press added the point (its gesture already begun).
    pub added: bool,
    /// A segment grabbed: the points it moves (`index` unused). A click
    /// without a drag adds `pending` instead.
    pub segment: Vec<usize>,
    pub pending: Option<EnvelopePoint>,
}

impl ArrangerView {
    /// The shown envelope lane's points of an audio clip or take (empty:
    /// none).
    fn envelope_points(model: &Session, audio: AudioRef, lane: EnvelopeLane) -> Vec<EnvelopePoint> {
        model
            .clip_envelope(audio)
            .map(|e| e.lane(lane).to_vec())
            .unwrap_or_default()
    }

    /// Does pressing `clip` edit its envelope (a lane shown, an audio
    /// clip)?
    pub(crate) fn edits_envelope(model: &Session, clip: &Clip) -> Option<EnvelopeLane> {
        let lane = model.clip_envelope_shown()?;
        matches!(clip.content, ClipContent::Audio(_)).then_some(lane)
    }

    /// An audio clip's envelope over its body `rect`.
    pub(crate) fn clip_envelope_place(&self, clip: &Clip, rect: Rect) -> Option<EnvelopePlace> {
        Some(EnvelopePlace {
            audio: clip.id.into(),
            offset: 0,
            length: clip.as_audio()?.length,
            content: self.content_rect(rect),
        })
    }

    /// Take `k`'s part of its lane in folder `clip` (the lanes open).
    pub(crate) fn take_envelope_place(
        &self,
        model: &Session,
        size: Size,
        clip: &Clip,
        k: usize,
    ) -> Option<EnvelopePlace> {
        if !model.takes_open(clip.id) {
            return None;
        }
        let take = clip.as_takes()?.takes.get(k)?;
        let rect = self.clip_view_rect(model, size, clip.id)?;
        let lane_h = model.take_lane_height(clip.track);
        // The main lane's rect starts 3 px into its row.
        let lanes_top = rect.y - 3.0 + self.base_h(model, clip.track);
        let lane = Rect::new(rect.x, lanes_top + k as f32 * lane_h, rect.w, lane_h - 1.0);
        Some(self.take_body_place(model, clip, k, take, lane))
    }

    /// Take `k` (`take`) of folder `clip` drawn in `lane`: its body.
    pub(crate) fn take_body_place(
        &self,
        model: &Session,
        clip: &Clip,
        k: usize,
        take: &faderframe_project::Take,
        lane: Rect,
    ) -> EnvelopePlace {
        let x0 = self.x_of_frame(model, clip, take.start);
        let x1 = self.x_of_frame(model, clip, take.end());
        let body = Rect::new(x0, lane.y + 1.0, (x1 - x0).max(0.0), lane.h - 2.0);
        let audio = model
            .project()
            .take_ref(clip.id, k)
            .map_or(AudioRef::Clip(clip.id), AudioRef::Take);
        EnvelopePlace {
            audio,
            offset: take.start,
            length: take.audio.length,
            content: body.inset_xy(0.0, 2.0),
        }
    }

    /// y of a value in the clip's content area.
    fn envelope_y(content: Rect, lane: EnvelopeLane, v: f64) -> f32 {
        content.bottom() - lane.to_unit(v) as f32 * content.h
    }

    /// x of frame `at` of the clip or take at `place` (in clip `clip`).
    fn envelope_x(&self, model: &Session, clip: &Clip, place: &EnvelopePlace, at: i64) -> f32 {
        self.x_of_frame(model, clip, place.offset + at)
    }

    /// The point under `pos` (index), if any.
    fn envelope_point_at(
        &self,
        model: &Session,
        clip: &Clip,
        place: &EnvelopePlace,
        lane: EnvelopeLane,
        points: &[EnvelopePoint],
        pos: Point,
    ) -> Option<usize> {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let at = Point::new(
                    self.envelope_x(model, clip, place, p.at),
                    Self::envelope_y(place.content, lane, p.value),
                );
                (i, at.distance(pos))
            })
            .filter(|(_, d)| *d <= POINT_PICK)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The shown lane's point under `pos` on audio clip `clip` (drawn at
    /// `rect`).
    pub(crate) fn envelope_point_under(
        &self,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        pos: Point,
    ) -> Option<usize> {
        let lane = Self::edits_envelope(model, clip)?;
        let place = self.clip_envelope_place(clip, rect)?;
        let points = Self::envelope_points(model, place.audio, lane);
        self.envelope_point_at(model, clip, &place, lane, &points, pos)
    }

    /// The segment of the line under `pos` (within [`LINE_PICK`]): the
    /// points on either side of it (one at the clip's ends); none without
    /// points.
    #[allow(clippy::too_many_arguments)]
    fn envelope_segment_at(
        &self,
        model: &Session,
        clip: &Clip,
        place: &EnvelopePlace,
        lane: EnvelopeLane,
        points: &[EnvelopePoint],
        pos: Point,
    ) -> Option<Vec<usize>> {
        if points.is_empty() || pos.x < place.content.x || pos.x > place.content.right() {
            return None;
        }
        let free = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        let at = self.envelope_frame(model, clip, place, pos.x, free);
        let value = faderframe_project::clip_envelope::value_at(points, at as f64)?;
        if (Self::envelope_y(place.content, lane, value) - pos.y).abs() > LINE_PICK {
            return None;
        }
        let i = points.partition_point(|p| p.at <= at);
        Some(
            [i.checked_sub(1), (i < points.len()).then_some(i)]
                .into_iter()
                .flatten()
                .collect(),
        )
    }

    /// The shown lane's segment under `pos` on audio clip `clip` (drawn at
    /// `rect`).
    pub(crate) fn envelope_segment_under(
        &self,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        pos: Point,
    ) -> Option<Vec<usize>> {
        let lane = Self::edits_envelope(model, clip)?;
        let place = self.clip_envelope_place(clip, rect)?;
        let points = Self::envelope_points(model, place.audio, lane);
        self.envelope_segment_at(model, clip, &place, lane, &points, pos)
    }

    /// The envelope a press at `pos` would edit — an audio clip's body or
    /// a take in its lane, a lane shown — and the point under it, if any.
    fn envelope_target(
        &self,
        model: &Session,
        size: Size,
        pos: Point,
    ) -> Option<(ClipId, EnvelopePlace, EnvelopeLane, Option<EnvelopeHover>)> {
        let lane = model.clip_envelope_shown()?;
        let (c, place) = match self.hit_test(pos, size, model)? {
            Hit::Clip { clip, .. } => {
                let c = model.project().clip(clip)?;
                let rect = self.clip_view_rect(model, size, clip)?;
                if !matches!(
                    self.clip_zone(model, c, rect, pos),
                    crate::clip_edit::ClipZone::Envelope
                        | crate::clip_edit::ClipZone::EnvelopePoint(_)
                        | crate::clip_edit::ClipZone::EnvelopeSegment
                ) {
                    return None;
                }
                (c, self.clip_envelope_place(c, rect)?)
            }
            Hit::TakeLane { clip, take, .. } => {
                let c = model.project().clip(clip)?;
                let place = self.take_envelope_place(model, size, c, take)?;
                if pos.x < place.content.x - 3.0 || pos.x > place.content.right() + 3.0 {
                    return None;
                }
                (c, place)
            }
            _ => return None,
        };
        let points = Self::envelope_points(model, place.audio, lane);
        let hover = match self.envelope_point_at(model, c, &place, lane, &points, pos) {
            Some(i) => Some(EnvelopeHover::Point(i)),
            None => self
                .envelope_segment_at(model, c, &place, lane, &points, pos)
                .map(EnvelopeHover::Segment),
        };
        Some((c.id, place, lane, hover))
    }

    /// The envelope point or segment under `pos` (what is drawn lit while
    /// hovering).
    pub(crate) fn envelope_hovered(
        &self,
        model: &Session,
        size: Size,
        pos: Point,
    ) -> Option<(AudioRef, EnvelopeHover)> {
        let (_, place, _, hover) = self.envelope_target(model, size, pos)?;
        Some((place.audio, hover?))
    }

    /// The points of `audio` drawn lit: the ones being dragged, else the
    /// ones under the pointer.
    fn lit_envelope_points(&self, audio: AudioRef) -> Vec<usize> {
        if let Some(EditDrag::Envelope(d)) = &self.edit_drag {
            if d.place.audio != audio {
                return Vec::new();
            }
            return if d.segment.is_empty() {
                vec![d.index]
            } else {
                d.segment.clone()
            };
        }
        self.envelope_hover
            .as_ref()
            .filter(|(a, _)| *a == audio)
            .map_or_else(Vec::new, |(_, h)| h.points())
    }

    /// The tooltip over an envelope a press would edit: a point's value and
    /// time and what pressing it does, or what a click on the line does.
    pub(crate) fn envelope_tooltip(
        &self,
        model: &Session,
        size: Size,
        pos: Point,
    ) -> Option<String> {
        let (clip, place, lane, hover) = self.envelope_target(model, size, pos)?;
        if let Some(EnvelopeHover::Segment(v)) = &hover {
            return Some(format!(
                "{} envelope · Drag: move this segment up or down ({}; Ctrl: fine) · Click: \
                 add a point on the line",
                lane.label(),
                if v.len() == 1 {
                    "its point"
                } else {
                    "both points"
                }
            ));
        }
        let Some(EnvelopeHover::Point(i)) = hover else {
            return Some(format!(
                "{} envelope · Click: add a point on the line (it stays where it is), then \
                 drag it · Alt: off the grid",
                lane.label()
            ));
        };
        let point = *Self::envelope_points(model, place.audio, lane).get(i)?;
        let p = model.project();
        let c = p.clip(clip)?;
        let rate = f64::from(p.sample_rate);
        let start = p.timeline.to_samples(c.start, rate);
        let at = p.timeline.to_musical(start + place.offset + point.at, rate);
        Some(format!(
            "{} {} at {}\nDrag: move (Shift: keep its time, Ctrl: fine, Alt: off the grid) · \
             Double-click: remove · Right-click: its curve",
            lane.label(),
            lane.value_text(point.value),
            p.timeline.format_bbt(at)
        ))
    }

    /// The frame of the clip or take at x, on the grid unless Alt (or
    /// snapping is off).
    fn envelope_frame(
        &self,
        model: &Session,
        clip: &Clip,
        place: &EnvelopePlace,
        x: f32,
        mods: Modifiers,
    ) -> i64 {
        let p = model.project();
        let rate = f64::from(p.sample_rate);
        let t = self.snap(self.time_at(x), model, mods);
        p.timeline.to_samples(t, rate) - p.timeline.to_samples(clip.start, rate) - place.offset
    }

    /// A press on an audio clip's body, or on a take in its lane, while an
    /// envelope lane is shown.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn press_envelope(
        &mut self,
        c: &Clip,
        place: EnvelopePlace,
        lane: EnvelopeLane,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let mut points = Self::envelope_points(model, place.audio, lane);
        let hit = self.envelope_point_at(model, c, &place, lane, &points, pos);
        if clicks >= 2 {
            if let Some(i) = hit {
                points.remove(i);
                cx.emit(Action::SetClipEnvelope {
                    audio: place.audio,
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
        // On the line: a drag moves the segment, a click adds a point (on
        // release).
        let segment = if hit.is_none() {
            self.envelope_segment_at(model, c, &place, lane, &points, pos)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let pending = (!segment.is_empty()).then(|| {
            let at = self
                .envelope_frame(model, c, &place, pos.x, mods)
                .clamp(0, place.length);
            Self::point_on_line(&points, lane, at)
        });
        let (index, added) = match hit {
            Some(i) => (i, false),
            None if !segment.is_empty() => (0, false),
            None => {
                // A new point on the line, at the value the line has there
                // (the press's height would bend the line by however far
                // off it the click landed).
                let at = self
                    .envelope_frame(model, c, &place, pos.x, mods)
                    .clamp(0, place.length);
                let new = Self::point_on_line(&points, lane, at);
                let i = points.partition_point(|p| p.at <= at);
                points.insert(i, new);
                cx.emit(Action::BeginGesture("Clip Envelope".into()));
                cx.emit(Action::SetClipEnvelope {
                    audio: place.audio,
                    lane,
                    points: points.clone(),
                });
                (i, true)
            }
        };
        self.edit_drag = Some(EditDrag::Envelope(EnvelopeDrag {
            place,
            lane,
            index,
            points,
            origin: pos,
            moved: false,
            added,
            pending,
            segment: segment.clone(),
        }));
        cx.set_cursor(if segment.is_empty() {
            Cursor::Move
        } else {
            Cursor::ResizeVertical
        });
        cx.redraw();
        true
    }

    /// A new point at frame `at` on the line of `points` (its value there,
    /// the shape of the segment it splits).
    fn point_on_line(points: &[EnvelopePoint], lane: EnvelopeLane, at: i64) -> EnvelopePoint {
        let value = faderframe_project::clip_envelope::value_at(points, at as f64)
            .unwrap_or_else(|| lane.neutral());
        let i = points.partition_point(|p| p.at <= at);
        let shape = i
            .checked_sub(1)
            .and_then(|i| points.get(i))
            .map_or(CurveShape::Linear, |p| p.shape);
        EnvelopePoint { at, value, shape }
    }

    /// The press on an envelope ends: its gesture closes; a segment clicked
    /// without a drag gets its point.
    pub(crate) fn envelope_release(d: &EnvelopeDrag, cx: &mut EventCx<'_, Action>) {
        if d.moved || d.added {
            cx.emit(Action::EndGesture);
            return;
        }
        if let Some(new) = d.pending {
            let mut points = d.points.clone();
            let i = points.partition_point(|p| p.at <= new.at);
            points.insert(i, new);
            cx.emit(Action::SetClipEnvelope {
                audio: d.place.audio,
                lane: d.lane,
                points,
            });
        }
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
            if !d.added {
                cx.emit(Action::BeginGesture("Clip Envelope".into()));
            }
            d.moved = true;
        }
        let Some(c) = model.project().clip(d.place.audio.clip()) else {
            return;
        };
        let content = d.place.content;
        if !d.segment.is_empty() {
            // The segment: its points up or down together, by how far the
            // pointer went (Ctrl: fine); their times stay.
            let mut dy = (d.origin.y - pos.y) / content.h.max(1.0);
            if mods.ctrl {
                dy *= 0.1;
            }
            let mut points = d.points.clone();
            for &i in &d.segment {
                let v = d.lane.to_unit(d.points[i].value) + f64::from(dy);
                points[i].value = d.lane.clamp(d.lane.from_unit(v));
            }
            cx.emit(Action::SetClipEnvelope {
                audio: d.place.audio,
                lane: d.lane,
                points,
            });
            return;
        }
        let Some(old) = d.points.get(d.index).copied() else {
            return;
        };
        let lo = d.index.checked_sub(1).map_or(i64::MIN, |i| d.points[i].at);
        let hi = d.points.get(d.index + 1).map_or(i64::MAX, |p| p.at);
        let at = if mods.shift {
            old.at
        } else {
            self.envelope_frame(model, c, &d.place, pos.x, mods)
                .clamp(lo, hi)
        };
        // By how far the pointer went (on the lane's scale), not to where
        // it is: a point grabbed off its centre does not jump. Ctrl: fine,
        // a tenth of the move.
        let mut dy = (d.origin.y - pos.y) / content.h.max(1.0);
        if mods.ctrl {
            dy *= 0.1;
        }
        let value = d.lane.from_unit(d.lane.to_unit(old.value) + f64::from(dy));
        let mut points = d.points.clone();
        points[d.index] = EnvelopePoint {
            at,
            value: d.lane.clamp(value),
            shape: old.shape,
        };
        cx.emit(Action::SetClipEnvelope {
            audio: d.place.audio,
            lane: d.lane,
            points,
        });
    }

    /// Menu of an audio clip's envelope point under `pos`: its segment's
    /// shape, remove it.
    pub(crate) fn envelope_point_menu(
        &self,
        model: &Session,
        size: Size,
        clip: &Clip,
        pos: Point,
    ) -> Option<HostRequest<Action>> {
        let lane = Self::edits_envelope(model, clip)?;
        let rect = self.clip_view_rect(model, size, clip.id)?;
        let place = self.clip_envelope_place(clip, rect)?;
        self.envelope_point_menu_at(model, clip, place, lane, pos)
    }

    /// Menu of the envelope point under `pos` of a take in its lane.
    pub(crate) fn take_envelope_point_menu(
        &self,
        model: &Session,
        size: Size,
        clip: &Clip,
        take: usize,
        pos: Point,
    ) -> Option<HostRequest<Action>> {
        let lane = model.clip_envelope_shown()?;
        let place = self.take_envelope_place(model, size, clip, take)?;
        self.envelope_point_menu_at(model, clip, place, lane, pos)
    }

    fn envelope_point_menu_at(
        &self,
        model: &Session,
        clip: &Clip,
        place: EnvelopePlace,
        lane: EnvelopeLane,
        pos: Point,
    ) -> Option<HostRequest<Action>> {
        let points = Self::envelope_points(model, place.audio, lane);
        let i = self.envelope_point_at(model, clip, &place, lane, &points, pos)?;
        let with = |f: &dyn Fn(&mut Vec<EnvelopePoint>)| {
            let mut p = points.clone();
            f(&mut p);
            Action::SetClipEnvelope {
                audio: place.audio,
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

    /// The "Clip Envelope" submenu (the clip menu's, a take lane's) for
    /// `targets`.
    pub(crate) fn clip_envelope_items(
        model: &Session,
        targets: &[AudioRef],
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
                        audio: targets.to_vec(),
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

    /// The shown lane over an audio clip: the line and its points.
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
        let Some(place) = self.clip_envelope_place(clip, rect) else {
            return;
        };
        let points = Self::envelope_points(model, place.audio, lane);
        self.paint_envelope_at(p, model, clip, &place, lane, &points, true);
    }

    /// The shown lane over a take in its lane (`lane_rect`): the line and
    /// its points.
    pub(crate) fn paint_take_envelope(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        k: usize,
        take: &faderframe_project::Take,
        lane_rect: Rect,
    ) {
        let Some(lane) = model.clip_envelope_shown() else {
            return;
        };
        let place = self.take_body_place(model, clip, k, take, lane_rect);
        let points = take
            .audio
            .envelope
            .as_ref()
            .map(|e| e.lane(lane).to_vec())
            .unwrap_or_default();
        self.paint_envelope_at(p, model, clip, &place, lane, &points, true);
    }

    /// The shown lane over a take folder's main lane (`area`): what the
    /// comp plays, each piece its take's envelope there (a line only: it is
    /// edited in the take lanes).
    pub(crate) fn paint_comp_envelope(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        f: &TakeFolder,
        area: Rect,
    ) {
        let Some(lane) = model.clip_envelope_shown() else {
            return;
        };
        for piece in f.played() {
            let (x0, x1) = (
                self.x_of_frame(model, clip, piece.start),
                self.x_of_frame(model, clip, piece.start + piece.audio.length),
            );
            let place = EnvelopePlace {
                audio: AudioRef::Clip(clip.id),
                offset: piece.start,
                length: piece.audio.length,
                content: Rect::new(x0, area.y, (x1 - x0).max(0.0), area.h),
            };
            let points = piece
                .audio
                .envelope
                .as_ref()
                .map(|e| e.lane(lane).to_vec())
                .unwrap_or_default();
            self.paint_envelope_at(p, model, clip, &place, lane, &points, false);
        }
    }

    /// `points` of `lane` over `place` (in clip `clip`): the line across its
    /// content area, and the points (`with_points`).
    #[allow(clippy::too_many_arguments)]
    fn paint_envelope_at(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        place: &EnvelopePlace,
        lane: EnvelopeLane,
        points: &[EnvelopePoint],
        with_points: bool,
    ) {
        let content = place.content;
        if content.h < 6.0 || content.w < 2.0 {
            return;
        }
        let color = self.theme.arranger.automation;
        let y_of = |v: f64| Self::envelope_y(content, lane, v);
        let x_of = |at: i64| self.envelope_x(model, clip, place, at);
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
                    let xa = x_of(a.at);
                    line.line_to(Point::new(xa.clamp(x0, x1), y_of(a.value)));
                    let Some(b) = points.get(i + 1) else {
                        continue;
                    };
                    let xb = x_of(b.at);
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
        if !with_points {
            return;
        }
        let lit = self.lit_envelope_points(place.audio);
        for (i, a) in points.iter().enumerate() {
            let x = x_of(a.at);
            if x < x0 - POINT_LIT || x > x1 + POINT_LIT {
                continue;
            }
            let c = Point::new(x, y_of(a.value));
            let size = if lit.contains(&i) {
                POINT_LIT
            } else {
                POINT_SIZE
            };
            let r = Rect::new(c.x - size * 0.5, c.y - size * 0.5, size, size);
            if lit.contains(&i) {
                // The one a press would take: bigger, ringed in the text
                // colour, its value beside it.
                p.fill(r, color.lighten(0.25));
                p.stroke_rounded(r, 0.0, 1.5, self.theme.ui.text);
                self.paint_envelope_value(p, content, c, lane.value_text(a.value));
            } else {
                p.fill(r, color);
                p.stroke_rounded(r, 0.0, 1.0, self.theme.arranger.background);
            }
        }
    }

    /// A point's value in a small label beside it (above it, below near
    /// the top; to its left near the right end).
    fn paint_envelope_value(&self, p: &mut dyn Painter, content: Rect, at: Point, text: String) {
        let th = &self.theme;
        let style = TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).family(th.fonts.numerals);
        let w = p.text_width(&text, &style) + 8.0;
        let h = 14.0;
        let mut x = at.x + POINT_LIT;
        if x + w > content.right() {
            x = at.x - POINT_LIT - w;
        }
        let mut y = at.y - POINT_LIT - h;
        if y < content.y {
            y = at.y + POINT_LIT;
        }
        let r = Rect::new(x, y, w, h);
        p.fill_rounded(r, 3.0, &Paint::Solid(th.ui.background.with_alpha(0.85)));
        p.text(&text, r, &style.center());
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
