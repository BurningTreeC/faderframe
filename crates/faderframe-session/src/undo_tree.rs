//! The undo history as a tree, as the session presents and moves through
//! it ([`faderframe_project::History`] keeps it): an edit made after undoing
//! starts a branch beside the steps undone, so nothing undone is lost.
//!
//! * [`Session::history_tree`] lists every step kept as rows (each branch
//!   after the step it starts from, one level in) for the History view.
//! * [`Action::HistoryToNode`] goes to any step — undo to the common
//!   ancestor, redo down the other branch — and syncs the engine once, as
//!   [`Action::HistoryTo`] does for a place on the current line;
//!   [`Action::HistoryBranch`] goes to the newest step of a branch.
//! * Any step can be auditioned beside the mix as it is
//!   ([`Action::AuditionNode`], [`crate::compare`]) and two of them played
//!   against each other blind to find the edit that made the mix worse
//!   ([`crate::bisect`]).
//!
//! [`Action::HistoryToNode`]: crate::Action::HistoryToNode
//! [`Action::HistoryTo`]: crate::Action::HistoryTo
//! [`Action::HistoryBranch`]: crate::Action::HistoryBranch
//! [`Action::AuditionNode`]: crate::Action::AuditionNode

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_project::{History, Impact};
use std::time::{Duration, SystemTime};

pub use faderframe_project::{HistoryStep, TreeRow};

/// The label the history's start shows: the project as opened, or the
/// oldest state kept once older steps were forgotten.
pub fn start_label(forgot_older: bool) -> &'static str {
    if forgot_older {
        "Older steps forgotten"
    } else {
        "Project opened"
    }
}

/// How long ago `made` was, in words ("just now", "5 min ago", "2 h ago",
/// "3 days ago").
pub fn ago(made: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(made).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        0..=9 => "just now".into(),
        10..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => {
            let days = secs / 86_400;
            format!("{days} day{} ago", if days == 1 { "" } else { "s" })
        }
    }
}

impl Session {
    /// Every step of the history as rows of a tree (the start first; each
    /// branch after the step it starts from, one level in, before its own
    /// line goes on). The start's label is filled in.
    pub fn history_tree(&self) -> Vec<TreeRow> {
        let start = start_label(self.history.forgot_older());
        let mut rows = self.history.tree();
        for r in &mut rows {
            if r.step.id == History::ROOT {
                r.step.label = start.into();
            }
        }
        rows
    }

    /// The step the project is in now ([`History::ROOT`]: the start).
    pub fn history_current(&self) -> u64 {
        self.history.current()
    }

    /// One step as the views show it (the start's label filled in).
    pub fn history_step(&self, id: u64) -> Option<HistoryStep> {
        let mut s = self.history.step(id)?;
        if id == History::ROOT {
            s.label = start_label(self.history.forgot_older()).into();
        }
        Some(s)
    }

    /// A step's label ("Project opened" for the start).
    pub(crate) fn history_step_label(&self, id: u64) -> String {
        self.history_step(id)
            .map_or_else(|| format!("step {id}"), |s| s.label)
    }

    /// Go to step `id` of the history (any branch): undo to the common
    /// ancestor, redo down its branch, the engine synced once.
    pub(crate) fn history_to_node(&mut self, id: u64) -> Result<()> {
        if id != History::ROOT && !self.history.knows(id) {
            return Err(SessionError::Other(format!("the history has no step {id}")));
        }
        let moved = match self.history.go_to(&mut self.project, id) {
            Ok(m) => m,
            Err(e) => {
                // Part of the way may have been gone: everything again.
                self.sync(Impact::Graph)?;
                self.revision += 1;
                return Err(e.into());
            }
        };
        if moved.steps > 0 {
            self.sync(moved.impact)?;
            let label = if id == History::ROOT {
                "the start".to_string()
            } else {
                format!("‘{}’", self.history_step_label(id))
            };
            self.notify(NoticeLevel::Info, format!("history: at {label}"));
        }
        self.revision += 1;
        Ok(())
    }

    /// Go to the newest step of branch `n` (1: the first line of steps).
    pub(crate) fn history_branch(&mut self, n: u32) -> Result<()> {
        let tip = self.history.branch_tip(n).ok_or_else(|| {
            SessionError::Other(format!(
                "the history has no branch {n} (there are {})",
                self.history.branch_count()
            ))
        })?;
        self.history_to_node(tip)
    }

    /// The branch a step belongs to (for the views' "Branch n").
    pub fn history_branch_of(&self, id: u64) -> Option<u32> {
        self.history.step(id).map(|s| s.branch)
    }
}
