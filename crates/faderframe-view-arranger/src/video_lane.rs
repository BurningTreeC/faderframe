//! The Video lane under the ruler: a row per video track (its name in the
//! header column), each clip a filmstrip (tiles from the clip's start, so
//! they stay put while scrolling), the file's name on it. A click goes
//! there; a drag moves the clip (snapped to whole frames of the project's
//! timecode; Shift: free), up or down onto another track; its edges trim
//! it; a double-click shows the video window. The rows' menu adds, renames,
//! hides and removes video tracks.

use super::*;
use faderframe_core::{VideoClipId, VideoTrackId};
use faderframe_session::video::VideoOp;
use faderframe_ui_canvas::Pixels;

/// Thumbnail height asked for (one size for every zoom, so they cache).
const THUMB_H: u32 = 64;

/// A row of the lane.
pub(crate) const ROW_H: f32 = 40.0;

/// Grab width of a clip's edges.
const EDGE: f32 = 6.0;

/// Where in the Video lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoHit {
    /// Between clips, on a track's row (none when there are no tracks).
    Empty(Option<VideoTrackId>),
    Clip(VideoClipId, VideoPart),
    /// A row's name in the header column.
    Track(Option<VideoTrackId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPart {
    Body,
    Start,
    End,
}

#[derive(Clone, Debug)]
pub(crate) struct VideoDrag {
    pub clip: VideoClipId,
    pub part: VideoPart,
    /// The clip as pressed and the sample under the pointer then.
    pub start: i64,
    pub offset: i64,
    pub length: i64,
    pub grab: i64,
    pub origin: Point,
    pub moved: bool,
}

impl ArrangerView {
    /// Rows the lane has (one per video track; one without any).
    pub(crate) fn video_rows(model: &Session) -> usize {
        model.project().video.tracks.len().max(1)
    }

    /// The track on the row at `y` in the lane `r`.
    fn video_row_track(&self, y: f32, r: Rect, model: &Session) -> Option<VideoTrackId> {
        let row = ((y - r.y) / ROW_H).floor().max(0.0) as usize;
        model.project().video.tracks.get(row).map(|t| t.id)
    }

    /// Clip rectangles in the lane `r`, a row per track.
    fn video_rects(&self, r: Rect, model: &Session) -> Vec<(VideoClipId, Rect)> {
        let v = &model.project().video;
        let rate = model.project().sample_rate;
        let mut out = Vec::new();
        for (row, t) in v.tracks.iter().enumerate() {
            let y = r.y + row as f32 * ROW_H;
            for c in &t.clips {
                let x0 = self.x_of_sample(model, c.start);
                let x1 = self.x_of_sample(model, c.end(rate));
                if x1 < r.x || x0 > r.right() {
                    continue;
                }
                out.push((
                    c.id,
                    Rect::new(x0, y + 2.0, (x1 - x0).max(2.0), ROW_H - 4.0),
                ));
            }
        }
        out
    }

    pub(crate) fn video_hit(&self, pos: Point, r: Rect, model: &Session) -> VideoHit {
        let hit = self
            .video_rects(r, model)
            .into_iter()
            .rev()
            .find(|(_, cr)| cr.inset_xy(-EDGE / 2.0, 0.0).contains(pos));
        match hit {
            Some((id, cr)) => {
                let part = if cr.w > 3.0 * EDGE && pos.x <= cr.x + EDGE {
                    VideoPart::Start
                } else if cr.w > 3.0 * EDGE && pos.x >= cr.right() - EDGE {
                    VideoPart::End
                } else {
                    VideoPart::Body
                };
                VideoHit::Clip(id, part)
            }
            None => VideoHit::Empty(self.video_row_track(pos.y, r, model)),
        }
    }

    /// A row's name in the header column at `pos` (the lane's label `r`).
    pub(crate) fn video_label_hit(&self, pos: Point, r: Rect, model: &Session) -> VideoHit {
        VideoHit::Track(self.video_row_track(pos.y, r, model))
    }

    /// The rows' names in the header column (`r`: the lane's label area).
    pub(crate) fn paint_video_labels(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let tracks = &model.project().video.tracks;
        if tracks.is_empty() {
            controls::engraved(p, "Video", r.inset_xy(10.0, 0.0), th, Align::Start);
            return;
        }
        for (row, t) in tracks.iter().enumerate() {
            let rr = Rect::new(r.x, r.y + row as f32 * ROW_H, r.w, ROW_H);
            let colour = if t.hidden {
                th.ui.text_faint
            } else {
                th.ui.text_dim
            };
            let name = if t.hidden {
                format!("{} (hidden)", t.name)
            } else {
                t.name.clone()
            };
            p.text(
                &name,
                rr.inset_xy(10.0, 0.0),
                &TextStyle::new(10.0, colour).align(Align::Start),
            );
            if row > 0 {
                p.hline(rr.x, rr.right(), rr.y, th.arranger.header_border);
            }
        }
    }

    pub(crate) fn paint_video(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let project = model.project();
        let rate = project.sample_rate;
        for row in 1..project.video.tracks.len() {
            p.hline(
                r.x,
                r.right(),
                r.y + row as f32 * ROW_H,
                th.arranger.header_border,
            );
        }
        for (id, cr) in self.video_rects(r, model) {
            let Some((track, c)) = project.video.clip(id) else {
                continue;
            };
            let Some(src) = project.video.sources.get(&c.source) else {
                continue;
            };
            p.fill_rounded(cr, 3.0, &Paint::Solid(Color::rgb(0.05, 0.05, 0.06)));
            p.push_clip(cr);
            let th_h = (cr.h - 2.0).max(4.0);
            let aspect = src.width as f32 * src.par.0.max(1) as f32
                / (src.par.1.max(1) as f32 * src.height.max(1) as f32);
            let tw = (th_h * aspect).max(8.0);
            // Tiles from the clip's start, the first one in view onwards.
            let first = ((r.x - cr.x) / tw).floor().max(0.0);
            let mut tx = cr.x + first * tw;
            while tx < cr.right().min(r.right()) {
                let pos = self.sample_at_x(model, tx + tw / 2.0);
                if let Some(ft) = c.file_time(pos.clamp(c.start, c.end(rate) - 1), rate)
                    && let Some(f) = model.video_thumbnail(c.source, ft, THUMB_H)
                {
                    let key = (c.source.raw() << 40) ^ (f.time as u64).rotate_left(7) ^ 0x7768;
                    p.pixels(
                        &Pixels {
                            key,
                            width: f.width,
                            height: f.height,
                            rgba: &f.rgba,
                        },
                        Rect::new(tx, cr.y + 1.0, tw - 1.0, th_h),
                    );
                }
                tx += tw;
            }
            if track.hidden {
                p.fill(cr, th.arranger.ruler_bg.with_alpha(0.6));
            }
            let name = src.name();
            let label = Rect::new(
                cr.x + 3.0,
                cr.y + 2.0,
                (name.chars().count() as f32 * 5.8 + 10.0).min(cr.w - 6.0),
                14.0,
            );
            if label.w > 12.0 {
                p.fill_rounded(label, 2.0, &Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.6)));
                p.text(
                    &name,
                    label.inset_xy(4.0, 0.0),
                    &TextStyle::new(9.5, Color::rgb(1.0, 1.0, 1.0)),
                );
            }
            p.pop_clip();
            let drag = matches!(&self.video_drag, Some(d) if d.clip == id);
            p.stroke_rounded(cr, 3.0, 1.0, if drag { th.ui.accent } else { th.ui.border });
        }
    }

    pub(crate) fn video_press(
        &mut self,
        hit: VideoHit,
        pos: Point,
        clicks: u32,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let (id, part) = match hit {
            VideoHit::Clip(id, part) => (id, part),
            VideoHit::Empty(_) => {
                cx.emit(Action::Transport(TransportAction::Locate(
                    self.time_at(pos.x).max(MusicalTime::ZERO),
                )));
                return;
            }
            VideoHit::Track(_) => return,
        };
        if clicks >= 2 {
            cx.emit(Action::Workspace(faderframe_session::WorkspaceAction::ShowView(
                faderframe_workspace::ViewId::video(),
            )));
            return;
        }
        let Some((_, c)) = model.project().video.clip(id) else {
            return;
        };
        self.video_drag = Some(VideoDrag {
            clip: id,
            part,
            start: c.start,
            offset: c.offset,
            length: c.length,
            grab: self.sample_at_x(model, pos.x),
            origin: pos,
            moved: false,
        });
    }

    /// While dragging a clip (false: not dragging one).
    pub(crate) fn video_drag_move(
        &mut self,
        pos: Point,
        mods: Modifiers,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(mut d) = self.video_drag.take() else {
            return false;
        };
        if !d.moved && pos.distance(d.origin) >= DRAG_THRESHOLD {
            d.moved = true;
            cx.emit(Action::BeginGesture(
                match d.part {
                    VideoPart::Body => "Move Video Clip",
                    _ => "Trim Video Clip",
                }
                .into(),
            ));
        }
        if d.moved {
            let rate = model.project().sample_rate;
            // Whole frames of the project's timecode (Shift: free).
            let snap = |s: i64| {
                if mods.shift {
                    return s;
                }
                let fr = model.timecode().rate;
                let frame = fr.frame_at(s as f64 / rate as f64);
                (fr.seconds_of(frame) * rate as f64).round() as i64
            };
            let delta = self.sample_at_x(model, pos.x) - d.grab;
            let ns = |samples: i64| faderframe_project::video::samples_to_ns(samples, rate);
            let duration = model
                .project()
                .video
                .clip(d.clip)
                .and_then(|(_, c)| model.project().video.sources.get(&c.source))
                .map_or(i64::MAX, |s| s.duration);
            match d.part {
                VideoPart::Body => {
                    let track = self
                        .lane_rect(faderframe_session::lanes::GlobalLane::Video, size)
                        .and_then(|r| self.video_row_track(pos.y, r, model));
                    cx.emit(Action::Video(VideoOp::MoveClip {
                        clip: d.clip,
                        start: snap(d.start + delta).max(-(rate as i64) * 3600),
                        track,
                    }));
                    cx.set_cursor(Cursor::Grabbing);
                }
                VideoPart::Start => {
                    // The start moves, the end stays: the in-point follows.
                    let start = snap(d.start + delta);
                    let moved = ns(start - d.start);
                    let offset = (d.offset + moved).clamp(0, duration - 1);
                    let shift = offset - d.offset;
                    cx.emit(Action::Video(VideoOp::TrimClip {
                        clip: d.clip,
                        start: d.start + faderframe_project::video::ns_to_samples(shift, rate),
                        offset,
                        length: (d.length - shift).max(1),
                    }));
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
                VideoPart::End => {
                    let end = snap(
                        d.start + faderframe_project::video::ns_to_samples(d.length, rate) + delta,
                    );
                    let length = ns(end - d.start).clamp(1, duration - d.offset);
                    cx.emit(Action::Video(VideoOp::TrimClip {
                        clip: d.clip,
                        start: d.start,
                        offset: d.offset,
                        length,
                    }));
                    cx.set_cursor(Cursor::ResizeHorizontal);
                }
            }
        }
        self.video_drag = Some(d);
        true
    }

    pub(crate) fn video_release(&mut self, model: &Session, cx: &mut EventCx<'_, Action>) -> bool {
        let Some(d) = self.video_drag.take() else {
            return false;
        };
        if d.moved {
            cx.emit(Action::EndGesture);
        } else {
            cx.emit(Action::Transport(TransportAction::Locate(
                model
                    .engine()
                    .samples_to_musical(model.project(), d.grab.max(0)),
            )));
        }
        cx.redraw();
        true
    }

    pub(crate) fn video_menu(
        &self,
        hit: VideoHit,
        model: &Session,
        pos: Point,
    ) -> HostRequest<Action> {
        let p = model.project();
        let mut items = vec![MenuItem::new(
            "Import Video…",
            Action::Video(VideoOp::ChooseImport),
        )];
        let track = match hit {
            VideoHit::Clip(id, _) => {
                let has_tc = p
                    .video
                    .clip(id)
                    .and_then(|(_, c)| p.video.sources.get(&c.source))
                    .is_some_and(|s| s.timecode.is_some());
                if has_tc {
                    items.push(MenuItem::new(
                        "Spot to Its Timecode",
                        Action::Video(VideoOp::SpotToTimecode(id)),
                    ));
                }
                items.push(MenuItem::new(
                    "Detect Cuts (Markers)",
                    Action::Video(VideoOp::DetectCuts(id)),
                ));
                // The sound follows this picture from another one (the
                // cut found by matching their frames).
                let others: Vec<MenuItem<Action>> = p
                    .video
                    .tracks
                    .iter()
                    .flat_map(|t| &t.clips)
                    .filter(|c| c.id != id)
                    .map(|c| {
                        let name = p
                            .video
                            .sources
                            .get(&c.source)
                            .map_or_else(|| "Video".into(), |s| s.name());
                        MenuItem::new(
                            format!("from {name}"),
                            Action::Video(VideoOp::ConformPicture { old: c.id, new: id }),
                        )
                    })
                    .collect();
                if !others.is_empty() {
                    items.push(MenuItem::submenu("Conform Sound to This Picture", others));
                }
                items.push(MenuItem::new(
                    "Export Movie…",
                    Action::Video(VideoOp::ChooseExport),
                ));
                // Onto another track.
                let (here, start) = p
                    .video
                    .clip(id)
                    .map_or((None, 0), |(t, c)| (Some(t.id), c.start));
                let others: Vec<MenuItem<Action>> = p
                    .video
                    .tracks
                    .iter()
                    .filter(|t| Some(t.id) != here)
                    .map(|t| {
                        MenuItem::new(
                            t.name.clone(),
                            Action::Video(VideoOp::MoveClip {
                                clip: id,
                                start,
                                track: Some(t.id),
                            }),
                        )
                    })
                    .collect();
                if !others.is_empty() {
                    items.push(MenuItem::submenu("Move to Track", others));
                }
                items.push(
                    MenuItem::new("Remove Video Clip", Action::Video(VideoOp::RemoveClip(id)))
                        .separated(),
                );
                here
            }
            VideoHit::Empty(t) | VideoHit::Track(t) => t,
        };
        if let Some(t) = track.and_then(|id| p.video.tracks.iter().find(|t| t.id == id)) {
            items.push(
                MenuItem::new(
                    if t.hidden {
                        format!("Show “{}”", t.name)
                    } else {
                        format!("Hide “{}”", t.name)
                    },
                    Action::Video(VideoOp::ToggleTrack(t.id)),
                )
                .separated(),
            );
            if p.video.tracks.len() > 1 || t.clips.is_empty() {
                items.push(MenuItem::new(
                    format!("Remove “{}”", t.name),
                    Action::Video(VideoOp::RemoveTrack(t.id)),
                ));
            }
        }
        items.push(MenuItem::new(
            "Add Video Track",
            Action::Video(VideoOp::AddTrack),
        ));
        items.push(
            MenuItem::new(
                "Show Video",
                Action::Workspace(faderframe_session::WorkspaceAction::ShowView(
                    faderframe_workspace::ViewId::video(),
                )),
            )
            .separated(),
        );
        HostRequest::ContextMenu { at: pos, items }
    }

    /// Renaming a row's track (double-click on its name).
    pub(crate) fn video_rename(
        &self,
        hit: VideoHit,
        r: Rect,
        model: &Session,
    ) -> Option<HostRequest<Action>> {
        let VideoHit::Track(Some(id)) = hit else {
            return None;
        };
        let p = model.project();
        let row = p.video.tracks.iter().position(|t| t.id == id)?;
        let name = p.video.tracks[row].name.clone();
        Some(HostRequest::TextInput {
            at: Rect::new(r.x + 4.0, r.y + row as f32 * ROW_H + 10.0, r.w - 8.0, 20.0),
            initial: name,
            commit: Box::new(move |text| {
                let t = text.trim();
                (!t.is_empty()).then(|| {
                    Action::Video(VideoOp::RenameTrack {
                        track: id,
                        name: t.to_string(),
                    })
                })
            }),
        })
    }

    pub(crate) fn video_tooltip(&self, hit: VideoHit, model: &Session) -> String {
        let p = model.project();
        let clip = match hit {
            VideoHit::Clip(id, VideoPart::Body) => id,
            VideoHit::Clip(..) => {
                return "Drag to trim (whole frames; Shift: free)".into();
            }
            VideoHit::Track(Some(_)) => {
                return "Double-click to rename · Right-click: hide, remove, add a track".into();
            }
            _ => return "Import a video (File → Import Video…) to see it here".into(),
        };
        let Some((_, c)) = p.video.clip(clip) else {
            return String::new();
        };
        let Some(s) = p.video.sources.get(&c.source) else {
            return String::new();
        };
        let state = model.video_source_state(c.source);
        let tc = model.timecode();
        let at = tc.label(c.start, p.sample_rate);
        let proxy = if state.proxy {
            " · proxy ready".to_string()
        } else if let Some(sh) = state.proxying {
            format!(" · making a proxy {:.0} %", sh * 100.0)
        } else {
            String::new()
        };
        format!(
            "{} · {} {}×{} · starts at {at}{proxy} · Drag to move (Shift: off the frame grid), up or down to another track · Edges trim · Double-click: video window",
            s.name(),
            s.codec,
            s.width,
            s.height
        )
    }
}

/// Whether the Video lane shows (the project has video tracks).
pub(crate) fn has_video(model: &Session) -> bool {
    !model.project().video.tracks.is_empty()
}
