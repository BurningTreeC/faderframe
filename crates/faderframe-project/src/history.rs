//! The undo history as a tree.
//!
//! Every step is a node holding the commands that join it to the state
//! before it (its parent). An edit after undoing starts a new branch beside
//! the steps undone instead of dropping them; undo and redo follow the
//! current branch (redo goes to the child last made or visited), and
//! [`History::go_to`] reaches any node: undo to the common ancestor, then
//! redo down the other branch.
//!
//! A node's commands point one way at a time: while it is *done* (on the
//! way from the root to the current node) they take it back, else they do
//! it again — replaying them turns them into the opposite (see
//! [`History::undo`]). The root is the state the history starts from (the
//! project as opened, or the oldest step kept once older ones are
//! forgotten).

use crate::Project;
use crate::edit::{CoalesceKey, Command, EditError, Impact};
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

/// One undo step being recorded: the inverse commands of everything done
/// inside it.
#[derive(Clone, Debug)]
struct Transaction {
    label: String,
    /// Inverses in application order (undo applies them in reverse).
    inverses: Vec<Command>,
    keys: Vec<CoalesceKey>,
    /// Where [`History::rewind`] goes back to: how many inverses and keys
    /// there were at [`History::mark`].
    mark: Option<(usize, usize)>,
    /// Join the last step when ending, while that is still the one this
    /// revision names ([`History::amend_on_end`]).
    amend: Option<u64>,
}

impl Transaction {
    fn new(label: String) -> Self {
        Self {
            label,
            inverses: Vec::new(),
            keys: Vec::new(),
            mark: None,
            amend: None,
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

/// One step of the tree (the root: the state the history starts from).
#[derive(Clone, Debug)]
struct Node {
    label: String,
    parent: Option<u64>,
    /// Steps made from here, oldest first (more than one: branches).
    children: Vec<u64>,
    /// The child redo goes to: the one made or visited last.
    next: Option<u64>,
    /// Done: what takes the step back; else what does it again. In
    /// application order (a replay applies them in reverse).
    commands: Vec<Command>,
    /// On the way from the root to the current node.
    done: bool,
    made: SystemTime,
    /// The branch it belongs to: its parent's when it was the parent's
    /// first child, else a new one (numbered from 1 in the order made).
    branch: u32,
}

/// A step as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryStep {
    /// The step's id ([`History::ROOT`]: the state the history starts
    /// from).
    pub id: u64,
    pub parent: Option<u64>,
    /// Empty for the root.
    pub label: String,
    pub made: SystemTime,
    /// The branch it belongs to (1: the first line of steps).
    pub branch: u32,
    /// On the way from the root to the current state (the root always).
    pub done: bool,
    /// The project is in this state now.
    pub current: bool,
    /// Redo goes through it from the current state.
    pub to_redo: bool,
    /// Steps from the root.
    pub depth: usize,
}

/// A row of the history laid out as a tree: each branch after the step it
/// starts from, one level further in, before the line it left goes on.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub step: HistoryStep,
    pub indent: usize,
    /// The first row of a branch (other than the first line).
    pub starts_branch: bool,
}

/// What a move through the history did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moved {
    /// Steps undone and redone.
    pub steps: usize,
    pub impact: Impact,
}

impl Default for Moved {
    fn default() -> Self {
        Self {
            steps: 0,
            impact: Impact::None,
        }
    }
}

/// Undo/redo history built on command inverses, kept as a tree.
///
/// Edits made between [`History::begin`] and [`History::end`] form one undo
/// step ("gesture"), e.g. a whole fader drag or a multi-clip move. Repeated
/// set-commands on the same target inside one gesture are coalesced.
#[derive(Debug)]
pub struct History {
    nodes: HashMap<u64, Node>,
    current: u64,
    open: Option<Transaction>,
    depth: u32,
    /// Steps kept at most (branches included).
    limit: usize,
    /// Number of committed changes since creation/last save marker.
    revision: u64,
    /// The id the next step gets.
    next_id: u64,
    /// Branches made (the first line is branch 1).
    branches: u32,
    /// Older steps were forgotten: the root is a later state than the
    /// start.
    forgot: bool,
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

/// Apply `commands` last to first (as stored), returning their opposites
/// in the order a later replay needs. A command that fails takes back what
/// was applied before it: the project is as it was, the commands are kept.
fn replay(project: &mut Project, commands: &[Command]) -> Result<Vec<Command>, EditError> {
    // Stored commands are replayed last-to-first. Applying them yields the
    // opposite commands in that same last-to-first order, which is exactly
    // the storage order a later replay (which again iterates in reverse)
    // needs to re-apply them first-to-last.
    let mut opposite = Vec::with_capacity(commands.len());
    for c in commands.iter().rev() {
        match c.clone().apply(project) {
            Ok(o) => opposite.push(o),
            Err(e) => {
                for o in opposite.into_iter().rev() {
                    let _ = o.apply(project);
                }
                return Err(e);
            }
        }
    }
    Ok(opposite)
}

fn impact_of(commands: &[Command]) -> Impact {
    commands
        .iter()
        .map(Command::impact)
        .max()
        .unwrap_or(Impact::None)
}

impl History {
    /// The state the history starts from.
    pub const ROOT: u64 = 0;

