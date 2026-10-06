use crate::Project;
use crate::edit::{CoalesceKey, Command, EditError, Impact};

/// One undo step: the inverse commands of everything done inside it.
#[derive(Clone, Debug)]
struct Transaction {
    label: String,
    /// Inverses in application order (undo applies them in reverse).
    inverses: Vec<Command>,
    keys: Vec<CoalesceKey>,
}

impl Transaction {
    fn new(label: String) -> Self {
        Self {
            label,
            inverses: Vec::new(),
            keys: Vec::new(),
        }
    }

    fn record(&mut self, inverse: Command, key: Option<CoalesceKey>) {
        if let Some(k) = key {
            // Idempotent setters: the first inverse already restores the
            // pre-transaction value; later ones would be redundant.
            if self.keys.contains(&k) {
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
