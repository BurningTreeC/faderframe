//! Others in a live session (see `faderframe_session::collab`): their
//! selected tracks edged, their selected clips outlined, their playheads
//! as thin lines named at the top — each in the person's colour, under
//! the own playhead.

use super::{ArrangerView, color_of};
use faderframe_project::Track;
use faderframe_session::Session;
use faderframe_ui_canvas::{Painter, Rect, Size, TextStyle};

impl ArrangerView {
    pub(crate) fn paint_collaborators(
        &self,
        p: &mut dyn Painter,
        lanes: Rect,
        size: Size,
        tracks: &[&Track],
        model: &Session,
    ) {
        let others = model.collab_presences();
        if others.is_empty() {
            return;
        }
        let project = model.project();
        for (n, o) in others.iter().enumerate() {
            let c = color_of(o.colour);
            for (i, t) in tracks.iter().enumerate() {
                let row = self.row_rect(i, size);
                if row.bottom() < lanes.y || row.y > lanes.bottom() {
                    continue;
                }
                if o.presence.tracks.contains(&t.id) {
                    p.fill(Rect::new(lanes.x, row.y, 3.0, row.h), c.with_alpha(0.8));
                }
                for id in &t.clips {
                    if !o.presence.clips.contains(id) {
                        continue;
                    }
                    if let Some(clip) = project.clip(*id) {
                        let r = self.clip_rect(clip, row, model);
                        p.stroke_rounded(r.inset(-1.5), 4.0, 1.5, c.with_alpha(0.9));
                    }
                }
            }
            if let Some((a, b)) = o.presence.range {
                let (x0, x1) = (self.x_of(a), self.x_of(b));
                if x1 > x0 {
                    p.fill(Rect::new(x0, lanes.y, x1 - x0, 3.0), c.with_alpha(0.55));
                }
            }
            let x = self.x_of(o.presence.playhead);
            if x < lanes.x || x > lanes.right() {
                continue;
            }
            p.fill(
                Rect::new(x - 0.5, lanes.y, 1.0, lanes.h),
                c.with_alpha(0.85),
            );
            // The name on a tab at the top (staggered when several meet),
            // inked as clip names are on their colour.
            let ink = if c.luminance() > 0.42 {
                self.theme.arranger.clip_text
            } else {
                self.theme.ui.text
            };
            let style = TextStyle::new(self.theme.fonts.tiny, ink).bold();
            let w = p.text_width(&o.name, &style) + 8.0;
            let tab = Rect::new(x, lanes.y + 2.0 + n as f32 * 14.0, w, 13.0);
            p.fill_rounded(
                tab,
                3.0,
                &faderframe_ui_canvas::Paint::Solid(c.with_alpha(0.9)),
            );
            p.text(&o.name, tab, &style.center());
        }
    }
}
