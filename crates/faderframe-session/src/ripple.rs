//! Ripple editing: when an edit changes how much time something takes,
//! what follows moves with it — chosen per track or across the whole
//! project.
//!
//! * **When**: Shuffle mode always ripples (and its moves reorder clips);
//!   with [`RippleSettings::on`] the other modes ripple too — Grid snaps,
//!   Slip moves freely — and moves push what follows them along.
//! * **Where** ([`RippleScope`]): *Edited Tracks* (track-selective: only
//!   the tracks the edit is on), *Selected Tracks* (the edited tracks and
//!   every selected track, together), *All Tracks* (global: every track
//!   and the timeline — markers, sections, the loop and punch ranges, the
//!   Play Start Marker, tempo and metre, key, chords and lyrics as
//!   [`RippleSettings`] chooses — so the whole project stays in sync).
//!   A track left out ([`faderframe_project::Track::ripple_excluded`]) is
//!   never moved by edits elsewhere; frozen tracks are left out with a
//!   notice (their audio is fixed).
//! * **How**: every ripple is a list of [`RippleShift`]s — at or after a
//!   cut everything moves by an offset; a negative one takes the time
//!   before the cut out. On the tracks the edit is on, clips move and
//!   none is cut (a neighbour in a crossfade is not trimmed); the tracks
//!   kept in sync with them are cut: their time opens or closes at the
//!   same place, a clip across the cut is split. Automation is its own
//!   entity on every track that ripples, moved by the same shifts at the
//!   same cuts ([`faderframe_automation::AutomationCurve::ripple`]: the
//!   curve is split there and its value duplicated, its shape kept on
//!   both sides). Several edited clips at once: each moves what follows it
//!   on its own track; the tracks kept in sync follow the first of them.
//! * **What ripples**: trims (and time-stretch trims of the end) keep the
//!   clip's start and move what follows by the change of length; moves
//!   push what follows the moved clips (Shuffle reorders instead, the
//!   time a track takes staying the same); deleting clips, clearing and
//!   cutting a range close the gap; paste, duplicate, repeat and Insert
//!   Silence open room; nudges and Spot push; Trim to Selection closes up
//!   both ends. Explicit commands ([`RippleOp`]) ripple whatever the mode,
//!   with the scope they name: delete the selected clips or the range,
//!   insert time, remove the gaps between clips — on their tracks or in
//!   all tracks — and [`Session::apply_ripple_offset`].
//! * **Drags** are lossless: a ripple splits clips and cuts curves on the
//!   way, which the next pointer motion could not take back, so the
//!   running gesture is rewound to its first ripple
//!   ([`faderframe_project::History::rewind`]) and computed afresh from
//!   there each motion — dragging back gives the project back exactly,
//!   and the whole drag is one undo step. What a drag moves is not a snap
//!   target while it moves.

use crate::clipboard::{LaneWork, merged};
use crate::editing::EditMode;
use crate::{NoticeLevel, Result, Session};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::ripple::{ClipRipple, RippleGlobals, clip_commands, global_commands};
use faderframe_project::{Clip, Command, Impact, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::{BTreeMap, BTreeSet};

pub use faderframe_project::ripple::RippleShift;

/// Which tracks a ripple moves.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RippleScope {
    /// Only the tracks the edit is on.
    #[default]
    Edited,
    /// The edited tracks and every selected track, together.
    Selected,
    /// Every track and the timeline: the whole project stays in sync.
    All,
}

