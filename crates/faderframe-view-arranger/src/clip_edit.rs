//! Pro-style clip editing in the arranger: the edit tools and the Smart
//! tool's zones, range selection, trims and time-stretch trims, fades
//! (length, shape and drawn bend), clip gain, Shuffle/Spot/relative-grid
//! moves, warp markers and transients, and the editing keys.
//!
//! Every edit is a session action; drags send absolute values per motion
//! inside one gesture (the session recomputes from where the gesture
//! started), and act on every selected clip when the pressed clip is part
//! of the selection.

use super::*;
use faderframe_project::{AudioClip, ClipFades, FadeShape, Warp, WarpAlgorithm, bend_factor};
use faderframe_session::warping::WarpDrag;
use faderframe_session::{
    ClipEdge, EditMode, EditRange, EditTool, GridMode, NudgeTarget, ZoomRequest, parse_position,
};

/// Pixels from a clip edge that trim.
const EDGE_GRAB: f32 = 6.0;
/// Fade handle size.
const HANDLE: f32 = 7.0;
/// Pick radius of handles, markers and transients.
const PICK: f32 = 5.0;

/// Pickable points of a clip: source frame and view x.
type Picks = Vec<(i64, f32)>;

/// What pressing a clip at a point does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClipZone {
    Move,
    Range,
    Trim(ClipEdge),
    Fade(ClipEdge),
    Bend(ClipEdge),
    Gain,
    /// A warp marker (by its source frame).
    WarpMarker(i64),
    /// A detected transient (source frame).
    Transient(i64),
    /// Audio in warp view: drag to warp, double-click for a marker.
    WarpBody,
}

impl ClipZone {
    fn cursor(self) -> Cursor {
        match self {
            ClipZone::Move => Cursor::Grab,
            ClipZone::Range => Cursor::Text,
            ClipZone::Trim(_) | ClipZone::WarpMarker(_) | ClipZone::Transient(_) => {
                Cursor::ResizeHorizontal
            }
            ClipZone::Fade(_) | ClipZone::WarpBody => Cursor::Crosshair,
            ClipZone::Bend(_) | ClipZone::Gain => Cursor::ResizeVertical,
        }
    }
}

/// An editing drag in progress (kept apart from `Drag`, which is `Copy`).
#[derive(Clone, Debug)]
pub(crate) enum EditDrag {
    Move {
        clips: Vec<ClipId>,
        anchor: ClipId,
        grab: MusicalTime,
        click: MusicalTime,
        origin: Point,
        origin_start: MusicalTime,
        origin_row: usize,
        moved: bool,
        additive: bool,
        /// Shuffle mode: where the clip would land (painted, applied on
        /// release).
        ghost: Option<(TrackId, MusicalTime)>,
    },
    Trim {
        clips: Vec<ClipId>,
        edge: ClipEdge,
        origin_edge: MusicalTime,
        stretch: bool,
        origin: Point,
        moved: bool,
    },
    Fade {
        clips: Vec<ClipId>,
        anchor: ClipId,
        edge: ClipEdge,
        origin: Point,
        moved: bool,
    },
    Bend {
        clips: Vec<ClipId>,
        edge: ClipEdge,
        start: i16,
        origin: Point,
        moved: bool,
    },
    Gain {
        clips: Vec<ClipId>,
        origin: Point,
        moved: bool,
    },
    Range {
        anchor: MusicalTime,
        anchor_row: usize,
        origin: Point,
        moved: bool,
        rows: (usize, usize),
    },
    Warp {
        clip: ClipId,
        source: i64,
        drag: WarpDrag,
        origin: Point,
        moved: bool,
    },
    /// Separation grabber: the range was separated; becomes a move of the
    /// separated clip once the model has it.
    Separate {
        track: TrackId,
        at: MusicalTime,
        origin: Point,
    },
    Zoom {
        from: f32,
        to: f32,
        out: bool,
    },
    Pencil {
        track: TrackId,
        from: MusicalTime,
        to: MusicalTime,
    },
    /// Pencil at sample level: redrawing an audio clip's samples.
    Redraw {
        clip: ClipId,
        /// One stacked channel, or all.
        channel: Option<usize>,
        map: crate::FrameMap,
        /// The waveform lane drawn in (values from y).
        lane: Rect,
        /// Drawn values by source frame.
        points: std::collections::BTreeMap<i64, f32>,
        last: (i64, f32),
    },
}

fn fades_of(c: &Clip) -> Option<(ClipFades, i64)> {
    match &c.content {
        ClipContent::Audio(a) => Some((a.fades, a.length)),
        ClipContent::Takes(f) => Some((f.fades, f.length)),
        ClipContent::Midi(_) => None,
    }
}

fn gain_of(c: &Clip) -> Option<f32> {
    match &c.content {
        ClipContent::Audio(a) => Some(a.gain_db),
        ClipContent::Takes(f) => Some(f.gain_db),
        ClipContent::Midi(_) => None,
    }
}

fn locate(t: MusicalTime) -> Action {
    Action::Transport(TransportAction::Locate(t))
}

impl ArrangerView {
    // --- geometry --------------------------------------------------------------

    /// A clip's rectangle in view coordinates.
    pub(crate) fn clip_view_rect(&self, model: &Session, size: Size, clip: ClipId) -> Option<Rect> {
        let c = model.project().clip(clip)?;
        let i = Self::lane_tracks(model)
            .iter()
            .position(|t| t.id == c.track)?;
        Some(self.clip_rect(c, self.row_rect(i, size), model))
    }

    fn header_band(&self, rect: Rect) -> f32 {
        self.theme.arranger.clip_header.min(rect.h * 0.5)
    }

    pub(crate) fn content_rect(&self, rect: Rect) -> Rect {
        let h = self.header_band(rect);
        Rect::new(rect.x, rect.y + h, rect.w, rect.h - h).inset_xy(0.0, 2.0)
    }

    /// x of a clip-relative project frame.
    pub(crate) fn x_of_frame(&self, model: &Session, clip: &Clip, frame: i64) -> f32 {
        self.x_of_sample(
            model,
            Self::clip_start_sample(model, clip) + Self::engine_frames(model, frame),
        )
    }

    /// Clip-relative project frame at x.
    fn frame_at(&self, model: &Session, clip: &Clip, x: f32) -> i64 {
        let p = model.project();
        let rate = p.sample_rate as f64;
        p.timeline.to_samples(self.time_at(x), rate) - p.timeline.to_samples(clip.start, rate)
    }

    /// The clip gain control in the clip's name strip: a knob and its
    /// value (right end of the visible part of the clip).
    pub(crate) fn gain_badge(&self, clip: &Clip, rect: Rect) -> Option<Rect> {
        gain_of(clip)?;
        let h = self.header_band(rect);
        let right = rect.right().min(self.view_w);
        (right - rect.x.max(self.header_w()) >= 110.0 && h >= 12.0)
            .then(|| Rect::new(right - 70.0, rect.y + 1.0, 67.0, h - 2.0))
    }

    /// The knob part of the gain control.
    pub(crate) fn gain_knob(badge: Rect) -> Rect {
        let d = badge.h.min(20.0);
        Rect::new(badge.x, badge.center().y - d * 0.5, d, d)
    }

    /// Knob position of a clip gain: bipolar around 0 dB, ±24 dB at the
    /// ends.
    pub(crate) fn gain_knob_value(db: f32) -> f32 {
        (0.5 + db.clamp(-24.0, 24.0) / 48.0).clamp(0.0, 1.0)
    }

