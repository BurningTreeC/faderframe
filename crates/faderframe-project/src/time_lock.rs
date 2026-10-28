//! Time-locked tracks: material placed in time rather than with the music.
//!
//! Positions are stored in musical time ([`MusicalTime`]) everywhere, so a
//! tempo change moves what is placed in bars and beats — right for music,
//! wrong for a film's dialogue and effects or a performance recorded to
//! picture, which must stay where they sound while the picture stays. A
//! track's [`crate::TimeBase`] says which it is. On a time-locked track every
//! edit that changes the tempo map is followed, in the same undo step, by
//! moves that put its material back on the samples it had:
//!
//! * **Clips** keep their start sample: audio clips and take folders (their
//!   length is in frames already), and MIDI clips' starts. The notes in a
//!   MIDI clip are music: they keep their places in the clip's bars and
//!   beats and play at the tempo from the clip's start (Logic's region
//!   lock) — a cue starts on its hit point and its notes follow the tempo
//!   the composer gives it; a MIDI hit that must stay put is a clip of its
//!   own.
//! * **Automation** keeps every point's sample (its value and shape kept).
//! * A **frozen** track's render keeps its sample too.
//! * **ADR cues** are always locked, whatever any track's time base: a cue
//!   marks a line of the picture's dialogue, and the picture never moves
//!   with the tempo. Their start and end keep their samples (one `SetAdr`
//!   in the same undo step), and the beeps' track the session makes for
//!   them is time-locked.
//!
//! The rule for one edit (a command, often a batch): material keeps the
//! sample it had unless the edit places it; a position the edit gives is
//! read against the tempo map in effect where the edit gives it — so the
//! clip commands of a ripple (worked out with the tempo before the batch,
//! which changes it last) and a rearrangement (whose positions are in its
//! own new timeline) both land where they mean. Positions given to clips
//! on time-locked tracks always end on whole samples (moves, trims,
//! splits, paste, recording): the grid snaps as the user asks, onto the
//! sample under the grid line. Every conversion goes through whole
//! samples at the project rate ([`TempoMap::musical_at_sample`]), so any
//! number of tempo changes — a drag of the tempo lane is many — leave the
//! sample exactly where it was.
//!
//! [`plan`] looks at an edit before it applies; [`TimeLock::keep`] gives
//! the moves after it. The session runs both around every edit
//! (`faderframe_session::Session::edit`); undo restores the exact
//! positions the moves replaced.

use crate::{Command, Project};
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationPoint};
use faderframe_core::{AdrCueId, AutomationLaneId, ClipId, TrackId};
use faderframe_timeline::{MusicalTime, TempoMap, Timeline};
use std::collections::{BTreeMap, BTreeSet};

/// An edit's effect on time-locked material, worked out before it applies
/// (see the module docs).
#[derive(Debug)]
pub struct TimeLock {
    rate: f64,
    /// The tempo maps the edit goes through, in order: the first is the
    /// project's before it.
    maps: Vec<TempoMap>,
    /// Clips the edit places: the tempo map in effect then (an index into
    /// `maps`; the last placement counts).
    placed: BTreeMap<ClipId, usize>,
    /// Lanes the edit sets, likewise.
    placed_lanes: BTreeMap<(TrackId, AutomationLaneId), usize>,
    /// Freezes the edit sets.
    placed_freezes: BTreeSet<TrackId>,
    /// Time-locked tracks' clips before the edit (start).
    clips: BTreeMap<ClipId, MusicalTime>,
    /// Their lanes before the edit.
    lanes: BTreeMap<(TrackId, AutomationLaneId), AutomationCurve>,
    /// Their freezes' starts before the edit.
    freezes: BTreeMap<TrackId, MusicalTime>,
    /// The ADR cues the edit sets (the tempo map in effect then).
    placed_adr: Option<usize>,
    /// The ADR cues before the edit (start, end).
    cues: BTreeMap<AdrCueId, (MusicalTime, MusicalTime)>,
}

/// Does `cmd` (or a command in it) change the tempo map?
pub fn changes_tempo(cmd: &Command) -> bool {
    match cmd {
        Command::SetTempo { .. } | Command::SetTimeline { .. } | Command::SetArrangement { .. } => {
            true
        }
        Command::Batch { commands, .. } => commands.iter().any(changes_tempo),
        _ => false,
    }
}

/// Does `cmd` switch the arrangement shown (a scratch pad)? Its clips and
/// tempo are then another arrangement's: nothing to keep.
fn shows_arrangement(cmd: &Command) -> bool {
    match cmd {
        Command::ShowArrangement { .. } => true,
        Command::Batch { commands, .. } => commands.iter().any(shows_arrangement),
        _ => false,
    }
}

/// The project's rate as the engine converts positions with it.
fn rate_of(p: &Project) -> f64 {
    f64::from(p.sample_rate.max(1))
}