impl RippleScope {
    pub const ALL: [RippleScope; 3] = [Self::Edited, Self::Selected, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::Edited => "Edited Tracks",
            Self::Selected => "Selected Tracks",
            Self::All => "All Tracks",
        }
    }

    /// The short form the edit toolbar shows.
    pub fn short(self) -> &'static str {
        match self {
            Self::Edited => "Track",
            Self::Selected => "Selected",
            Self::All => "All",
        }
    }

    /// The name scripts and menus use.
    pub fn id(self) -> &'static str {
        match self {
            Self::Edited => "edited",
            Self::Selected => "selected",
            Self::All => "all",
        }
    }

    pub fn from_id(id: &str) -> Option<RippleScope> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// How ripple editing works (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RippleSettings {
    /// Ripple in Grid, Slip and Spot modes too (Shuffle always ripples).
    pub on: bool,
    pub scope: RippleScope,
    /// All Tracks: markers, sections, the loop and punch ranges and the
    /// Play Start Marker move along.
    pub markers: bool,
    /// All Tracks: tempo and time-signature changes move along.
    pub tempo: bool,
    /// All Tracks: key changes, chords and lyrics move along.
    pub harmony: bool,
}

impl Default for RippleSettings {
    fn default() -> Self {
        Self {
            on: false,
            scope: RippleScope::Edited,
            markers: true,
            tempo: true,
            harmony: true,
        }
    }
}

impl RippleSettings {
    /// The timeline's parts a ripple of all tracks moves.
    pub fn globals(&self) -> RippleGlobals {
        RippleGlobals {
            markers: self.markers,
            tempo: self.tempo,
            harmony: self.harmony,
        }
    }
}

/// Ripple settings and the explicit ripple commands.
#[derive(Clone, Debug, PartialEq)]
pub enum RippleOp {
    /// Change the settings.
    Settings(RippleSettings),
    /// Ripple editing on or off: off leaves Shuffle (for Slip), which
    /// always ripples; on ripples in the mode there is.
    Toggle,
    /// Delete the selected clips; the time they took closes up.
    DeleteClips(RippleScope),
    /// Delete the edit range with what is in it; what follows moves up.
    DeleteRange(RippleScope),
    /// Open the edit range's length at its start.
    InsertTime(RippleScope),
    /// Close the gaps between the selected clips (All Tracks: the time no
    /// track has a clip in, between the first and the last selected clip,
    /// or over the whole song without a selection).
    RemoveGaps(RippleScope),
    /// At or after `cut`, move everything by `delta` on one track (`Some`)
    /// or on every track with the timeline (`None`), as
    /// [`Session::apply_ripple_offset`].
    Offset {
        cut: MusicalTime,
        delta: MusicalTime,
        track: Option<TrackId>,
    },
    /// Leave tracks out of rippling, or take them in again.
    Exclude {
        tracks: Vec<TrackId>,
        excluded: bool,
    },
}

/// An entry of the ripple menus (the edit toolbar's, the arranger's).
#[derive(Clone, Debug, PartialEq)]
pub struct RippleMenuEntry {
    pub label: String,
    /// `None`: shown greyed out (nothing to do it to).
    pub action: Option<crate::Action>,
    pub checked: bool,
    /// A separator goes above it.
    pub separated: bool,
}

/// What a running ripple drag moves (not a snap target while it moves).
#[derive(Clone, Debug, Default)]
pub(crate) struct RippleMoved {
    pub clips: BTreeSet<ClipId>,
    /// The timeline moved from here on (All Tracks).
    pub timeline_from: Option<MusicalTime>,
}

/// The ripple state of the running gesture.
#[derive(Debug, Default)]
pub(crate) struct RippleGesture {
    /// How much the engine must update for the last rewind (synced with
    /// the next edit, or on its own when that changes less).
    pub rewound: Option<Impact>,
    /// What the last motion moved.
    pub moved: Option<RippleMoved>,
    /// Frozen tracks left out were noted (once a gesture).
    pub noted: bool,
}

/// The spans clips take on the timeline, merged.
fn spans_of(s: &Session, clips: &[&Clip]) -> Vec<(MusicalTime, MusicalTime)> {
    merged(clips.iter().map(|c| (c.start, s.clip_end(c))).collect())
}

