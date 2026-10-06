//! The undo history: every step of the project's history in order — the
//! project as opened first, then each step done, then (dimmed) the steps
//! that can be redone. The current step is marked; a click on any row
//! undoes or redoes to just after it (one action, the engine synced once).

#![forbid(unsafe_code)]

use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, FontFamily, Painter, Point, PointerButton, Rect, ScrollAxis, ScrollInfo,
    Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 22.0;

pub struct HistoryView {
    theme: Theme,
    scroll: f32,
    hover: Option<usize>,
    /// The steps done when last painted (to keep the current one in view).
    seen: Option<(usize, usize)>,
}

impl HistoryView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            scroll: 0.0,
            hover: None,
            seen: None,
        }
    }

    /// The rows: the start, the steps done, the steps to redo; and how many
    /// are done.
    fn rows(model: &Session) -> (Vec<String>, usize) {
        let (done, redo) = model.history_steps();
        let n = done.len();
        let rows = std::iter::once("Project opened".to_string())
            .chain(done)
            .chain(redo)
            .collect();
        (rows, n)
    }

    fn list(&self, size: Size) -> Rect {
        Rect::new(0.0, HEADER_H, size.w, (size.h - HEADER_H).max(0.0))
    }

    fn row_rect(&self, i: usize, size: Size) -> Rect {
        let list = self.list(size);
        Rect::new(
            list.x,
            list.y + i as f32 * ROW_H - self.scroll,
            list.w,
            ROW_H,
        )
    }

    /// The row under `pos`.
    pub fn row_at(&self, pos: Point, size: Size, model: &Session) -> Option<usize> {
        let list = self.list(size);
        if !list.contains(pos) {
            return None;
        }
        let i = ((pos.y - list.y + self.scroll) / ROW_H).floor();
        let (rows, _) = Self::rows(model);
        (i >= 0.0 && (i as usize) < rows.len()).then_some(i as usize)
    }

    fn clamp(&mut self, count: usize, size: Size) {
        let max = (count as f32 * ROW_H - self.list(size).h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }
}