    pub fn new(limit: usize) -> Self {
        let mut h = Self {
            nodes: HashMap::new(),
            current: Self::ROOT,
            open: None,
            depth: 0,
            limit: limit.max(1),
            revision: 0,
            next_id: 1,
            branches: 1,
            forgot: false,
        };
        h.plant();
        h
    }

    /// Only the root.
    fn plant(&mut self) {
        self.nodes.clear();
        self.nodes.insert(
            Self::ROOT,
            Node {
                label: String::new(),
                parent: None,
                children: Vec::new(),
                next: None,
                commands: Vec::new(),
                done: true,
                made: SystemTime::now(),
                branch: 1,
            },
        );
        self.current = Self::ROOT;
        self.branches = 1;
        self.forgot = false;
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// The open gesture recorded something.
    fn open_changed(&self) -> bool {
        self.open.as_ref().is_some_and(|t| !t.inverses.is_empty())
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
            match tx.amend {
                Some(token) if self.still_last(token, &tx.label) => {
                    if let Some(n) = self.nodes.get_mut(&self.current) {
                        n.commands.extend(tx.inverses);
                    }
                    self.revision += 1;
                }
                _ => self.push(tx),
            }
        }
    }

    /// When the open gesture ends, its changes join the last step instead
    /// of making one of their own — while that step is still the one
    /// `token` names (the [`Self::revision`] right after it was made:
    /// nothing done, undone or redone since, no step made from it) and
    /// carries the gesture's label. A script that waited for an answer
    /// goes on in the step it began.
    pub fn amend_on_end(&mut self, token: u64) {
        if let Some(tx) = self.open.as_mut() {
            tx.amend = Some(token);
        }
    }

    /// Is the last step still the one made at revision `token`, named
    /// `label` (see [`Self::amend_on_end`])?
    pub fn still_last(&self, token: u64, label: &str) -> bool {
        token == self.revision
            && self.open.as_ref().is_none_or(|tx| tx.inverses.is_empty())
            && self.current != Self::ROOT
            && self
                .node(self.current)
                .is_some_and(|n| n.label == label && n.children.is_empty())
    }

    /// Forget the open gesture's mark ([`Self::mark`]): what comes next is
    /// worked out from the project as it is now, as at a new gesture's
    /// start, while the changes stay in the gesture (a script's edits one
    /// after another in its one step).
    pub fn unmark(&mut self) {
        if let Some(tx) = self.open.as_mut() {
            tx.mark = None;
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
        let impact = impact_of(&undo);
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
        let impact = impact_of(&tx.inverses);
        replay(project, &tx.inverses)?;
        Ok(impact)
    }

    /// A finished step: a new child of the current node, which it becomes
    /// (steps undone before stay, a branch beside it).
    fn push(&mut self, tx: Transaction) {
        let id = self.fresh_id();
        let parent = self.current;
        let Some(p) = self.nodes.get(&parent) else {
            return;
        };
        let branch = if p.children.is_empty() {
            p.branch
        } else {
            self.branches += 1;
            self.branches
        };
        self.nodes.insert(
            id,
            Node {
                label: tx.label,
                parent: Some(parent),
                children: Vec::new(),
                next: None,
                commands: tx.inverses,
                done: true,
                made: SystemTime::now(),
                branch,
            },
        );
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.children.push(id);
            p.next = Some(id);
        }
        self.current = id;
        self.revision += 1;
        self.forget();
    }

    /// Keep at most `limit` steps: the oldest branch tips off the current
    /// line go first; a history that is one line forgets its oldest step
    /// (the root becomes the state after it).
    fn forget(&mut self) {
        while self.nodes.len() > self.limit + 1 {
            let line = self.line();
            let stale = self
                .nodes
                .iter()
                .filter(|(id, n)| {
                    **id != Self::ROOT && n.children.is_empty() && !line.contains(*id)
                })
                .map(|(id, _)| *id)
                .min();
            if let Some(leaf) = stale {
                self.remove_leaf(leaf);
                continue;
            }
            let Some(root) = self.node(Self::ROOT) else {
                break;
            };
            let only = match root.children.as_slice() {
                [c] => *c,
                _ => break,
            };
            if !self.node(only).is_some_and(|n| n.done) {
                break;
            }
            self.merge_into_root(only);
        }
    }

    fn remove_leaf(&mut self, leaf: u64) {
        let Some(node) = self.nodes.remove(&leaf) else {
            return;
        };
        if let Some(p) = node.parent.and_then(|p| self.nodes.get_mut(&p)) {
            p.children.retain(|c| *c != leaf);
            if p.next == Some(leaf) {
                p.next = p.children.last().copied();
            }
        }
    }

    /// The root's only child, done, becomes the root (its commands go).
    fn merge_into_root(&mut self, child: u64) {
        let Some(node) = self.nodes.remove(&child) else {
            return;
        };
        for c in &node.children {
            if let Some(n) = self.nodes.get_mut(c) {
                n.parent = Some(Self::ROOT);
            }
        }
        if let Some(root) = self.nodes.get_mut(&Self::ROOT) {
            root.children = node.children;
            root.next = node.next;
            root.made = node.made;
        }
        if self.current == child {
            self.current = Self::ROOT;
        }
        self.forgot = true;
    }

