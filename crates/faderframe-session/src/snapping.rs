//! Snap to Objects ([`EditFlag::SnapToObjects`]): edits snap to clip
//! starts and ends — on the same and the other shown tracks — markers,
//! section bounds, the loop and edit-selection bounds, the Play Start
//! Marker and the playhead (while stopped); in the piano roll also the
//! other notes' starts and ends.
//!
//! The rules (the editors apply them; see `docs/ARCHITECTURE.md`):
//!
//! * Alt bypasses all snapping.
//! * An object within the snap distance — screen pixels, so the same at
//!   every zoom — beats the grid; the nearest object wins (at equal
//!   distance the kind listed first in [`SnapObject`]).
//! * Without one the edit mode decides as before: Grid snaps to the grid
//!   (absolute or by relative steps), Slip and the others move freely
//!   (on whole samples with Snap to Samples).
//! * What is being edited is never a target: the moved or trimmed clips,
//!   the dragged marker or section, the selection while it is being
//!   drawn, the loop while it is being dragged — nor what a ripple drag
//!   moves along with them (the clips after the edit, and with All Tracks
//!   the markers, sections, loop and Play Start Marker after it).
//! * A moving group snaps by its leading or trailing edge (or the grabbed
//!   clip's), whichever is nearest an object; a trim by the trimmed edge;
//!   a range by its moving edge.
//! * Shuffle moves are not snapped (the clips butt against each other
//!   where they land) and Spot types the position.
//!
//! [`EditFlag::SnapToObjects`]: crate::EditFlag::SnapToObjects

use crate::Session;
use faderframe_core::{ClipId, MarkerId, NoteId, SectionId, TrackId};
use faderframe_timeline::MusicalTime;

/// The snap distance in screen pixels (the same at every zoom): editors
/// turn it into time at their zoom.
pub const SNAP_PX: f32 = 8.0;

/// What a position snapped to (in the order ties are decided).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapObject {
    ClipStart(ClipId),
    ClipEnd(ClipId),
    Marker(MarkerId),
    SectionStart(SectionId),
    SectionEnd(SectionId),
    LoopStart,
    LoopEnd,
    SelectionStart,
    SelectionEnd,
    PlayStart,
    Playhead,
    /// A note's start or end (the piano roll's own objects).
    NoteStart(NoteId),
    NoteEnd(NoteId),
}

impl SnapObject {
    /// A short name for the guide line.
    pub fn label(self) -> &'static str {
        match self {
            Self::ClipStart(_) => "Clip Start",
            Self::ClipEnd(_) => "Clip End",
            Self::Marker(_) => "Marker",
            Self::SectionStart(_) => "Section Start",
            Self::SectionEnd(_) => "Section End",
            Self::LoopStart => "Loop Start",
            Self::LoopEnd => "Loop End",
            Self::SelectionStart => "Selection Start",
            Self::SelectionEnd => "Selection End",
            Self::PlayStart => "Play Start",
            Self::Playhead => "Playhead",
            Self::NoteStart(_) => "Note Start",
            Self::NoteEnd(_) => "Note End",
        }
    }
}

/// An object a position snapped to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapHit {
    /// Where the object is.
    pub at: MusicalTime,
    pub object: SnapObject,
    /// The track of a clip edge.
    pub track: Option<TrackId>,
}

/// What a drag must not snap to (itself).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapSkip {
    pub clips: Vec<ClipId>,
    pub marker: Option<MarkerId>,
    pub section: Option<SectionId>,
    /// The edit selection's bounds.
    pub selection: bool,
    /// The loop's bounds.
    pub loop_range: bool,
    /// The Play Start Marker.
    pub play_start: bool,
}

impl SnapSkip {
    /// Skip these clips (being moved or trimmed).
    pub fn clips(clips: &[ClipId]) -> Self {
        Self {
            clips: clips.to_vec(),
            ..Self::default()
        }
    }

    /// Skip the edit selection's bounds (a range being drawn).
    pub fn selection_bounds() -> Self {
        Self {
            selection: true,
            ..Self::default()
        }
    }

    /// Skip the loop's bounds (the loop being dragged).
    pub fn loop_bounds() -> Self {
        Self {
            loop_range: true,
            ..Self::default()
        }
    }