/// Shifts taking `spans` out.
fn removals(spans: &[(MusicalTime, MusicalTime)]) -> Vec<RippleShift> {
    spans
        .iter()
        .map(|&(a, b)| RippleShift::remove(a, b))
        .collect()
}

impl Session {
    /// Does editing ripple now (Shuffle, or ripple on)?
    pub fn ripples(&self) -> bool {
        self.editor.edit_mode == EditMode::Shuffle || self.editor.ripple.on
    }

    pub fn ripple_settings(&self) -> RippleSettings {
        self.editor.ripple
    }

    /// The ripple menu: on/off, the scope, what of the timeline follows
    /// a global ripple, the explicit commands (greyed out without what
    /// they work on) and leaving the selected tracks out.
    pub fn ripple_menu(&self) -> Vec<RippleMenuEntry> {
        use crate::Action;
        let r = self.editor.ripple;
        let set = |s: RippleSettings| Some(Action::Ripple(RippleOp::Settings(s)));
        let entry =
            |label: &str, action: Option<Action>, checked: bool, separated: bool| RippleMenuEntry {
                label: label.into(),
                action,
                checked,
                separated,
            };
        let mut out = vec![entry(
            if self.editor.edit_mode == EditMode::Shuffle {
                "Ripple Editing (Shuffle always ripples)"
            } else {
                "Ripple Editing"
            },
            Some(Action::Ripple(RippleOp::Toggle)),
            self.ripples(),
            false,
        )];
        for (i, scope) in RippleScope::ALL.into_iter().enumerate() {
            let label = match scope {
                RippleScope::Edited => "Edited Tracks Only",
                RippleScope::Selected => "Edited and Selected Tracks",
                RippleScope::All => "All Tracks and the Timeline",
            };
            out.push(entry(
                label,
                set(RippleSettings { scope, ..r }),
                r.scope == scope,
                i == 0,
            ));
        }
        out.push(entry(
            "All Tracks: Markers, Sections and Ranges Follow",
            set(RippleSettings {
                markers: !r.markers,
                ..r
            }),
            r.markers,
            true,
        ));
        out.push(entry(
            "All Tracks: Tempo and Metre Follow",
            set(RippleSettings {
                tempo: !r.tempo,
                ..r
            }),
            r.tempo,
            false,
        ));
        out.push(entry(
            "All Tracks: Key, Chords and Lyrics Follow",
            set(RippleSettings {
                harmony: !r.harmony,
                ..r
            }),
            r.harmony,
            false,
        ));
        let clips = !self.selected_arranger_clips().is_empty();
        let range = self.selection.range.is_some_and(|x| !x.is_empty());
        let op = |o: RippleOp, ok: bool| ok.then_some(Action::Ripple(o));
        for (i, (label, action)) in [
            (
                "Ripple Delete Selected Clips",
                op(RippleOp::DeleteClips(RippleScope::Edited), clips),
            ),
            (
                "Ripple Delete Selected Clips in All Tracks",
                op(RippleOp::DeleteClips(RippleScope::All), clips),
            ),
            (
                "Ripple Delete Selection Range",
                op(RippleOp::DeleteRange(RippleScope::Edited), range),
            ),
            (
                "Ripple Delete Selection Range in All Tracks",
                op(RippleOp::DeleteRange(RippleScope::All), range),
            ),
            (
                "Insert Time in All Tracks",
                op(RippleOp::InsertTime(RippleScope::All), range),
            ),
            (
                "Remove Gaps Between Selected Clips",
                op(RippleOp::RemoveGaps(RippleScope::Edited), clips),
            ),
            (
                "Remove Gaps in All Tracks",
                op(RippleOp::RemoveGaps(RippleScope::All), true),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            out.push(entry(label, action, false, i == 0));
        }
        let tracks: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| self.selection.tracks.contains(&t.id) && t.kind != TrackKind::Master)
            .map(|t| t.id)
            .collect();
        let excluded = !tracks.is_empty()
            && tracks
                .iter()
                .all(|t| self.project.track(*t).is_some_and(|t| t.ripple_excluded));
        out.push(entry(
            "Leave Selected Tracks Out of Ripples",
            (!tracks.is_empty()).then_some(Action::Ripple(RippleOp::Exclude {
                tracks,
                excluded: !excluded,
            })),
            excluded,
            true,
        ));
        out
    }