    /// Apply `cmd` to `project` and record its inverse.
    pub fn apply(&mut self, project: &mut Project, cmd: Command) -> Result<Impact, EditError> {
        let label = cmd.label();
        let key = cmd.coalesce_key();
        let impact = cmd.impact();
        let inverse = cmd.apply(project)?;
        match &mut self.open {
            // Redo is off while the gesture has changes (see `can_redo`);
            // the steps it would redo stay in the tree.
            Some(tx) => tx.record(inverse, key),
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
            && self.current != Self::ROOT
            // Steps made from it would no longer redo onto it.
            && self
                .node(self.current)
                .is_some_and(|n| n.label == label && n.children.is_empty());
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
        if let Some(n) = self.nodes.get_mut(&self.current) {
            n.commands.push(inverse);
        }
        self.revision += 1;
        Ok((impact, self.revision))
    }

    pub fn can_undo(&self) -> bool {
        self.current != Self::ROOT || self.open_changed()
    }

    /// A redo is possible: a step after the current one on its line, and
    /// no gesture with changes open.
    pub fn can_redo(&self) -> bool {
        !self.open_changed() && self.node(self.current).is_some_and(|n| n.next.is_some())
    }

    pub fn undo_label(&self) -> Option<&str> {
        (self.current != Self::ROOT)
            .then(|| self.node(self.current))
            .flatten()
            .map(|n| n.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        if self.open_changed() {
            return None;
        }
        let next = self.node(self.current)?.next?;
        self.node(next).map(|n| n.label.as_str())
    }

    /// The steps done, from the root to the current node (oldest first).
    fn done_path(&self) -> Vec<u64> {
        let mut path = self.ancestors(self.current);
        path.pop(); // the root
        path.reverse();
        path
    }

    /// The steps redo goes through from the current node, the next first.
    fn redo_path(&self) -> Vec<u64> {
        let mut path = Vec::new();
        let mut at = self.current;
        while let Some(next) = self.node(at).and_then(|n| n.next) {
            if path.len() > self.nodes.len() {
                break;
            }
            path.push(next);
            at = next;
        }
        path
    }

    /// The current line: the steps done and those redo goes through.
    fn line(&self) -> HashSet<u64> {
        let mut line: HashSet<u64> = self.ancestors(self.current).into_iter().collect();
        line.extend(self.redo_path());
        line
    }

    /// The steps that can be undone, oldest first.
    pub fn undo_labels(&self) -> Vec<&str> {
        self.labels(&self.done_path())
    }

    /// The steps that can be redone, the next first.
    pub fn redo_labels(&self) -> Vec<&str> {
        if self.open_changed() {
            return Vec::new();
        }
        self.labels(&self.redo_path())
    }

    fn labels(&self, ids: &[u64]) -> Vec<&str> {
        ids.iter()
            .filter_map(|id| self.node(*id))
            .map(|n| n.label.as_str())
            .collect()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The id of the step an undo would take back now (`None`: nothing to
    /// undo). A step keeps its id while it is undone and redone, so a
    /// caller can tell later whether the step it made is still done
    /// ([`Self::is_done`]).
    pub fn last_step(&self) -> Option<u64> {
        (self.current != Self::ROOT).then_some(self.current)
    }

    /// The step is done (undoing would take it back), not undone or
    /// forgotten.
    pub fn is_done(&self, step: u64) -> bool {
        step != Self::ROOT && self.node(step).is_some_and(|n| n.done)
    }

    /// The history still holds the step (done, or on any branch).
    pub fn knows(&self, step: u64) -> bool {
        step != Self::ROOT && self.nodes.contains_key(&step)
    }

    /// What takes back one step that is done, whatever was done after it:
    /// its inverses as a batch named `label`, to be applied as a new step
    /// (undoing that brings the step back; a batch that fails rolls back,
    /// so a step whose things a later step removed is not taken back at
    /// all). `None` when the step is not done.
    pub fn inverse_of(&self, step: u64, label: &str) -> Option<Command> {
        let n = self.node(step).filter(|n| n.done && step != Self::ROOT)?;
        Some(Command::Batch {
            label: label.to_string(),
            commands: n.commands.iter().rev().cloned().collect(),
        })
    }

    /// What does one step that is not done again on top of whatever is
    /// done now (a step undone, or one on another branch): its commands as
    /// a batch named `label`, to be applied as a new step (a batch that
    /// fails rolls back). `None` when the step is done or unknown.
    pub fn again_of(&self, step: u64, label: &str) -> Option<Command> {
        let n = self.node(step).filter(|n| !n.done && step != Self::ROOT)?;
        Some(Command::Batch {
            label: label.to_string(),
            commands: n.commands.iter().rev().cloned().collect(),
        })
    }

    // --- the tree ------------------------------------------------------------

    /// The node the project is in now ([`Self::ROOT`]: nothing done).
    pub fn current(&self) -> u64 {
        self.current
    }

    /// Older steps were forgotten (the limit): the root is not the project
    /// as opened any more.
    pub fn forgot_older(&self) -> bool {
        self.forgot
    }

    /// Steps kept (the root not counted).
    pub fn len(&self) -> usize {
        self.nodes.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Branches made (the first line counts as one).
    pub fn branch_count(&self) -> u32 {
        self.branches
    }

    /// `id`, its parent, … the root.
    fn ancestors(&self, id: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut at = Some(id);
        while let Some(a) = at {
            let Some(n) = self.node(a) else {
                break;
            };
            out.push(a);
            if out.len() > self.nodes.len() {
                break;
            }
            at = n.parent;
        }
        out
    }

    /// Steps from the root to `id`.
    pub fn depth_of(&self, id: u64) -> Option<usize> {
        self.nodes
            .contains_key(&id)
            .then(|| self.ancestors(id).len() - 1)
    }

    fn redo_set(&self) -> HashSet<u64> {
        if self.open_changed() {
            return HashSet::new();
        }
        self.redo_path().into_iter().collect()
    }

    fn info(&self, id: u64, depth: usize, to_redo: &HashSet<u64>) -> Option<HistoryStep> {
        let n = self.node(id)?;
        Some(HistoryStep {
            id,
            parent: n.parent,
            label: n.label.clone(),
            made: n.made,
            branch: n.branch,
            done: n.done,
            current: id == self.current,
            to_redo: to_redo.contains(&id),
            depth,
        })
    }

    /// A step as the views show it.
    pub fn step(&self, id: u64) -> Option<HistoryStep> {
        self.info(id, self.depth_of(id)?, &self.redo_set())
    }

    /// The steps made from `id`, oldest first.
    pub fn children_of(&self, id: u64) -> Vec<u64> {
        self.node(id)
            .map(|n| n.children.clone())
            .unwrap_or_default()
    }

    /// The nodes from `from` to `to` (both included): up to their common
    /// ancestor, then down. `None` when either is unknown.
    pub fn path_between(&self, from: u64, to: u64) -> Option<Vec<u64>> {
        if !self.nodes.contains_key(&from) || !self.nodes.contains_key(&to) {
            return None;
        }
        let up = self.ancestors(from);
        let down = self.ancestors(to);
        let on_down: HashSet<u64> = down.iter().copied().collect();
        let meet = up.iter().position(|a| on_down.contains(a))?;
        let lca = up[meet];
        let mut path: Vec<u64> = up[..=meet].to_vec();
        let at = down.iter().position(|d| *d == lca)?;
        path.extend(down[..at].iter().rev());
        Some(path)
    }

    /// The node `done` steps from the root on the current line (the steps
    /// done, then those redo goes through; past its end: the end).
    pub fn line_node(&self, done: usize) -> u64 {
        let done_path = self.done_path();
        if done <= done_path.len() {
            return match done {
                0 => Self::ROOT,
                d => done_path[d - 1],
            };
        }
        let redo = self.redo_path();
        redo.get((done - done_path.len()).min(redo.len()).saturating_sub(1))
            .copied()
            .unwrap_or(self.current)
    }

    /// The newest step of branch `n` (its end); `None` when it is unknown
    /// or forgotten.
    pub fn branch_tip(&self, branch: u32) -> Option<u64> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.branch == branch)
            .filter_map(|(id, _)| Some((self.depth_of(*id)?, *id)))
            .max()
            .map(|(_, id)| id)
    }

    /// The whole tree as rows: depth first from the root; at each step the
    /// branches made from it (oldest first, one level in), then its own
    /// branch goes on at its level.
    pub fn tree(&self) -> Vec<TreeRow> {
        let to_redo = self.redo_set();
        let mut rows = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(Self::ROOT, 0usize, 0usize, false)];
        while let Some((id, depth, indent, starts_branch)) = stack.pop() {
            let Some(n) = self.node(id) else {
                continue;
            };
            if let Some(step) = self.info(id, depth, &to_redo) {
                rows.push(TreeRow {
                    step,
                    indent,
                    starts_branch,
                });
            }
            let goes_on = n
                .children
                .iter()
                .copied()
                .find(|c| self.node(*c).is_some_and(|c| c.branch == n.branch))
                .or_else(|| n.children.first().copied());
            if let Some(c) = goes_on {
                stack.push((c, depth + 1, indent, false));
            }
            for &c in n.children.iter().rev() {
                if Some(c) != goes_on {
                    stack.push((c, depth + 1, indent + 1, true));
                }
            }
        }
        rows
    }

    /// The project as it is in node `id` — made on a copy of `project`:
    /// neither it nor the history changes. An unfinished gesture belongs to
    /// the present (undone first, as [`Self::undo`] would).
    pub fn project_at_node(&self, project: &Project, id: u64) -> Result<Project, EditError> {
        let path = self
            .path_between(self.current, id)
            .ok_or_else(|| EditError::Invalid(format!("the history holds no step {id}")))?;
        let mut p = project.clone();
        let apply = |p: &mut Project, commands: &[Command]| -> Result<(), EditError> {
            for c in commands.iter().rev() {
                c.clone().apply(p)?;
            }
            Ok(())
        };
        if let Some(tx) = &self.open {
            apply(&mut p, &tx.inverses)?;
        }
        for w in path.windows(2) {
            let (from, to) = (w[0], w[1]);
            // Up: the step left is done (its commands take it back); down:
            // the step entered is not (its commands do it).
            let step = if self.node(from).and_then(|n| n.parent) == Some(to) {
                from
            } else {
                to
            };
            if let Some(n) = self.node(step) {
                apply(&mut p, &n.commands)?;
            }
        }
        Ok(p)
    }

    /// The project as it is with `done` steps done (as [`Self::undo_labels`]
    /// counts them; fewer than now: as it was, more: as the steps to redo
    /// make it) — made on a copy of `project`: neither it nor the history
    /// changes. An unfinished gesture belongs to the present (undone first,
    /// as [`Self::undo`] would).
    pub fn project_at(&self, project: &Project, done: usize) -> Result<Project, EditError> {
        self.project_at_node(project, self.line_node(done))
    }

    pub fn undo(&mut self, project: &mut Project) -> Result<Option<Replayed>, EditError> {
        // Undo closes an unfinished gesture first.
        if self.depth > 0 {
            self.depth = 1;
            self.end();
        }
        let id = self.current;
        if id == Self::ROOT {
            return Ok(None);
        }
        let Some(n) = self.nodes.get_mut(&id) else {
            return Ok(None);
        };
        let impact = impact_of(&n.commands);
        n.commands = replay(project, &n.commands)?;
        n.done = false;
        let label = n.label.clone();
        let parent = n.parent.unwrap_or(Self::ROOT);
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.next = Some(id);
        }
        self.current = parent;
        self.revision += 1;
        Ok(Some(Replayed { label, impact }))
    }

    pub fn redo(&mut self, project: &mut Project) -> Result<Option<Replayed>, EditError> {
        if self.open_changed() {
            return Ok(None);
        }
        let Some(id) = self.node(self.current).and_then(|n| n.next) else {
            return Ok(None);
        };
        let Some(n) = self.nodes.get_mut(&id) else {
            return Ok(None);
        };
        let impact = impact_of(&n.commands);
        n.commands = replay(project, &n.commands)?;
        n.done = true;
        let label = n.label.clone();
        self.current = id;
        self.revision += 1;
        Ok(Some(Replayed { label, impact }))
    }

    /// Go to node `id`: undo to the common ancestor, then redo down its
    /// branch (redo goes that way afterwards). An unfinished gesture is
    /// closed first, as [`Self::undo`] does. A step that fails stops the
    /// way there (the project as the last step reached left it).
    pub fn go_to(&mut self, project: &mut Project, id: u64) -> Result<Moved, EditError> {
        if self.depth > 0 {
            self.depth = 1;
            self.end();
        }
        let path = self
            .path_between(self.current, id)
            .ok_or_else(|| EditError::Invalid(format!("the history holds no step {id}")))?;
        let mut moved = Moved::default();
        for w in path.windows(2) {
            let (from, to) = (w[0], w[1]);
            let r = if self.node(from).and_then(|n| n.parent) == Some(to) {
                self.undo(project)?
            } else {
                if let Some(n) = self.nodes.get_mut(&from) {
                    n.next = Some(to);
                }
                self.redo(project)?
            };
            if let Some(r) = r {
                moved.steps += 1;
                moved.impact = moved.impact.max(r.impact);
            }
        }
        Ok(moved)
    }

    pub fn clear(&mut self) {
        self.plant();
        self.open = None;
        self.depth = 0;
    }
}

#[cfg(test)]
mod project_at_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_step_long_ago_is_made_on_a_copy_as_undoing_would_make_it() {
        let mut p = Project::new("A", 48_000);
        let start = p.timeline.tempo.points()[0].bpm;
        let mut h = History::default();
        for bpm in [100.0, 110.0, 120.0, 130.0] {
            h.apply(&mut p, Command::SetTempo { bpm }).unwrap();
        }
        h.undo(&mut p).unwrap();
        let bpm = |p: &Project| p.timeline.tempo.points()[0].bpm;
        // Back to the start, two steps back, and forward to the step
        // undone.
        assert_eq!(bpm(&h.project_at(&p, 0).unwrap()), start);
        let then = h.project_at(&p, 1).unwrap();
        assert_eq!(bpm(&then), 100.0);
        assert_eq!(bpm(&h.project_at(&p, 3).unwrap()), 120.0);
        assert_eq!(bpm(&h.project_at(&p, 4).unwrap()), 130.0);
        // Nothing moved.
        assert_eq!(bpm(&p), 120.0);
        assert_eq!((h.undo_labels().len(), h.redo_labels().len()), (3, 1));
        // As undoing really makes it.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(then, p);
    }
}

#[cfg(test)]
mod step_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn bpm(p: &Project) -> f64 {
        p.timeline
            .tempo
            .bpm_at(faderframe_timeline::MusicalTime::ZERO)
    }

