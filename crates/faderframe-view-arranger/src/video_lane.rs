//! The Video lane under the ruler: each video clip as a filmstrip (tiles
//! from the clip's start, so they stay put while scrolling; the top video
//! track drawn over the ones below), the file's name on it. A click goes
//! there, a drag moves the clip (snapped to whole frames of the project's
//! timecode; Shift: free) as one undo step, a double-click shows the video
//! window.

use super::*;
use faderframe_core::VideoClipId;
use faderframe_session::video::VideoOp;
use faderframe_ui_canvas::Pixels;

/// Thumbnail height asked for (one size for every zoom, so they cache).
const THUMB_H: u32 = 64;

#[derive(Clone, Debug)]
pub(crate) struct VideoDrag {
    pub clip: VideoClipId,
    /// The clip's start and the sample under the pointer when pressed.
    pub start: i64,
    pub grab: i64,
    pub origin: Point,
    pub moved: bool,
}

impl ArrangerView {
    /// Clip rectangles in the lane `r`, bottom track first.
    fn video_rects(&self, r: Rect, model: &Session) -> Vec<(VideoClipId, Rect)> {
        let v = &model.project().video;
        let rate = model.project().sample_rate;
        let mut out = Vec::new();
        for t in v.tracks.iter().rev().filter(|t| !t.hidden) {
            for c in &t.clips {
                let x0 = self.x_of_sample(model, c.start);
                let x1 = self.x_of_sample(model, c.end(rate));
                if x1 < r.x || x0 > r.right() {
                    continue;
                }
                out.push((
                    c.id,
                    Rect::new(x0, r.y + 2.0, (x1 - x0).max(2.0), r.h - 4.0),
                ));
            }
        }
        out
    }

    pub(crate) fn video_hit(&self, pos: Point, r: Rect, model: &Session) -> Option<VideoClipId> {
        self.video_rects(r, model)
            .into_iter()
            .rev()
            .find(|(_, cr)| cr.contains(pos))
            .map(|(id, _)| id)
    }

    pub(crate) fn paint_video(&self, p: &mut dyn Painter, r: Rect, model: &Session) {
        let th = &self.theme;
        let project = model.project();
        let rate = project.sample_rate;
        for (id, cr) in self.video_rects(r, model) {
            let Some((_, c)) = project.video.clip(id) else {
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
        clip: Option<VideoClipId>,
        pos: Point,
        clicks: u32,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(id) = clip else {
            cx.emit(Action::Transport(TransportAction::Locate(
                self.time_at(pos.x).max(MusicalTime::ZERO),
            )));
            return;
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
            start: c.start,
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
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(mut d) = self.video_drag.take() else {
            return false;
        };
        if !d.moved && pos.distance(d.origin) >= DRAG_THRESHOLD {
            d.moved = true;
            cx.emit(Action::BeginGesture("Move Video Clip".into()));
        }
        if d.moved {
            let rate = model.project().sample_rate;
            let mut start = d.start + self.sample_at_x(model, pos.x) - d.grab;
            if !mods.shift {
                // Whole frames of the project's timecode.
                let fr = model.timecode().rate;
                let frame = fr.frame_at(start as f64 / rate as f64);
                start = (fr.seconds_of(frame) * rate as f64).round() as i64;
            }
            cx.emit(Action::Video(VideoOp::MoveClip {
                clip: d.clip,
                start: start.max(-(rate as i64) * 3600),
            }));
            cx.set_cursor(Cursor::Grabbing);
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
        clip: Option<VideoClipId>,
        model: &Session,
        pos: Point,
    ) -> HostRequest<Action> {
        let mut items = vec![MenuItem::new(
            "Import Video…",
            Action::Video(VideoOp::ChooseImport),
        )];
        if let Some(id) = clip {
            let p = model.project();
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
                "Export Movie…",
                Action::Video(VideoOp::ChooseExport),
            ));
            items.push(
                MenuItem::new("Remove Video Clip", Action::Video(VideoOp::RemoveClip(id)))
                    .separated(),
            );
        }
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

    pub(crate) fn video_tooltip(&self, clip: Option<VideoClipId>, model: &Session) -> String {
        let p = model.project();
        let Some((_, c)) = clip.and_then(|id| p.video.clip(id)) else {
            return "Import a video (File → Import Video…) to see it here".into();
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
            "{} · {} {}×{} · starts at {at}{proxy} · Drag to move (Shift: off the frame grid) · Double-click: video window",
            s.name(),
            s.codec,
            s.width,
            s.height
        )
    }
}

/// Whether the Video lane shows (the project has video clips).
pub(crate) fn has_video(model: &Session) -> bool {
    model
        .project()
        .video
        .tracks
        .iter()
        .any(|t| !t.clips.is_empty())
}