    /// Centre of a fade's length handle.
    fn fade_handle(
        &self,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        edge: ClipEdge,
    ) -> Option<Point> {
        let (f, len) = fades_of(clip)?;
        let content = self.content_rect(rect);
        let half = HANDLE * 0.5 + 1.0;
        let x = match edge {
            ClipEdge::Start => self.x_of_frame(model, clip, f.fade_in).max(rect.x + half),
            ClipEdge::End => self
                .x_of_frame(model, clip, len - f.fade_out)
                .min(rect.right() - half),
        };
        Some(Point::new(x, content.y + half))
    }

    /// Fade extent in x (`x0` at the clip edge, `x1` at the fade's inner
    /// end) or `None` without a fade.
    fn fade_span(
        &self,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        edge: ClipEdge,
    ) -> Option<(f32, f32)> {
        let (f, len) = fades_of(clip)?;
        match edge {
            ClipEdge::Start if f.fade_in > 0 => {
                Some((rect.x, self.x_of_frame(model, clip, f.fade_in)))
            }
            ClipEdge::End if f.fade_out > 0 => {
                Some((rect.right(), self.x_of_frame(model, clip, len - f.fade_out)))
            }
            _ => None,
        }
    }

    /// Centre of a fade's bend handle (on the curve, half way).
    fn bend_handle(
        &self,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        edge: ClipEdge,
    ) -> Option<Point> {
        let (f, _) = fades_of(clip)?;
        let (x0, x1) = self.fade_span(model, clip, rect, edge)?;
        if (x1 - x0).abs() < 18.0 {
            return None;
        }
        let (shape, bend) = match edge {
            ClipEdge::Start => (f.fade_in_shape, f.fade_in_bend),
            ClipEdge::End => (f.fade_out_shape, f.fade_out_bend),
        };
        let content = self.content_rect(rect);
        let g = shape.gain_bent(0.5, bend_factor(bend));
        Some(Point::new(
            (x0 + x1) * 0.5,
            content.bottom() - g * content.h,
        ))
    }

    /// Warp markers and transients of an audio clip in view x.
    fn warp_points(&self, model: &Session, clip: &Clip) -> (Picks, Picks) {
        let ClipContent::Audio(a) = &clip.content else {
            return (Vec::new(), Vec::new());
        };
        let markers = a.warp.as_ref().map_or_else(Vec::new, |w| {
            w.markers
                .iter()
                .map(|m| (m.source, self.x_of_frame(model, clip, m.at)))
                .collect()
        });
        let transients = model
            .clip_transient_frames(clip)
            .into_iter()
            .map(|f| {
                (
                    Self::source_of_output(a, f),
                    self.x_of_frame(model, clip, f),
                )
            })
            .collect();
        (markers, transients)
    }

    fn source_of_output(a: &AudioClip, frame: i64) -> i64 {
        a.source_at(frame as f64).round() as i64
    }

    /// The zone of `clip` (drawn at `rect`) under `pos` for the current
    /// tool.
    pub fn clip_zone(&self, model: &Session, clip: &Clip, rect: Rect, pos: Point) -> ClipZone {
        let e = &model.editor;
        let tool = e.tool;
        let content = self.content_rect(rect);
        let header_bottom = rect.y + self.header_band(rect);
        let near = |p: Option<Point>| p.is_some_and(|p| p.distance(pos) <= PICK + 1.0);
        let fade_tools = matches!(
            tool,
            EditTool::Smart | EditTool::Trim | EditTool::Select | EditTool::Grab
        );
        if self.gain_badge(clip, rect).is_some_and(|b| b.contains(pos)) {
            return ClipZone::Gain;
        }
        if fade_tools {
            for edge in [ClipEdge::Start, ClipEdge::End] {
                if near(self.bend_handle(model, clip, rect, edge)) {
                    return ClipZone::Bend(edge);
                }
                if near(self.fade_handle(model, clip, rect, edge)) {
                    return ClipZone::Fade(edge);
                }
            }
        }
        if e.warp
            && matches!(clip.content, ClipContent::Audio(_))
            && matches!(tool, EditTool::Smart | EditTool::Grab | EditTool::Select)
            && pos.y > content.y + content.h * 0.35
        {
            let (markers, transients) = self.warp_points(model, clip);
            let pick = |list: &[(i64, f32)]| {
                list.iter()
                    .filter(|(_, x)| (x - pos.x).abs() <= PICK)
                    .min_by(|a, b| (a.1 - pos.x).abs().total_cmp(&(b.1 - pos.x).abs()))
                    .map(|(s, _)| *s)
            };
            if let Some(s) = pick(&markers) {
                return ClipZone::WarpMarker(s);
            }
            if let Some(s) = pick(&transients) {
                return ClipZone::Transient(s);
            }
            if tool != EditTool::Select {
                return ClipZone::WarpBody;
            }
        }
        let edge_ok = rect.w > 3.0 * EDGE_GRAB && pos.y >= header_bottom;
        if edge_ok
            && matches!(
                tool,
                EditTool::Smart | EditTool::Trim | EditTool::TrimStretch
            )
        {
            if pos.x - rect.x < EDGE_GRAB {
                return ClipZone::Trim(ClipEdge::Start);
            }
            if rect.right() - pos.x < EDGE_GRAB {
                return ClipZone::Trim(ClipEdge::End);
            }
        }
        match tool {
            EditTool::Trim | EditTool::TrimStretch => ClipZone::Trim(if pos.x < rect.center().x {
                ClipEdge::Start
            } else {
                ClipEdge::End
            }),
            EditTool::Select => ClipZone::Range,
            EditTool::Smart => {
                if pos.y < header_bottom {
                    ClipZone::Move
                } else if pos.y < content.center().y {
                    // Top corners create fades.
                    let corner = (pos.y < content.y + content.h * 0.3) && fades_of(clip).is_some();
                    if corner && pos.x - rect.x < 14.0 {
                        ClipZone::Fade(ClipEdge::Start)
                    } else if corner && rect.right() - pos.x < 14.0 {
                        ClipZone::Fade(ClipEdge::End)
                    } else {
                        ClipZone::Range
                    }
                } else {
                    ClipZone::Move
                }
            }
            _ => ClipZone::Move,
        }
    }

    /// The zone under `pos` if it is over a clip.
    pub(crate) fn zone_at(
        &self,
        model: &Session,
        size: Size,
        pos: Point,
    ) -> Option<(ClipId, ClipZone)> {
        let Some(Hit::Clip { clip, .. }) = self.hit_test(pos, size, model) else {
            return None;
        };
        let c = model.project().clip(clip)?;
        let rect = self.clip_view_rect(model, size, clip)?;
        Some((clip, self.clip_zone(model, c, rect, pos)))
    }

    pub(crate) fn zone_cursor(zone: ClipZone) -> Cursor {
        zone.cursor()
    }

    // --- snapping ----------------------------------------------------------------

    /// Snap a moved position per edit mode: absolute grid, relative grid
    /// (keeps the offset from the grid), or free (Slip, Shuffle, Alt).
    fn snap_moved(
        &self,
        raw: MusicalTime,
        origin: MusicalTime,
        model: &Session,
        mods: Modifiers,
    ) -> MusicalTime {
        let e = &model.editor;
        if mods.alt || e.edit_mode != EditMode::Grid {
            return raw;
        }
        match e.grid_mode {
            GridMode::Absolute => self.snap(raw, model, mods),
            GridMode::Relative => {
                let step = e
                    .step(origin, &model.project().timeline.meter)
                    .ticks()
                    .max(1);
                let d = raw.ticks() - origin.ticks();
                let k = (d as f64 / step as f64).round() as i64;
                MusicalTime((origin.ticks() + k * step).max(0))
            }
        }
    }

    // --- pressing ----------------------------------------------------------------

