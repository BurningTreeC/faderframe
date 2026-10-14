use faderframe_core::{ClipId, NoteId, TrackId};
use std::collections::BTreeSet;

/// How a selection request combines with the current selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    Replace,
    Toggle,
    Add,
}

/// Editor selection shared by every view (the mixer highlights the tracks
/// selected in the arranger and vice versa). Not part of the undo history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub tracks: BTreeSet<TrackId>,
    pub clips: BTreeSet<ClipId>,
    pub notes: BTreeSet<NoteId>,
    /// The edit selection: a time range on the selected tracks (a cursor
    /// when empty).
    pub range: Option<crate::editing::EditRange>,
    /// Where a Shift-click's range starts, per kind: the item last clicked
    /// on its own (selected or toggled); a range or a rectangle of several
    /// keeps it, so Shift-clicks one after another all reach from it.
    pub anchor_track: Option<TrackId>,
    pub anchor_clip: Option<ClipId>,
    pub anchor_note: Option<NoteId>,
}

/// The anchor after a request: the item of a single-item request.
fn anchor<T: Copy>(anchor: &mut Option<T>, items: &[T], mode: SelectMode) {
    match (items, mode) {
        ([one], SelectMode::Replace | SelectMode::Toggle) => *anchor = Some(*one),
        ([], SelectMode::Replace) => *anchor = None,
        _ => {}
    }
}

fn apply<T: Ord + Copy>(set: &mut BTreeSet<T>, items: &[T], mode: SelectMode) {
    match mode {
        SelectMode::Replace => {
            set.clear();
            set.extend(items.iter().copied());
        }
        SelectMode::Add => set.extend(items.iter().copied()),
        SelectMode::Toggle => {
            for i in items {
                if !set.remove(i) {
                    set.insert(*i);
                }
            }
        }
    }
}

impl Selection {
    pub fn select_tracks(&mut self, tracks: &[TrackId], mode: SelectMode) {
        apply(&mut self.tracks, tracks, mode);
        anchor(&mut self.anchor_track, tracks, mode);
    }

    pub fn select_clips(&mut self, clips: &[ClipId], mode: SelectMode) {
        apply(&mut self.clips, clips, mode);
        anchor(&mut self.anchor_clip, clips, mode);
    }

    pub fn select_notes(&mut self, notes: &[NoteId], mode: SelectMode) {
        apply(&mut self.notes, notes, mode);
        anchor(&mut self.anchor_note, notes, mode);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The single focused track (first selected).
    pub fn primary_track(&self) -> Option<TrackId> {
        self.tracks.iter().next().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single click sets the anchor; a range (several at once) keeps it,
    /// so Shift-clicks one after another reach from the same place.
    #[test]
    fn the_anchor_is_the_last_single_click() {
        let mut s = Selection::default();
        s.select_clips(&[ClipId(3)], SelectMode::Replace);
        assert_eq!(s.anchor_clip, Some(ClipId(3)));
        s.select_clips(&[ClipId(3), ClipId(4), ClipId(5)], SelectMode::Replace);
        assert_eq!(s.anchor_clip, Some(ClipId(3)));
        s.select_clips(&[ClipId(9)], SelectMode::Toggle);
        assert_eq!(s.anchor_clip, Some(ClipId(9)));
        s.select_clips(&[], SelectMode::Replace);
        assert_eq!(s.anchor_clip, None);
        s.select_tracks(&[TrackId(2)], SelectMode::Replace);
        s.select_tracks(&[TrackId(2), TrackId(3)], SelectMode::Replace);
        assert_eq!(s.anchor_track, Some(TrackId(2)));
    }

    #[test]
    fn modes() {
        let mut s = Selection::default();
        s.select_tracks(&[TrackId(1), TrackId(2)], SelectMode::Replace);
        s.select_tracks(&[TrackId(2), TrackId(3)], SelectMode::Toggle);
        assert_eq!(
            s.tracks.iter().copied().collect::<Vec<_>>(),
            vec![TrackId(1), TrackId(3)]
        );
        s.select_tracks(&[TrackId(5)], SelectMode::Add);
        assert_eq!(s.tracks.len(), 3);
        s.select_tracks(&[TrackId(9)], SelectMode::Replace);
        assert_eq!(s.primary_track(), Some(TrackId(9)));
    }
}
