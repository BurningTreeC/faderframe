//! Folder groups (issue #5, Studio One's folder Group button): a folder's
//! Group switch ([`faderframe_project::Track::folder_group`], folders and
//! folder buses) makes the tracks with clips it holds — at every level —
//! one for recording and editing, so a dry and a wet guitar recorded on two
//! tracks are handled as one track.
//!
//! * **Who is grouped**: `Project::edit_group` — the outermost grouped
//!   folder a track is in (a folder: itself too). A grouped folder inside a
//!   grouped folder is part of the outer group; its own switch counts again
//!   once the outer one is off. So a track is in one group at most, and
//!   expansions are done once, never chained from group to group (no
//!   recursion). Members are audio, instrument and MIDI tracks; folders,
//!   buses, auxes and VCAs inside are skipped.
//! * **Record arm**: arming or disarming a member arms or disarms the
//!   others, however the arm comes (a button, Shift+R's batch, a surface,
//!   MIDI learn) — [`Session::folder_group_arms`] in `Session::edit`, in the
//!   same undo step.
//! * **Corresponding clips** ([`Session::grouped_clips`]): a clip on a
//!   member shares its place with a clip on another member when they
//!   overlap by at least half of the shorter one — the same take on the
//!   other track, a piece of a longer clip (and that longer clip), but not
//!   a neighbour that only touches it in a crossfade. A clip's
//!   corresponding clips are those sharing its place, and on from those
//!   (the pieces of a clip cut up only on one track all go with it): the
//!   same set whichever of them is clicked, so expanding twice changes
//!   nothing and the members stay aligned. Measured where the running
//!   gesture found the clips (a drag's moves do not change what
//!   corresponds), and only clips of the arranger (never the launcher's).
//! * **Selection**: selecting a clip selects its corresponding clips (Ctrl
//!   toggles them with it); a range on a member covers every member
//!   (`Session::edit_tracks`, which every range operation and the range's
//!   painting use; the track selection itself stays as clicked, so faders
//!   and mutes are not linked). Switching a group on extends the current
//!   selection.
//! * **Edits**: every edit of clips the views name (move, trim, stretch,
//!   fades, clip gain, slip, clip mute, Spot, Shuffle) and of the selected
//!   clips (delete, cut, copy, duplicate and repeat through the clipboard,
//!   nudge, split at the playhead) takes the corresponding clips along —
//!   one place: [`Session::grouped_action`] for the named ones, the
//!   selection (and `selected_arranger_clips`) for the others — so each is
//!   one undo step as before. A split at the playhead cuts every clip of
//!   the group the playhead crosses on a member a selected clip is cut on.
//!   Operations that do not apply to a clip (fades and gain of MIDI clips,
//!   take folders that do not trim) skip it as they always did; quantize,
//!   warp and analysis commands work per clip and are not expanded. A
//!   frozen member refuses the group's clip edits as it refuses its own
//!   (nothing changes; the notice names it), so the members never drift
//!   apart.