    /// The clips an edit of `clip` applies to: all selected when it is
    /// selected, else only it.
    fn targets(model: &Session, clip: ClipId) -> Vec<ClipId> {
        if model.selection.clips.contains(&clip) {
            model.selection.clips.iter().copied().collect()
        } else {
            vec![clip]
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn press_clip(
        &mut self,
        clip: ClipId,
        track: TrackId,
        at: MusicalTime,
        pos: Point,
        clicks: u32,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(c) = model.project().clip(clip) else {
            return false;
        };
        let Some(rect) = self.clip_view_rect(model, size, clip) else {
            return false;
        };
        let tool = model.editor.tool;
        match tool {
            EditTool::Zoom => return self.press_zoom(pos, mods, cx),
            EditTool::Scrub => {
                cx.emit(Action::Transport(
                    faderframe_session::TransportAction::Scrub(self.snap(at, model, mods)),
                ));
                self.drag = Some(Drag::Scrub { audible: true });
                return true;
            }
            EditTool::Pencil => {
                if clicks >= 2 && c.as_midi().is_some() {
                    cx.emit(Action::OpenClipEditor(clip));
                } else if let Some(drag) = self.start_redraw(c, rect, pos, model) {
                    self.edit_drag = Some(drag);
                    cx.set_cursor(Cursor::Crosshair);
                }
                return true;
            }
            _ => {}
        }
        let zone = self.clip_zone(model, c, rect, pos);
        let selected = model.selection.clips.contains(&clip);
        let additive = mods.shift || mods.toggle();
        if clicks >= 2 {
            return self.double_click_clip(c, zone, pos, model, cx);
        }
        // Selection: Shift/Ctrl add or remove; a plain press keeps a
        // selection the clip belongs to (so it can be dragged as a group).
        let targets: Vec<ClipId> = if additive {
            cx.emit(Action::SelectClips {
                clips: vec![clip],
                mode: SelectMode::Toggle,
            });
            cx.emit(Action::SelectTracks {
                tracks: vec![track],
                mode: SelectMode::Add,
            });
            if selected {
                // Removed from the selection: nothing to drag.
                return true;
            }
            let mut t: Vec<ClipId> = model.selection.clips.iter().copied().collect();
            t.push(clip);
            t
        } else {
            if !selected {
                cx.emit(Action::SelectClips {
                    clips: vec![clip],
                    mode: SelectMode::Replace,
                });
            }
            cx.emit(Action::SelectTracks {
                tracks: vec![track],
                mode: SelectMode::Replace,
            });
            Self::targets(model, clip)
        };
        let row = self.row_at(pos.y).unwrap_or(0);
        self.edit_drag = Some(match zone {
            ClipZone::Gain if mods.alt => {
                if let Some(b) = self.gain_badge(c, rect) {
                    cx.request(Self::gain_request(model, c, b));
                }
                return true;
            }
            ClipZone::Gain => EditDrag::Gain {
                clips: targets,
                origin: pos,
                moved: false,
            },
            ClipZone::Fade(edge) => EditDrag::Fade {
                clips: targets,
                anchor: clip,
                edge,
                origin: pos,
                moved: false,
            },
            ClipZone::Bend(edge) => {
                let (f, _) = fades_of(c).unwrap_or_default();
                EditDrag::Bend {
                    clips: targets,
                    edge,
                    start: match edge {
                        ClipEdge::Start => f.fade_in_bend,
                        ClipEdge::End => f.fade_out_bend,
                    },
                    origin: pos,
                    moved: false,
                }
            }
            ClipZone::Trim(edge) => EditDrag::Trim {
                clips: targets,
                edge,
                origin_edge: match edge {
                    ClipEdge::Start => c.start,
                    ClipEdge::End => {
                        let p = model.project();
                        c.end(&p.timeline, p.sample_rate)
                    }
                },
                stretch: tool == EditTool::TrimStretch,
                origin: pos,
                moved: false,
            },
            ClipZone::WarpMarker(source) if mods.alt => {
                cx.emit(Action::RemoveWarpMarker { clip, source });
                return true;
            }
            ClipZone::WarpMarker(source) => EditDrag::Warp {
                clip,
                source,
                drag: if mods.ctrl {
                    WarpDrag::Telescope
                } else {
                    WarpDrag::Free
                },
                origin: pos,
                moved: false,
            },
            ClipZone::Transient(source) => EditDrag::Warp {
                clip,
                source,
                drag: if mods.ctrl {
                    WarpDrag::Telescope
                } else if mods.alt {
                    WarpDrag::Free
                } else {
                    WarpDrag::Transients
                },
                origin: pos,
                moved: false,
            },
            ClipZone::WarpBody => {
                let ClipContent::Audio(a) = &c.content else {
                    return true;
                };
                let frame = self.frame_at(model, c, pos.x).clamp(0, a.length);
                let source = Self::source_of_output(a, frame);
                // Inside the edit selection: a range warp pinned at its edges.
                let p = model.project();
                let rate = p.sample_rate as f64;
                let base = p.timeline.to_samples(c.start, rate);
                let range = model
                    .selection
                    .range
                    .filter(|r| !r.is_empty() && r.start <= at && at <= r.end);
                let drag = match range {
                    _ if mods.ctrl => WarpDrag::Telescope,
                    Some(r) => {
                        let f = |t: MusicalTime| {
                            let out = (p.timeline.to_samples(t, rate) - base).clamp(0, a.length);
                            Self::source_of_output(a, out)
                        };
                        WarpDrag::Range {
                            from: f(r.start),
                            to: f(r.end),
                        }
                    }
                    None => WarpDrag::Free,
                };
                EditDrag::Warp {
                    clip,
                    source,
                    drag,
                    origin: pos,
                    moved: false,
                }
            }
            ClipZone::Range => EditDrag::Range {
                anchor: self.snap(at, model, mods),
                anchor_row: row,
                origin: pos,
                moved: false,
                rows: (row, row),
            },
            ClipZone::Move => {
                if model.editor.edit_mode == EditMode::Spot && !additive {
                    cx.request(Self::spot_request(model, c, rect));
                    return true;
                }
                if tool == EditTool::GrabSeparation
                    && let Some(r) = model.selection.range.filter(|r| !r.is_empty())
                    && r.start <= at
                    && at < r.end
                {
                    cx.emit(Action::Separate);
                    self.edit_drag = Some(EditDrag::Separate {
                        track,
                        at,
                        origin: pos,
                    });
                    return true;
                }
                EditDrag::Move {
                    clips: targets,
                    anchor: clip,
                    grab: at - c.start,
                    click: at,
                    origin: pos,
                    origin_start: c.start,
                    origin_row: row,
                    moved: false,
                    additive,
                    ghost: None,
                }
            }
        });
        cx.set_cursor(match zone {
            ClipZone::Move => Cursor::Grabbing,
            z => z.cursor(),
        });
        true
    }

    fn double_click_clip(
        &mut self,
        c: &Clip,
        zone: ClipZone,
        pos: Point,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let targets = Self::targets(model, c.id);
        match zone {
            ClipZone::Gain => cx.emit(Action::SetClipsGain {
                clips: targets,
                db: 0.0,
            }),
            ClipZone::Fade(edge) => cx.request(Self::fade_menu(model, c, edge, pos)),
            ClipZone::Bend(edge) => cx.emit(Action::SetFade {
                clips: targets,
                edge,
                length: None,
                shape: None,
                bend: Some(0),
            }),
            ClipZone::WarpBody | ClipZone::Transient(_) | ClipZone::WarpMarker(_) => {
                if let ClipContent::Audio(a) = &c.content {
                    let source = match zone {
                        ClipZone::Transient(s) | ClipZone::WarpMarker(s) => s,
                        _ => Self::source_of_output(
                            a,
                            self.frame_at(model, c, pos.x).clamp(0, a.length),
                        ),
                    };
                    let w = a.warp.clone().unwrap_or_else(|| Warp::uniform(a.length));
                    cx.emit(Action::WarpTo {
                        clip: c.id,
                        source,
                        to: w.output_of(a.source_offset, a.length, source),
                        drag: WarpDrag::Free,
                    });
                }
            }
            ClipZone::Range => {
                let p = model.project();
                cx.emit(Action::SetEditRange(Some(EditRange::new(
                    c.start,
                    c.end(&p.timeline, p.sample_rate),
                ))));
            }
            ClipZone::Move | ClipZone::Trim(_) => {
                if c.as_midi().is_some() {
                    cx.emit(Action::OpenClipEditor(c.id));
                } else if c.as_takes().is_some() {
                    cx.emit(Action::ToggleTakeLanes(c.id));
                }
            }
        }
        true
    }

    /// Spot mode: type the clip's start position.
    fn spot_request(model: &Session, c: &Clip, at: Rect) -> HostRequest<Action> {
        let p = model.project();
        let unit = model.editor.counter_unit;
        let timeline = p.timeline.clone();
        let rate = p.sample_rate;
        let tc = p.timecode.unwrap_or_default();
        let id = c.id;
        let initial = match unit {
            faderframe_session::CounterUnit::BarsBeats => p.timeline.format_bbt(c.start),
            _ => faderframe_session::format_position(
                p,
                p.timeline.to_samples(c.start, rate as f64),
                unit,
            ),
        };
        HostRequest::TextInput {
            at: Rect::new(at.x, at.y, at.w.max(120.0), 24.0),
            initial,
            commit: Box::new(move |text| {
                parse_position(text, unit, &timeline, rate, tc)
                    .map(|start| Action::SpotClip { clip: id, start })
            }),
        }
    }

    /// Fade shape menu (right-click or double-click on a fade).
    pub(crate) fn fade_menu(
        model: &Session,
        c: &Clip,
        edge: ClipEdge,
        at: Point,
    ) -> HostRequest<Action> {
        let targets = Self::targets(model, c.id);
        let (f, _) = fades_of(c).unwrap_or_default();
        let (shape, bend, len) = match edge {
            ClipEdge::Start => (f.fade_in_shape, f.fade_in_bend, f.fade_in),
            ClipEdge::End => (f.fade_out_shape, f.fade_out_bend, f.fade_out),
        };
        let name = match edge {
            ClipEdge::Start => "Fade In",
            ClipEdge::End => "Fade Out",
        };
        let mut items = vec![MenuItem::disabled(name)];
        for s in FadeShape::ALL {
            items.push(
                MenuItem::new(
                    s.label(),
                    Action::SetFade {
                        clips: targets.clone(),
                        edge,
                        length: (len == 0).then_some(model.project().sample_rate as i64 / 20),
                        shape: Some(s),
                        bend: None,
                    },
                )
                .checked(len > 0 && s == shape),
            );
        }
        if bend != 0 {
            items.push(
                MenuItem::new(
                    "Reset Drawn Curve",
                    Action::SetFade {
                        clips: targets.clone(),
                        edge,
                        length: None,
                        shape: None,
                        bend: Some(0),
                    },
                )
                .separated(),
            );
        }
        if len > 0 {
            items.push(
                MenuItem::new(
                    format!("Remove {name}"),
                    Action::SetFade {
                        clips: targets,
                        edge,
                        length: Some(0),
                        shape: None,
                        bend: Some(0),
                    },
                )
                .separated(),
            );
        }
        HostRequest::ContextMenu { at, items }
    }

    /// Clip gain typed in (clip menu).
    pub(crate) fn gain_request(model: &Session, c: &Clip, at: Rect) -> HostRequest<Action> {
        let targets = Self::targets(model, c.id);
        HostRequest::TextInput {
            at: Rect::new(at.x, at.y, at.w.max(90.0), 24.0),
            initial: format!("{:.1}", gain_of(c).unwrap_or(0.0)),
            commit: Box::new(move |text| {
                text.trim()
                    .trim_end_matches("dB")
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|db| Action::SetClipsGain {
                        clips: targets.clone(),
                        db,
                    })
            }),
        }
    }

    /// The Zoom tool: drag a range, click to zoom in (Alt: out).
    pub(crate) fn press_zoom(
        &mut self,
        pos: Point,
        mods: Modifiers,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.edit_drag = Some(EditDrag::Zoom {
            from: pos.x,
            to: pos.x,
            out: mods.alt,
        });
        cx.set_cursor(Cursor::Crosshair);
        true
    }

    /// A press on an empty lane with the editing tools.
    pub(crate) fn press_lane(
        &mut self,
        track: TrackId,
        at: MusicalTime,
        pos: Point,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let row = self.row_at(pos.y).unwrap_or(0);
        match model.editor.tool {
            EditTool::Zoom => self.press_zoom(pos, mods, cx),
            EditTool::Scrub => {
                cx.emit(Action::Transport(
                    faderframe_session::TransportAction::Scrub(self.snap(at, model, mods)),
                ));
                self.drag = Some(Drag::Scrub { audible: true });
                true
            }
            EditTool::Pencil => {
                let instrument = model
                    .project()
                    .track(track)
                    .is_some_and(|t| matches!(t.kind, TrackKind::Instrument | TrackKind::Midi));
                if instrument {
                    let start = self.snap(at, model, mods);
                    self.edit_drag = Some(EditDrag::Pencil {
                        track,
                        from: start,
                        to: start,
                    });
                    cx.set_cursor(Cursor::Crosshair);
                }
                true
            }
            _ => {
                if mods.shift
                    && let Some(r) = model.selection.range
                {
                    // Shift extends the selection to the click.
                    let t = self.snap(at, model, mods);
                    let anchor = if t >= r.start { r.start } else { r.end };
                    cx.emit(Action::SetEditRange(Some(EditRange::new(anchor, t))));
                    cx.emit(Action::SelectTracks {
                        tracks: vec![track],
                        mode: SelectMode::Add,
                    });
                    return true;
                }
                cx.emit(Action::SelectClips {
                    clips: vec![],
                    mode: SelectMode::Replace,
                });
                cx.emit(Action::SelectTracks {
                    tracks: vec![track],
                    mode: SelectMode::Replace,
                });
                self.edit_drag = Some(EditDrag::Range {
                    anchor: self.snap(at, model, mods),
                    anchor_row: row,
                    origin: pos,
                    moved: false,
                    rows: (row, row),
                });
                cx.set_cursor(Cursor::Text);
                true
            }
        }
    }

    // --- dragging ----------------------------------------------------------------

    /// Continue an editing drag; false when none is running.
    /// A Pencil press on an audio clip at sample level starts redrawing.
    fn start_redraw(&self, c: &Clip, rect: Rect, pos: Point, model: &Session) -> Option<EditDrag> {
        self.sample_zoom(model)?;
        let ClipContent::Audio(a) = &c.content else {
            return None;
        };
        if a.warp.is_some() || a.reversed {
            return None;
        }
        let ratio = model.frame_ratio();
        let pr = model.peak_rate(a.source) / model.sample_rate() as f64;
        let map = crate::FrameMap {
            source: a.source,
            start: model.engine().musical_to_samples(model.project(), c.start),
            len: (a.length as f64 * ratio) as i64,
            src_off: a.source_offset as f64 * ratio * pr,
            pr,
            gain: faderframe_core::db_to_gain(a.gain_db),
        };
        let channels = model.source_frames(a.source, 0, 0)?.len();
        let content = self.clip_content_rect(rect);
        let lanes = Self::wave_lanes(content, channels);
        let (i, (lane, _)) = lanes
            .iter()
            .enumerate()
            .find(|(_, (l, _))| pos.y >= l.y && pos.y < l.bottom())?;
        let channel = (lanes.len() > 1).then_some(i);
        let point = self.redraw_point(&map, *lane, pos, model)?;
        let mut points = std::collections::BTreeMap::new();
        points.insert(point.0, point.1);
        Some(EditDrag::Redraw {
            clip: c.id,
            channel,
            map,
            lane: *lane,
            points,
            last: point,
        })
    }

    /// The source frame and value under `pos` (inside the clip).
    fn redraw_point(
        &self,
        map: &crate::FrameMap,
        lane: Rect,
        pos: Point,
        model: &Session,
    ) -> Option<(i64, f32)> {
        let s = self.sample_at_x(model, pos.x);
        let f = map.frame_at(s);
        let (first, last) = map.frames();
        if f < first || f >= last {
            return None;
        }
        let half = lane.h * 0.46;
        let v = (lane.center().y - pos.y) / half.max(1.0) / map.gain.max(1e-3);
        Some((f, v.clamp(-1.0, 1.0)))
    }

    /// The samples being redrawn, over the clip's waveform.
    pub(crate) fn paint_redraw(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: ClipId,
        _content: Rect,
    ) {
        let Some(EditDrag::Redraw {
            clip: c,
            map,
            lane,
            points,
            ..
        }) = &self.edit_drag
        else {
            return;
        };
        if *c != clip {
            return;
        }
        let half = lane.h * 0.46;
        let mid = lane.center().y;
        let color = self.theme.ui.accent;
        let mut path = faderframe_ui_canvas::Path::new();
        for (i, (f, v)) in points.iter().enumerate() {
            let pt = Point::new(
                self.x_of_sample(model, map.sample_of(*f)),
                mid - (v * map.gain).clamp(-1.0, 1.0) * half,
            );
            if i == 0 {
                path.move_to(pt);
            } else {
                path.line_to(pt);
            }
            p.circle(pt, 2.0, color);
        }
        p.stroke_path(&path, 1.6, color);
    }

    pub(crate) fn edit_drag_move(
        &mut self,
        pos: Point,
        mods: Modifiers,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(mut drag) = self.edit_drag.take() else {
            return false;
        };
        let started =
            |moved: &mut bool, origin: Point, cx: &mut EventCx<'_, Action>, label: &str| {
                if !*moved {
                    if pos.distance(origin) < DRAG_THRESHOLD {
                        return false;
                    }
                    *moved = true;
                    if !label.is_empty() {
                        cx.emit(Action::BeginGesture(label.into()));
                    }
                }
                true
            };
        let t_at = self.time_at(pos.x).max(MusicalTime::ZERO);
        match &mut drag {
            EditDrag::Move {
                clips,
                anchor,
                grab,
                origin,
                origin_start,
                origin_row,
                moved,
                ghost,
                ..
            } => {
                let shuffle = model.editor.edit_mode == EditMode::Shuffle;
                let label = if shuffle { "" } else { "Move Clip" };
                if started(moved, *origin, cx, label) {
                    let raw = MusicalTime((t_at.ticks() - grab.ticks()).max(0));
                    let start = self.snap_moved(raw, *origin_start, model, mods);
                    let lanes = Self::lane_tracks(model);
                    let row = self
                        .row_at(pos.y)
                        .unwrap_or(*origin_row)
                        .min(lanes.len().saturating_sub(1));
                    if shuffle {
                        let c = model.project().clip(*anchor);
                        let track = lanes
                            .get(row)
                            .filter(|t| c.is_some_and(|c| clip_fits(t, c)))
                            .map(|t| t.id)
                            .or(c.map(|c| c.track));
                        *ghost = track.map(|t| (t, start));
                        cx.redraw();
                    } else {
                        cx.emit(Action::MoveClips {
                            clips: clips.clone(),
                            by: start.ticks() - origin_start.ticks(),
                            tracks: row as i32 - *origin_row as i32,
                        });
                    }
                }
            }
            EditDrag::Trim {
                clips,
                edge,
                origin_edge,
                stretch,
                origin,
                moved,
            } => {
                let label = if *stretch {
                    "Time Stretch"
                } else {
                    "Trim Clip"
                };
                if started(moved, *origin, cx, label) {
                    let to = self.snap_moved(t_at, *origin_edge, model, mods);
                    cx.emit(Action::TrimClips {
                        clips: clips.clone(),
                        edge: *edge,
                        by: to.ticks() - origin_edge.ticks(),
                        stretch: *stretch,
                    });
                }
            }
            EditDrag::Fade {
                clips,
                anchor,
                edge,
                origin,
                moved,
            } => {
                if started(moved, *origin, cx, "Fade")
                    && let Some(c) = model.project().clip(*anchor)
                {
                    let p = model.project();
                    let rate = p.sample_rate as f64;
                    let at = p.timeline.to_samples(t_at, rate);
                    let start = p.timeline.to_samples(c.start, rate);
                    let end = p
                        .timeline
                        .to_samples(c.end(&p.timeline, p.sample_rate), rate);
                    let length = match edge {
                        ClipEdge::Start => at - start,
                        ClipEdge::End => end - at,
                    }
                    .max(0);
                    cx.emit(Action::SetFade {
                        clips: clips.clone(),
                        edge: *edge,
                        length: Some(length),
                        shape: None,
                        bend: None,
                    });
                }
            }
            EditDrag::Bend {
                clips,
                edge,
                start,
                origin,
                moved,
            } => {
                if started(moved, *origin, cx, "Fade Curve") {
                    let bend = (*start as f32 + (origin.y - pos.y) * 1.5)
                        .round()
                        .clamp(-100.0, 100.0);
                    cx.emit(Action::SetFade {
                        clips: clips.clone(),
                        edge: *edge,
                        length: None,
                        shape: None,
                        bend: Some(bend as i16),
                    });
                }
            }
            EditDrag::Gain {
                clips,
                origin,
                moved,
            } => {
                if started(moved, *origin, cx, "Clip Gain") {
                    let per_px = if mods.fine() { 0.02 } else { 0.1 };
                    let delta = ((origin.y - pos.y) * per_px * 10.0).round() / 10.0;
                    cx.emit(Action::ClipGain {
                        clips: clips.clone(),
                        delta_db: delta,
                    });
                }
            }
            EditDrag::Range {
                anchor,
                anchor_row,
                origin,
                moved,
                rows,
            } => {
                if started(moved, *origin, cx, "") {
                    let t = self.snap(t_at, model, mods);
                    let lanes = Self::lane_tracks(model);
                    let row = self
                        .row_at(pos.y)
                        .unwrap_or(*anchor_row)
                        .min(lanes.len().saturating_sub(1));
                    let span = ((*anchor_row).min(row), (*anchor_row).max(row));
                    if span != *rows {
                        *rows = span;
                        let tracks = lanes
                            .get(span.0..=span.1.min(lanes.len().saturating_sub(1)))
                            .unwrap_or(&[])
                            .iter()
                            .map(|t| t.id)
                            .collect();
                        cx.emit(Action::SelectTracks {
                            tracks,
                            mode: SelectMode::Replace,
                        });
                    }
                    cx.emit(Action::SetEditRange(Some(EditRange::new(*anchor, t))));
                }
            }
            EditDrag::Warp {
                clip,
                source,
                drag,
                origin,
                moved,
            } => {
                if started(moved, *origin, cx, "Warp")
                    && let Some(c) = model.project().clip(*clip)
                {
                    let t = self.snap(t_at, model, mods);
                    let p = model.project();
                    let rate = p.sample_rate as f64;
                    let to = p.timeline.to_samples(t, rate) - p.timeline.to_samples(c.start, rate);
                    cx.emit(Action::WarpTo {
                        clip: *clip,
                        source: *source,
                        to,
                        drag: *drag,
                    });
                }
            }
            EditDrag::Separate { track, at, origin } => {
                if pos.distance(*origin) >= DRAG_THRESHOLD {
                    // The separated piece starts at the range start.
                    let piece = model
                        .project()
                        .clips_of(*track)
                        .into_iter()
                        .find(|c| {
                            let p = model.project();
                            c.start <= *at && *at < c.end(&p.timeline, p.sample_rate)
                        })
                        .map(|c| (c.id, c.start));
                    if let Some((id, start)) = piece {
                        cx.emit(Action::SelectClips {
                            clips: vec![id],
                            mode: SelectMode::Replace,
                        });
                        drag = EditDrag::Move {
                            clips: vec![id],
                            anchor: id,
                            grab: *at - start,
                            click: *at,
                            origin: *origin,
                            origin_start: start,
                            origin_row: self.row_at(origin.y).unwrap_or(0),
                            moved: false,
                            additive: false,
                            ghost: None,
                        };
                        self.edit_drag = Some(drag);
                        return self.edit_drag_move(pos, mods, model, cx);
                    }
                }
            }
            EditDrag::Zoom { to, .. } => {
                *to = pos.x;
                cx.redraw();
            }
            EditDrag::Pencil { from, to, .. } => {
                *to = self.snap(t_at, model, mods).max(*from);
                cx.redraw();
            }
            EditDrag::Redraw {
                map,
                lane,
                points,
                last,
                ..
            } => {
                if let Some((f, v)) = self.redraw_point(map, *lane, pos, model) {
                    // Every frame between the last point and this one
                    // (fast strokes leave no gaps).
                    let (lf, lv) = *last;
                    let n = (f - lf).abs();
                    for k in 0..=n {
                        let t = if n == 0 { 1.0 } else { k as f32 / n as f32 };
                        let frame = lf + (f - lf).signum() * k;
                        points.insert(frame, lv + (v - lv) * t);
                    }
                    *last = (f, v);
                    cx.redraw();
                }
            }
        }
        self.edit_drag = Some(drag);
        true
    }

