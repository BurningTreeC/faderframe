//! Ids as they are read and written, for merging two lines of edits of a
//! project (`faderframe_project::merge`).
//!
//! Every id kind is allocated from one counter ([`super::IdAllocator`]),
//! so two copies of a project edited apart hand out the same numbers for
//! different things. Before a merge the other side's ids are moved out of
//! the way: reading its project inside [`with_shifts`] adds each range's
//! offset to the ids in it, wherever they are (tracks, clips, notes,
//! references between them, side tables) — every id goes through here.
//! [`noting_largest`] reports the largest id a serialisation wrote (ids an
//! allocator never handed out included).
//!
//! Outside those calls ids read and write as plain numbers; the check is
//! one thread-local flag.

use std::cell::{Cell, RefCell};

/// Ids in `[start, end)` are read as `id + by`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdRange {
    pub start: u64,
    pub end: u64,
    pub by: i64,
}

impl IdRange {
    /// Where `id` goes (`None`: not in the range).
    pub fn apply(&self, id: u64) -> Option<u64> {
        (self.start..self.end)
            .contains(&id)
            .then(|| id.wrapping_add_signed(self.by))
    }
}

thread_local! {
    static SHIFTING: Cell<bool> = const { Cell::new(false) };
    static SHIFTS: RefCell<Vec<IdRange>> = const { RefCell::new(Vec::new()) };
    static NOTING: Cell<bool> = const { Cell::new(false) };
    static LARGEST: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` (a deserialisation) with ids in `ranges` moved by their offsets.
pub fn with_shifts<R>(ranges: &[IdRange], f: impl FnOnce() -> R) -> R {
    let before = SHIFTS.with(|s| std::mem::replace(&mut *s.borrow_mut(), ranges.to_vec()));
    let was = SHIFTING.with(|s| s.replace(!ranges.is_empty()));
    let out = f();
    SHIFTING.with(|s| s.set(was));
    SHIFTS.with(|s| *s.borrow_mut() = before);
    out
}

/// Run `f` (a serialisation) and report the largest id it wrote (0: none).
pub fn noting_largest<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let was = NOTING.with(|n| n.replace(true));
    let before = LARGEST.with(|l| l.replace(0));
    let out = f();
    let largest = LARGEST.with(|l| l.replace(before.max(l.get())));
    NOTING.with(|n| n.set(was));
    (out, largest)
}

/// An id read: moved when a range takes it.
pub(crate) fn read(id: u64) -> u64 {
    if !SHIFTING.with(Cell::get) {
        return id;
    }
    SHIFTS.with(|s| s.borrow().iter().find_map(|r| r.apply(id)).unwrap_or(id))
}

/// An id written.
pub(crate) fn wrote(id: u64) {
    if NOTING.with(Cell::get) {
        LARGEST.with(|l| l.set(l.get().max(id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClipId, TrackId};
    use std::collections::BTreeMap;

    #[test]
    fn ids_move_where_they_are_read_and_nowhere_else() {
        let ranges = [IdRange {
            start: 10,
            end: 20,
            by: 100,
        }];
        let read: (TrackId, Vec<ClipId>, u64) =
            with_shifts(&ranges, || serde_json::from_str("[12, [3, 19, 20], 15]")).unwrap();
        assert_eq!(
            read,
            (TrackId(112), vec![ClipId(3), ClipId(119), ClipId(20)], 15)
        );
        // Map keys too.
        let map: BTreeMap<ClipId, TrackId> =
            with_shifts(&ranges, || serde_json::from_str(r#"{"11": 9}"#)).unwrap();
        assert_eq!(
            map.into_iter().collect::<Vec<_>>(),
            [(ClipId(111), TrackId(9))]
        );
        // Outside: plain numbers.
        let plain: TrackId = serde_json::from_str("12").unwrap();
        assert_eq!(plain, TrackId(12));
    }

    #[test]
    fn the_largest_id_written_is_noted() {
        let (text, largest) =
            noting_largest(|| serde_json::to_string(&(TrackId(4), 99u64, ClipId(7))));
        assert_eq!((text.unwrap().as_str(), largest), ("[4,99,7]", 7));
        let (_, none) = noting_largest(|| serde_json::to_string(&5u64));
        assert_eq!(none, 0);
    }
}
