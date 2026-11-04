//! The undo history as a tree: every step kept — the project as opened
//! first, then each step in the order made. An edit made after undoing
//! starts a branch: its steps follow the step it was made from, one level
//! in, before the line it left goes on; steps not done now (undone, or on
//! another branch) are dimmed, those redo would go through less so. The
//! current step is marked; a click on any row goes there (undo to the
//! common ancestor, redo down its branch: one action, the engine synced
//! once).
//!
//! **Auditioning**: a row's "A/B" button (or its menu) renders the mix as
//! it is in that step beside the mix as it is now, without undoing
//! anything (`Action::AuditionNode`); the bar under the header then flips
//! between **Now** and **Then** (also Ctrl+Alt+A), matches their loudness,
//! says what is heard and how far the render is, and ends the comparison.
//!
//! **Audio Bisect** (a row's menu, or Edit → Audio Bisect): the last step
//! that sounded good is marked (a click while getting ready), the bad one
//! is the project now unless another is marked; the bar then asks round
//! after round which of two states heard blind, A or B, is better — the
//! rows in question tinted, the good and bad ends labelled — until the step
//! that made it worse is found: hear it (before/after), take just it back,
//! or go there.

#![forbid(unsafe_code)]

use faderframe_session::bisect::{BisectAnswer, BisectOp, BisectSide, BisectStage, BisectStatus};
use faderframe_session::compare::{CompareOp, Comparing};
use faderframe_session::undo_tree::TreeRow;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    CanvasView, Color, EventCx, FontFamily, HostRequest, MenuItem, Paint, Painter, Point,
    PointerButton, Rect, ScrollAxis, ScrollInfo, Size, TextStyle, Theme, ViewEvent,
};

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 22.0;
/// The audition's bar (under the header while one exists).
const COMPARE_H: f32 = 30.0;
/// One level of a branch.
const INDENT_W: f32 = 14.0;
/// Audio Bisect's bar: a line of text, then lines of buttons.
const BISECT_TEXT_H: f32 = 28.0;
const BISECT_LINE_H: f32 = 28.0;
const BUTTON_H: f32 = 22.0;

/// The audition bar's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareHit {
    Now,
    Then,
    Match,
    Close,
}

/// Audio Bisect's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectHit {
    Start,
    Cancel,
    HearA,
    HearB,
    Match,
    BetterA,
    BetterB,
    CantTell,
    HearFound,
    TakeBack,
    GoBefore,
    GoAfter,
}

/// A button of Audio Bisect's bar.
#[derive(Clone, Debug, PartialEq)]
pub struct BisectButton {
    pub hit: BisectHit,
    pub rect: Rect,
    pub label: String,
    pub enabled: bool,
    pub lit: bool,
}

pub struct HistoryView {
    theme: Theme,
    scroll: f32,
    hover: Option<usize>,
    /// The current step and the rows when last painted (to keep the
    /// current one in view).
    seen: Option<(u64, usize)>,
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

    /// An audition of the history exists (its bar is shown).
    fn auditioning(model: &Session) -> bool {
        model.audition_info().is_some()
            && model
                .compare_status()
                .is_some_and(|c| matches!(c.what, Comparing::History { .. }))
    }

    /// Where Audio Bisect's bar starts.
    fn bisect_top(model: &Session) -> f32 {
        if Self::auditioning(model) {
            HEADER_H + COMPARE_H
        } else {
            HEADER_H
        }
    }

