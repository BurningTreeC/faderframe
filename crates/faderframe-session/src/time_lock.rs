//! Time-locked tracks in the session (see `faderframe_project::time_lock`):
//! every edit keeps their material in time — [`Session::edit`] works out
//! what an edit does to them before it applies and adds the moves that
//! keep their samples in the same undo step — and the menus offer the
//! choice.
//!
//! What the editing operations do with time-locked material:
//!
//! * **Tempo changes** (the tempo lane, Set Tempo, the tempo from a clip,
//!   tempo through hit points, MIDI files' tempo maps, anything else that
//!   sets the timeline): clips, automation and a frozen render keep their
//!   samples; musical tracks move with the beats.
//! * **Moves, trims, splits, paste, duplicate, recording and ADR takes**:
//!   what they place on a time-locked track lands on a whole sample (the
//!   grid snaps as asked: onto the sample under the grid line) and stays
//!   there through later tempo changes.
//! * **Ripple**: an explicit edit in time. On a time-locked track it moves
//!   what follows by the *time* the edit takes out or opens (in samples,
//!   `RippleShift::map_in_time`), so locked material keeps its spacing in
//!   time even across a tempo change; with All Tracks moving the tempo
//!   along, the same is true of the musical tracks.
//! * **Section moves, copies and deletes** move time with everything in
//!   it, the tempo included: locked material moves with its section (it
//!   is content of that span of time) and keeps its place in it; what the
//!   rearrangement leaves where it was keeps its sample.
//! * **Duplicating a track** keeps its time base.
//!
//! [`Session::edit`]: crate::Session::edit

use crate::{Action, InputChoice, Result, Session};
use faderframe_core::TrackId;
use faderframe_project::time_lock::TimeLock;
use faderframe_project::{Command, Impact, TimeBase, TrackKind};

impl Session {
    /// The time-locked material an edit changes, worked out before it
    /// applies (`None`: nothing to keep).
    pub(crate) fn time_lock_before(&self, cmd: &Command) -> Option<TimeLock> {
        faderframe_project::time_lock::plan(&self.project, cmd)
    }

    /// After an edit (inside its undo step): the moves keeping time-locked
    /// material where it sounds.
    pub(crate) fn keep_time_locked(&mut self, lock: Option<TimeLock>) -> Result<Impact> {
        let Some(lock) = lock else {
            return Ok(Impact::None);
        };
        let mut impact = Impact::None;
        for cmd in lock.keep(&self.project) {
            impact = impact.max(self.history.apply(&mut self.project, cmd)?);
        }
        Ok(impact)
    }

    /// Is the track placed in time rather than with the music?
    pub fn is_time_locked(&self, track: TrackId) -> bool {
        self.project
            .track(track)
            .is_some_and(|t| t.is_time_locked())
    }

    /// The time bases a track can have (the track menus' Time Base:
    /// Musical / Time-locked; none for folders, which hold no material).
    pub fn time_base_choices(&self, track: TrackId) -> Vec<InputChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if t.kind == TrackKind::Folder {
            return Vec::new();
        }
        TimeBase::ALL
            .into_iter()
            .map(|base| InputChoice {
                label: base.label().into(),
                action: Action::Edit(Command::SetTrackTimeBase { track, base }),
                checked: t.time_base == base,
                group_start: false,
            })
            .collect()
    }
}