    /// The scope ripples have now (`None`: editing does not ripple).
    pub(crate) fn ripple_scope(&self) -> Option<RippleScope> {
        self.ripples().then_some(self.editor.ripple.scope)
    }

    /// The tracks kept in sync with an edit on `edited` in `scope` (cut
    /// at the edit's cuts): none for Edited Tracks, the other selected
    /// tracks (and those grouped with them) for Selected Tracks, every
    /// other track for All Tracks — never one left out of rippling, a
    /// folder (it holds nothing) or the edited ones themselves.
    pub fn ripple_followers(&self, edited: &[TrackId], scope: RippleScope) -> Vec<TrackId> {
        let candidates: Vec<TrackId> = match scope {
            RippleScope::Edited => return Vec::new(),
            RippleScope::Selected => {
                let selected: Vec<TrackId> = self
                    .project
                    .tracks
                    .iter()
                    .filter(|t| self.selection.tracks.contains(&t.id))
                    .map(|t| t.id)
                    .collect();
                self.with_grouped_tracks(selected)
            }
            RippleScope::All => self.project.tracks.iter().map(|t| t.id).collect(),
        };
        candidates
            .into_iter()
            .filter(|id| !edited.contains(id))
            .filter(|id| {
                self.project
                    .track(*id)
                    .is_some_and(|t| !t.ripple_excluded && t.kind != TrackKind::Folder)
            })
            .collect()
    }

    /// Is `track` frozen where edits apply (the song; in a scratch pad its
    /// clips are the pad's)?
    fn frozen_here(&self, track: TrackId) -> bool {
        self.project.scratch_pads.shown.is_none()
            && self
                .project
                .track(track)
                .is_some_and(|t| t.freeze.is_some())
    }

    /// The commands making `followers` follow `shifts` (cut there: clips
    /// across a cut split) with their automation in `lanes`, and — with
    /// `global` — the timeline's parts the settings choose. Returns the
    /// tracks' commands and the timeline's (which go last in the batch:
    /// clip commands are worked out with the old tempo). Frozen followers
    /// are left out (noted once a gesture).
    pub(crate) fn follow_commands(
        &mut self,
        shifts: &[RippleShift],
        followers: &[TrackId],
        global: bool,
        lanes: &mut LaneWork,
    ) -> (Vec<Command>, Vec<Command>) {
        let shifts = RippleShift::ordered(shifts);
        if shifts.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let first = shifts
            .iter()
            .map(|s| s.cut.min(s.cut + s.delta))
            .min()
            .unwrap_or(MusicalTime::ZERO);
        let mut cmds = Vec::new();
        let mut frozen: Vec<String> = Vec::new();
        let mut ids = self.project.ids.clone();
        for &track in followers {
            if self.frozen_here(track) {
                // Only worth a word when it had something to move.
                let reaches = self
                    .project
                    .clips_of(track)
                    .iter()
                    .any(|c| self.clip_end(c) > first)
                    || self.project.track(track).is_some_and(|t| {
                        t.automation
                            .lanes
                            .iter()
                            .any(|l| l.curve.points().last().is_some_and(|p| p.time >= first))
                    });
                if reaches && let Some(t) = self.project.track(track) {
                    frozen.push(t.name.clone());
                }
                continue;
            }
            cmds.extend(clip_commands(
                &self.project,
                &mut ids,
                track,
                &shifts,
                ClipRipple::Cut,
            ));
            lanes.ripple(&mut self.project, track, &shifts);
        }
        self.project.ids = ids;
        if !frozen.is_empty() {
            self.note_frozen(&frozen);
        }
        let timeline = if global {
            global_commands(&self.project, &shifts, self.editor.ripple.globals())
        } else {
            Vec::new()
        };
        if self.history.in_gesture() {
            let mut moved = RippleMoved {
                timeline_from: (global && !timeline.is_empty()).then_some(first),
                ..RippleMoved::default()
            };
            for c in &cmds {
                match c {
                    Command::MoveClip { clip, .. } => {
                        moved.clips.insert(*clip);
                    }
                    Command::SplitClip { new_clip, .. } => {
                        moved.clips.insert(*new_clip);
                    }
                    _ => {}
                }
            }
            self.note_ripple_moved(moved);
        }
        (cmds, timeline)
    }

