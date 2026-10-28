//! The undo history: every step of the project's history in order — the
//! project as opened first, then each step done, then (dimmed) the steps
//! that can be redone. The current step is marked; a click on any row
//! undoes or redoes to just after it (one action, the engine synced once).
//!
//! **Auditioning**: a row's "A/B" button (or its menu) renders the mix as
//! it was there beside the mix as it is, without undoing anything
//! (`Action::AuditionHistory`); the bar under the header then flips
//! between **Now** and **Then** (also Ctrl+Alt+A), matches their loudness,
//! says what is heard and how far the render is, and ends the comparison.

#![forbid(unsafe_code)]

use faderframe_session::compare::{CompareOp, Comparing};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, EventCx, FontFamily, HostRequest, MenuItem, Paint, Painter, Point, PointerButton,
    Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 22.0;
/// The audition's bar (under the header while one exists).
const COMPARE_H: f32 = 30.0;

/// The audition bar's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareHit {
    Now,
    Then,
    Match,
    Close,
}

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

    /// An audition of the history exists (its bar is shown).
    fn auditioning(model: &Session) -> bool {
        model.audition_info().is_some()
            && model
                .compare_status()
                .is_some_and(|c| matches!(c.what, Comparing::History { .. }))
    }

    fn top(model: &Session) -> f32 {
        if Self::auditioning(model) {
            HEADER_H + COMPARE_H
        } else {
            HEADER_H
        }
    }

    fn list(&self, size: Size, model: &Session) -> Rect {
        let top = Self::top(model);
        Rect::new(0.0, top, size.w, (size.h - top).max(0.0))
    }

    pub fn row_rect(&self, i: usize, size: Size, model: &Session) -> Rect {
        let list = self.list(size, model);
        Rect::new(
            list.x,
            list.y + i as f32 * ROW_H - self.scroll,
            list.w,
            ROW_H,
        )
    }

    /// A row's "A/B" button (audition it).
    pub fn audition_button(row: Rect) -> Rect {
        Rect::new(row.right() - 50.0, row.y + 2.0, 42.0, row.h - 4.0)
    }

    /// The audition bar's controls.
    pub fn compare_buttons(size: Size) -> [(CompareHit, Rect); 4] {
        let (y, h) = (HEADER_H + 4.0, COMPARE_H - 8.0);
        [
            (CompareHit::Now, Rect::new(10.0, y, 54.0, h)),
            (CompareHit::Then, Rect::new(64.0, y, 54.0, h)),
            (CompareHit::Match, Rect::new(126.0, y, 120.0, h)),
            (CompareHit::Close, Rect::new(size.w - 34.0, y, 26.0, h)),
        ]
    }

    fn compare_hit(pos: Point, size: Size, model: &Session) -> Option<CompareHit> {
        if !Self::auditioning(model) {
            return None;
        }
        Self::compare_buttons(size)
            .into_iter()
            .find(|(_, r)| r.contains(pos))
            .map(|(h, _)| h)
    }

    fn paint_compare(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let (Some(info), Some(status)) = (model.audition_info(), model.compare_status()) else {
            return;
        };
        let bar = Rect::new(0.0, HEADER_H, size.w, COMPARE_H);
        p.fill(bar, th.ui.surface_alt);
        p.hline(0.0, size.w, bar.bottom() - 0.5, th.ui.border);
        let ready = status.ready;
        for (hit, r) in Self::compare_buttons(size) {
            let (label, lit) = match hit {
                CompareHit::Now => ("Now", !status.hearing_other),
                CompareHit::Then => ("Then", status.hearing_other),
                CompareHit::Match => (
                    if model.audition_matches_loudness() {
                        "☑ Match Loudness"
                    } else {
                        "☐ Match Loudness"
                    },
                    false,
                ),
                CompareHit::Close => ("✕", false),
            };
            let enabled = ready || matches!(hit, CompareHit::Close | CompareHit::Now);
            p.fill_rounded(
                r,
                4.0,
                &Paint::Solid(if lit {
                    th.ui.accent.with_alpha(0.55)
                } else {
                    th.ui.surface
                }),
            );
            p.stroke_rounded(r, 4.0, 1.0, th.ui.border);
            p.text(
                label,
                r,
                &TextStyle::new(
                    th.fonts.small,
                    if enabled {
                        th.ui.text
                    } else {
                        th.ui.text_faint
                    },
                )
                .center(),
            );
        }
        let text = match (info.progress, &info.error) {
            (_, Some(e)) => format!("Could not render: {e}"),
            (Some(f), None) => format!("Rendering the mix as it was… {:.0} %", f * 100.0),
            (None, None) => {
                let levels = match (info.then_lufs, info.now_lufs) {
                    (Some(t), Some(n)) if t.is_finite() && n.is_finite() => {
                        format!(" · Then {t:.1} / Now {n:.1} LUFS")
                    }
                    _ => String::new(),
                };
                format!("{}{levels}", status.text)
            }
        };
        let x = 256.0;
        p.text(
            &text,
            Rect::new(x, bar.y, (size.w - x - 44.0).max(0.0), bar.h),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
    }

    fn row_menu(i: usize, done: usize, label: &str) -> Vec<MenuItem<Action>> {
        let mut items = Vec::new();
        if i != done {
            items.push(MenuItem::new(
                format!("Go Back to Just After ‘{label}’"),
                Action::HistoryTo(i),
            ));
            items.push(MenuItem::new(
                "Audition: Compare with Now (A/B)",
                Action::AuditionHistory(i),
            ));
        } else {
            items.push(MenuItem::disabled("This is the project now"));
        }
        items
    }

    /// The row under `pos`.
    pub fn row_at(&self, pos: Point, size: Size, model: &Session) -> Option<usize> {
        let list = self.list(size, model);
        if !list.contains(pos) {
            return None;
        }
        let i = ((pos.y - list.y + self.scroll) / ROW_H).floor();
        let (rows, _) = Self::rows(model);
        (i >= 0.0 && (i as usize) < rows.len()).then_some(i as usize)
    }

    fn clamp(&mut self, count: usize, size: Size, model: &Session) {
        let max = (count as f32 * ROW_H - self.list(size, model).h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }
}

impl CanvasView<Session, Action> for HistoryView {
    /// The steps as a list (the current one selected); Enter goes back or
    /// forward to a step.
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let (rows, done) = Self::rows(model);
        let items = rows.iter().enumerate().map(|(i, label)| {
            let state = if i > done { ", undone" } else { "" };
            AccessNode::new(
                access_id(&[31, i as u64]),
                AccessRole::ListItem,
                format!("{label}{state}"),
            )
            .at(self.row_rect(i, size, model))
            .selected(i == done)
            .on_activate(Action::HistoryTo(i))
        });
        vec![
            AccessNode::new(access_id(&[30]), AccessRole::List, "Undo history")
                .at(self.list(size, model))
                .with_children(items),
        ]
        .into_iter()
        .chain(Self::auditioning(model).then(|| {
            let hearing = model.compare_status().is_some_and(|c| c.hearing_other);
            AccessNode::new(access_id(&[32]), AccessRole::ToggleButton, "Hear Then")
                .checked(hearing)
                .on_activate(Action::Compare(CompareOp::Flip))
        }))
        .collect()
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Undo History".into())
    }

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
            let list = self.list(size, model);
            let top = done as f32 * ROW_H;
            if top < self.scroll {
                self.scroll = top;
            } else if top + ROW_H > self.scroll + list.h {
                self.scroll = top + ROW_H - list.h;
            }
        }
        self.clamp(rows.len(), size, model);
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
        self.paint_compare(p, size, model, th);
        let auditioned = model.compare_status().and_then(|c| match c.what {
            Comparing::History { done } => Some(done),
            Comparing::Reference(_) => None,
        });
        let list = self.list(size, model);
        p.push_clip(list);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((self.scroll + list.h) / ROW_H).ceil() as usize).min(rows.len());
        for (i, label) in rows.iter().enumerate().take(last).skip(first) {
            let r = self.row_rect(i, size, model);
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
                Rect::new(r.x + 50.0, r.y, r.w - 110.0, r.h),
                &if current { style.bold() } else { style },
            );
            if auditioned == Some(i) {
                p.text(
                    "◆ Then",
                    Rect::new(r.right() - 104.0, r.y, 50.0, r.h),
                    &TextStyle::new(th.fonts.tiny + 0.5, th.ui.accent).right(),
                );
            }
            if !current && self.hover == Some(i) {
                let b = Self::audition_button(r);
                p.fill_rounded(b, 3.0, &Paint::Solid(th.ui.surface_alt));
                p.stroke_rounded(b, 3.0, 1.0, th.ui.border);
                p.text(
                    "A/B",
                    b,
                    &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text).center(),
                );
            }
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
                if let Some(hit) = Self::compare_hit(pos, size, model) {
                    let ready = model.compare_status().is_some_and(|c| c.ready);
                    match hit {
                        CompareHit::Now => cx.emit(Action::Compare(CompareOp::Hear(false))),
                        CompareHit::Then if ready => {
                            cx.emit(Action::Compare(CompareOp::Hear(true)));
                        }
                        CompareHit::Then => {}
                        CompareHit::Match => cx.emit(Action::Compare(CompareOp::MatchLoudness(
                            !model.audition_matches_loudness(),
                        ))),
                        CompareHit::Close => cx.emit(Action::Compare(CompareOp::Off)),
                    }
                    return true;
                }
                let Some(i) = self.row_at(pos, size, model) else {
                    return false;
                };
                let (_, done) = Self::rows(model);
                if i != done {
                    if Self::audition_button(self.row_rect(i, size, model)).contains(pos) {
                        cx.emit(Action::AuditionHistory(i));
                    } else {
                        cx.emit(Action::HistoryTo(i));
                    }
                }
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let Some(i) = self.row_at(pos, size, model) else {
                    return false;
                };
                let (rows, done) = Self::rows(model);
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::row_menu(i, done, &rows[i]),
                });
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
                self.clamp(rows.len(), size, model);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        if let Some(hit) = Self::compare_hit(pos, size, model) {
            return Some(
                match hit {
                    CompareHit::Now => "Hear the mix as it is (Ctrl+Alt+A flips)",
                    CompareHit::Then => "Hear the mix as it was at that step (Ctrl+Alt+A flips)",
                    CompareHit::Match => "Play Then as loud as Now (EBU R128)",
                    CompareHit::Close => "End the comparison (its renders are deleted)",
                }
                .into(),
            );
        }
        let i = self.row_at(pos, size, model)?;
        let (rows, done) = Self::rows(model);
        if i != done && Self::audition_button(self.row_rect(i, size, model)).contains(pos) {
            return Some(format!(
                "Audition: hear the mix as it was just after ‘{}’ beside the mix as it is \
                 (the loop, the edit selection or the bars round the playhead; nothing is undone)",
                rows[i]
            ));
        }
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
        let list = self.list(size, model);
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
        let row = view.row_rect(2, size, &s);
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

    #[test]
    fn a_step_is_auditioned_beside_the_mix_and_flipped() {
        let mut s = Session::demo(EngineConfig::default()).unwrap();
        let bass = s
            .project()
            .tracks
            .iter()
            .find(|t| t.name == "Bass")
            .unwrap()
            .id;
        // One bar looped: a short audition.
        let meter = s.project().timeline.meter.clone();
        let range = faderframe_project::MusicalRange::new(meter.bar_start(2), meter.bar_start(3));
        s.dispatch(Action::Edit(Command::SetLoop {
            range,
            enabled: true,
        }))
        .unwrap();
        s.dispatch(Action::Edit(Command::SetTrackVolume {
            track: bass,
            db: -20.0,
        }))
        .unwrap();
        let mut view = HistoryView::new(Theme::default());
        let size = Size::new(500.0, 400.0);
        let (mut actions, mut requests) = (Vec::new(), Vec::new());
        // The row's menu offers it; hovering shows its A/B button.
        let row = view.row_rect(1, size, &s);
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(
            &ViewEvent::PointerDown {
                pos: row.center(),
                button: PointerButton::Secondary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &s,
            &mut cx,
        );
        assert_eq!(requests.len(), 1);
        let button = HistoryView::audition_button(row).center();
        view.event(
            &ViewEvent::PointerMove {
                pos: button,
                modifiers: Modifiers::NONE,
                dragging: false,
            },
            size,
            &s,
            &mut EventCx::new(&mut actions, &mut requests),
        );
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(p.texts().contains(&"A/B"));
        view.event(
            &ViewEvent::PointerDown {
                pos: button,
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            size,
            &s,
            &mut EventCx::new(&mut actions, &mut requests),
        );
        assert_eq!(actions, [Action::AuditionHistory(1)]);
        s.dispatch(actions.remove(0)).unwrap();
        assert!(s.wait_for_compare(std::time::Duration::from_secs(300)));
        // The bar: Now, Then, the match, what is heard.
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        let texts = p.texts();
        for t in ["Now", "Then", "☑ Match Loudness", "◆ Then"] {
            assert!(texts.contains(&t), "{t} in {texts:?}");
        }
        assert!(
            texts.iter().any(|t| t.starts_with("Hearing: Now")),
            "{texts:?}"
        );
        let at = |hit| {
            HistoryView::compare_buttons(size)
                .into_iter()
                .find(|(h, _)| *h == hit)
                .unwrap()
                .1
                .center()
        };
        for (hit, want) in [
            (CompareHit::Then, Action::Compare(CompareOp::Hear(true))),
            (
                CompareHit::Match,
                Action::Compare(CompareOp::MatchLoudness(false)),
            ),
            (CompareHit::Close, Action::Compare(CompareOp::Off)),
        ] {
            let mut got = Vec::new();
            view.event(
                &ViewEvent::PointerDown {
                    pos: at(hit),
                    button: PointerButton::Primary,
                    modifiers: Modifiers::NONE,
                    clicks: 1,
                },
                size,
                &s,
                &mut EventCx::new(&mut got, &mut requests),
            );
            assert_eq!(got, std::slice::from_ref(&want));
            s.dispatch(want).unwrap();
        }
        // Ended: the list as before.
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(!p.texts().contains(&"Then"));
        assert_eq!(s.project().track(bass).unwrap().volume_db, -20.0);
    }
}