    /// Finish an editing drag; false when none is running.
    pub(crate) fn edit_release(
        &mut self,
        model: &Session,
        size: Size,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(drag) = self.edit_drag.take() else {
            return false;
        };
        match drag {
            EditDrag::Move {
                anchor,
                click,
                moved,
                additive,
                ghost,
                ..
            } => {
                if moved {
                    match ghost {
                        Some((track, at)) => cx.emit(Action::ShuffleClip {
                            clip: anchor,
                            track,
                            at,
                        }),
                        None if model.editor.edit_mode != EditMode::Shuffle => {
                            cx.emit(Action::EndGesture)
                        }
                        None => {}
                    }
                } else {
                    // A click: only this clip, and the playhead goes there.
                    if !additive && model.selection.clips.len() > 1 {
                        cx.emit(Action::SelectClips {
                            clips: vec![anchor],
                            mode: SelectMode::Replace,
                        });
                    }
                    self.click_locate(click, model, cx);
                }
            }
            EditDrag::Trim { moved, .. }
            | EditDrag::Fade { moved, .. }
            | EditDrag::Bend { moved, .. }
            | EditDrag::Gain { moved, .. }
            | EditDrag::Warp { moved, .. } => {
                if moved {
                    cx.emit(Action::EndGesture);
                }
            }
            EditDrag::Range { anchor, moved, .. } => {
                if !moved {
                    self.click_locate(anchor, model, cx);
                }
            }
            EditDrag::Separate { .. } => {}
            EditDrag::Zoom { from, to, out } => {
                if (to - from).abs() > 6.0 {
                    let a = self.time_at(from.min(to));
                    let b = self.time_at(from.max(to));
                    self.zoom_to(a, b, model, size);
                } else {
                    self.zoom_at(from, if out { 0.5 } else { 2.0 }, model, size);
                }
                cx.redraw();
            }
            EditDrag::Redraw {
                clip,
                channel,
                points,
                ..
            } => {
                if let (Some((&start, _)), Some((&end, _))) =
                    (points.first_key_value(), points.last_key_value())
                {
                    // Contiguous values (the stroke filled every frame).
                    let samples: Vec<f32> = (start..=end)
                        .map(|f| points.get(&f).copied().unwrap_or(0.0))
                        .collect();
                    cx.emit(Action::RedrawAudio {
                        clip,
                        channel,
                        start,
                        samples,
                    });
                }
            }
            EditDrag::Pencil { track, from, to } => {
                let length = if to > from {
                    to - from
                } else {
                    model.editor.step(from, &model.project().timeline.meter)
                };
                cx.emit(Action::CreateMidiClip {
                    track,
                    start: from,
                    length,
                });
            }
        }
        cx.set_cursor(Cursor::Default);
        true
    }