    /// Frozen tracks the ripple left out: a notice (once a gesture).
    fn note_frozen(&mut self, names: &[String]) {
        if self.history.in_gesture() {
            if self.ripple_gesture.noted {
                return;
            }
            self.ripple_gesture.noted = true;
        }
        self.notify(
            NoticeLevel::Warning,
            format!(
                "{} frozen: left out of the ripple (unfreeze to keep {} in sync)",
                names.join(", "),
                if names.len() == 1 { "it" } else { "them" }
            ),
        );
    }

    /// Add what a ripple motion moved to what the gesture's snapping
    /// leaves out.
    pub(crate) fn note_ripple_moved(&mut self, moved: RippleMoved) {
        if !self.history.in_gesture() {
            return;
        }
        let m = self
            .ripple_gesture
            .moved
            .get_or_insert_with(RippleMoved::default);
        m.clips.extend(moved.clips);
        m.timeline_from = match (m.timeline_from, moved.timeline_from) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }

    /// Is `clip` being moved by the running ripple drag?
    pub(crate) fn ripple_moving_clip(&self, clip: ClipId) -> bool {
        self.ripple_gesture
            .moved
            .as_ref()
            .is_some_and(|m| m.clips.contains(&clip))
    }

    /// From where the running ripple drag moves the timeline's parts.
    pub(crate) fn ripple_moving_timeline(&self) -> Option<MusicalTime> {
        self.ripple_gesture
            .moved
            .as_ref()
            .and_then(|m| m.timeline_from)
    }

    /// The commands moving `track`'s clips (but `skip`, which the edit
    /// places itself) by `shifts` — none is cut — with its automation in
    /// `lanes`: a track the edit is on.
    pub(crate) fn ripple_moved_commands(
        &mut self,
        track: TrackId,
        shifts: &[RippleShift],
        skip: &BTreeSet<ClipId>,
        lanes: &mut LaneWork,
    ) -> Vec<Command> {
        let mut ids = self.project.ids.clone();
        let cmds = clip_commands(
            &self.project,
            &mut ids,
            track,
            shifts,
            ClipRipple::Moved { skip },
        );
        self.project.ids = ids;
        lanes.ripple(&mut self.project, track, shifts);
        if self.history.in_gesture() {
            let clips = cmds
                .iter()
                .filter_map(|c| match c {
                    Command::MoveClip { clip, .. } => Some(*clip),
                    _ => None,
                })
                .collect();
            self.note_ripple_moved(RippleMoved {
                clips,
                timeline_from: None,
            });
        }
        cmds
    }

    /// Before a ripple drag's motion: take back what the gesture did since
    /// its first ripple (marked now if this is it), so the motion is worked
    /// out afresh from how things were (see the module docs). Nothing
    /// outside a gesture.
    pub(crate) fn rewind_ripple(&mut self) -> Result<()> {
        if !self.history.mark() {
            return Ok(());
        }
        let impact = self.history.rewind(&mut self.project)?;
        if impact > Impact::None {
            let before = self.ripple_gesture.rewound.unwrap_or(Impact::None);
            self.ripple_gesture.rewound = Some(before.max(impact));
        }
        // What the motion moves is found again.
        self.ripple_gesture.moved = None;
        Ok(())
    }

