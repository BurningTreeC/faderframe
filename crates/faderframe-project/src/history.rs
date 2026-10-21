use crate::Project;
use crate::edit::{CoalesceKey, Command, EditError, Impact};

/// One undo step: the inverse commands of everything done inside it.
#[derive(Clone, Debug)]
struct Transaction {
    label: String,
    /// Inverses in application order (undo applies them in reverse).
    inverses: Vec<Command>,
    keys: Vec<CoalesceKey>,
    /// Where [`History::rewind`] goes back to: how many inverses and keys
    /// there were at [`History::mark`].
    mark: Option<(usize, usize)>,
}

impl Transaction {
    fn new(label: String) -> Self {
        Self {
            label,
            inverses: Vec::new(),
            keys: Vec::new(),
            mark: None,
        }
    }

    fn record(&mut self, inverse: Command, key: Option<CoalesceKey>) {
        if let Some(k) = key {
            // Idempotent setters: the first inverse already restores the
            // pre-transaction value; later ones would be redundant. After
            // a mark only those since count: a rewind must undo what was
            // set after it, whatever was set before.
            let since = self.mark.map_or(0, |m| m.1);
            if self.keys[since..].contains(&k) {
                return;
            }
            self.keys.push(k);
        }
        self.inverses.push(inverse);
    }
}

/// Undo/redo history built on command inverses.
///
/// Edits made between [`History::begin`] and [`History::end`] form one undo
/// step ("gesture"), e.g. a whole fader drag or a multi-clip move. Repeated
/// set-commands on the same target inside one gesture are coalesced.
#[derive(Debug)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    open: Option<Transaction>,
    depth: u32,
    limit: usize,
    /// Number of committed changes since creation/last save marker.
    revision: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new(500)
    }
}