    /// A click (no drag): clear the range and, with Link Timeline, move
    /// the playhead to the clicked (snapped) position.
    fn click_locate(&self, at: MusicalTime, model: &Session, cx: &mut EventCx<'_, Action>) {
        let t = self.snap(at, model, Modifiers::NONE);
        if model.selection.range.is_some() {
            cx.emit(Action::SetEditRange(None));
        }
        if model.editor.link_timeline {
            cx.emit(locate(t));
        }
    }

    /// Fit `[a, b)` into the lanes.
    pub(crate) fn zoom_to(&mut self, a: MusicalTime, b: MusicalTime, model: &Session, size: Size) {
        let lanes_w = (size.w - self.header_w()).max(1.0);
        let q = (b - a).quarters().max(1e-3) as f32;
        self.ppq = (lanes_w * 0.94 / q).clamp(MIN_PPQ, MAX_PPQ);
        self.scroll_x = a.quarters() * self.ppq as f64 - lanes_w as f64 * 0.03;
        self.clamp_scroll(model, size);
    }

    /// Apply a zoom request from the edit toolbar (once per request).
    pub(crate) fn apply_zoom_request(&mut self, model: &Session, size: Size) {
        let (seq, z) = model.editor.zoom_request;
        if seq == self.zoom_seen {
            return;
        }
        self.zoom_seen = seq;
        let at = self.x_of(model.playhead()).clamp(self.header_w(), size.w);
        match z {
            ZoomRequest::In => self.zoom_at(at, 1.6, model, size),
            ZoomRequest::Out => self.zoom_at(at, 1.0 / 1.6, model, size),
            ZoomRequest::Selection => {
                let p = model.project();
                let range = model.selection.range.filter(|r| !r.is_empty()).or_else(|| {
                    let clips: Vec<&Clip> = model
                        .selection
                        .clips
                        .iter()
                        .filter_map(|c| p.clip(*c))
                        .collect();
                    let a = clips.iter().map(|c| c.start).min()?;
                    let b = clips
                        .iter()
                        .map(|c| c.end(&p.timeline, p.sample_rate))
                        .max()?;
                    Some(EditRange::new(a, b))
                });
                if let Some(r) = range {
                    self.zoom_to(r.start, r.end, model, size);
                }
            }
            ZoomRequest::Fit => {
                let end = model
                    .project()
                    .content_end()
                    .max(MusicalTime::from_quarters(4.0));
                self.zoom_to(MusicalTime::ZERO, end, model, size);
            }
        }
    }