    /// Skip the Play Start Marker (being dragged).
    pub fn play_start_marker() -> Self {
        Self {
            play_start: true,
            ..Self::default()
        }
    }
}

impl Session {
    /// The object nearest `t` within `reach` either side that edits snap
    /// to, skipping what `skip` names; `None` when there is none or Snap to
    /// Objects is off.
    pub fn snap_to_object(
        &self,
        t: MusicalTime,
        reach: MusicalTime,
        skip: &SnapSkip,
    ) -> Option<SnapHit> {
        self.snap_edges_to_object(&[t], reach, skip)
            .map(|(_, hit)| hit)
    }

    /// Of `edges` (positions a drag would put things at), the one nearest
    /// an object within `reach`: how far to shift them all so that edge
    /// lands on it, and the object. `None` when no edge is near one (or
    /// Snap to Objects is off).
    pub fn snap_edges_to_object(
        &self,
        edges: &[MusicalTime],
        reach: MusicalTime,
        skip: &SnapSkip,
    ) -> Option<(MusicalTime, SnapHit)> {
        self.snap_edges_to_objects_and(edges, reach, skip, &[])
    }

    /// [`Self::snap_edges_to_object`] with `extra` objects of the editor
    /// asking (the piano roll's notes).
    pub fn snap_edges_to_objects_and(
        &self,
        edges: &[MusicalTime],
        reach: MusicalTime,
        skip: &SnapSkip,
        extra: &[(MusicalTime, SnapObject)],
    ) -> Option<(MusicalTime, SnapHit)> {
        if !self.editor.snap_objects || edges.is_empty() || reach.ticks() < 0 {
            return None;
        }
        let mut best: Option<(i64, MusicalTime, SnapHit)> = None;
        let mut consider = |at: MusicalTime, object: SnapObject, track: Option<TrackId>| {
            for &e in edges {
                let d = (at.ticks() - e.ticks()).abs();
                if d <= reach.ticks() && best.as_ref().is_none_or(|(bd, ..)| d < *bd) {
                    best = Some((d, at - e, SnapHit { at, object, track }));
                }
            }
        };
        let p = &self.project;
        for track in &p.tracks {
            if track.clips.is_empty() || !self.track_shown(track) {
                continue;
            }
            for id in &track.clips {
                if skip.clips.contains(id) || self.ripple_moving_clip(*id) {
                    continue;
                }
                let Some(c) = p.clip(*id) else { continue };
                consider(c.start, SnapObject::ClipStart(c.id), Some(track.id));
                consider(
                    c.end(&p.timeline, p.sample_rate),
                    SnapObject::ClipEnd(c.id),
                    Some(track.id),
                );
            }
        }
        // The timeline's parts a ripple drag moves along are no target.
        let still = |at: MusicalTime| self.ripple_moving_timeline().is_none_or(|from| at < from);
        for m in &p.markers {
            if skip.marker != Some(m.id) && still(m.position) {
                consider(m.position, SnapObject::Marker(m.id), None);
            }
        }
        for s in &p.sections {
            if skip.section != Some(s.id) {
                if still(s.start) {
                    consider(s.start, SnapObject::SectionStart(s.id), None);
                }
                if still(s.end) {
                    consider(s.end, SnapObject::SectionEnd(s.id), None);
                }
            }
        }
        if let Some(r) = p.loop_range.filter(|_| !skip.loop_range) {
            if still(r.start) {
                consider(r.start, SnapObject::LoopStart, None);
            }
            if still(r.end) {
                consider(r.end, SnapObject::LoopEnd, None);
            }
        }
        if let Some(r) = self
            .selection
            .range
            .filter(|r| !r.is_empty() && !skip.selection)
        {
            consider(r.start, SnapObject::SelectionStart, None);
            consider(r.end, SnapObject::SelectionEnd, None);
        }
        if let Some(m) = p.play_start.filter(|m| !skip.play_start && still(m.at)) {
            consider(m.at, SnapObject::PlayStart, None);
        }
        if !self.transport().playing {
            consider(self.playhead(), SnapObject::Playhead, None);
        }
        for &(at, object) in extra {
            consider(at, object, None);
        }
        best.map(|(_, shift, hit)| (shift, hit))
    }
}