    /// Apply a ripple drag's motion as `label` (see [`Self::rewind_ripple`]):
    /// the engine learns of the rewind too, with the edit or on its own.
    pub(crate) fn ripple_batch(&mut self, label: &str, cmds: Vec<Command>) -> Result<()> {
        let rewound = self.ripple_gesture.rewound.take();
        let impact = cmds
            .iter()
            .map(Command::impact)
            .max()
            .unwrap_or(Impact::None);
        let applied = !cmds.is_empty();
        let r = self.batch(label, cmds);
        if let Some(i) = rewound
            && (!applied || r.is_err() || i > impact)
        {
            self.sync(i)?;
        }
        r
    }

    /// The gesture ended: its ripple state goes.
    pub(crate) fn ripple_gesture_ended(&mut self) {
        self.ripple_gesture = RippleGesture::default();
    }

    /// Move everything at or after `cut_time` by `delta_t`: on one track
    /// (`Some`: its clips and automation) or globally (`None`: every track
    /// not left out of rippling, and the timeline's parts the settings
    /// choose). A positive offset opens time at the cut, a negative one
    /// takes `[cut_time + delta_t, cut_time)` out; a clip across the cut
    /// is split there and the automation is split and its value duplicated
    /// ([`faderframe_automation::AutomationCurve::ripple`]). One undo step.
    pub fn apply_ripple_offset(
        &mut self,
        cut_time: MusicalTime,
        delta_t: MusicalTime,
        track_id: Option<TrackId>,
    ) -> Result<()> {
        let shift = RippleShift::new(cut_time, delta_t);
        if shift.is_empty() {
            return Ok(());
        }
        let tracks = match track_id {
            Some(t) => vec![t],
            None => self.ripple_followers(&[], RippleScope::All),
        };
        let mut lanes = LaneWork::default();
        let (mut cmds, timeline) =
            self.follow_commands(&[shift], &tracks, track_id.is_none(), &mut lanes);
        cmds.extend(lanes.commands());
        cmds.extend(timeline);
        self.batch("Ripple", cmds)
    }

    pub(crate) fn ripple_op(&mut self, op: RippleOp) -> Result<()> {
        match op {
            RippleOp::Settings(s) => {
                self.editor.ripple = s;
                self.revision += 1;
                Ok(())
            }
            RippleOp::Toggle => {
                if self.ripples() {
                    self.editor.ripple.on = false;
                    if self.editor.edit_mode == EditMode::Shuffle {
                        self.editor.edit_mode = EditMode::Slip;
                        self.editor.snap = false;
                    }
                } else {
                    self.editor.ripple.on = true;
                }
                self.revision += 1;
                Ok(())
            }
            RippleOp::DeleteClips(scope) => self.ripple_delete_clips(scope, None),
            RippleOp::DeleteRange(scope) => {
                self.clear_range_scoped("Ripple Delete", Some(scope))?;
                Ok(())
            }
            RippleOp::InsertTime(scope) => self.insert_time_scoped("Insert Time", Some(scope)),
            RippleOp::RemoveGaps(scope) => self.remove_gaps(scope),
            RippleOp::Offset { cut, delta, track } => self.apply_ripple_offset(cut, delta, track),
            RippleOp::Exclude { tracks, excluded } => {
                let cmds: Vec<Command> = tracks
                    .into_iter()
                    .filter(|t| {
                        self.project
                            .track(*t)
                            .is_some_and(|t| t.ripple_excluded != excluded)
                    })
                    .map(|track| Command::SetTrackRippleExcluded { track, excluded })
                    .collect();
                self.batch(
                    if excluded {
                        "Exclude from Ripple"
                    } else {
                        "Include in Ripple"
                    },
                    cmds,
                )
            }
        }
    }