    // --- keys --------------------------------------------------------------------

    /// Editing keys; `None` when the key is not an editing key.
    pub(crate) fn edit_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> Option<bool> {
        let ctrl = mods.ctrl || mods.meta;
        let selected: Vec<ClipId> = model.selection.clips.iter().copied().collect();
        let has_range = model.selection.range.is_some_and(|r| !r.is_empty());
        let action = match key {
            // Edit modes and tools (Alt + digit; F5–F7, F9, F10 for tools).
            Key::Char(d @ '1'..='4') if mods.alt => {
                Action::SetEditMode(EditMode::ALL[(d as u8 - b'1') as usize])
            }
            Key::Char(d @ ('5'..='9' | '0')) if mods.alt => Action::SetEditTool(match d {
                '5' => EditTool::Zoom,
                '6' => EditTool::Trim,
                '7' => EditTool::Select,
                '8' => EditTool::Grab,
                '9' => EditTool::Scrub,
                _ => EditTool::Pencil,
            }),
            Key::Char('s' | 'S') if mods.alt => Action::SetEditTool(EditTool::Smart),
            Key::F(5) => Action::SetEditTool(EditTool::Zoom),
            Key::F(6) => Action::SetEditTool(EditTool::Trim),
            Key::F(7) => Action::SetEditTool(EditTool::Select),
            Key::F(9) => Action::SetEditTool(EditTool::Scrub),
            Key::F(10) => Action::SetEditTool(EditTool::Pencil),
            Key::Char('b' | 'B') if !ctrl && !mods.alt => Action::Separate,
            Key::Char('q' | 'Q') if !ctrl && !mods.alt && !selected.is_empty() => {
                Action::QuantizeClips(selected.clone())
            }
            Key::Char('c' | 'C') if ctrl && has_range => Action::CopyRange,
            Key::Char('x' | 'X') if ctrl && has_range => Action::CutRange,
            Key::Char('v' | 'V') if ctrl => Action::PasteRange,
            Key::Char('d' | 'D') if ctrl && has_range => Action::RepeatRange(1),
            Key::Char('r' | 'R') if mods.alt && has_range => {
                let at = Rect::new(size.w * 0.5 - 60.0, self.ruler_h() + 8.0, 120.0, 24.0);
                cx.request(HostRequest::TextInput {
                    at,
                    initial: "2".into(),
                    commit: Box::new(|t| {
                        t.trim()
                            .parse::<u32>()
                            .ok()
                            .filter(|n| (1..=999).contains(n))
                            .map(Action::RepeatRange)
                    }),
                });
                return Some(true);
            }
            Key::Char('t' | 'T') if ctrl && mods.alt => Action::TrimToSelection,
            Key::Char(',' | '.' | '<' | '>') => Action::Nudge {
                forward: matches!(key, Key::Char('.' | '>')),
                target: if mods.alt {
                    NudgeTarget::TrimStart
                } else if ctrl {
                    NudgeTarget::TrimEnd
                } else {
                    NudgeTarget::Move
                },
            },
            Key::Tab => Action::TabTo {
                forward: !mods.shift,
                extend: ctrl,
            },
            Key::Up | Key::Down if ctrl && mods.shift && !selected.is_empty() => Action::ClipGain {
                clips: selected,
                delta_db: if key == Key::Up { 0.5 } else { -0.5 },
            },
            Key::Char(']') if ctrl => Action::Zoom(ZoomRequest::In),
            Key::Char('[') if ctrl => Action::Zoom(ZoomRequest::Out),
            Key::Escape if model.selection.range.is_some() => Action::SetEditRange(None),
            _ => return None,
        };
        cx.emit(action);
        Some(true)
    }