impl CanvasView<Session, Action> for HistoryView {
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        let th = theme;
        p.fill(Rect::from_size(size), th.ui.background);
        let (rows, done) = Self::rows(model);
        // New steps: keep the current one in view.
        if self.seen != Some((done, rows.len())) {
            self.seen = Some((done, rows.len()));
            let list = self.list(size);
            let top = done as f32 * ROW_H;
            if top < self.scroll {
                self.scroll = top;
            } else if top + ROW_H > self.scroll + list.h {
                self.scroll = top + ROW_H - list.h;
            }
        }
        self.clamp(rows.len(), size);
        let header = Rect::new(0.0, 0.0, size.w, HEADER_H);
        p.fill(header, th.ui.surface);
        p.hline(0.0, size.w, HEADER_H - 0.5, th.ui.border);
        let redo = rows.len() - 1 - done;
        let summary = match (done, redo) {
            (0, 0) => "Nothing done yet".to_string(),
            (d, 0) => format!("{d} step{} · click one to go back to it", plural(d)),
            (d, r) => format!(
                "{d} step{} done · {r} to redo · click one to go to it",
                plural(d)
            ),
        };
        p.text(
            "History",
            Rect::new(12.0, 0.0, 80.0, HEADER_H),
            &TextStyle::new(th.fonts.normal, th.ui.text).bold(),
        );
        p.text(
            &summary,
            Rect::new(92.0, 0.0, size.w - 104.0, HEADER_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        let list = self.list(size);
        p.push_clip(list);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((self.scroll + list.h) / ROW_H).ceil() as usize).min(rows.len());
        for (i, label) in rows.iter().enumerate().take(last).skip(first) {
            let r = self.row_rect(i, size);
            let current = i == done;
            if current {
                p.fill(r, th.ui.selection.with_alpha(0.35));
                p.fill(Rect::new(r.x, r.y, 3.0, r.h), th.ui.accent);
            } else if self.hover == Some(i) {
                p.fill(r, th.ui.text.with_alpha(0.06));
            }
            let future = i > done;
            let color = if future { th.ui.text_faint } else { th.ui.text };
            p.text(
                &format!("{i}"),
                Rect::new(r.x + 8.0, r.y, 32.0, r.h),
                &TextStyle::new(
                    th.fonts.small,
                    if current {
                        th.ui.text_dim
                    } else {
                        th.ui.text_faint
                    },
                )
                .family(FontFamily::Mono)
                .right(),
            );
            let style = TextStyle::new(th.fonts.small, color);
            p.text(
                label,
                Rect::new(r.x + 50.0, r.y, r.w - 60.0, r.h),
                &if current { style.bold() } else { style },
            );
        }
        p.pop_clip();
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match *ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                ..
            } => {
                let Some(i) = self.row_at(pos, size, model) else {
                    return false;
                };
                let (_, done) = Self::rows(model);
                if i != done {
                    cx.emit(Action::HistoryTo(i));
                }
                true
            }
            ViewEvent::PointerMove { pos, .. } => {
                let hover = self.row_at(pos, size, model);
                if hover != self.hover {
                    self.hover = hover;
                    cx.redraw();
                }
                false
            }
            ViewEvent::PointerLeave => {
                if self.hover.take().is_some() {
                    cx.redraw();
                }
                false
            }
            ViewEvent::Scroll { dy, precise, .. } => {
                self.scroll += if precise { dy } else { dy * ROW_H * 3.0 };
                let (rows, _) = Self::rows(model);
                self.clamp(rows.len(), size);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        let i = self.row_at(pos, size, model)?;
        let (rows, done) = Self::rows(model);
        Some(match i {
            _ if i == done => "Where the project is now".into(),
            0 => "Click to undo everything".into(),
            _ if i < done => format!("Click to undo back to just after ‘{}’", rows[i]),
            _ => format!("Click to redo up to ‘{}’", rows[i]),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(220.0, 120.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let (rows, _) = Self::rows(model);
        let list = self.list(size);
        Some(ScrollInfo {
            content: rows.len() as f32 * ROW_H,
            viewport: list.h,
            offset: self.scroll,
            start: list.y,
            end: 0.0,
        })
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f32) {
        if axis == ScrollAxis::Vertical {
            self.scroll = offset.max(0.0);
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::Command;
    use faderframe_ui_canvas::{Modifiers, RecordingPainter};

    #[test]
    fn the_history_lists_the_steps_and_a_click_goes_to_one() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let bass = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Bass")
            .unwrap()
            .id;
        for (i, db) in [-4.0, -5.0, -6.0].into_iter().enumerate() {
            s.dispatch(Action::Edit(Command::SetTrackVolume { track: bass, db }))
                .unwrap();
            s.dispatch(Action::Edit(Command::RenameTrack {
                track: bass,
                name: format!("Bass {i}"),
            }))
            .unwrap();
        }
        s.dispatch(Action::Undo).unwrap();
        let mut view = HistoryView::new(Theme::default());
        let size = Size::new(400.0, 600.0);
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        let texts = p.texts();
        assert!(texts.contains(&"Project opened"));
        assert!(texts.contains(&"5 steps done · 1 to redo · click one to go to it"));
        // A click on the second step: the project as just after it.
        let row = view.row_rect(2, size);
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(
            &ViewEvent::PointerDown {
                pos: row.center(),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &s,
            &mut cx,
        );
        assert_eq!(actions, [Action::HistoryTo(2)]);
        s.dispatch(actions.remove(0)).unwrap();
        let t = s.project().track(bass).unwrap();
        assert_eq!((t.volume_db, t.name.as_str()), (-4.0, "Bass 0"));
        assert_eq!(s.history_steps().0.len(), 2);
        assert_eq!(s.history_steps().1.len(), 4);
        // Forward again to the last step.
        s.dispatch(Action::HistoryTo(6)).unwrap();
        assert_eq!(s.project().track(bass).unwrap().name, "Bass 2");
        // And all the way back.
        s.dispatch(Action::HistoryTo(0)).unwrap();
        assert_eq!(s.project().track(bass).unwrap().name, "Bass");
    }
}