    /// Delete the selected clips and close the time they took: on each
    /// clip's own track (Edited Tracks), or the time of all of them on the
    /// tracks kept in sync and the edited ones (Selected, All). `label`:
    /// the undo step (`None`: "Ripple Delete Clip(s)").
    pub(crate) fn ripple_delete_clips(
        &mut self,
        scope: RippleScope,
        label: Option<&str>,
    ) -> Result<()> {
        let clips = self.selected_arranger_clips();
        if clips.is_empty() {
            return Ok(());
        }
        let base: Vec<Clip> = clips
            .iter()
            .filter_map(|c| self.project.clip(*c).cloned())
            .collect();
        let mut by_track: BTreeMap<TrackId, Vec<&Clip>> = BTreeMap::new();
        for c in &base {
            by_track.entry(c.track).or_default().push(c);
        }
        let every: Vec<&Clip> = base.iter().collect();
        let all = removals(&spans_of(self, &every));
        let per_track: Vec<(TrackId, Vec<RippleShift>)> = by_track
            .iter()
            .map(|(t, cs)| {
                let own = if scope == RippleScope::Edited {
                    removals(&spans_of(self, cs))
                } else {
                    all.clone()
                };
                (*t, own)
            })
            .collect();
        let skip: BTreeSet<ClipId> = clips.iter().copied().collect();
        let mut cmds: Vec<Command> = clips
            .iter()
            .map(|&clip| Command::RemoveClip { clip })
            .collect();
        let mut lanes = LaneWork::default();
        let edited: Vec<TrackId> = per_track.iter().map(|(t, _)| *t).collect();
        for (track, shifts) in &per_track {
            cmds.extend(self.ripple_moved_commands(*track, shifts, &skip, &mut lanes));
        }
        let followers = self.ripple_followers(&edited, scope);
        let (follow, timeline) =
            self.follow_commands(&all, &followers, scope == RippleScope::All, &mut lanes);
        cmds.extend(follow);
        cmds.extend(lanes.commands());
        cmds.extend(timeline);
        let label = label.unwrap_or(if clips.len() == 1 {
            "Ripple Delete Clip"
        } else {
            "Ripple Delete Clips"
        });
        self.batch(label, cmds)?;
        for c in &clips {
            self.selection.clips.remove(c);
        }
        Ok(())
    }