/// A curve with every point's time moved from where it sounds under `from`
/// to the same sample under `to`.
pub fn remap_curve(
    curve: &AutomationCurve,
    from: &TempoMap,
    to: &TempoMap,
    rate: f64,
) -> AutomationCurve {
    AutomationCurve::from_points(
        curve
            .points()
            .iter()
            .map(|q| AutomationPoint {
                time: to.musical_at_sample(from.musical_to_samples(q.time, rate), rate),
                ..*q
            })
            .collect(),
    )
}

/// Ripple a time-locked track's curve in time: each shift takes out or
/// opens the time its span lasts under `tl` (in samples), so the points
/// keep their spacing in time across tempo changes (see
/// [`crate::ripple::RippleShift::map_in_time`]). The shifts are in the
/// coordinates before them all, as [`AutomationCurve::ripple`] takes them.
pub fn ripple_curve_in_time(
    curve: &mut AutomationCurve,
    shifts: &[crate::ripple::RippleShift],
    tl: &Timeline,
    rate: f64,
) {
    let shifts = crate::ripple::RippleShift::ordered(shifts);
    if shifts.is_empty() || curve.is_empty() {
        return;
    }
    let s = |t: MusicalTime| MusicalTime(tl.to_samples(t, rate));
    // In samples, as though they were ticks: the curve's ripple is the
    // same in any unit.
    let mut timed = AutomationCurve::from_points(
        curve
            .points()
            .iter()
            .map(|q| AutomationPoint {
                time: s(q.time),
                ..*q
            })
            .collect(),
    );
    for sh in &shifts {
        let cut = s(sh.cut);
        timed.ripple(cut, s(sh.cut + sh.delta) - cut);
    }
    *curve = AutomationCurve::from_points(
        timed
            .points()
            .iter()
            .map(|q| AutomationPoint {
                time: tl.at_sample(q.time.ticks(), rate),
                ..*q
            })
            .collect(),
    );
}

/// What a time-locked project must keep through `cmd` (`None`: nothing —
/// no track is time-locked, or the edit neither changes the tempo nor
/// places anything on a time-locked track).
pub fn plan(p: &Project, cmd: &Command) -> Option<TimeLock> {
    if (!p.tracks.iter().any(|t| t.is_time_locked()) && p.adr.cues.is_empty())
        || shows_arrangement(cmd)
    {
        return None;
    }
    let mut lock = TimeLock {
        rate: rate_of(p),
        maps: vec![p.timeline.tempo.clone()],
        placed: BTreeMap::new(),
        placed_lanes: BTreeMap::new(),
        placed_freezes: BTreeSet::new(),
        clips: BTreeMap::new(),
        lanes: BTreeMap::new(),
        freezes: BTreeMap::new(),
        placed_adr: None,
        cues: BTreeMap::new(),
    };
    let mut on_locked = false;
    lock.walk(p, cmd, &mut on_locked);
    if lock.maps.len() == 1 && !on_locked {
        return None;
    }
    if lock.maps.len() > 1 {
        for t in p.tracks.iter().filter(|t| t.is_time_locked()) {
            for c in p.clips_of(t.id) {
                lock.clips.insert(c.id, c.start);
            }
            for l in &t.automation.lanes {
                lock.lanes.insert((t.id, l.id), l.curve.clone());
            }
            if let Some(f) = &t.freeze {
                lock.freezes.insert(t.id, f.start);
            }
        }
        for c in &p.adr.cues {
            lock.cues.insert(c.id, (c.start, c.end));
        }
    }
    Some(lock)
}

impl TimeLock {
    /// The index of the tempo map in effect now.
    fn now(&self) -> usize {
        self.maps.len() - 1
    }

    /// A new tempo map from here on (when it differs).
    fn tempo(&mut self, map: TempoMap) {
        if self.maps.last() != Some(&map) {
            self.maps.push(map);
        }
    }

    fn place(&mut self, p: &Project, clip: ClipId, track: Option<TrackId>, on_locked: &mut bool) {
        let track = track.or_else(|| p.clip(clip).map(|c| c.track));
        // A clip made earlier in the same edit: its track is not known
        // yet, so it counts.
        *on_locked |= track.is_none_or(|t| p.track(t).is_some_and(|t| t.is_time_locked()));
        self.placed.insert(clip, self.now());
    }

