//! The journal of a project: every command applied to it, as it was
//! applied, with its inverse.
//!
//! Live collaboration (`faderframe-session`'s `collab`) sends what changes
//! a project to the other sessions sharing it. Edits reach the project by
//! many ways — the undo history, undo and redo, auditions applied and
//! taken back, a merge replacing it whole — but every one of them is a
//! [`Command`] applied by [`Command::apply`], which writes it here while
//! the journal is on. Commands inside a [`Command::Batch`] are written as
//! the batch they are in, once.
//!
//! The journal belongs to the project it is in, not to copies of it: a
//! clone (a render's copy, a state of the undo history made to listen to)
//! starts with the journal off, so what is done to copies is never sent.
//! A command that replaces the whole project ([`Command::ReplaceProject`])
//! keeps the journal where it was. Files never hold it.

use crate::Command;
use std::fmt;

/// A command as it was applied, and the command that takes it back.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub forward: Command,
    pub inverse: Command,
}

/// See the [module](self) documentation.
#[derive(Default)]
pub struct Journal {
    /// `None` while off.
    applied: Option<Vec<Applied>>,
}

impl Journal {
    /// Start writing what is applied (what was written before is kept).
    pub fn start(&mut self) {
        self.applied.get_or_insert_with(Vec::new);
    }

    /// Stop writing; what was written goes.
    pub fn stop(&mut self) {
        self.applied = None;
    }

    pub fn is_on(&self) -> bool {
        self.applied.is_some()
    }

    /// What was applied since the last call (oldest first); the journal
    /// stays on.
    pub fn take(&mut self) -> Vec<Applied> {
        self.applied
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Whether anything waits to be taken.
    pub fn is_empty(&self) -> bool {
        self.applied.as_ref().is_none_or(Vec::is_empty)
    }

    pub(crate) fn record(&mut self, forward: Command, inverse: Command) {
        if let Some(a) = &mut self.applied {
            a.push(Applied { forward, inverse });
        }
    }
}

/// Copies of a project do not journal.
impl Clone for Journal {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// The journal is no part of what a project is.
impl PartialEq for Journal {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.applied {
            Some(a) => write!(f, "Journal({} waiting)", a.len()),
            None => f.write_str("Journal(off)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Command, History, Project};
    use faderframe_core::TrackId;

    fn master(p: &Project) -> TrackId {
        p.master_id().expect("a master")
    }

    #[test]
    fn applied_commands_are_written_once_with_their_inverses() {
        let mut p = Project::new("j", 48_000);
        let m = master(&p);
        p.journal.start();
        let mut h = History::default();
        h.apply(
            &mut p,
            Command::Batch {
                label: "two".into(),
                commands: vec![
                    Command::SetTrackVolume { track: m, db: -3.0 },
                    Command::SetTrackPan { track: m, pan: 0.5 },
                ],
            },
        )
        .expect("applies");
        h.undo(&mut p).expect("undoes");
        let a = p.journal.take();
        assert_eq!(a.len(), 2, "the batch once, its undo once");
        assert!(matches!(&a[0].forward, Command::Batch { commands, .. } if commands.len() == 2));
        assert_eq!(a[1].forward, a[0].inverse);
        assert!(p.journal.is_empty());
        assert!(p.journal.is_on());
    }

    #[test]
    fn copies_do_not_journal_and_a_replaced_project_keeps_it() {
        let mut p = Project::new("j", 48_000);
        let m = master(&p);
        p.journal.start();
        let mut copy = p.clone();
        assert!(!copy.journal.is_on());
        Command::SetTrackVolume { track: m, db: -1.0 }
            .apply(&mut copy)
            .expect("applies");
        assert!(p.journal.is_empty());
        let other = Project::new("other", 48_000);
        Command::ReplaceProject {
            project: Box::new(other),
            label: "Merge".into(),
        }
        .apply(&mut p)
        .expect("replaces");
        assert!(p.journal.is_on());
        assert_eq!(p.name, "other");
        assert_eq!(p.journal.take().len(), 1);
        assert_eq!(copy, {
            let mut c = copy.clone();
            c.journal.start();
            c
        });
    }
}