    /// Close the gaps (see [`RippleOp::RemoveGaps`]).
    pub(crate) fn remove_gaps(&mut self, scope: RippleScope) -> Result<()> {
        let clips: Vec<Clip> = self
            .selected_arranger_clips()
            .into_iter()
            .filter_map(|c| self.project.clip(c).cloned())
            .collect();
        // Interior gaps of merged spans, a whole sample long at least.
        let gaps = |s: &Session, spans: &[(MusicalTime, MusicalTime)]| -> Vec<RippleShift> {
            spans
                .windows(2)
                .filter(|w| s.frames_between(w[0].1, w[1].0) > 0)
                .map(|w| RippleShift::remove(w[0].1, w[1].0))
                .collect()
        };
        let mut lanes = LaneWork::default();
        let mut cmds = Vec::new();
        let none = BTreeSet::new();
        if scope == RippleScope::All {
            // The time no track that ripples has a clip in, between the
            // selection's ends (the song's without one).
            let followers = self.ripple_followers(&[], RippleScope::All);
            let (lo, hi) = match (clips.iter().map(|c| c.start).min(), self.selection.range) {
                (Some(lo), _) => (
                    lo,
                    clips.iter().map(|c| self.clip_end(c)).max().unwrap_or(lo),
                ),
                (None, Some(r)) if !r.is_empty() => (r.start, r.end),
                _ => (MusicalTime::ZERO, MusicalTime::MAX),
            };
            let launcher = self.project.launcher_clips();
            let mut spans = Vec::new();
            for t in &followers {
                for c in self.project.clips_of(*t) {
                    if launcher.contains(&c.id) {
                        continue;
                    }
                    let (a, b) = (c.start.max(lo), self.clip_end(c).min(hi));
                    if b > a {
                        spans.push((a, b));
                    }
                }
            }
            let shifts = gaps(self, &merged(spans));
            let (follow, timeline) = self.follow_commands(&shifts, &followers, true, &mut lanes);
            cmds.extend(follow);
            cmds.extend(lanes.commands());
            cmds.extend(timeline);
            return self.batch("Remove Gaps", cmds);
        }
        if clips.is_empty() {
            return Ok(());
        }
        let mut by_track: BTreeMap<TrackId, Vec<&Clip>> = BTreeMap::new();
        for c in &clips {
            by_track.entry(c.track).or_default().push(c);
        }
        let every: Vec<&Clip> = clips.iter().collect();
        let all = gaps(self, &spans_of(self, &every));
        let per_track: Vec<(TrackId, Vec<RippleShift>)> = by_track
            .iter()
            .map(|(t, cs)| {
                let own = if scope == RippleScope::Edited {
                    gaps(self, &spans_of(self, cs))
                } else {
                    all.clone()
                };
                (*t, own)
            })
            .collect();
        let edited: Vec<TrackId> = per_track.iter().map(|(t, _)| *t).collect();
        for (track, shifts) in &per_track {
            cmds.extend(self.ripple_moved_commands(*track, shifts, &none, &mut lanes));
        }
        let followers = self.ripple_followers(&edited, scope);
        let (follow, timeline) = self.follow_commands(&all, &followers, false, &mut lanes);
        cmds.extend(follow);
        cmds.extend(lanes.commands());
        cmds.extend(timeline);
        self.batch("Remove Gaps", cmds)
    }

    /// The commands pushing what follows `moved` (as the drag found them)
    /// on their own tracks `by` ticks along with them — clips from the
    /// first moved one on (those passed over stay), the automation from
    /// there — and the tracks kept in sync after the first of them.
    pub(crate) fn push_commands(&mut self, moved: &[Clip], by: i64) -> Vec<Command> {
        let Some(scope) = self.ripple_scope() else {
            return Vec::new();
        };
        if by == 0 || moved.is_empty() {
            return Vec::new();
        }
        let by = MusicalTime(by);
        let mut from: BTreeMap<TrackId, MusicalTime> = BTreeMap::new();
        for c in moved {
            let e = from.entry(c.track).or_insert(c.start);
            *e = (*e).min(c.start);
        }
        let skip: BTreeSet<ClipId> = moved.iter().map(|c| c.id).collect();
        let mut cmds = Vec::new();
        let mut lanes = LaneWork::default();
        let mut pushed = BTreeSet::new();
        for (&track, &at) in &from {
            for c in self.project.clips_of(track) {
                if !skip.contains(&c.id) && c.start >= at {
                    let start = (c.start + by).max(MusicalTime::ZERO);
                    cmds.push(Command::MoveClip {
                        clip: c.id,
                        track,
                        start,
                    });
                    pushed.insert(c.id);
                }
            }
            lanes.ripple(&mut self.project, track, &[RippleShift::new(at, by)]);
        }
        self.note_ripple_moved(RippleMoved {
            clips: pushed,
            timeline_from: None,
        });
        let edited: Vec<TrackId> = from.keys().copied().collect();
        let first = from.values().copied().min().unwrap_or(MusicalTime::ZERO);
        let followers = self.ripple_followers(&edited, scope);
        let (follow, timeline) = self.follow_commands(
            &[RippleShift::new(first, by)],
            &followers,
            scope == RippleScope::All,
            &mut lanes,
        );
        cmds.extend(follow);
        cmds.extend(lanes.commands());
        cmds.extend(timeline);
        cmds
    }
}
