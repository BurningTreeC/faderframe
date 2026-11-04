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
//! Notes, markers and sections can be placed in time on their own, on any
//! track: a locked note (a hit inside a musical cue) keeps its samples
//! while its clip and the other notes follow the beats; a locked marker
//! (a picture cut: the cuts found in the picture and the shots a conform
//! adds are locked) or section (a scene, a reel) keeps its samples. The
//! piano roll and the event list lock notes (`NoteOp::SetTimeLocked`,
//! `NoteOp::ToggleTimeLocked`); Quantize and Humanize leave locked notes'
//! timing alone. The marker and arranger lanes' menus lock markers and
//! sections ([`Session::marker_time_lock`], [`Session::section_time_lock`]).
//!
//! [`Session::edit`]: crate::Session::edit

use crate::{Action, InputChoice, Result, Session};
use faderframe_core::{ClipId, MarkerId, NoteId, SectionId, TrackId};
use faderframe_project::time_lock::TimeLock;
use faderframe_project::{Command, Impact, Marker, Section, TimeBase, TrackKind};

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

    /// The `index`-th note (from 0) of a track's MIDI clips in time order
    /// (notes in a clip by start, then key): its clip and id.
    pub fn track_note(&self, track: TrackId, index: usize) -> Option<(ClipId, NoteId)> {
        let p = &self.project;
        let mut clips = p.clips_of(track);
        clips.sort_by_key(|c| (c.start, c.id));
        clips
            .into_iter()
            .filter_map(|c| c.as_midi().map(|m| (c.id, m)))
            .flat_map(|(id, m)| {
                let mut notes: Vec<_> = m.notes.iter().collect();
                notes.sort_by_key(|n| (n.start, n.key, n.id));
                notes.into_iter().map(move |n| (id, n.id))
            })
            .nth(index)
    }

    /// How many of a track's notes are placed in time.
    pub fn time_locked_notes(&self, track: TrackId) -> usize {
        self.project
            .clips_of(track)
            .iter()
            .filter_map(|c| c.as_midi())
            .map(|m| m.notes.iter().filter(|n| n.time_locked).count())
            .sum()
    }

    /// Unlock every note of a track from time (one undo step; `None`:
    /// none is locked).
    pub fn unlock_track_notes(&self, track: TrackId) -> Option<Action> {
        let commands: Vec<Command> = self
            .project
            .clips_of(track)
            .into_iter()
            .filter_map(|c| {
                let m = c.as_midi()?;
                m.notes.iter().any(|n| n.time_locked).then(|| {
                    let mut m = m.clone();
                    m.notes.iter_mut().for_each(|n| n.time_locked = false);
                    Command::SetClipContent {
                        clip: c.id,
                        start: c.start,
                        content: Box::new(faderframe_project::ClipContent::Midi(m)),
                    }
                })
            })
            .collect();
        (!commands.is_empty()).then(|| {
            Action::Edit(Command::Batch {
                label: "Unlock Notes from Time".into(),
                commands,
            })
        })
    }

    /// The edit placing a marker in time (`locked`) or with the music
    /// (`None`: no such marker, or it is so already). Locking it puts it
    /// on a whole sample.
    pub fn marker_time_lock(&self, marker: MarkerId, locked: bool) -> Option<Action> {
        let m = self.project.markers.iter().find(|m| m.id == marker)?;
        (m.is_time_locked() != locked).then(|| {
            Action::Edit(Command::UpdateMarker {
                marker: Marker {
                    time_base: base_of(locked),
                    ..m.clone()
                },
            })
        })
    }

    /// The edit placing a section in time or with the music (as
    /// [`Self::marker_time_lock`]).
    pub fn section_time_lock(&self, section: SectionId, locked: bool) -> Option<Action> {
        let s = self.project.sections.iter().find(|s| s.id == section)?;
        (s.is_time_locked() != locked).then(|| {
            Action::Edit(Command::UpdateSection {
                section: Section {
                    time_base: base_of(locked),
                    ..s.clone()
                },
            })
        })
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

fn base_of(locked: bool) -> TimeBase {
    if locked {
        TimeBase::TimeLocked
    } else {
        TimeBase::Musical
    }
}
