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
//!   the composer gives it.
//! * **Automation** keeps every point's sample (its value and shape kept).
//! * A **frozen** track's render keeps its sample too.
//!
//! Some material is placed in time on any track, musical or not:
//!
//! * **Locked notes** ([`crate::MidiNote::time_locked`]): a hit inside a
//!   musical cue. A tempo change keeps the note's start and end on their
//!   samples (its expression points on theirs) while the clip and its
//!   other notes follow the beats. A clip the edit moves carries its
//!   locked notes along at the same time from its start. A locked note
//!   the tempo change would take out of its clip makes the clip grow to
//!   hold it, so the hit is still heard where it was.
//! * **Locked markers and sections** ([`crate::Marker::time_base`],
//!   [`crate::Section::time_base`]): a picture cut, a scene or a reel keep
//!   their samples (a section both bounds).
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

use crate::{Clip, ClipContent, Command, MidiClip, Project};
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationPoint};
use faderframe_core::{AdrCueId, AutomationLaneId, ClipId, MarkerId, SectionId, TrackId};
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
    /// Clips whose notes the edit sets (the tempo map in effect then).
    placed_notes: BTreeMap<ClipId, usize>,
    /// Clips holding locked notes before the edit (their start and
    /// content): those a tempo change keeps, or the edit moves or sets.
    notes: BTreeMap<ClipId, (MusicalTime, MidiClip)>,
    /// Markers the edit sets (the tempo map in effect then).
    placed_markers: BTreeMap<MarkerId, usize>,
    /// Locked markers before the edit.
    markers: BTreeMap<MarkerId, MusicalTime>,
    /// Sections the edit sets.
    placed_sections: BTreeMap<SectionId, usize>,
    /// Locked sections before the edit (start, end).
    sections: BTreeMap<SectionId, (MusicalTime, MusicalTime)>,
}

/// Does the MIDI clip hold a locked note?
fn holds_locked(m: &MidiClip) -> bool {
    m.notes.iter().any(|n| n.time_locked)
}

/// Does the content hold a locked note?
fn content_locked(c: &ClipContent) -> bool {
    matches!(c, ClipContent::Midi(m) if holds_locked(m))
}

/// Does the clip hold a locked note?
fn clip_locked(c: &Clip) -> bool {
    content_locked(&c.content)
}

/// Is anything in the project placed in time on its own: a locked note
/// (in an arrangement's clips), marker or section?
fn holds_locked_items(p: &Project) -> bool {
    p.markers.iter().any(|m| m.is_time_locked())
        || p.sections.iter().any(|s| s.is_time_locked())
        || p.tracks
            .iter()
            .flat_map(|t| p.clips_of(t.id))
            .any(clip_locked)
}

/// Does `cmd` (or a command in it) place clips, notes, markers or
/// sections (what may hold or be material locked on its own)?
fn places_items(cmd: &Command) -> bool {
    match cmd {
        Command::Batch { commands, .. } => commands.iter().any(places_items),
        Command::MoveClip { .. }
        | Command::SetClipContent { .. }
        | Command::AddClip { .. }
        | Command::SplitClip { .. }
        | Command::AddNote { .. }
        | Command::UpdateNote { .. }
        | Command::RemoveNote { .. }
        | Command::AddMarker { .. }
        | Command::UpdateMarker { .. }
        | Command::AddSection { .. }
        | Command::UpdateSection { .. }
        | Command::SetArrangement { .. } => true,
        _ => false,
    }
}