    /// Note what `cmd` places and the tempo maps it goes through, in order.
    fn walk(&mut self, p: &Project, cmd: &Command, on_locked: &mut bool) {
        match cmd {
            Command::Batch { commands, .. } => {
                for c in commands {
                    self.walk(p, c, on_locked);
                }
            }
            Command::SetTempo { bpm } => {
                if bpm.is_finite() {
                    let mut map = self.maps[self.now()].clone();
                    map.set_initial_bpm(*bpm);
                    self.tempo(map);
                }
            }
            Command::SetTimeline { timeline } => self.tempo(timeline.tempo.clone()),
            Command::SetArrangement { arrangement } => {
                self.tempo(arrangement.timeline.tempo.clone());
                // What it moves (or makes) it places in its own timeline;
                // what it leaves where it was keeps its sample.
                for (track, clips, set) in &arrangement.tracks {
                    let Some(t) = p.track(*track).filter(|t| t.is_time_locked()) else {
                        continue;
                    };
                    for id in clips {
                        let moved = match (arrangement.clips.get(id), p.clip(*id)) {
                            (Some(new), Some(old)) => {
                                new.start != old.start || new.track != old.track
                            }
                            _ => true,
                        };
                        if moved {
                            *on_locked = true;
                            self.placed.insert(*id, self.now());
                        }
                    }
                    for l in &set.lanes {
                        let same = t
                            .automation
                            .lanes
                            .iter()
                            .any(|o| o.id == l.id && o.curve == l.curve);
                        if !same {
                            self.placed_lanes.insert((*track, l.id), self.now());
                        }
                    }
                }
            }
            Command::MoveClip { clip, track, .. } => self.place(p, *clip, Some(*track), on_locked),
            Command::SetClipContent { clip, .. } => self.place(p, *clip, None, on_locked),
            Command::AddClip { clip } => self.place(p, clip.id, Some(clip.track), on_locked),
            Command::SplitClip { clip, new_clip, .. } => {
                let track = p.clip(*clip).map(|c| c.track);
                self.place(p, *new_clip, track, on_locked);
            }
            Command::SetAutomationLane { track, lane }
            | Command::AddAutomationLane { track, lane } => {
                self.placed_lanes.insert((*track, lane.id), self.now());
            }
            Command::SetTrackFreeze { track, .. } => {
                self.placed_freezes.insert(*track);
            }
            Command::SetAdr { .. } => self.placed_adr = Some(self.now()),
            _ => {}
        }
    }

    /// The moves keeping time-locked material in time after the edit
    /// applied to `p` (see the module docs): clips back on their samples
    /// (and given ones onto whole samples), lanes' points back on theirs,
    /// frozen renders too.
    pub fn keep(&self, p: &Project) -> Vec<Command> {
        let rate = self.rate;
        let now = &p.timeline.tempo;
        let tempo_changed = self.maps.len() > 1;
        let mut cmds = Vec::new();
        for t in p.tracks.iter().filter(|t| t.is_time_locked()) {
            for c in p.clips_of(t.id) {
                let from = match (self.placed.get(&c.id), self.clips.get(&c.id)) {
                    (Some(&i), _) => &self.maps[i],
                    // Left where it was: its sample before the edit.
                    (None, Some(&start)) if start == c.start && tempo_changed => &self.maps[0],
                    _ => continue,
                };
                let start = now.musical_at_sample(from.musical_to_samples(c.start, rate), rate);
                if start != c.start {
                    cmds.push(Command::MoveClip {
                        clip: c.id,
                        track: t.id,
                        start,
                    });
                }
            }
            if !tempo_changed {
                continue;
            }
            for l in &t.automation.lanes {
                let key = (t.id, l.id);
                let from = match (self.placed_lanes.get(&key), self.lanes.get(&key)) {
                    (Some(&i), _) => &self.maps[i],
                    (None, Some(curve)) if *curve == l.curve => &self.maps[0],
                    _ => continue,
                };
                if from == now {
                    continue;
                }
                let curve = remap_curve(&l.curve, from, now, rate);
                if curve != l.curve {
                    cmds.push(Command::SetAutomationLane {
                        track: t.id,
                        lane: Box::new(AutomationLane { curve, ..l.clone() }),
                    });
                }
            }
            if let Some(f) = &t.freeze
                && !self.placed_freezes.contains(&t.id)
                && self.freezes.get(&t.id) == Some(&f.start)
            {
                let start =
                    now.musical_at_sample(self.maps[0].musical_to_samples(f.start, rate), rate);
                if start != f.start {
                    cmds.push(Command::SetTrackFreeze {
                        track: t.id,
                        freeze: Some(crate::Freeze { start, ..f.clone() }),
                    });
                }
            }
        }
        if let Some(adr) = self.keep_adr(p) {
            cmds.push(Command::SetAdr { adr: Box::new(adr) });
        }
        cmds
    }

    /// The ADR cues back on their samples after a tempo change (`None`:
    /// none moved): cues the edit set are read against the tempo map in
    /// effect where it set them, the others were where they were.
    fn keep_adr(&self, p: &Project) -> Option<crate::adr::Adr> {
        let now = &p.timeline.tempo;
        if self.maps.len() < 2 || p.adr.cues.is_empty() {
            return None;
        }
        let at = |from: &TempoMap, t: MusicalTime| {
            now.musical_at_sample(from.musical_to_samples(t, self.rate), self.rate)
        };
        let mut adr = p.adr.clone();
        let mut moved = false;
        for c in &mut adr.cues {
            let from = match (self.placed_adr, self.cues.get(&c.id)) {
                (Some(i), _) => &self.maps[i],
                (None, Some(&(start, end))) if (start, end) == (c.start, c.end) => &self.maps[0],
                _ => continue,
            };
            if from == now {
                continue;
            }
            let (start, end) = (at(from, c.start), at(from, c.end));
            if (start, end) != (c.start, c.end) {
                (c.start, c.end) = (start, end);
                moved = true;
            }
        }
        moved.then_some(adr)
    }
}

#[cfg(test)]
mod tests;