    // --- painting ----------------------------------------------------------------

    /// Fades as curves (shape and drawn bend), with their handles when the
    /// clip is selected or hovered.
    pub(crate) fn paint_fades(
        &self,
        p: &mut dyn Painter,
        model: &Session,
        clip: &Clip,
        rect: Rect,
        color: Color,
    ) {
        let Some((f, _)) = fades_of(clip) else {
            return;
        };
        let content = self.content_rect(rect);
        for edge in [ClipEdge::Start, ClipEdge::End] {
            let Some((x0, x1)) = self.fade_span(model, clip, rect, edge) else {
                continue;
            };
            let (shape, bend) = match edge {
                ClipEdge::Start => (f.fade_in_shape, f.fade_in_bend),
                ClipEdge::End => (f.fade_out_shape, f.fade_out_bend),
            };
            let bend = bend_factor(bend);
            let n = (((x1 - x0).abs() / 3.0) as usize).clamp(2, 48);
            let curve: Vec<Point> = (0..=n)
                .map(|i| {
                    let t = i as f32 / n as f32;
                    let g = shape.gain_bent(t, bend);
                    Point::new(x0 + (x1 - x0) * t, content.bottom() - g * content.h)
                })
                .collect();
            // Shade what the fade removes (above the curve).
            let mut shade = Path::new();
            shade.move_to(Point::new(x0, content.y));
            for pt in &curve {
                shade.line_to(*pt);
            }
            shade.close();
            p.fill_path(&shade, Color::rgba(0.0, 0.0, 0.0, 0.32));
            let mut line = Path::new();
            line.move_to(curve[0]);
            for pt in &curve[1..] {
                line.line_to(*pt);
            }
            p.stroke_path(&line, 1.2, color.lighten(0.45).with_alpha(0.9));
        }
        let hot = model.selection.clips.contains(&clip.id)
            || self.zone_hover.is_some_and(|(c, _)| c == clip.id);
        if hot {
            for edge in [ClipEdge::Start, ClipEdge::End] {
                if let Some(c) = self.fade_handle(model, clip, rect, edge) {
                    let r = Rect::new(c.x - HANDLE * 0.5, c.y - HANDLE * 0.5, HANDLE, HANDLE);
                    p.fill(r, Color::rgba(1.0, 1.0, 1.0, 0.85));
                    p.stroke_rounded(r, 0.0, 1.0, Color::rgba(0.0, 0.0, 0.0, 0.6));
                }
                if let Some(c) = self.bend_handle(model, clip, rect, edge) {
                    p.circle(c, 3.5, Color::rgba(1.0, 1.0, 1.0, 0.85));
                }
            }
        }
    }