    /// Audio Bisect's buttons for `status`, laid out left to right in
    /// lines under its text, and the bar's height.
    pub fn bisect_buttons(size: Size, model: &Session) -> (f32, Vec<BisectButton>) {
        let Some(st) = model.bisect_status() else {
            return (0.0, Vec::new());
        };
        let wanted = bisect_wanted(&st);
        let top = Self::bisect_top(model);
        let (mut x, mut y) = (10.0, top + BISECT_TEXT_H);
        let mut out = Vec::with_capacity(wanted.len());
        for (hit, label, enabled, lit) in wanted {
            let w = (14.0 + 6.4 * label.chars().count() as f32).max(36.0);
            if x + w > size.w - 8.0 && x > 10.0 {
                x = 10.0;
                y += BISECT_LINE_H;
            }
            out.push(BisectButton {
                hit,
                rect: Rect::new(x, y + 2.0, w, BUTTON_H),
                label,
                enabled,
                lit,
            });
            x += w + 6.0;
        }
        let height = y + BISECT_LINE_H - top;
        (height, out)
    }

    fn top(&self, size: Size, model: &Session) -> f32 {
        Self::bisect_top(model) + Self::bisect_buttons(size, model).0
    }

    fn list(&self, size: Size, model: &Session) -> Rect {
        let top = self.top(size, model);
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

    fn bisect_hit(pos: Point, size: Size, model: &Session) -> Option<BisectButton> {
        Self::bisect_buttons(size, model)
            .1
            .into_iter()
            .find(|b| b.rect.contains(pos))
    }

    /// What a bar button does (`None`: nothing now).
    pub fn bisect_action(hit: BisectHit, model: &Session) -> Option<Action> {
        let st = model.bisect_status()?;
        let op = match hit {
            BisectHit::Start => BisectOp::Start {
                good: st.good?,
                bad: st.bad,
            },
            BisectHit::Cancel => BisectOp::Cancel,
            BisectHit::HearA => BisectOp::Hear(BisectSide::A),
            BisectHit::HearB => BisectOp::Hear(BisectSide::B),
            BisectHit::Match => BisectOp::MatchLoudness(!st.match_loudness),
            BisectHit::BetterA => BisectOp::Answer(BisectAnswer::A),
            BisectHit::BetterB => BisectOp::Answer(BisectAnswer::B),
            BisectHit::CantTell => BisectOp::Answer(BisectAnswer::CantTell),
            BisectHit::HearFound => BisectOp::AuditionFound,
            BisectHit::TakeBack => BisectOp::UndoFound,
            BisectHit::GoBefore => BisectOp::GoToFound { after: false },
            BisectHit::GoAfter => BisectOp::GoToFound { after: true },
        };
        Some(Action::Bisect(op))
    }

    fn paint_button(
        p: &mut dyn Painter,
        r: Rect,
        label: &str,
        enabled: bool,
        lit: bool,
        th: &Theme,
    ) {
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
            Self::paint_button(p, r, label, enabled, lit, th);
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

    fn paint_bisect(&self, p: &mut dyn Painter, size: Size, model: &Session, th: &Theme) {
        let Some(st) = model.bisect_status() else {
            return;
        };
        let (h, buttons) = Self::bisect_buttons(size, model);
        let bar = Rect::new(0.0, Self::bisect_top(model), size.w, h);
        p.fill(bar, th.ui.surface_alt);
        p.hline(0.0, size.w, bar.bottom() - 0.5, th.ui.border);
        p.text(
            "Audio Bisect",
            Rect::new(10.0, bar.y, 92.0, BISECT_TEXT_H),
            &TextStyle::new(th.fonts.small, th.ui.accent).bold(),
        );
        p.text(
            &st.text,
            Rect::new(104.0, bar.y, (size.w - 112.0).max(0.0), BISECT_TEXT_H),
            &TextStyle::new(th.fonts.small, th.ui.text_dim),
        );
        for b in &buttons {
            Self::paint_button(p, b.rect, &b.label, b.enabled, b.lit, th);
        }
    }

    fn row_menu(model: &Session, row: &TreeRow) -> Vec<MenuItem<Action>> {
        let id = row.step.id;
        let mut items = Vec::new();
        if row.step.current {
            items.push(MenuItem::disabled("This is the project now"));
        } else {
            let go = if row.step.parent.is_none() {
                "Go Back to the Start".to_string()
            } else {
                format!("Go to Just After ‘{}’", row.step.label)
            };
            items.push(MenuItem::new(go, Action::HistoryToNode(id)));
            items.push(MenuItem::new(
                "Audition: Compare with Now (A/B)",
                Action::AuditionNode(id),
            ));
            items.push(
                MenuItem::new(
                    "Audio Bisect: Find the Edit That Made It Worse Since Here",
                    Action::Bisect(BisectOp::Start {
                        good: id,
                        bad: None,
                    }),
                )
                .separated(),
            );
        }
        let marking = model
            .bisect_status()
            .is_some_and(|s| s.stage == BisectStage::Setup);
        let good = MenuItem::new(
            "Audio Bisect: Mark as Good",
            Action::Bisect(BisectOp::Mark {
                node: id,
                good: true,
            }),
        );
        items.push(if row.step.current {
            good.separated()
        } else {
            good
        });
        if marking || !row.step.current {
            items.push(MenuItem::new(
                "Audio Bisect: Mark as Bad",
                Action::Bisect(BisectOp::Mark {
                    node: id,
                    good: false,
                }),
            ));
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
        let n = model.history_tree().len();
        (i >= 0.0 && (i as usize) < n).then_some(i as usize)
    }

    fn clamp(&mut self, count: usize, size: Size, model: &Session) {
        let max = (count as f32 * ROW_H - self.list(size, model).h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// What a click on a row does: go there, or mark it good while Audio
    /// Bisect gets ready.
    fn click(model: &Session, row: &TreeRow) -> Option<Action> {
        if model
            .bisect_status()
            .is_some_and(|s| s.stage == BisectStage::Setup)
        {
            return Some(Action::Bisect(BisectOp::Mark {
                node: row.step.id,
                good: true,
            }));
        }
        (!row.step.current).then_some(Action::HistoryToNode(row.step.id))
    }

    /// A row's mark in Audio Bisect (text and colour; getting ready, the
    /// current step is the bad one unless another is marked).
    fn bisect_badge(
        st: &BisectStatus,
        id: u64,
        current: bool,
        th: &Theme,
    ) -> Option<(&'static str, Color)> {
        let meter = &th.console.meter;
        if let Some(f) = &st.found
            && f.steps.iter().any(|s| s.node == id)
        {
            return Some((if f.exact { "FOUND" } else { "AMONG" }, th.ui.accent));
        }
        if st.good == Some(id) {
            return Some(("GOOD", meter.green));
        }
        let bad_now = st.stage == BisectStage::Setup && st.bad.is_none() && current;
        if st.bad == Some(id) || bad_now {
            return Some(("BAD", meter.red));
        }
        if st.skipped.contains(&id) {
            return Some(("skipped", th.ui.text_faint));
        }
        if st.range.contains(&id) {
            return Some(("?", th.ui.text_dim));
        }
        None
    }
}

/// Audio Bisect's buttons for where it is: (hit, label, enabled, lit).
fn bisect_wanted(st: &BisectStatus) -> Vec<(BisectHit, String, bool, bool)> {
    let s = |x: &str| x.to_string();
    match st.stage {
        BisectStage::Setup => vec![
            (BisectHit::Start, s("Start"), st.good.is_some(), false),
            (BisectHit::Cancel, s("Cancel"), true, false),
        ],
        BisectStage::Comparing => {
            let ready = st.ready;
            vec![
                (
                    BisectHit::HearA,
                    s("A"),
                    ready,
                    st.hearing == Some(BisectSide::A),
                ),
                (
                    BisectHit::HearB,
                    s("B"),
                    ready,
                    st.hearing == Some(BisectSide::B),
                ),
                (
                    BisectHit::Match,
                    s(if st.match_loudness {
                        "☑ Match Loudness"
                    } else {
                        "☐ Match Loudness"
                    }),
                    true,
                    false,
                ),
                (BisectHit::BetterA, s("A Is Better"), ready, false),
                (BisectHit::BetterB, s("B Is Better"), ready, false),
                (BisectHit::CantTell, s("Can’t Tell"), ready, false),
                (BisectHit::Cancel, s("Cancel"), true, false),
            ]
        }
        BisectStage::Found => {
            let mut v = Vec::new();
            if st.blind {
                v.push((
                    BisectHit::HearFound,
                    s("Hear It (Before/After)"),
                    true,
                    false,
                ));
            } else {
                v.push((
                    BisectHit::HearA,
                    s("Before"),
                    st.ready,
                    st.hearing == Some(BisectSide::A),
                ));
                v.push((
                    BisectHit::HearB,
                    s("After"),
                    st.ready,
                    st.hearing == Some(BisectSide::B),
                ));
            }
            match st.found.as_ref().map(|f| &f.take_back) {
                Some(Ok(label)) => v.push((BisectHit::TakeBack, label.clone(), true, false)),
                _ => v.push((BisectHit::TakeBack, s("Undo Just That Step"), false, false)),
            }
            v.push((BisectHit::GoBefore, s("Go Before It"), true, false));
            v.push((BisectHit::GoAfter, s("Go There"), true, false));
            v.push((BisectHit::Cancel, s("Close"), true, false));
            v
        }
    }
}

impl CanvasView<Session, Action> for HistoryView {
    /// The steps as a list (the current one selected); Enter goes to a
    /// step. Audio Bisect's buttons follow.
    fn accessible(
        &self,
        size: Size,
        model: &Session,
    ) -> Vec<faderframe_ui_canvas::AccessNode<Action>> {
        use faderframe_ui_canvas::{AccessNode, AccessRole, access_id};
        let rows = model.history_tree();
        let list = self.list(size, model);
        let items = rows.iter().enumerate().map(|(i, r)| {
            let mut state = String::new();
            if !r.step.done {
                state.push_str(", not done");
            }
            if r.starts_branch {
                state.push_str(&format!(", branch {}", r.step.branch));
            }
            let mut node = AccessNode::new(
                access_id(&[31, r.step.id]),
                AccessRole::ListItem,
                format!("{}{state}", r.step.label),
            )
            .at(Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - self.scroll,
                list.w,
                ROW_H,
            ))
            .selected(r.step.current);
            if let Some(a) = Self::click(model, r) {
                node = node.on_activate(a);
            }
            node
        });
        let mut nodes = vec![
            AccessNode::new(access_id(&[30]), AccessRole::List, "Undo history")
                .at(list)
                .with_children(items),
        ];
        if Self::auditioning(model) {
            let hearing = model.compare_status().is_some_and(|c| c.hearing_other);
            nodes.push(
                AccessNode::new(access_id(&[32]), AccessRole::ToggleButton, "Hear Then")
                    .checked(hearing)
                    .on_activate(Action::Compare(CompareOp::Flip)),
            );
        }
        for b in Self::bisect_buttons(size, model).1 {
            let mut node = AccessNode::new(
                access_id(&[33, b.hit as u64]),
                AccessRole::Button,
                format!("Audio Bisect: {}", b.label),
            )
            .at(b.rect);
            if b.enabled
                && let Some(a) = Self::bisect_action(b.hit, model)
            {
                node = node.on_activate(a);
            }
            nodes.push(node);
        }
        nodes
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
        let rows = model.history_tree();
        let current = rows.iter().position(|r| r.step.current).unwrap_or(0);
        let current_id = rows.get(current).map_or(0, |r| r.step.id);
        // New steps: keep the current one in view.
        if self.seen != Some((current_id, rows.len())) {
            self.seen = Some((current_id, rows.len()));
            let list = self.list(size, model);
            let top = current as f32 * ROW_H;
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
        let steps = rows.len().saturating_sub(1);
        let done = rows
            .iter()
            .filter(|r| r.step.done)
            .count()
            .saturating_sub(1);
        let branches = rows.iter().filter(|r| r.starts_branch).count();
        let summary = match (steps, branches) {
            (0, _) => "Nothing done yet".to_string(),
            (n, 0) if n == done => format!("{n} step{} · click one to go back to it", plural(n)),
            (n, 0) => format!(
                "{done} step{} done · {} to redo · click one to go to it",
                plural(done),
                n - done
            ),
            (n, b) => format!(
                "{n} step{} · {} branch{} · {done} done · click one to go to it",
                plural(n),
                b + 1,
                if b + 1 == 1 { "" } else { "es" }
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
        self.paint_bisect(p, size, model, th);
        let auditioned = model.compare_status().and_then(|c| match c.what {
            Comparing::History { node } => Some(node),
            _ => None,
        });
        let bisect = model.bisect_status();
        let marking = bisect
            .as_ref()
            .is_some_and(|s| s.stage == BisectStage::Setup);
        let list = self.list(size, model);
        p.push_clip(list);
        let first = (self.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (((self.scroll + list.h) / ROW_H).ceil() as usize).min(rows.len());
        for (i, row) in rows.iter().enumerate().take(last).skip(first) {
            let r = Rect::new(
                list.x,
                list.y + i as f32 * ROW_H - self.scroll,
                list.w,
                ROW_H,
            );
            let step = &row.step;
            if bisect
                .as_ref()
                .is_some_and(|s| s.stage != BisectStage::Setup && s.range.contains(&step.id))
            {
                p.fill(r, th.ui.accent.with_alpha(0.07));
            }
            if step.current {
                p.fill(r, th.ui.selection.with_alpha(0.35));
                p.fill(Rect::new(r.x, r.y, 3.0, r.h), th.ui.accent);
            } else if self.hover == Some(i) {
                p.fill(r, th.ui.text.with_alpha(0.06));
            }
            // The branch's guides: a line per level, an elbow where it
            // starts.
            let x0 = r.x + 6.0 + row.indent as f32 * INDENT_W;
            for k in 0..row.indent {
                let gx = r.x + 6.0 + k as f32 * INDENT_W + INDENT_W * 0.5;
                let elbow = row.starts_branch && k + 1 == row.indent;
                let y1 = if elbow { r.center().y } else { r.bottom() };
                p.vline(gx, r.y, y1, th.ui.border);
                if elbow {
                    p.hline(gx, x0, r.center().y, th.ui.border);
                }
            }
            let color = if step.done {
                th.ui.text
            } else if step.to_redo {
                th.ui.text_dim
            } else {
                th.ui.text_faint
            };
            p.text(
                &if step.parent.is_none() {
                    "·".to_string()
                } else {
                    format!("{}", step.id)
                },
                Rect::new(x0, r.y, 30.0, r.h),
                &TextStyle::new(
                    th.fonts.small,
                    if step.current {
                        th.ui.text_dim
                    } else {
                        th.ui.text_faint
                    },
                )
                .family(FontFamily::Mono)
                .right(),
            );
            let style = TextStyle::new(th.fonts.small, color);
            let label_x = x0 + 38.0;
            p.text(
                &step.label,
                Rect::new(label_x, r.y, (r.right() - 120.0 - label_x).max(0.0), r.h),
                &if step.current { style.bold() } else { style },
            );
            let badge = Rect::new(r.right() - 116.0, r.y, 62.0, r.h);
            let mark = bisect
                .as_ref()
                .and_then(|s| Self::bisect_badge(s, step.id, step.current, th))
                .or_else(|| (auditioned == Some(step.id)).then_some(("◆ Then", th.ui.accent)));
            if let Some((text, c)) = mark {
                p.text(text, badge, &TextStyle::new(th.fonts.tiny + 0.5, c).right());
            } else if row.starts_branch {
                p.text(
                    &format!("branch {}", step.branch),
                    badge,
                    &TextStyle::new(th.fonts.tiny + 0.5, th.ui.text_faint).right(),
                );
            }
            if !step.current && !marking && self.hover == Some(i) {
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
                if let Some(b) = Self::bisect_hit(pos, size, model) {
                    if b.enabled
                        && let Some(a) = Self::bisect_action(b.hit, model)
                    {
                        cx.emit(a);
                    }
                    return true;
                }
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
                let rows = model.history_tree();
                let row = &rows[i];
                let marking = model
                    .bisect_status()
                    .is_some_and(|s| s.stage == BisectStage::Setup);
                if !row.step.current
                    && !marking
                    && Self::audition_button(self.row_rect(i, size, model)).contains(pos)
                {
                    cx.emit(Action::AuditionNode(row.step.id));
                } else if let Some(a) = Self::click(model, row) {
                    cx.emit(a);
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
                let rows = model.history_tree();
                cx.request(HostRequest::ContextMenu {
                    at: pos,
                    items: Self::row_menu(model, &rows[i]),
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
                let n = model.history_tree().len();
                self.clamp(n, size, model);
                cx.redraw();
                true
            }
            _ => false,
        }
    }

    fn tooltip(&self, pos: Point, size: Size, model: &Session) -> Option<String> {
        if let Some(b) = Self::bisect_hit(pos, size, model) {
            return Some(
                match b.hit {
                    BisectHit::Start => "Search between the step marked good and the bad one",
                    BisectHit::Cancel => "End Audio Bisect: nothing is changed",
                    BisectHit::HearA | BisectHit::HearB => {
                        "Hear this side (Ctrl+Alt+A flips; which state is which is not told)"
                    }
                    BisectHit::Match => "Play both sides as loud (EBU R128)",
                    BisectHit::BetterA | BisectHit::BetterB => "This side sounds better",
                    BisectHit::CantTell => {
                        "No telling: compared once more with the bad state, then skipped"
                    }
                    BisectHit::HearFound => "Hear the states just before and just after the step",
                    BisectHit::TakeBack => {
                        "Take just that step back, as a new step (the steps after it stay)"
                    }
                    BisectHit::GoBefore => {
                        "Go to the last good state (edits made there branch off)"
                    }
                    BisectHit::GoAfter => "Go to the state just after the step",
                }
                .into(),
            );
        }
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
        let rows = model.history_tree();
        let step = &rows[i].step;
        let when = faderframe_session::undo_tree::ago(step.made, std::time::SystemTime::now());
        if model
            .bisect_status()
            .is_some_and(|s| s.stage == BisectStage::Setup)
        {
            return Some(format!(
                "Click to mark ‘{}’ as the last good state",
                step.label
            ));
        }
        if !step.current && Self::audition_button(self.row_rect(i, size, model)).contains(pos) {
            return Some(format!(
                "Audition: hear the mix as it is just after ‘{}’ beside the mix as it is \
                 (the loop, the edit selection or the bars round the playhead; nothing is undone)",
                step.label
            ));
        }
        Some(match () {
            _ if step.current => format!("Where the project is now ({when})"),
            _ if step.parent.is_none() => "Click to undo everything".into(),
            _ if step.done => format!("Click to undo back to just after ‘{}’", step.label),
            _ if step.to_redo => format!("Click to redo up to ‘{}’ ({when})", step.label),
            _ => format!(
                "Click to go to ‘{}’ on branch {} ({when})",
                step.label, step.branch
            ),
        })
    }

    fn min_size(&self) -> Size {
        Size::new(220.0, 120.0)
    }

    fn scroll_info(&self, axis: ScrollAxis, size: Size, model: &Session) -> Option<ScrollInfo> {
        if axis != ScrollAxis::Vertical {
            return None;
        }
        let n = model.history_tree().len();
        let list = self.list(size, model);
        Some(ScrollInfo {
            content: n as f32 * ROW_H,
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
mod tests;