use crate::{Action, Result, SelectMode, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::Command;
use faderframe_timeline::MusicalTime;
use std::collections::{BTreeSet, HashMap, HashSet};

impl Session {
    /// Is any folder grouped? (Nothing to expand otherwise.)
    fn any_folder_grouped(&self) -> bool {
        self.project
            .tracks
            .iter()
            .any(|t| t.folder_group && t.is_folder())
    }

    /// Is `folder`'s Group switch on?
    pub fn folder_grouped(&self, folder: TrackId) -> bool {
        self.project
            .track(folder)
            .is_some_and(|t| t.is_folder() && t.folder_group)
    }

    /// The grouped folder `track` is edited with (see
    /// `Project::edit_group`), if any.
    pub fn edit_group_of(&self, track: TrackId) -> Option<TrackId> {
        let t = self.project.track(track)?;
        self.project.edit_group(t).map(|g| g.id)
    }

    /// `tracks` and the tracks grouped with them (new ones after, in
    /// project order).
    pub(crate) fn with_grouped_tracks(&self, mut tracks: Vec<TrackId>) -> Vec<TrackId> {
        if !self.any_folder_grouped() {
            return tracks;
        }
        let mut extra: BTreeSet<TrackId> = BTreeSet::new();
        for t in &tracks {
            extra.extend(self.project.grouped_with(*t));
        }
        let order: Vec<TrackId> = self.project.tracks.iter().map(|t| t.id).collect();
        let mut extra: Vec<TrackId> = extra.into_iter().filter(|t| !tracks.contains(t)).collect();
        extra.sort_by_key(|t| order.iter().position(|o| o == t));
        tracks.extend(extra);
        tracks
    }

    /// `clips` and their corresponding clips on the tracks they are grouped
    /// with (see the module docs), the given ones first. Unchanged when
    /// nothing is grouped. The same for every clip of the result (the
    /// clips of a place go together, whichever is asked for).
    pub fn grouped_clips(&self, clips: &[ClipId]) -> Vec<ClipId> {
        let mut seen: HashSet<ClipId> = HashSet::with_capacity(clips.len());
        let mut out: Vec<ClipId> = clips.iter().copied().filter(|c| seen.insert(*c)).collect();
        if !self.any_folder_grouped() {
            return out;
        }
        let p = &self.project;
        let launcher = p.launcher_clips();
        // As the running gesture found them.
        let base = |id: ClipId| self.gesture_base.get(&id).or_else(|| p.clip(id));
        let mut mates: HashMap<TrackId, Vec<TrackId>> = HashMap::new();
        // Each member's clips once: id, start, end.
        let mut spans: HashMap<TrackId, Vec<(ClipId, MusicalTime, MusicalTime)>> = HashMap::new();
        // From each clip found to the clips it shares its place with on
        // the other members, and on from those.
        let mut next = 0;
        while next < out.len() {
            let id = out[next];
            next += 1;
            if launcher.contains(&id) {
                continue;
            }
            let Some(c) = base(id) else { continue };
            let (start, end) = (c.start, self.clip_end(c));
            let others = mates
                .entry(c.track)
                .or_insert_with(|| p.grouped_with(c.track))
                .clone();
            for m in others {
                let list = spans.entry(m).or_insert_with(|| {
                    // On the track now or, moved away in this gesture,
                    // where the gesture found them.
                    let mut ids: BTreeSet<ClipId> = p
                        .track(m)
                        .map(|t| t.clips.iter().copied().collect())
                        .unwrap_or_default();
                    ids.extend(self.gesture_base.keys().copied());
                    ids.into_iter()
                        .filter(|o| !launcher.contains(o))
                        .filter_map(base)
                        .filter(|o| o.track == m)
                        .map(|o| (o.id, o.start, self.clip_end(o)))
                        .collect()
                });
                for &(o, s, e) in list.iter() {
                    if shares_place((start, end), (s, e)) && seen.insert(o) {
                        out.push(o);
                    }
                }
            }
        }
        out
    }

    /// [`Self::grouped_clips`] for the targets of an edit: within a
    /// gesture the expansion of its first event is kept (every event of a
    /// drag names the same clips).
    fn grouped_targets(&mut self, clips: Vec<ClipId>) -> Vec<ClipId> {
        if !self.any_folder_grouped() {
            return clips;
        }
        if !self.history.in_gesture() {
            self.grouped_targets = None;
            return self.grouped_clips(&clips);
        }
        if let Some((asked, found)) = &self.grouped_targets
            && *asked == clips
        {
            return found.clone();
        }
        let found = self.grouped_clips(&clips);
        self.grouped_targets = Some((clips, found.clone()));
        found
    }

    /// Select `clips` with their corresponding clips: Replace and Add take
    /// them all; Toggle sets each clip's corresponding clips as the clip
    /// itself becomes (on or off).
    pub(crate) fn select_clips_grouped(&mut self, clips: &[ClipId], mode: SelectMode) {
        if !self.any_folder_grouped() {
            self.selection.select_clips(clips, mode);
            return;
        }
        match mode {
            SelectMode::Replace | SelectMode::Add => {
                let all = self.grouped_clips(clips);
                // The anchor stays the clicked clip.
                self.selection.select_clips(clips, mode);
                self.selection.select_clips(&all, SelectMode::Add);
            }
            SelectMode::Toggle => {
                let (off, on): (Vec<ClipId>, Vec<ClipId>) = clips
                    .iter()
                    .copied()
                    .partition(|c| self.selection.clips.contains(c));
                let off = self.grouped_clips(&off);
                let on = self.grouped_clips(&on);
                self.selection.select_clips(clips, SelectMode::Toggle);
                for c in off {
                    self.selection.clips.remove(&c);
                }
                self.selection.clips.extend(on);
            }
        }
    }

    /// The clips a split at `at` of `cut` (clips it crosses) cuts besides
    /// them in folder groups: every clip `at` crosses on the other members
    /// of the groups the cut clips are in.
    pub(crate) fn grouped_cuts(
        &self,
        cut: &[faderframe_project::Clip],
        at: MusicalTime,
    ) -> Vec<faderframe_project::Clip> {
        if !self.any_folder_grouped() {
            return Vec::new();
        }
        let mut seen: HashSet<ClipId> = cut.iter().map(|c| c.id).collect();
        let mut tracks: BTreeSet<TrackId> = BTreeSet::new();
        for c in cut {
            tracks.extend(self.project.grouped_with(c.track));
        }
        let mut out = Vec::new();
        for t in tracks {
            for c in self.project.clips_of(t) {
                if self.cuts_inside(c, at) && seen.insert(c.id) {
                    out.push(c.clone());
                }
            }
        }
        out
    }

    /// The selected clips with their corresponding clips.
    pub(crate) fn grouped_selection(&self) -> Vec<ClipId> {
        let selected: Vec<ClipId> = self.selection.clips.iter().copied().collect();
        self.grouped_clips(&selected)
    }

    /// Switch `folder`'s Group on or off (one undo step). Switched on, the
    /// selection extends to the clips corresponding to the selected ones.
    pub(crate) fn set_folder_group(&mut self, folder: TrackId, on: bool) -> Result<()> {
        let f = self
            .project
            .track(folder)
            .filter(|t| t.is_folder())
            .ok_or_else(|| SessionError::Other("that track is not a folder".into()))?;
        if f.folder_group == on {
            return Ok(());
        }
        self.edit(Command::SetFolderGroup { track: folder, on })?;
        if on {
            let all = self.grouped_selection();
            self.selection.clips.extend(all);
            self.revision += 1;
        }
        Ok(())
    }

    /// Arming or disarming a member of a folder group arms or disarms the
    /// other members too (wherever the arm comes from: a single edit or a
    /// batch); one command, one undo step. Members already set, or set by
    /// the command itself, are left alone.
    pub(crate) fn folder_group_arms(&self, cmd: Command) -> Command {
        fn arms(cmd: &Command, out: &mut Vec<(TrackId, bool)>) {
            match cmd {
                Command::SetTrackRecordArm { track, on } => out.push((*track, *on)),
                Command::Batch { commands, .. } => {
                    for c in commands {
                        arms(c, out);
                    }
                }
                _ => {}
            }
        }
        if !self.any_folder_grouped() {
            return cmd;
        }
        let mut set = Vec::new();
        arms(&cmd, &mut set);
        if set.is_empty() {
            return cmd;
        }
        let mut done: HashSet<TrackId> = set.iter().map(|(t, _)| *t).collect();
        let mut more = Vec::new();
        for (track, on) in set {
            for m in self.project.grouped_with(track) {
                if self.project.track(m).is_some_and(|t| t.record_arm != on) && done.insert(m) {
                    more.push(Command::SetTrackRecordArm { track: m, on });
                }
            }
        }
        if more.is_empty() {
            return cmd;
        }
        Command::Batch {
            label: cmd.label(),
            commands: std::iter::once(cmd).chain(more).collect(),
        }
    }

    /// `action` with the clips it edits extended by their corresponding
    /// clips (see the module docs); other actions as they are.
    pub(crate) fn grouped_action(&mut self, action: Action) -> Action {
        if !self.any_folder_grouped() {
            return action;
        }
        match action {
            Action::MoveClips { clips, by, tracks } => Action::MoveClips {
                clips: self.grouped_targets(clips),
                by,
                tracks,
            },
            Action::TrimClips {
                clips,
                edge,
                by,
                stretch,
            } => Action::TrimClips {
                clips: self.grouped_targets(clips),
                edge,
                by,
                stretch,
            },
            Action::ClipGain { clips, delta_db } => Action::ClipGain {
                clips: self.grouped_targets(clips),
                delta_db,
            },
            Action::SetClipsGain { clips, db } => Action::SetClipsGain {
                clips: self.grouped_targets(clips),
                db,
            },
            Action::SetFade {
                clips,
                edge,
                length,
                shape,
                bend,
            } => Action::SetFade {
                clips: self.grouped_targets(clips),
                edge,
                length,
                shape,
                bend,
            },
            Action::SlipClips { clips, by } => Action::SlipClips {
                clips: self.grouped_targets(clips),
                by,
            },
            Action::SetClipsMuted { clips, muted } => Action::SetClipsMuted {
                clips: self.grouped_targets(clips),
                muted,
            },
            // A typed position: the others move as far.
            Action::SpotClip { clip, start } => {
                let all = self.grouped_clips(&[clip]);
                match self.project.clip(clip) {
                    Some(c) if all.len() > 1 => Action::MoveClips {
                        clips: all,
                        by: start.max(MusicalTime::ZERO).ticks() - c.start.ticks(),
                        tracks: 0,
                    },
                    _ => Action::SpotClip { clip, start },
                }
            }
            other => other,
        }
    }

    /// Shuffle a clip, its corresponding clips along on their own tracks
    /// by as far (when it stays on its track); one undo step.
    pub(crate) fn shuffle_grouped(
        &mut self,
        clip: ClipId,
        track: TrackId,
        at: MusicalTime,
    ) -> Result<()> {
        let Some(c) = self.project.clip(clip).cloned() else {
            return self.shuffle_clip(clip, track, at);
        };
        let others: Vec<(ClipId, TrackId, MusicalTime)> = if track == c.track {
            self.grouped_clips(&[clip])
                .into_iter()
                .skip(1)
                .filter_map(|o| self.project.clip(o))
                .map(|o| {
                    let to = MusicalTime((o.start.ticks() + at.ticks() - c.start.ticks()).max(0));
                    (o.id, o.track, to)
                })
                .collect()
        } else {
            Vec::new()
        };
        if others.is_empty() {
            return self.shuffle_clip(clip, track, at);
        }
        self.history.begin("Shuffle Clip");
        let mut result = self.shuffle_clip(clip, track, at);
        for (o, t, to) in others {
            if result.is_err() {
                break;
            }
            result = self.shuffle_clip(o, t, to);
        }
        self.history.end();
        result
    }
}

/// Do two clips on different members share their place: do they overlap
/// by at least half of the shorter one? (A crossfade's overlap is not
/// enough; a piece inside a longer clip is.)
fn shares_place(a: (MusicalTime, MusicalTime), b: (MusicalTime, MusicalTime)) -> bool {
    let overlap = a.1.min(b.1).ticks() - a.0.max(b.0).ticks();
    let shorter = (a.1 - a.0).ticks().min((b.1 - b.0).ticks());
    overlap > 0 && 2 * overlap >= shorter
}

#[cfg(test)]
mod tests {
    use super::shares_place;
    use faderframe_timeline::MusicalTime;

    #[test]
    fn clips_share_their_place_by_half_of_the_shorter() {
        let q = |a: f64, b: f64| (MusicalTime::from_quarters(a), MusicalTime::from_quarters(b));
        assert!(shares_place(q(0.0, 4.0), q(0.0, 4.0)), "the same take");
        assert!(shares_place(q(0.0, 16.0), q(4.0, 5.0)), "a piece inside");
        assert!(shares_place(q(4.0, 5.0), q(0.0, 16.0)), "both ways");
        assert!(shares_place(q(0.0, 4.0), q(2.0, 6.0)), "half");
        assert!(!shares_place(q(0.0, 4.0), q(3.5, 8.0)), "a crossfade");
        assert!(!shares_place(q(0.0, 4.0), q(4.0, 8.0)), "butted");
    }
}