    /// Clip gain readout (and the gain line while it is being edited or
    /// hovered).
    pub(crate) fn paint_gain(&self, p: &mut dyn Painter, clip: &Clip, rect: Rect, text: Color) {
        let (Some(db), Some(badge)) = (gain_of(clip), self.gain_badge(clip, rect)) else {
            return;
        };
        let active = matches!(self.zone_hover, Some((c, ClipZone::Gain)) if c == clip.id)
            || matches!(&self.edit_drag, Some(EditDrag::Gain { clips, .. }) if clips.contains(&clip.id));
        if active {
            p.fill_rounded(badge, 3.0, &Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.3)));
        }
        let knob = Self::gain_knob(badge);
        controls::knob(
            p,
            knob,
            Self::gain_knob_value(db),
            true,
            KnobLook {
                cap: Color::hex(0x8a8f99),
                ring: if db.abs() < 0.05 {
                    text.with_alpha(0.5)
                } else {
                    Color::hex(0xffcf66)
                },
            },
            &self.theme,
        );
        let label = if db.abs() < 0.05 {
            "0.0 dB".to_string()
        } else {
            format!("{db:+.1} dB")
        };
        let label_rect = Rect::new(
            knob.right() + 2.0,
            badge.y,
            badge.right() - knob.right() - 4.0,
            badge.h,
        );
        p.text(
            &label,
            label_rect,
            &TextStyle::new(
                self.theme.fonts.tiny + 0.5,
                text.with_alpha(if active || db.abs() >= 0.05 { 1.0 } else { 0.7 }),
            )
            .align(Align::End),
        );
        if active {
            // 0 dB at three quarters of the height, +12 dB at the top.
            let content = self.content_rect(rect);
            let pos = (self.law.db_to_position(db) / self.law.db_to_position(12.0)).clamp(0.0, 1.0);
            let y = content.bottom() - pos * content.h;
            p.hline(rect.x, rect.right(), y, Color::rgba(1.0, 0.85, 0.4, 0.9));
        }
    }

    /// Transients and warp markers of an audio clip.
    pub(crate) fn paint_warp(&self, p: &mut dyn Painter, model: &Session, clip: &Clip, rect: Rect) {
        let e = &model.editor;
        let ClipContent::Audio(a) = &clip.content else {
            return;
        };
        let content = self.content_rect(rect);
        let warped = a.warp.is_some();
        if !(e.show_transients || e.warp || warped) {
            return;
        }
        let (markers, transients) = self.warp_points(model, clip);
        if e.show_transients || e.warp {
            for (_, x) in &transients {
                if *x >= rect.x && *x <= rect.right() {
                    let alpha = if e.warp { 0.5 } else { 0.38 };
                    p.vline(
                        *x,
                        content.y + content.h * 0.5,
                        content.bottom(),
                        Color::rgba(1.0, 1.0, 1.0, alpha),
                    );
                }
            }
        }
        let accent = Color::hex(0xffb347);
        for (_, x) in &markers {
            if *x < rect.x || *x > rect.right() {
                continue;
            }
            if e.warp {
                p.vline(*x, content.y, content.bottom(), accent.with_alpha(0.85));
                let mut tri = Path::new();
                tri.move_to(Point::new(x - 4.0, content.bottom()))
                    .line_to(Point::new(x + 4.0, content.bottom()))
                    .line_to(Point::new(*x, content.bottom() - 6.0))
                    .close();
                p.fill_path(&tri, accent);
            } else {
                p.vline(
                    *x,
                    content.bottom() - 5.0,
                    content.bottom(),
                    accent.with_alpha(0.7),
                );
            }
        }
        if warped && rect.w > 70.0 {
            let label = match a.warp.as_ref().map(|w| w.algorithm) {
                Some(WarpAlgorithm::Varispeed) => "VARI",
                Some(WarpAlgorithm::Rhythmic) => "WARP·R",
                _ => "WARP",
            };
            let r = Rect::new(
                rect.x.max(content.x) + 4.0,
                content.bottom() - 13.0,
                44.0,
                11.0,
            );
            p.text(
                label,
                r,
                &TextStyle::new(self.theme.fonts.tiny, accent).bold(),
            );
        }
    }

    /// The edit selection, the Shuffle ghost and tool previews.
    pub(crate) fn paint_edit_overlay(
        &self,
        p: &mut dyn Painter,
        lanes: Rect,
        size: Size,
        tracks: &[&Track],
        model: &Session,
    ) {
        if let Some(r) = model.selection.range.filter(|r| !r.is_empty()) {
            let (x0, x1) = (
                self.x_of(r.start).max(lanes.x),
                self.x_of(r.end).min(lanes.right()),
            );
            if x1 > x0 {
                for (i, t) in tracks.iter().enumerate() {
                    if model.selection.tracks.contains(&t.id) {
                        let row = self.row_rect(i, size);
                        p.fill(
                            Rect::new(x0, row.y, x1 - x0, row.h),
                            Color::rgba(0.55, 0.75, 1.0, 0.2),
                        );
                    }
                }
                for x in [self.x_of(r.start), self.x_of(r.end)] {
                    p.vline(x, lanes.y, lanes.bottom(), Color::rgba(0.6, 0.8, 1.0, 0.55));
                }
            }
        }
        match &self.edit_drag {
            Some(EditDrag::Move {
                anchor,
                ghost: Some((track, at)),
                ..
            }) => {
                let p_ = model.project();
                if let (Some(c), Some(i)) =
                    (p_.clip(*anchor), tracks.iter().position(|t| t.id == *track))
                {
                    let len = c.end(&p_.timeline, p_.sample_rate) - c.start;
                    let row = self.row_rect(i, size);
                    let (x0, x1) = (self.x_of(*at), self.x_of(*at + len));
                    let r = Rect::new(
                        x0,
                        row.y + 3.0,
                        (x1 - x0).max(2.0),
                        self.base_h(model, *track) - 6.0,
                    );
                    p.stroke_rounded(r, 3.0, 1.5, self.theme.ui.text.with_alpha(0.8));
                    p.fill(r, self.theme.ui.text.with_alpha(0.1));
                }
            }
            Some(EditDrag::Zoom { from, to, .. }) => {
                let (a, b) = (from.min(*to), from.max(*to));
                p.fill(
                    Rect::new(a, lanes.y, b - a, lanes.h),
                    self.theme.ui.selection.with_alpha(0.18),
                );
            }
            Some(EditDrag::Pencil { track, from, to }) => {
                if let Some(i) = tracks.iter().position(|t| t.id == *track) {
                    let row = self.row_rect(i, size);
                    let (x0, x1) = (self.x_of(*from), self.x_of(*to).max(self.x_of(*from) + 4.0));
                    p.stroke_rounded(
                        Rect::new(x0, row.y + 3.0, x1 - x0, self.base_h(model, *track) - 6.0),
                        3.0,
                        1.5,
                        self.theme.ui.text.with_alpha(0.7),
                    );
                }
            }
            _ => {}
        }
    }

    /// Clip menu entries for editing (gain, fades, warp), for the clicked
    /// clip and the rest of the selection.
    pub(crate) fn clip_edit_menu(
        model: &Session,
        c: &Clip,
        at: Point,
        items: &mut Vec<MenuItem<Action>>,
    ) {
        let targets = Self::targets(model, c.id);
        let audio: Vec<ClipId> = targets
            .iter()
            .copied()
            .filter(|id| {
                model
                    .project()
                    .clip(*id)
                    .is_some_and(|c| matches!(c.content, ClipContent::Audio(_)))
            })
            .collect();
        if gain_of(c).is_some() {
            items.push(
                MenuItem::new(
                    "Clip Gain +1 dB",
                    Action::ClipGain {
                        clips: targets.clone(),
                        delta_db: 1.0,
                    },
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Clip Gain −1 dB",
                Action::ClipGain {
                    clips: targets.clone(),
                    delta_db: -1.0,
                },
            ));
            items.push(MenuItem::new(
                "Reset Clip Gain",
                Action::SetClipsGain {
                    clips: targets.clone(),
                    db: 0.0,
                },
            ));
            let rate = model.project().sample_rate as i64;
            for (label, edge) in [
                ("Fade In: ", ClipEdge::Start),
                ("Fade Out: ", ClipEdge::End),
            ] {
                let current = fades_of(c).map(|(f, _)| match edge {
                    ClipEdge::Start => (f.fade_in, f.fade_in_shape),
                    ClipEdge::End => (f.fade_out, f.fade_out_shape),
                });
                for (i, s) in FadeShape::ALL.into_iter().enumerate() {
                    let item = MenuItem::new(
                        format!("{label}{}", s.label()),
                        Action::SetFade {
                            clips: targets.clone(),
                            edge,
                            length: current
                                .filter(|(l, _)| *l > 0)
                                .map_or(Some(rate / 20), |_| None),
                            shape: Some(s),
                            bend: None,
                        },
                    )
                    .checked(current.is_some_and(|(l, cur)| l > 0 && cur == s));
                    items.push(if i == 0 { item.separated() } else { item });
                }
            }
        }
        items.push(
            MenuItem::new("Quantize (Q)", Action::QuantizeClips(targets.clone())).separated(),
        );
        items.push(MenuItem::new(
            "Humanize",
            Action::HumanizeClips(targets.clone()),
        ));
        if !audio.is_empty() {
            items.push(
                MenuItem::new(
                    "Separate at Transients",
                    Action::SeparateAtTransients(audio.clone()),
                )
                .separated(),
            );
            let current = match &c.content {
                ClipContent::Audio(a) => a.warp.as_ref().map(|w| w.algorithm),
                _ => None,
            };
            for alg in WarpAlgorithm::ALL {
                items.push(
                    MenuItem::new(
                        format!("Warp: {}", alg.label()),
                        Action::SetWarpAlgorithm {
                            clips: audio.clone(),
                            algorithm: alg,
                        },
                    )
                    .checked(current == Some(alg)),
                );
            }
            if current.is_some() {
                items.push(MenuItem::new("Remove Warp", Action::ClearWarp(audio)));
            }
        }
        let _ = at;
    }
}