    #[test]
    fn a_step_keeps_its_id_through_undo_and_redo() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        let first = h.last_step().unwrap();
        h.apply(&mut p, Command::SetTempo { bpm: 110.0 }).unwrap();
        let second = h.last_step().unwrap();
        assert_ne!(first, second);
        h.undo(&mut p).unwrap();
        assert!(h.is_done(first) && !h.is_done(second));
        h.redo(&mut p).unwrap();
        assert_eq!(h.last_step(), Some(second));
        assert!(h.is_done(second));
    }

    #[test]
    fn a_step_long_done_is_taken_back_as_a_new_step() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        let tempo = h.last_step().unwrap();
        h.apply(&mut p, Command::RenameProject { name: "B".into() })
            .unwrap();
        let start = bpm(&Project::new("A", 48_000));
        let back = h.inverse_of(tempo, "Undo tempo").unwrap();
        h.apply(&mut p, back).unwrap();
        assert_eq!(bpm(&p), start);
        assert_eq!(p.name, "B", "the later step stays");
        let back = h.last_step().unwrap();
        assert!(h.is_done(tempo) && h.is_done(back));
        assert_eq!(h.undo_label(), Some("Undo tempo"));
        // Undoing the revert brings the step back.
        h.undo(&mut p).unwrap();
        assert_eq!(bpm(&p), 100.0);
        assert!(h.knows(back) && !h.is_done(back));
        assert!(h.inverse_of(999, "nothing").is_none());
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

    #[test]
    fn a_gesture_ending_amends_the_step_it_names() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        let bpm = |p: &Project| {
            p.timeline
                .tempo
                .bpm_at(faderframe_timeline::MusicalTime::ZERO)
        };
        h.begin("Script");
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        h.end();
        let token = h.revision();
        assert!(h.still_last(token, "Script"));
        assert!(!h.still_last(token, "Other"));
        // The rest of the script joins the step.
        h.begin("Script");
        h.amend_on_end(token);
        h.apply(&mut p, Command::SetTempo { bpm: 110.0 }).unwrap();
        h.end();
        assert_eq!(h.undo_labels(), vec!["Script"]);
        h.undo(&mut p).unwrap();
        assert_eq!(bpm(&p), 120.0);
        h.redo(&mut p).unwrap();
        assert_eq!(bpm(&p), 110.0);
        // Something done in between: a step of its own.
        let token = h.revision();
        h.apply(&mut p, Command::SetTempo { bpm: 90.0 }).unwrap();
        assert!(!h.still_last(token, "Script"));
        h.begin("Script");
        h.amend_on_end(token);
        h.apply(&mut p, Command::SetTempo { bpm: 80.0 }).unwrap();
        h.end();
        assert_eq!(h.undo_labels().len(), 3);
    }

    #[test]
    fn unmarking_lets_the_next_rewind_start_afresh() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        h.begin("Script");
        assert!(h.mark());
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        h.unmark();
        assert!(h.mark());
        h.apply(&mut p, Command::SetTempo { bpm: 110.0 }).unwrap();
        h.rewind(&mut p).unwrap();
        // Only what came after the new mark was taken back.
        assert_eq!(
            p.timeline
                .tempo
                .bpm_at(faderframe_timeline::MusicalTime::ZERO),
            100.0
        );
        h.end();
        assert_eq!(h.undo_labels(), vec!["Script"]);
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

