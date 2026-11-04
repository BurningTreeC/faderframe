//! Where a saved project comes from. Every save stamps its file with a
//! [`Lineage`]: an id of its own, the id allocator's next value there, and
//! the saved states it descends from, newest first. Two files that went
//! their own ways from one project find the newest state they share in
//! their stamps — the base a three-way merge ([`crate::merge`]) starts from.
//!
//! Ids: every id kind comes from one counter, so two lines of edits hand
//! out the same numbers for different things. A merge moves the other
//! side's ids out of the way ([`faderframe_core::id::translate`]) and the
//! merged state remembers how ([`IdShift`]): a later merge with the same
//! line (or a merge the other side made with this one) reads ids the
//! earlier merge brought in as the things they already are.
//!
//! States merged in are listed as ancestors too, marked `merged`: their
//! ids are another line's, so only states both reach on their own lines
//! (`native`) mark where the counter was shared.

use faderframe_core::id::translate::IdRange;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hasher};

/// At most this many ancestors are kept (the oldest go).
pub const MAX_ANCESTORS: usize = 256;

/// A saved state a project descends from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ancestor {
    pub id: String,
    /// The id allocator's next value in that state.
    pub ids: u64,
    /// One more than its newest ancestor's (a descendant is always
    /// later).
    pub generation: u32,
    /// Reached only through a merge: its ids are another line's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
}

/// How a merge moved the ids of the state it took in: ids of `state`'s
/// line in `[start, end)` are `id + by` in the merged state (`merge`) and
/// everything descending from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdShift {
    /// The state merged in.
    pub state: String,
    /// The merged state (the line the moved ids live in).
    pub merge: String,
    pub start: u64,
    pub end: u64,
    pub by: i64,
}

/// A saved state's stamp (see the module).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    pub id: String,
    /// The id allocator's next value in this state.
    pub ids: u64,
    pub generation: u32,
    /// The states it descends from, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestors: Vec<Ancestor>,
    /// Ids moved by merges on its line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shifts: Vec<IdShift>,
}