/// What an undo/redo did.
#[derive(Clone, Debug, PartialEq)]
pub struct Replayed {
    pub label: String,
    pub impact: Impact,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            depth: 0,
            limit: limit.max(1),
            revision: 0,
        }
    }

    /// Start a gesture. Nested begins are flattened into the outermost.
    pub fn begin(&mut self, label: impl Into<String>) {
        if self.depth == 0 {
            self.open = Some(Transaction::new(label.into()));
        }
        self.depth += 1;
    }

    /// End a gesture; commits it if it recorded anything.
    pub fn end(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0
            && let Some(tx) = self.open.take()
            && !tx.inverses.is_empty()
        {
            self.push(tx);
        }
    }

    pub fn in_gesture(&self) -> bool {
        self.depth > 0
    }

    /// Mark the open gesture where it is now, for [`History::rewind`]; a
    /// gesture keeps its first mark until it ends. False (nothing marked)
    /// outside a gesture.
    pub fn mark(&mut self) -> bool {
        match &mut self.open {
            Some(tx) if self.depth > 0 => {
                if tx.mark.is_none() {
                    tx.mark = Some((tx.inverses.len(), tx.keys.len()));
                }
                true
            }
            _ => false,
        }
    }

    /// Undo what the open gesture did since its mark; the gesture stays
    /// open and marked. A drag whose every motion is computed afresh from
    /// how things were at the mark — one that splits clips or cuts curves
    /// on the way, which no later motion could take back — rewinds before
    /// each motion. Returns how much the engine must update.
    pub fn rewind(&mut self, project: &mut Project) -> Result<Impact, EditError> {
        let Some(tx) = self.open.as_mut().filter(|_| self.depth > 0) else {
            return Ok(Impact::None);
        };
        let Some((inverses, keys)) = tx.mark else {
            return Ok(Impact::None);
        };
        let undo = tx.inverses.split_off(inverses.min(tx.inverses.len()));
        tx.keys.truncate(keys);
        let impact = undo
            .iter()
            .map(Command::impact)
            .max()
            .unwrap_or(Impact::None);
        for inv in undo.into_iter().rev() {
            inv.apply(project)?;
        }
        Ok(impact)
    }

    /// Drop the open gesture, undoing what it did (nothing is recorded).
    pub fn cancel(&mut self, project: &mut Project) -> Result<Impact, EditError> {
        if self.depth == 0 {
            return Ok(Impact::None);
        }
        self.depth = 0;
        let Some(tx) = self.open.take() else {
            return Ok(Impact::None);
        };
        let impact = Self::impact_of(&tx);
        Self::replay(project, tx)?;
        Ok(impact)
    }

    fn push(&mut self, tx: Transaction) {
        self.undo.push(tx);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
    }

    /// Apply `cmd` to `project` and record its inverse.
    pub fn apply(&mut self, project: &mut Project, cmd: Command) -> Result<Impact, EditError> {
        let label = cmd.label();
        let key = cmd.coalesce_key();
        let impact = cmd.impact();
        let inverse = cmd.apply(project)?;
        match &mut self.open {
            Some(tx) => {
                tx.record(inverse, key);
                self.redo.clear();
            }
            None => {
                let mut tx = Transaction::new(label);
                tx.record(inverse, key);
                self.push(tx);
            }
        }
        Ok(impact)
    }

    /// Apply `cmd` as part of the last undo step while that is still the
    /// one `token` names (returned by the call before: nothing was done,
    /// undone or redone since, no gesture is open); else as a step of its
    /// own named `label`. Returns the impact and the token for next time
    /// (work written as it goes — a recording — stays one step).
    pub fn apply_amending(
        &mut self,
        project: &mut Project,
        cmd: Command,
        label: &str,
        token: Option<u64>,
    ) -> Result<(Impact, u64), EditError> {
        let ours = token == Some(self.revision)
            && self.open.is_none()
            && self.undo.last().is_some_and(|t| t.label == label);
        if !ours {
            let cmd = Command::Batch {
                label: label.to_string(),
                commands: vec![cmd],
            };
            let impact = self.apply(project, cmd)?;
            return Ok((impact, self.revision));
        }
        let impact = cmd.impact();
        let inverse = cmd.apply(project)?;
        if let Some(tx) = self.undo.last_mut() {
            tx.inverses.push(inverse);
        }
        self.redo.clear();
        self.revision += 1;
        Ok((impact, self.revision))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.open.as_ref().is_some_and(|t| !t.inverses.is_empty())
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    /// The steps that can be undone, oldest first.
    pub fn undo_labels(&self) -> Vec<&str> {
        self.undo.iter().map(|t| t.label.as_str()).collect()
    }

    /// The steps that can be redone, the next first.
    pub fn redo_labels(&self) -> Vec<&str> {
        self.redo.iter().rev().map(|t| t.label.as_str()).collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn replay(project: &mut Project, tx: Transaction) -> Result<Transaction, EditError> {
        // Stored inverses are replayed last-to-first. Applying them yields
        // the opposite commands in that same last-to-first order, which is
        // exactly the storage order a later replay (which again iterates in
        // reverse) needs to re-apply them first-to-last.
        let mut opposite = Transaction::new(tx.label);
        for inv in tx.inverses.into_iter().rev() {
            opposite.inverses.push(inv.apply(project)?);
        }
        Ok(opposite)
    }

    fn impact_of(tx: &Transaction) -> Impact {
        tx.inverses
            .iter()
            .map(Command::impact)
            .max()
            .unwrap_or(Impact::None)
    }

    pub fn undo(&mut self, project: &mut Project) -> Result<Option<Replayed>, EditError> {
        // Undo closes an unfinished gesture first.
        if self.depth > 0 {
            self.depth = 1;
            self.end();
        }
        let Some(tx) = self.undo.pop() else {
            return Ok(None);
        };
        let impact = Self::impact_of(&tx);
        let label = tx.label.clone();
        let redo = Self::replay(project, tx)?;
        self.redo.push(redo);
        self.revision += 1;
        Ok(Some(Replayed { label, impact }))
    }

    pub fn redo(&mut self, project: &mut Project) -> Result<Option<Replayed>, EditError> {
        let Some(tx) = self.redo.pop() else {
            return Ok(None);
        };
        let impact = Self::impact_of(&tx);
        let label = tx.label.clone();
        let undo = Self::replay(project, tx)?;
        self.undo.push(undo);
        self.revision += 1;
        Ok(Some(Replayed { label, impact }))
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = None;
        self.depth = 0;
    }
}

#[cfg(test)]
mod amend_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::Project;

    #[test]
    fn amending_stays_one_step_until_something_else_happens() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        let tempo = |bpm| Command::SetTempo { bpm };
        let (_, t) = h.apply_amending(&mut p, tempo(100.0), "Rec", None).unwrap();
        let (_, t) = h
            .apply_amending(&mut p, tempo(110.0), "Rec", Some(t))
            .unwrap();
        assert_eq!(h.undo_labels(), vec!["Rec"]);
        // Another edit in between: a new step.
        h.apply(&mut p, tempo(90.0)).unwrap();
        h.apply_amending(&mut p, tempo(120.0), "Rec", Some(t))
            .unwrap();
        assert_eq!(h.undo_labels().len(), 3);
        // One undo takes back everything the first step amended.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(
            p.timeline
                .tempo
                .bpm_at(faderframe_timeline::MusicalTime::ZERO),
            120.0
        );
    }
}

#[cfg(test)]
mod rewind_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::Project;
    use faderframe_timeline::MusicalTime;

    fn bpm(p: &Project) -> f64 {
        p.timeline.tempo.bpm_at(MusicalTime::ZERO)
    }

    /// A rewind undoes what followed the mark — a setter done before it
    /// too — and the gesture stays one undo step.
    #[test]
    fn a_rewind_goes_back_to_the_mark_and_the_gesture_stays_one_step() {
        let mut p = Project::new("A", 48_000);
        let before = bpm(&p);
        let mut h = History::default();
        assert!(!h.mark(), "no gesture, no mark");
        h.begin("Drag");
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        assert!(h.mark());
        for v in [110.0, 120.0, 130.0] {
            h.rewind(&mut p).unwrap();
            assert_eq!(bpm(&p), 100.0, "back at the mark");
            h.apply(&mut p, Command::SetTempo { bpm: v }).unwrap();
            h.apply(
                &mut p,
                Command::RenameProject {
                    name: format!("{v}"),
                },
            )
            .unwrap();
        }
        // A second mark keeps the first.
        assert!(h.mark());
        h.rewind(&mut p).unwrap();
        assert_eq!(bpm(&p), 100.0);
        assert_eq!(p.name, "A");
        h.apply(&mut p, Command::SetTempo { bpm: 140.0 }).unwrap();
        h.end();
        assert_eq!(h.undo_labels(), vec!["Drag"]);
        h.undo(&mut p).unwrap();
        assert_eq!(bpm(&p), before, "one undo takes the whole gesture back");
        h.redo(&mut p).unwrap();
        assert_eq!(bpm(&p), 140.0);
    }
}