/// Does `cmd` (or a command in it) set locked notes, markers or sections?
fn sets_locked(cmd: &Command) -> bool {
    match cmd {
        Command::Batch { commands, .. } => commands.iter().any(sets_locked),
        Command::AddNote { note, .. } | Command::UpdateNote { note, .. } => note.time_locked,
        Command::SetClipContent { content, .. } => content_locked(content),
        Command::AddClip { clip } => clip_locked(clip),
        Command::AddMarker { marker } | Command::UpdateMarker { marker } => marker.is_time_locked(),
        Command::AddSection { section } | Command::UpdateSection { section } => {
            section.is_time_locked()
        }
        Command::SetArrangement { arrangement } => {
            arrangement.markers.iter().any(|m| m.is_time_locked())
                || arrangement.sections.iter().any(|s| s.is_time_locked())
                || arrangement.clips.values().any(clip_locked)
        }
        _ => false,
    }
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
    let anything = p.tracks.iter().any(|t| t.is_time_locked())
        || !p.adr.cues.is_empty()
        || ((changes_tempo(cmd) || places_items(cmd))
            && (sets_locked(cmd) || holds_locked_items(p)));
    if !anything || shows_arrangement(cmd) {
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
        placed_notes: BTreeMap::new(),
        notes: BTreeMap::new(),
        placed_markers: BTreeMap::new(),
        markers: BTreeMap::new(),
        placed_sections: BTreeMap::new(),
        sections: BTreeMap::new(),
    };
    let mut to_keep = false;
    lock.walk(p, cmd, &mut to_keep);
    if lock.maps.len() == 1 && !to_keep {
        return None;
    }
    let tempo_changed = lock.maps.len() > 1;
    // Locked notes: of every clip through a tempo change, else of the
    // clips the edit moves or sets.
    for t in &p.tracks {
        for c in p.clips_of(t.id) {
            if let Some(m) = c.as_midi()
                && holds_locked(m)
                && (tempo_changed
                    || lock.placed.contains_key(&c.id)
                    || lock.placed_notes.contains_key(&c.id))
            {
                lock.notes.insert(c.id, (c.start, m.clone()));
            }
        }
    }
    if tempo_changed {
        for m in p.markers.iter().filter(|m| m.is_time_locked()) {
            lock.markers.insert(m.id, m.position);
        }
        for s in p.sections.iter().filter(|s| s.is_time_locked()) {
            lock.sections.insert(s.id, (s.start, s.end));
        }
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

    fn place(&mut self, p: &Project, clip: ClipId, track: Option<TrackId>, to_keep: &mut bool) {
        let track = track.or_else(|| p.clip(clip).map(|c| c.track));
        // A clip made earlier in the same edit: its track is not known
        // yet, so it counts.
        *to_keep |= track.is_none_or(|t| p.track(t).is_some_and(|t| t.is_time_locked()));
        // Its locked notes move with it.
        *to_keep |= p.clip(clip).is_some_and(clip_locked);
        self.placed.insert(clip, self.now());
    }

    /// The edit sets `clip`'s notes (`locked`: to some locked ones).
    fn place_notes(&mut self, p: &Project, clip: ClipId, locked: bool, to_keep: &mut bool) {
        *to_keep |= locked || p.clip(clip).is_some_and(clip_locked);
        self.placed_notes.insert(clip, self.now());
    }

    /// Note what `cmd` places and the tempo maps it goes through, in order
    /// (`to_keep`: it places something on a time-locked track, a clip
    /// holding locked notes, or locked notes, markers or sections).
    fn walk(&mut self, p: &Project, cmd: &Command, to_keep: &mut bool) {
        match cmd {
            Command::Batch { commands, .. } => {
                for c in commands {
                    self.walk(p, c, to_keep);
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
                self.walk_arrangement(p, arrangement, to_keep);
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
                            *to_keep = true;
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
            Command::MoveClip { clip, track, .. } => self.place(p, *clip, Some(*track), to_keep),
            Command::SetClipContent { clip, content, .. } => {
                self.place(p, *clip, None, to_keep);
                self.place_notes(p, *clip, content_locked(content), to_keep);
            }
            Command::AddClip { clip } => {
                self.place(p, clip.id, Some(clip.track), to_keep);
                self.place_notes(p, clip.id, clip_locked(clip), to_keep);
            }
            Command::SplitClip { clip, new_clip, .. } => {
                let track = p.clip(*clip).map(|c| c.track);
                self.place(p, *new_clip, track, to_keep);
                let locked = p.clip(*clip).is_some_and(clip_locked);
                self.place_notes(p, *new_clip, locked, to_keep);
            }
            Command::AddNote { clip, note } | Command::UpdateNote { clip, note } => {
                self.place_notes(p, *clip, note.time_locked, to_keep);
            }
            Command::RemoveNote { clip, .. } => self.place_notes(p, *clip, false, to_keep),
            Command::AddMarker { marker } | Command::UpdateMarker { marker } => {
                *to_keep |= marker.is_time_locked();
                self.placed_markers.insert(marker.id, self.now());
            }
            Command::AddSection { section } | Command::UpdateSection { section } => {
                *to_keep |= section.is_time_locked();
                self.placed_sections.insert(section.id, self.now());
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

    /// What a rearrangement places on its own: the markers, sections and
    /// clips' notes it moves or makes (in its own timeline); what it
    /// leaves where it was keeps its sample.
    fn walk_arrangement(
        &mut self,
        p: &Project,
        arrangement: &crate::arrange::Arrangement,
        to_keep: &mut bool,
    ) {
        let now = self.now();
        for m in &arrangement.markers {
            if p.markers.iter().any(|o| o == m) {
                continue;
            }
            *to_keep |= m.is_time_locked();
            self.placed_markers.insert(m.id, now);
        }
        for s in &arrangement.sections {
            if p.sections.iter().any(|o| o == s) {
                continue;
            }
            *to_keep |= s.is_time_locked();
            self.placed_sections.insert(s.id, now);
        }
        for (id, c) in &arrangement.clips {
            let Some(m) = c.as_midi() else {
                continue;
            };
            let old = p.clip(*id);
            if old.is_some_and(|o| o.start == c.start && o.content == c.content) {
                continue;
            }
            let locked = holds_locked(m) || old.is_some_and(clip_locked);
            *to_keep |= locked;
            if locked {
                // Moved, its notes keep their time from its start; cut or
                // made, they are as the arrangement gives them.
                if old.is_none_or(|o| o.content != c.content) {
                    self.placed_notes.insert(*id, now);
                } else {
                    self.placed.entry(*id).or_insert(now);
                }
            }
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
        // Clips moved back on their samples: where they start now.
        let mut moved: BTreeMap<ClipId, MusicalTime> = BTreeMap::new();
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
                    moved.insert(c.id, start);
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
        // Locked notes, on any track.
        for t in &p.tracks {
            for c in p.clips_of(t.id) {
                let Some(m) = c.as_midi().filter(|m| holds_locked(m)) else {
                    continue;
                };
                let start = moved.get(&c.id).copied().unwrap_or(c.start);
                if let Some((start, content)) = self.keep_notes(p, c, m, start) {
                    cmds.push(Command::SetClipContent {
                        clip: c.id,
                        start,
                        content: Box::new(ClipContent::Midi(content)),
                    });
                }
            }
        }
        cmds.extend(self.keep_markers(p));
        if let Some(adr) = self.keep_adr(p) {
            cmds.push(Command::SetAdr { adr: Box::new(adr) });
        }
        cmds
    }

    /// A clip's locked notes back on their samples (`None`: none moved).
    /// `start` is where the clip starts once its own move (on a locked
    /// track) is made; the result says where it starts then — earlier
    /// when it grew to hold a locked note — and what it holds.
    fn keep_notes(
        &self,
        p: &Project,
        c: &Clip,
        m: &MidiClip,
        start: MusicalTime,
    ) -> Option<(MusicalTime, MidiClip)> {
        let rate = self.rate;
        let now = &p.timeline.tempo;
        let tempo_changed = self.maps.len() > 1;
        let before = self.notes.get(&c.id);
        let placed = self
            .placed_notes
            .get(&c.id)
            .or_else(|| self.placed.get(&c.id))
            .copied()
            .unwrap_or_else(|| self.now());
        let clip_at = now.musical_to_samples(start, rate);
        let mut out = m.clone();
        // How far before the clip's start and past its end the kept notes
        // that were in it now reach.
        let mut earliest = MusicalTime::ZERO;
        let mut length = out.length;
        let mut changed = false;
        for n in out.notes.iter_mut().filter(|n| n.time_locked) {
            let old = before.and_then(|(at, b)| {
                b.note(n.id)
                    .filter(|o| o.time_locked)
                    .map(|o| (*at, *o, b.length))
            });
            // Where the note's times sound now: under `from`, from `base`
            // (a clip start), moved by `shift` samples.
            let (from, base, shift, inside) = match old {
                Some((was, o, len)) if o.start == n.start && o.length == n.length => {
                    let carried = was != c.start;
                    if !tempo_changed && !carried {
                        continue;
                    }
                    let from = &self.maps[0];
                    // A clip the edit moves takes it along at the same
                    // time from its start.
                    let shift = if carried {
                        clip_at - from.musical_to_samples(was, rate)
                    } else {
                        0
                    };
                    let inside = (o.start >= MusicalTime::ZERO && o.start < len)
                        .then_some((o.end() <= len, len - o.start));
                    (from, was, shift, inside)
                }
                // Placed by the edit: read against the tempo map in
                // effect where it was given.
                _ => {
                    let from = &self.maps[placed];
                    let shift = clip_at - from.musical_to_samples(c.start, rate);
                    (from, c.start, shift, None)
                }
            };
            let sounds = |t: MusicalTime| from.musical_to_samples(base + t, rate) + shift;
            let at = sounds(n.start);
            // On the clip's own first sample: its start (which may lie
            // between samples, a hair after the sample's tick).
            let on = if at == clip_at {
                start
            } else {
                now.musical_at_sample(at, rate)
            };
            let off = now
                .musical_at_sample(sounds(n.end()), rate)
                .max(on + MusicalTime(1));
            let note_start = n.start;
            if let Some(e) = out.expressions.iter_mut().find(|e| e.note == n.id) {
                for kind in crate::ExpressionKind::ALL {
                    for q in e.curve_mut(kind) {
                        let t = now.musical_at_sample(sounds(note_start + q.time), rate);
                        let time = (t - on).max(MusicalTime::ZERO);
                        changed |= time != q.time;
                        q.time = time;
                    }
                }
            }
            let (rel, len) = (on - start, off - on);
            changed |= rel != n.start || len != n.length;
            n.start = rel;
            n.length = len;
            // It was in the clip: it stays in it.
            if let Some((whole, span)) = inside {
                earliest = earliest.min(rel);
                length = length.max(if whole { rel + len } else { rel + span });
            }
        }
        let given = start;
        let mut start = start;
        if earliest < MusicalTime::ZERO {
            // The clip grows back to the earliest: what it holds keeps its
            // place in the music.
            let d = MusicalTime::ZERO - earliest;
            start += earliest;
            length += d;
            for n in &mut out.notes {
                n.start += d;
            }
            for l in &mut out.controllers {
                for q in &mut l.points {
                    q.time += d;
                }
            }
            for e in &mut out.sysex {
                e.time += d;
            }
        }
        if length != out.length {
            out.length = length;
        }
        if !changed && start == given {
            return None;
        }
        out.sort_notes();
        (out != *m || start != given).then_some((start, out))
    }

    /// Locked markers and sections back on their samples (those the edit
    /// set read against the tempo map in effect where it set them).
    fn keep_markers(&self, p: &Project) -> Vec<Command> {
        let (rate, now) = (self.rate, &p.timeline.tempo);
        let tempo_changed = self.maps.len() > 1;
        let at = |from: &TempoMap, t: MusicalTime| {
            now.musical_at_sample(from.musical_to_samples(t, rate), rate)
        };
        let mut cmds = Vec::new();
        for m in p.markers.iter().filter(|m| m.is_time_locked()) {
            let from = match (self.placed_markers.get(&m.id), self.markers.get(&m.id)) {
                (Some(&i), _) => &self.maps[i],
                (None, Some(&was)) if was == m.position && tempo_changed => &self.maps[0],
                _ => continue,
            };
            let position = at(from, m.position);
            if position != m.position {
                cmds.push(Command::UpdateMarker {
                    marker: crate::Marker {
                        position,
                        ..m.clone()
                    },
                });
            }
        }
        for s in p.sections.iter().filter(|s| s.is_time_locked()) {
            let from = match (self.placed_sections.get(&s.id), self.sections.get(&s.id)) {
                (Some(&i), _) => &self.maps[i],
                (None, Some(&was)) if was == (s.start, s.end) && tempo_changed => &self.maps[0],
                _ => continue,
            };
            let (start, end) = (at(from, s.start), at(from, s.end));
            let end = end.max(start + MusicalTime(1));
            if (start, end) != (s.start, s.end) {
                cmds.push(Command::UpdateSection {
                    section: crate::Section {
                        start,
                        end,
                        ..s.clone()
                    },
                });
            }
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