/// A new state id: 128 random bits in hex.
pub fn new_id() -> String {
    let random = || {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", random(), random())
}

impl Lineage {
    /// The first saved state of a project.
    pub fn root(ids: u64) -> Self {
        Self {
            id: new_id(),
            ids,
            generation: 0,
            ancestors: Vec::new(),
            shifts: Vec::new(),
        }
    }

    /// The stamp of a state saved from `parent` (`None`: a first save).
    pub fn child_of(parent: Option<&Lineage>, ids: u64) -> Self {
        parent.map_or_else(|| Self::root(ids), |p| p.child(ids))
    }

    /// The stamp of a state saved from this one.
    pub fn child(&self, ids: u64) -> Self {
        let mut ancestors = Vec::with_capacity(self.ancestors.len() + 1);
        ancestors.push(self.as_ancestor());
        ancestors.extend(self.ancestors.iter().cloned());
        ancestors.truncate(MAX_ANCESTORS);
        Self {
            id: new_id(),
            ids,
            generation: self.generation + 1,
            ancestors,
            shifts: self.shifts.clone(),
        }
    }

    pub fn as_ancestor(&self) -> Ancestor {
        Ancestor {
            id: self.id.clone(),
            ids: self.ids,
            generation: self.generation,
            merged: false,
        }
    }

    /// This state and every state it descends from, newest first.
    pub fn line(&self) -> impl Iterator<Item = Ancestor> + '_ {
        std::iter::once(self.as_ancestor()).chain(self.ancestors.iter().cloned())
    }

    /// Is `id` this state or one it descends from?
    pub fn knows(&self, id: &str) -> bool {
        self.id == id || self.ancestors.iter().any(|a| a.id == id)
    }

    /// The states both descend from, newest first.
    pub fn common_ancestors(&self, other: &Lineage) -> Vec<Ancestor> {
        let theirs: BTreeSet<&str> = std::iter::once(other.id.as_str())
            .chain(other.ancestors.iter().map(|a| a.id.as_str()))
            .collect();
        let mut out: Vec<Ancestor> = self
            .line()
            .filter(|a| theirs.contains(a.id.as_str()))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.generation));
        out
    }

    /// The newest state both reach on their own lines (not through a
    /// merge): up to its allocator the two counted the same ids.
    pub fn branch_point(&self, other: &Lineage) -> Option<Ancestor> {
        let native = |l: &Lineage, id: &str| l.line().any(|a| a.id == id && !a.merged);
        self.common_ancestors(other)
            .into_iter()
            .find(|a| native(self, &a.id) && native(other, &a.id))
    }

    /// The ranges reading `theirs`' ids into this line, merges between the
    /// two lines known: ids a merge on this line moved (forward), and ids
    /// of this line a merge on theirs moved (back). Their own new ids are
    /// not among them (the merge places those).
    pub fn known_ranges(&self, theirs: &Lineage) -> Vec<IdRange> {
        let mut out = Vec::new();
        for s in &self.shifts {
            if theirs.knows(&s.state) && !theirs.knows(&s.merge) {
                out.push(IdRange {
                    start: s.start,
                    end: s.end,
                    by: s.by,
                });
            }
        }
        for s in &theirs.shifts {
            if self.knows(&s.state) && !self.knows(&s.merge) {
                out.push(IdRange {
                    start: s.start.wrapping_add_signed(s.by),
                    end: s.end.wrapping_add_signed(s.by),
                    by: -s.by,
                });
            }
        }
        out
    }

    /// The working state after `theirs` was merged into this one (a stamp
    /// no file has: the next save is its child), its new ids moved by
    /// `shift` and the allocator at `ids`.
    pub fn merged(&self, theirs: &Lineage, shift: Option<(u64, u64, i64)>, ids: u64) -> Lineage {
        let id = new_id();
        let mut ancestors: Vec<Ancestor> = self.line().collect();
        let known: BTreeSet<String> = ancestors.iter().map(|a| a.id.clone()).collect();
        ancestors.extend(
            theirs
                .line()
                .filter(|a| !known.contains(&a.id))
                .map(|mut a| {
                    a.merged = true;
                    a
                }),
        );
        ancestors.sort_by_key(|a| std::cmp::Reverse(a.generation));
        ancestors.truncate(MAX_ANCESTORS);
        let mut shifts = self.shifts.clone();
        if let Some((start, end, by)) = shift.filter(|s| s.1 > s.0 && s.2 != 0) {
            shifts.push(IdShift {
                state: theirs.id.clone(),
                merge: id.clone(),
                start,
                end,
                by,
            });
        }
        Lineage {
            id,
            ids,
            generation: self.generation.max(theirs.generation) + 1,
            ancestors,
            shifts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_new_each_time() {
        let a = new_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, new_id());
    }

    #[test]
    fn two_lines_find_the_newest_state_they_share() {
        let v = Lineage::root(100);
        let ours = v.child(120).child(130);
        let theirs = v.child(110);
        assert_eq!(ours.branch_point(&theirs).map(|a| a.id), Some(v.id.clone()));
        assert_eq!(ours.common_ancestors(&theirs)[0].ids, 100);
        assert!(Lineage::root(1).branch_point(&ours).is_none());
    }

    #[test]
    fn a_merge_remembers_how_it_moved_the_other_lines_ids() {
        let v = Lineage::root(100);
        let ours = v.child(150);
        let t1 = v.child(120);
        // T1's ids from 100 on became ours from 150 on.
        let m = ours.merged(&t1, Some((100, 120, 50)), 170);
        assert!(m.knows(&t1.id) && m.knows(&ours.id));
        let saved = m.child(175);
        // Their next state: T1's ids read as the merge placed them; the
        // branch point is still V (T1 is another line's here).
        let t2 = t1.child(130);
        assert_eq!(
            saved.known_ranges(&t2),
            [IdRange {
                start: 100,
                end: 120,
                by: 50
            }]
        );
        assert_eq!(saved.branch_point(&t2).map(|a| a.id), Some(v.id.clone()));
        // Seen from their line, our state holds T1's things at the ids the
        // merge gave them: read back.
        assert_eq!(
            t2.known_ranges(&saved),
            [IdRange {
                start: 150,
                end: 170,
                by: -50
            }]
        );
        // A merge of our state on their line is read back the other way.
        let theirs_merge = t2.merged(&saved, Some((100, 175, 30)), 205);
        let back = saved.child(180).known_ranges(&theirs_merge);
        assert!(back.contains(&IdRange {
            start: 130,
            end: 205,
            by: -30
        }));
    }
}