#[cfg(test)]
mod tree_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Track, TrackColor, TrackKind};
    use faderframe_timeline::MusicalTime;

    fn bpm(p: &Project) -> f64 {
        p.timeline.tempo.bpm_at(MusicalTime::ZERO)
    }

    fn tempo(h: &mut History, p: &mut Project, bpm: f64) -> u64 {
        h.apply(p, Command::SetTempo { bpm }).unwrap();
        h.last_step().unwrap()
    }

    /// A, B, C; back to A; D: a branch beside B and C.
    fn forked() -> (History, Project, [u64; 4]) {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        let a = tempo(&mut h, &mut p, 100.0);
        let b = tempo(&mut h, &mut p, 110.0);
        let c = tempo(&mut h, &mut p, 120.0);
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        let d = tempo(&mut h, &mut p, 130.0);
        (h, p, [a, b, c, d])
    }

    #[test]
    fn an_edit_after_undoing_starts_a_branch_and_keeps_the_steps_undone() {
        let (h, p, [a, b, c, d]) = forked();
        assert_eq!(bpm(&p), 130.0);
        assert_eq!(h.undo_labels(), vec!["Change Tempo", "Change Tempo"]);
        assert!(h.redo_labels().is_empty() && !h.can_redo());
        // B and C are kept, undone, on a branch of their own.
        assert!(h.knows(b) && h.knows(c) && !h.is_done(b) && !h.is_done(c));
        assert!(h.is_done(a) && h.is_done(d));
        assert_eq!(h.children_of(a), vec![b, d]);
        assert_eq!(h.len(), 4);
        assert_eq!(h.branch_count(), 2);
        let step = |id| h.step(id).unwrap();
        assert_eq!((step(b).branch, step(c).branch, step(d).branch), (1, 1, 2));
        assert_eq!((step(d).depth, step(c).depth), (2, 3));
        assert!(step(d).current && step(a).done && !step(b).done);
        // The tree: the branch right after the step it starts from, one
        // level in; then the first line goes on.
        let rows: Vec<(u64, usize, bool)> = h
            .tree()
            .iter()
            .map(|r| (r.step.id, r.indent, r.starts_branch))
            .collect();
        assert_eq!(
            rows,
            vec![
                (History::ROOT, 0, false),
                (a, 0, false),
                (d, 1, true),
                (b, 0, false),
                (c, 0, false),
            ]
        );
        assert_eq!(h.branch_tip(1), Some(c));
        assert_eq!(h.branch_tip(2), Some(d));
        assert_eq!(h.branch_tip(3), None);
    }

    #[test]
    fn going_to_any_node_undoes_to_the_common_ancestor_and_redoes_down() {
        let (mut h, mut p, [a, b, c, d]) = forked();
        assert_eq!(h.path_between(d, c), Some(vec![d, a, b, c]));
        assert_eq!(h.path_between(c, c), Some(vec![c]));
        assert_eq!(h.path_between(History::ROOT, b), Some(vec![0, a, b]));
        assert_eq!(h.path_between(d, 999), None);
        let moved = h.go_to(&mut p, c).unwrap();
        assert_eq!(moved.steps, 3);
        assert_eq!(moved.impact, Impact::Timeline);
        assert_eq!((bpm(&p), h.current()), (120.0, c));
        assert_eq!(h.undo_labels().len(), 3);
        assert!(h.is_done(b) && !h.is_done(d) && h.knows(d));
        // Redo now follows the branch gone through.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(h.redo_labels().len(), 2);
        h.redo(&mut p).unwrap();
        assert_eq!(h.current(), b);
        // To the other branch and back to the start.
        h.go_to(&mut p, d).unwrap();
        assert_eq!(bpm(&p), 130.0);
        assert!(h.redo_labels().is_empty());
        let start = bpm(&Project::new("A", 48_000));
        let moved = h.go_to(&mut p, History::ROOT).unwrap();
        assert_eq!((moved.steps, bpm(&p)), (2, start));
        assert_eq!(h.last_step(), None);
        // Redo from the start goes down the branch last visited.
        assert_eq!(h.line_node(2), d);
        h.redo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        assert_eq!(h.current(), d);
        // Nowhere to go.
        assert!(h.go_to(&mut p, 999).is_err());
        assert_eq!(h.go_to(&mut p, d).unwrap().steps, 0);
    }

    #[test]
    fn a_node_made_on_a_copy_is_what_going_there_makes() {
        let (mut h, mut p, [a, b, c, d]) = forked();
        // An unfinished gesture belongs to the present.
        h.begin("Drag");
        h.apply(&mut p, Command::SetTempo { bpm: 90.0 }).unwrap();
        let ids = [History::ROOT, a, b, c, d];
        let copies: Vec<Project> = ids
            .iter()
            .map(|id| h.project_at_node(&p, *id).unwrap())
            .collect();
        assert_eq!(bpm(&p), 90.0, "nothing moved");
        // Going anywhere closes the gesture: a step after D.
        h.go_to(&mut p, d).unwrap();
        let drag = h.children_of(d)[0];
        h.go_to(&mut p, drag).unwrap();
        assert_eq!(bpm(&p), 90.0);
        for (id, copy) in ids.iter().zip(&copies) {
            h.go_to(&mut p, *id).unwrap();
            assert_eq!(&p, copy, "node {id}");
        }
        assert!(h.project_at_node(&p, 999).is_err());
    }

    #[test]
    fn redo_waits_while_a_gesture_has_changes_and_a_cancel_brings_it_back() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        tempo(&mut h, &mut p, 100.0);
        let b = tempo(&mut h, &mut p, 110.0);
        h.undo(&mut p).unwrap();
        h.begin("Drag");
        assert!(h.can_redo(), "nothing changed yet");
        h.apply(&mut p, Command::SetTempo { bpm: 140.0 }).unwrap();
        assert!(!h.can_redo() && h.redo_labels().is_empty());
        assert_eq!(h.redo(&mut p).unwrap(), None);
        h.cancel(&mut p).unwrap();
        assert_eq!(bpm(&p), 100.0);
        assert!(h.can_redo());
        h.redo(&mut p).unwrap();
        assert_eq!(h.current(), b);
    }

    #[test]
    fn mark_rewind_and_coalescing_work_on_a_branch() {
        let (mut h, mut p, [a, _, _, d]) = forked();
        h.go_to(&mut p, a).unwrap();
        let track = p.tracks[0].id;
        h.begin("Fader");
        for db in [-1.0, -2.0, -3.0] {
            h.apply(&mut p, Command::SetTrackVolume { track, db })
                .unwrap();
        }
        assert!(h.mark());
        for v in [80.0, 70.0] {
            h.rewind(&mut p).unwrap();
            h.apply(&mut p, Command::SetTempo { bpm: v }).unwrap();
        }
        h.end();
        let fader = h.last_step().unwrap();
        assert_eq!(h.children_of(a).len(), 3, "a third branch");
        assert_eq!((bpm(&p), p.tracks[0].volume_db), (70.0, -3.0));
        // One step; its one fader inverse takes the drag back whole.
        h.undo(&mut p).unwrap();
        assert_eq!((bpm(&p), p.tracks[0].volume_db), (100.0, 0.0));
        h.go_to(&mut p, d).unwrap();
        h.go_to(&mut p, fader).unwrap();
        assert_eq!((bpm(&p), p.tracks[0].volume_db), (70.0, -3.0));
    }

    #[test]
    fn amending_starts_a_step_of_its_own_after_going_anywhere() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        let cmd = |bpm| Command::SetTempo { bpm };
        let (_, t) = h.apply_amending(&mut p, cmd(100.0), "Rec", None).unwrap();
        let (_, t) = h
            .apply_amending(&mut p, cmd(110.0), "Rec", Some(t))
            .unwrap();
        let rec = h.last_step().unwrap();
        assert_eq!(h.len(), 1);
        // Undone and redone: the token is stale; the step stays as it was.
        h.undo(&mut p).unwrap();
        h.redo(&mut p).unwrap();
        h.apply_amending(&mut p, cmd(120.0), "Rec", Some(t))
            .unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h.children_of(rec).len(), 1);
        h.go_to(&mut p, History::ROOT).unwrap();
        h.go_to(&mut p, rec).unwrap();
        assert_eq!(bpm(&p), 110.0);
    }

    #[test]
    fn step_ids_stay_with_their_steps_across_branches() {
        let (mut h, mut p, [a, b, c, d]) = forked();
        assert_eq!([a, b, c, d], [1, 2, 3, 4], "ids given as steps are made");
        // A step on another branch: known, not done; done again over there.
        assert!(h.inverse_of(c, "x").is_none());
        let again = h.again_of(c, "Again").unwrap();
        assert!(h.again_of(d, "x").is_none() && h.again_of(History::ROOT, "x").is_none());
        h.apply(&mut p, again).unwrap();
        assert_eq!(bpm(&p), 120.0);
        let back = h.last_step().unwrap();
        assert_eq!(h.undo_label(), Some("Again"));
        // A step done long ago taken back alone.
        let undo_a = h.inverse_of(a, "Undo A").unwrap();
        h.apply(&mut p, undo_a).unwrap();
        assert_eq!(bpm(&p), bpm(&Project::new("A", 48_000)));
        h.go_to(&mut p, c).unwrap();
        assert_eq!(h.last_step(), Some(c));
        assert!(h.knows(back) && !h.is_done(back) && !h.is_done(d));
        assert!(!h.knows(History::ROOT) && !h.is_done(History::ROOT));
    }

    #[test]
    fn the_limit_forgets_branches_off_the_line_first_then_the_oldest_steps() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::new(4);
        let a = tempo(&mut h, &mut p, 100.0);
        let b = tempo(&mut h, &mut p, 110.0);
        let c = tempo(&mut h, &mut p, 120.0);
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        let d = tempo(&mut h, &mut p, 130.0);
        assert_eq!(h.len(), 4);
        let e = tempo(&mut h, &mut p, 140.0);
        // C, the oldest tip off the line, went; B is a tip now.
        assert!(!h.knows(c) && h.knows(b) && h.len() == 4);
        let f = tempo(&mut h, &mut p, 150.0);
        assert!(!h.knows(b) && h.len() == 4);
        assert!(!h.forgot_older());
        // One line: its oldest step goes, the root is the state after it.
        let g = tempo(&mut h, &mut p, 160.0);
        assert!(!h.knows(a) && h.forgot_older());
        assert_eq!(h.undo_labels().len(), 4);
        assert_eq!(h.children_of(History::ROOT), vec![d]);
        h.go_to(&mut p, History::ROOT).unwrap();
        assert_eq!(bpm(&p), 100.0, "as just after A");
        h.go_to(&mut p, g).unwrap();
        assert_eq!(bpm(&p), 160.0);
        assert!([d, e, f, g].iter().all(|s| h.is_done(*s)));
    }

    #[test]
    fn a_step_that_cannot_be_undone_leaves_the_project_and_the_history_as_they_were() {
        let mut p = Project::new("A", 48_000);
        let mut h = History::default();
        let t = Track::new(
            p.ids.allocate(),
            TrackKind::Audio,
            "Gtr",
            TrackColor::palette(2),
        );
        let id = t.id;
        h.apply(
            &mut p,
            Command::AddTrack {
                track: Box::new(t),
                index: 0,
            },
        )
        .unwrap();
        h.begin("Both");
        h.apply(
            &mut p,
            Command::RenameTrack {
                track: id,
                name: "Lead".into(),
            },
        )
        .unwrap();
        h.apply(&mut p, Command::SetTempo { bpm: 100.0 }).unwrap();
        h.end();
        let step = h.last_step().unwrap();
        // Something outside the history took the track away: the rename
        // cannot be taken back, so neither is the tempo.
        p.tracks.retain(|t| t.id != id);
        let before = p.clone();
        assert!(h.undo(&mut p).is_err());
        assert_eq!(p, before);
        assert_eq!(h.current(), step);
        assert!(h.is_done(step));
        assert!(h.inverse_of(step, "x").is_some());
    }
}
