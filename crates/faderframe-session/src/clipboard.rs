//! The arranger's clipboard: Copy, Cut, Paste, Duplicate and Repeat.
//!
//! One clipboard holds *values*, never references into the project:
//! whole clips with all of their content (audio with take folders and
//! comps, warp, pitch edits, clip effects, spectral edits, fades and gain;
//! MIDI with notes, note expression, controllers and SysEx), the audio
//! sources those clips need (by value too: a paste still works after the
//! copied track was removed or its import undone — no file is copied, the
//! source is registered again), and for a copied time range the
//! automation of that range.
//!
//! * **Copy** takes the edit selection's range on the selected tracks when
//!   it has a length (Pro Tools' edit selection), else the selected clips
//!   (never clips of the clip launcher).
//! * **Lanes** are the copied tracks in the order the arranger shows them
//!   (folder order), each with how many rows lay between it and the one
//!   above, so the material keeps its track layout.
//! * **Paste** puts it at the insertion point — the playhead, or without
//!   Link Timeline the edit cursor — on the grid in Grid mode (keeping the
//!   offset from the grid with a relative grid), on whole samples
//!   otherwise. The top lane lands on the destination track: a selected
//!   track that is the top lane's own track, else the first selected
//!   track, else the top lane's own track. The other lanes follow at their
//!   distance below; a lane goes on down to the next track that takes its
//!   clips (audio → audio tracks, MIDI → instrument and MIDI tracks) and
//!   is left out with a notice when none does. In Shuffle mode the paste
//!   inserts (what follows moves later, automation too); in the other
//!   modes it replaces what lies under the pasted clips (a range: under
//!   the whole range). Pasted clips get fresh ids (never aliases), become
//!   the selection, and the insertion point moves to the end of what was
//!   pasted, so pasting again lays copies end to end.
//! * **Duplicate** (Repeat ×1) places copies of the selection right after
//!   it, end to end, on the same tracks, and selects the last copy so
//!   duplicating again continues.
//! * Every paste or duplicate is one undo step.
//!
//! Time-range operations carry automation (copy, paste, duplicate, and the
//! time Shuffle and Insert Silence add or take away); object operations on
//! clips do not (as moves never did).

use crate::editing::{EditMode, EditRange, GridMode};
use crate::{NoticeLevel, Result, SelectMode, Session};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
};
use faderframe_core::{AudioSourceId, ClipId, IdAllocator, NoteId, TrackId};
use faderframe_project::{AudioSource, Clip, ClipContent, Command, Project, SourceSpec, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::{BTreeSet, HashMap};

/// What the clipboard holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipboardKind {
    #[default]
    Empty,
    /// A time range on tracks: the parts of the clips in it and its
    /// automation; tracks with nothing in it keep their place.
    Range,
    /// Whole clips.
    Clips,
}

/// What a lane's clips are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneContent {
    Audio,
    Midi,
}

/// One copied track.
#[derive(Clone, Debug)]
pub struct CopiedLane {
    /// The track it was copied from.
    pub origin: TrackId,
    /// Arranger rows between the previous lane's track and this one's.
    pub gap: usize,
    /// Clips, their starts relative to the clipboard's start.
    pub clips: Vec<Clip>,
    /// The automation of a copied range: points relative to its start.
    pub automation: Vec<(AutomationTarget, Vec<AutomationPoint>)>,
}

impl CopiedLane {
    /// What its clips are (`None`: no clips).
    pub fn content(&self) -> Option<LaneContent> {
        self.clips.first().map(|c| {
            if c.content.is_audio() {
                LaneContent::Audio
            } else {
                LaneContent::Midi
            }
        })
    }
}

/// The arranger's clipboard (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Clipboard {
    kind: ClipboardKind,
    /// From the start of the copied material to its end.
    length: MusicalTime,
    /// Where the copied material started (a relative grid keeps its
    /// offset from the grid).
    origin: MusicalTime,
    lanes: Vec<CopiedLane>,
    /// Every source the clips need.
    sources: Vec<AudioSource>,
    /// The project rate the clips' frames are in.
    rate: u32,
}

impl Clipboard {
    pub fn kind(&self) -> ClipboardKind {
        self.kind
    }

    pub fn is_empty(&self) -> bool {
        self.kind == ClipboardKind::Empty
    }

    pub fn length(&self) -> MusicalTime {
        self.length
    }

    pub fn lanes(&self) -> &[CopiedLane] {
        &self.lanes
    }

    pub fn sources(&self) -> &[AudioSource] {
        &self.sources
    }

    /// Clips on the clipboard.
    pub fn clip_count(&self) -> usize {
        self.lanes.iter().map(|l| l.clips.len()).sum()
    }
}

/// Can a lane's clips go on `kind`?
fn takes(kind: TrackKind, content: LaneContent) -> bool {
    match content {
        LaneContent::Audio => kind == TrackKind::Audio,
        LaneContent::Midi => matches!(kind, TrackKind::Instrument | TrackKind::Midi),
    }
}

/// Lanes a track's automation of a range may go to: lanes of the track
/// itself (and its own devices, sends and cue sends) only there, the
/// track's own controls on any track with a strip.
fn generic(target: &AutomationTarget) -> bool {
    matches!(
        target,
        AutomationTarget::TrackVolume
            | AutomationTarget::TrackPan
            | AutomationTarget::TrackMute
            | AutomationTarget::Surround(_)
            | AutomationTarget::DualPanLeft
            | AutomationTarget::DualPanRight
    )
}

/// One tick: the last moment of a range.
const TICK: MusicalTime = MusicalTime(1);

/// The points of `curve` in `[a, b)`, relative to `a`, with its values at
/// both ends.
fn range_points(curve: &AutomationCurve, a: MusicalTime, b: MusicalTime) -> Vec<AutomationPoint> {
    let mut pts = Vec::new();
    if let Some(v) = curve.value_at(a)
        && !curve.points().iter().any(|q| q.time == a)
    {
        pts.push(AutomationPoint {
            time: MusicalTime::ZERO,
            value: v,
            shape: Default::default(),
        });
    }
    pts.extend(
        curve
            .points()
            .iter()
            .filter(|q| q.time >= a && q.time < b)
            .map(|q| AutomationPoint {
                time: q.time - a,
                ..*q
            }),
    );
    if let Some(v) = curve.value_at(b - TICK) {
        pts.push(AutomationPoint {
            time: b - a,
            value: v,
            shape: Default::default(),
        });
    }
    pts
}

/// `curve` with `len` of time inserted at `at`: what follows moves later,
/// the value at `at` holds over the gap.
pub(crate) fn curve_inserted(
    curve: &AutomationCurve,
    at: MusicalTime,
    len: MusicalTime,
) -> AutomationCurve {
    let pts = curve.points();
    let (Some(v), true) = (curve.value_at(at), len > MusicalTime::ZERO) else {
        return curve.clone();
    };
    let before = pts.iter().any(|q| q.time < at);
    let after = pts.iter().any(|q| q.time >= at);
    let mut out: Vec<AutomationPoint> = pts.iter().filter(|q| q.time < at).copied().collect();
    if before && after {
        out.push(AutomationPoint {
            time: at,
            value: v,
            shape: Default::default(),
        });
        out.push(AutomationPoint {
            time: at + len,
            value: v,
            shape: Default::default(),
        });
    }
    out.extend(
        pts.iter()
            .filter(|q| q.time >= at)
            .map(|q| AutomationPoint {
                time: q.time + len,
                ..*q
            }),
    );
    AutomationCurve::from_points(out)
}

/// `curve` with `[a, b)` taken out: what follows moves earlier (a jump at
/// `a` from the value there to the value at `b`).
pub(crate) fn curve_removed(
    curve: &AutomationCurve,
    a: MusicalTime,
    b: MusicalTime,
) -> AutomationCurve {
    if b <= a || curve.is_empty() {
        return curve.clone();
    }
    let pts = curve.points();
    let len = b - a;
    let before = pts.iter().any(|q| q.time < a);
    let after = pts.iter().any(|q| q.time >= b);
    let mut out: Vec<AutomationPoint> = pts.iter().filter(|q| q.time < a).copied().collect();
    if before && let Some(v) = curve.value_at(a) {
        out.push(AutomationPoint {
            time: a,
            value: v,
            shape: Default::default(),
        });
    }
    if after
        && let Some(v) = curve.value_at(b)
        && !pts.iter().any(|q| q.time == b)
    {
        out.push(AutomationPoint {
            time: a,
            value: v,
            shape: Default::default(),
        });
    }
    out.extend(pts.iter().filter(|q| q.time >= b).map(|q| AutomationPoint {
        time: q.time - len,
        ..*q
    }));
    AutomationCurve::from_points(out)
}

/// `curve` with the points in `[a, b)` taken out and nothing moving (a
/// range cleared outside Shuffle): anchors at both edges keep the values
/// outside as they were, and the line runs straight between them.
pub(crate) fn curve_cleared(
    curve: &AutomationCurve,
    a: MusicalTime,
    b: MusicalTime,
) -> AutomationCurve {
    let pts = curve.points();
    if b <= a || !pts.iter().any(|q| q.time >= a && q.time < b) {
        return curve.clone();
    }
    let (Some(va), Some(vb)) = (curve.value_at(a), curve.value_at(b)) else {
        return curve.clone();
    };
    let mut out: Vec<AutomationPoint> = pts.iter().filter(|q| q.time < a).copied().collect();
    out.push(AutomationPoint {
        time: a,
        value: va,
        shape: Default::default(),
    });
    if !pts.iter().any(|q| q.time == b) {
        // The segment across `b` goes on in its shape.
        let shape = pts
            .iter()
            .rev()
            .find(|q| q.time < b)
            .map(|q| q.shape)
            .unwrap_or_default();
        out.push(AutomationPoint {
            time: b,
            value: vb,
            shape,
        });
    }
    out.extend(pts.iter().filter(|q| q.time >= b).copied());
    AutomationCurve::from_points(out)
}

/// Working copies of the automation lanes one batch changes: each lane is
/// written once, with every change of the batch in it.
#[derive(Default)]
pub(crate) struct LaneWork {
    /// (track, lane as it will be, made by this batch).
    lanes: Vec<(TrackId, AutomationLane, bool)>,
}

impl LaneWork {
    /// The working copy of `track`'s lane for `target`; a new lane when
    /// the track has none and `create`.
    fn lane(
        &mut self,
        p: &mut Project,
        track: TrackId,
        target: AutomationTarget,
        create: bool,
    ) -> Option<&mut AutomationLane> {
        let i = match self
            .lanes
            .iter()
            .position(|(t, l, _)| *t == track && l.target == target)
        {
            Some(i) => i,
            None => {
                let t = p.track(track)?;
                match t.automation.lane(target) {
                    Some(l) => self.lanes.push((track, l.clone(), false)),
                    None if create => {
                        let id = p.ids.allocate();
                        self.lanes.push((
                            track,
                            AutomationLane {
                                id,
                                target,
                                curve: AutomationCurve::new(),
                                mode: AutomationMode::Read,
                                visible: true,
                            },
                            true,
                        ));
                    }
                    None => return None,
                }
                self.lanes.len() - 1
            }
        };
        self.lanes.get_mut(i).map(|(_, l, _)| l)
    }

    /// Insert `len` of time at `at` into every lane of `track`.
    pub(crate) fn insert_time(
        &mut self,
        p: &mut Project,
        track: TrackId,
        at: MusicalTime,
        len: MusicalTime,
    ) {
        for target in Self::targets(p, track) {
            if let Some(l) = self.lane(p, track, target, false) {
                l.curve = curve_inserted(&l.curve, at, len);
            }
        }
    }

    /// Take `[a, b)` out of every lane of `track`.
    pub(crate) fn remove_time(
        &mut self,
        p: &mut Project,
        track: TrackId,
        a: MusicalTime,
        b: MusicalTime,
    ) {
        for target in Self::targets(p, track) {
            if let Some(l) = self.lane(p, track, target, false) {
                l.curve = curve_removed(&l.curve, a, b);
            }
        }
    }

    /// Clear `[a, b)` in every lane of `track`, nothing moving.
    pub(crate) fn clear_time(
        &mut self,
        p: &mut Project,
        track: TrackId,
        a: MusicalTime,
        b: MusicalTime,
    ) {
        for target in Self::targets(p, track) {
            if let Some(l) = self.lane(p, track, target, false) {
                l.curve = curve_cleared(&l.curve, a, b);
            }
        }
    }

    /// The targets of `track`'s lanes that have points.
    fn targets(p: &Project, track: TrackId) -> Vec<AutomationTarget> {
        p.track(track)
            .map(|t| {
                t.automation
                    .lanes
                    .iter()
                    .filter(|l| !l.curve.is_empty())
                    .map(|l| l.target)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The commands writing the changed lanes.
    pub(crate) fn commands(self) -> Vec<Command> {
        self.lanes
            .into_iter()
            .map(|(track, lane, new)| {
                if new {
                    Command::AddAutomationLane {
                        track,
                        lane: Box::new(lane),
                    }
                } else {
                    Command::SetAutomationLane {
                        track,
                        lane: Box::new(lane),
                    }
                }
            })
            .collect()
    }
}

/// What a paste does.
struct Pasted {
    commands: Vec<Command>,
    /// The new clips, per position pasted at.
    clips: Vec<Vec<ClipId>>,
    /// The tracks that got clips.
    tracks: Vec<TrackId>,
    /// Clips left out: their media is gone.
    missing_media: usize,
}

/// Give a MIDI clip's notes fresh ids (their expression follows).
fn renumber_notes(content: &mut ClipContent, ids: &mut IdAllocator) {
    let ClipContent::Midi(m) = content else {
        return;
    };
    let mut map: HashMap<NoteId, NoteId> = HashMap::new();
    for n in &mut m.notes {
        let id: NoteId = ids.allocate();
        map.insert(n.id, id);
        n.id = id;
    }
    m.expressions.retain_mut(|e| match map.get(&e.note) {
        Some(id) => {
            e.note = *id;
            true
        }
        None => false,
    });
}

/// Merge overlapping spans (sorted by start).
fn merged(mut spans: Vec<(MusicalTime, MusicalTime)>) -> Vec<(MusicalTime, MusicalTime)> {
    spans.retain(|(a, b)| b > a);
    spans.sort();
    let mut out: Vec<(MusicalTime, MusicalTime)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

impl Session {
    /// What Copy put on the clipboard.
    pub fn clipboard(&self) -> &Clipboard {
        &self.clipboard
    }

    /// The arranger's rows: tracks in folder order, without the master and
    /// the tracks of closed folders.
    pub fn arranger_rows(&self) -> Vec<TrackId> {
        self.project
            .folder_order()
            .into_iter()
            .filter(|t| t.kind != TrackKind::Master && self.track_shown(t))
            .map(|t| t.id)
            .collect()
    }

    /// Where pastes go: the playhead (with Link Timeline it follows the
    /// edit selection), or without Link Timeline the edit cursor.
    pub fn insertion_point(&self) -> MusicalTime {
        match self.selection.range {
            Some(r) if !self.editor.link_timeline => r.start,
            _ => self.playhead(),
        }
    }

    /// The selected clips of the arranger (not the launcher's), in the
    /// arranger's row order, then by start; in folder groups with the
    /// clips at the same place on the other members.
    pub fn selected_arranger_clips(&self) -> Vec<ClipId> {
        let p = &self.project;
        let launcher = p.launcher_clips();
        let order: Vec<TrackId> = p.folder_order().iter().map(|t| t.id).collect();
        let mut clips: Vec<&Clip> = self
            .grouped_selection()
            .iter()
            .filter(|c| !launcher.contains(c))
            .filter_map(|c| p.clip(*c))
            .filter(|c| p.track(c.track).is_some_and(|t| t.clips.contains(&c.id)))
            .collect();
        clips.sort_by_key(|c| {
            (
                order
                    .iter()
                    .position(|t| *t == c.track)
                    .unwrap_or(usize::MAX),
                c.start,
                c.id,
            )
        });
        clips.into_iter().map(|c| c.id).collect()
    }

    /// `tracks` sorted in folder order.
    fn in_folder_order(&self, tracks: &mut [TrackId]) {
        let order: Vec<TrackId> = self.project.folder_order().iter().map(|t| t.id).collect();
        tracks.sort_by_key(|t| order.iter().position(|o| o == t).unwrap_or(usize::MAX));
    }

    /// For tracks in folder order: the arranger rows between each and the
    /// one before.
    fn row_gaps(&self, tracks: &[TrackId]) -> Vec<usize> {
        let rows = self.arranger_rows();
        let mut prev: Option<usize> = None;
        tracks
            .iter()
            .map(|t| {
                let i = rows.iter().position(|r| r == t);
                let gap = match (prev, i) {
                    (Some(a), Some(b)) if b > a => b - a - 1,
                    _ => 0,
                };
                if i.is_some() {
                    prev = i;
                }
                gap
            })
            .collect()
    }

    /// The sources the lanes' clips need, by value.
    fn sources_of(&self, lanes: &[CopiedLane]) -> Vec<AudioSource> {
        let ids: BTreeSet<AudioSourceId> = lanes
            .iter()
            .flat_map(|l| &l.clips)
            .flat_map(|c| c.content.referenced_sources())
            .collect();
        ids.into_iter()
            .filter_map(|id| self.project.sources.get(&id).cloned())
            .collect()
    }

    /// A clipboard holding `clips` (whole).
    fn clips_clipboard(&self, clips: &[ClipId]) -> Clipboard {
        let p = &self.project;
        let found: Vec<&Clip> = clips.iter().filter_map(|c| p.clip(*c)).collect();
        let (Some(origin), Some(end)) = (
            found.iter().map(|c| c.start).min(),
            found.iter().map(|c| self.clip_end(c)).max(),
        ) else {
            return Clipboard::default();
        };
        let mut tracks: Vec<TrackId> = Vec::new();
        for c in &found {
            if !tracks.contains(&c.track) {
                tracks.push(c.track);
            }
        }
        self.in_folder_order(&mut tracks);
        let gaps = self.row_gaps(&tracks);
        let lanes: Vec<CopiedLane> = tracks
            .iter()
            .zip(gaps)
            .map(|(t, gap)| {
                let mut on: Vec<Clip> = found
                    .iter()
                    .filter(|c| c.track == *t)
                    .map(|c| Clip {
                        start: c.start - origin,
                        ..(*c).clone()
                    })
                    .collect();
                on.sort_by_key(|c| (c.start, c.id));
                CopiedLane {
                    origin: *t,
                    gap,
                    clips: on,
                    automation: Vec::new(),
                }
            })
            .collect();
        Clipboard {
            kind: ClipboardKind::Clips,
            length: end - origin,
            origin,
            sources: self.sources_of(&lanes),
            lanes,
            rate: p.sample_rate,
        }
    }

    /// A clipboard holding the range `r` of `tracks`: the parts of their
    /// clips in it and their automation.
    fn range_clipboard(&self, r: EditRange, tracks: &[TrackId]) -> Clipboard {
        let mut tracks = tracks.to_vec();
        self.in_folder_order(&mut tracks);
        let gaps = self.row_gaps(&tracks);
        let p = &self.project;
        let lanes: Vec<CopiedLane> = tracks
            .iter()
            .zip(gaps)
            .map(|(t, gap)| CopiedLane {
                origin: *t,
                gap,
                clips: p
                    .clips_of(*t)
                    .into_iter()
                    .filter_map(|c| c.slice(r.start, r.end, c.id, &p.timeline, p.sample_rate))
                    .map(|part| Clip {
                        start: part.start - r.start,
                        ..part
                    })
                    .collect(),
                automation: p
                    .track(*t)
                    .map(|t| {
                        t.automation
                            .lanes
                            .iter()
                            .filter(|l| !l.curve.is_empty())
                            .map(|l| (l.target, range_points(&l.curve, r.start, r.end)))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        Clipboard {
            kind: ClipboardKind::Range,
            length: r.length(),
            origin: r.start,
            sources: self.sources_of(&lanes),
            lanes,
            rate: p.sample_rate,
        }
    }

    /// Copy the edit selection: its range on the selected tracks when it
    /// has a length, else the selected clips. Nothing selected leaves the
    /// clipboard as it was; returns whether something was copied.
    pub(crate) fn copy(&mut self) -> Result<bool> {
        if self.selection.range.is_some_and(|r| !r.is_empty()) {
            self.copy_range()?;
            return Ok(true);
        }
        let clips = self.selected_arranger_clips();
        if clips.is_empty() {
            return Ok(false);
        }
        self.clipboard = self.clips_clipboard(&clips);
        Ok(true)
    }

    /// Copy the range on the selected tracks.
    pub(crate) fn copy_range(&mut self) -> Result<()> {
        let Some(r) = self.selection.range.filter(|r| !r.is_empty()) else {
            return Ok(());
        };
        let tracks = self.edit_tracks();
        if tracks.is_empty() {
            return Ok(());
        }
        self.clipboard = self.range_clipboard(r, &tracks);
        Ok(())
    }

    /// Cut: copy, then clear the range (Shuffle closes the gap) or remove
    /// the copied clips.
    pub(crate) fn cut(&mut self) -> Result<()> {
        if self.selection.range.is_some_and(|r| !r.is_empty()) {
            self.copy_range()?;
            self.clear_range_as("Cut")?;
            return Ok(());
        }
        let clips = self.selected_arranger_clips();
        if clips.is_empty() {
            return Ok(());
        }
        self.clipboard = self.clips_clipboard(&clips);
        self.batch(
            if clips.len() == 1 {
                "Cut Clip"
            } else {
                "Cut Clips"
            },
            clips
                .iter()
                .map(|&clip| Command::RemoveClip { clip })
                .collect(),
        )?;
        // Gone (a refused cut, a frozen track, keeps its selection).
        for c in &clips {
            self.selection.clips.remove(c);
        }
        Ok(())
    }

    /// Where a paste of `cb` goes (see the module docs).
    fn paste_position(&self, cb: &Clipboard) -> MusicalTime {
        let meter = &self.project.timeline.meter;
        let at = self.insertion_point();
        let at = if self.editor.edit_mode == EditMode::Grid {
            let grid = self.editor.grid;
            let base = faderframe_timeline::snap_nearest(at, grid, meter);
            match self.editor.grid_mode {
                GridMode::Absolute => base,
                GridMode::Relative => {
                    base + (cb.origin - faderframe_timeline::snap_floor(cb.origin, grid, meter))
                }
            }
        } else {
            self.on_sample(at)
        };
        at.max(MusicalTime::ZERO)
    }

    /// The track each lane of `cb` goes to (see the module docs); `None`:
    /// no track takes it.
    fn paste_plan(&self, cb: &Clipboard) -> Vec<Option<TrackId>> {
        let rows = self.arranger_rows();
        let p = &self.project;
        let selected: Vec<usize> = (0..rows.len())
            .filter(|i| self.selection.tracks.contains(&rows[*i]))
            .collect();
        let own = cb
            .lanes
            .first()
            .and_then(|l| rows.iter().position(|r| *r == l.origin));
        let anchor = match (selected.first(), own) {
            (Some(_), Some(o)) if selected.contains(&o) => o,
            (Some(&s), _) => s,
            (None, Some(o)) => o,
            (None, None) => 0,
        };
        let mut plan = Vec::with_capacity(cb.lanes.len());
        let mut prev: Option<usize> = None;
        for lane in &cb.lanes {
            let want = prev.map_or(anchor, |r| r + 1 + lane.gap);
            let row = match lane.content() {
                // Nothing but automation (or nothing): its own row.
                None => (want < rows.len()).then_some(want),
                Some(content) => (want..rows.len())
                    .find(|&i| p.track(rows[i]).is_some_and(|t| takes(t.kind, content))),
            };
            prev = Some(row.unwrap_or(want));
            plan.push(row.map(|i| rows[i]));
        }
        plan
    }

    /// The commands placing `cb` at each of `positions` (ascending; one
    /// after another when several) on the tracks of `plan`.
    fn paste_commands(
        &mut self,
        cb: &Clipboard,
        plan: &[Option<TrackId>],
        positions: &[MusicalTime],
    ) -> Pasted {
        let mut commands = Vec::new();
        // The lanes that go somewhere, and where.
        let placed: Vec<(&CopiedLane, TrackId)> = cb
            .lanes
            .iter()
            .zip(plan)
            .filter_map(|(l, t)| t.map(|t| (l, t)))
            .collect();
        // Sources the project no longer has come back (their files must
        // still be there) — those of clips being pasted.
        let needed: BTreeSet<AudioSourceId> = placed
            .iter()
            .flat_map(|(l, _)| &l.clips)
            .flat_map(|c| c.content.referenced_sources())
            .collect();
        let mut unavailable: BTreeSet<AudioSourceId> = BTreeSet::new();
        for s in cb.sources.iter().filter(|s| needed.contains(&s.id)) {
            if self.project.sources.contains_key(&s.id) {
                continue;
            }
            let there = match &s.spec {
                SourceSpec::File { path, .. } => path.exists(),
                SourceSpec::Generated { .. } => true,
            };
            if there {
                commands.push(Command::AddSource {
                    source: Box::new(s.clone()),
                });
            } else {
                unavailable.insert(s.id);
            }
        }
        let known: BTreeSet<AudioSourceId> = cb.sources.iter().map(|s| s.id).collect();
        let playable = |c: &Clip, p: &Project| {
            c.content.referenced_sources().iter().all(|s| {
                !unavailable.contains(s) && (p.sources.contains_key(s) || known.contains(s))
            })
        };
        let mut missing_media = 0;
        let mut tracks: Vec<TrackId> = Vec::new();
        for (l, t) in &placed {
            if !l.clips.is_empty() && !tracks.contains(t) {
                tracks.push(*t);
            }
        }
        let shuffle = self.editor.edit_mode == EditMode::Shuffle;
        let mut work = LaneWork::default();
        let first = positions.first().copied().unwrap_or(MusicalTime::ZERO);
        let total = MusicalTime(cb.length.ticks() * positions.len() as i64);
        if shuffle {
            // Make room: everything from the first position on moves later
            // (on the tracks getting clips or a range's automation).
            let mut touched = tracks.clone();
            for (l, t) in &placed {
                if !l.automation.is_empty() && !touched.contains(t) {
                    touched.push(*t);
                }
            }
            for &t in &touched {
                commands.extend(self.insert_time_commands(t, first, total));
                work.insert_time(&mut self.project, t, first, total);
            }
        } else {
            // Replace what lies under the pasted clips (a range: under it
            // all, on the tracks it has clips for).
            let rate = self.project.sample_rate;
            for &t in &tracks {
                let mut spans = Vec::new();
                for &at in positions {
                    for (l, _) in placed.iter().filter(|(_, d)| *d == t) {
                        if cb.kind == ClipboardKind::Range {
                            spans.push((at, at + cb.length));
                        } else {
                            for c in l.clips.iter().filter(|c| playable(c, &self.project)) {
                                let moved = Clip {
                                    start: at + c.start,
                                    ..c.clone()
                                };
                                spans.push((moved.start, moved.end(&self.project.timeline, rate)));
                            }
                        }
                    }
                }
                commands.extend(self.clear_spans_commands(t, merged(spans)));
            }
        }
        let mut clips = Vec::with_capacity(positions.len());
        for &at in positions {
            let mut made = Vec::new();
            for (l, t) in &placed {
                for c in &l.clips {
                    if !playable(c, &self.project) {
                        missing_media += 1;
                        continue;
                    }
                    let id: ClipId = self.project.ids.allocate();
                    let mut content = c.content.clone();
                    renumber_notes(&mut content, &mut self.project.ids);
                    commands.push(Command::AddClip {
                        clip: Box::new(Clip {
                            id,
                            track: *t,
                            start: at + c.start,
                            content,
                            ..c.clone()
                        }),
                    });
                    made.push(id);
                }
                // A range's automation (lanes of the track itself only on
                // the track itself).
                let can_automate = self.project.track(*t).is_some_and(|d| d.kind.has_audio());
                for (target, points) in &l.automation {
                    let fits = (generic(target) && can_automate) || *t == l.origin;
                    if points.is_empty() || !fits {
                        continue;
                    }
                    let to = at + cb.length;
                    let shifted: Vec<AutomationPoint> = points
                        .iter()
                        .map(|q| AutomationPoint {
                            time: at + q.time,
                            ..*q
                        })
                        .collect();
                    if let Some(lane) = work.lane(&mut self.project, *t, *target, generic(target)) {
                        lane.curve.replace_range(at, to, &shifted);
                    }
                }
            }
            clips.push(made);
        }
        commands.extend(work.commands());
        Pasted {
            commands,
            clips,
            tracks,
            missing_media,
        }
    }

    /// Commands removing `spans` (sorted, disjoint) from `track`'s clips:
    /// what lies inside goes, clips across an edge are cut there.
    pub(crate) fn clear_spans_commands(
        &mut self,
        track: TrackId,
        spans: Vec<(MusicalTime, MusicalTime)>,
    ) -> Vec<Command> {
        let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
        let mut cmds = Vec::new();
        for c in clips {
            let end = self.clip_end(&c);
            let audio = c.content.is_audio();
            // Strictly inside a piece (a whole sample in, for audio).
            let inside = |s: &Self, x: MusicalTime, lo: MusicalTime, hi: MusicalTime| {
                x > lo
                    && x < hi
                    && (!audio || (s.frames_between(lo, x) > 0 && s.frames_between(x, hi) > 0))
            };
            let mut piece = c.id;
            let mut from = c.start;
            for &(a, b) in spans.iter().filter(|(a, b)| *a < end && *b > c.start) {
                if inside(self, a, from, end) {
                    let mid: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: piece,
                        at: a,
                        new_clip: mid,
                    });
                    piece = mid;
                    from = a;
                }
                if inside(self, b, from, end) {
                    let right: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: piece,
                        at: b,
                        new_clip: right,
                    });
                    cmds.push(Command::RemoveClip { clip: piece });
                    piece = right;
                    from = b;
                } else {
                    cmds.push(Command::RemoveClip { clip: piece });
                    break;
                }
            }
        }
        cmds
    }

    /// Commands inserting `len` of time at `at` on `track`: clips from
    /// there on move later, one across it is cut there.
    pub(crate) fn insert_time_commands(
        &mut self,
        track: TrackId,
        at: MusicalTime,
        len: MusicalTime,
    ) -> Vec<Command> {
        let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
        let mut cmds = Vec::new();
        for c in clips {
            let end = self.clip_end(&c);
            if c.start >= at {
                cmds.push(Command::MoveClip {
                    clip: c.id,
                    track,
                    start: c.start + len,
                });
            } else if end > at
                && (!c.content.is_audio()
                    || (self.frames_between(c.start, at) > 0 && self.frames_between(at, end) > 0))
            {
                let right: ClipId = self.project.ids.allocate();
                cmds.push(Command::SplitClip {
                    clip: c.id,
                    at,
                    new_clip: right,
                });
                cmds.push(Command::MoveClip {
                    clip: right,
                    track,
                    start: at + len,
                });
            }
        }
        cmds
    }

    /// Paste the clipboard at the insertion point (see the module docs).
    pub(crate) fn paste(&mut self) -> Result<()> {
        if self.clipboard.is_empty() {
            return Ok(());
        }
        if self.clipboard.rate != self.project.sample_rate {
            self.notify(
                NoticeLevel::Warning,
                "The clipboard holds material of a project at another sample rate",
            );
            return Ok(());
        }
        let cb = self.clipboard.clone();
        let at = self.paste_position(&cb);
        let plan = self.paste_plan(&cb);
        let left_out: usize = cb
            .lanes
            .iter()
            .zip(&plan)
            .filter(|(l, t)| t.is_none() && !l.clips.is_empty())
            .map(|(l, _)| l.clips.len())
            .sum();
        let out = self.paste_commands(&cb, &plan, &[at]);
        let made: Vec<ClipId> = out.clips.into_iter().flatten().collect();
        if left_out > 0 {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "{left_out} copied clip{} not pasted: no track below the destination takes {}",
                    if left_out == 1 { " was" } else { "s were" },
                    if left_out == 1 { "it" } else { "them" }
                ),
            );
        }
        if out.missing_media > 0 {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "{} copied clip{} not pasted: {} audio file is gone",
                    out.missing_media,
                    if out.missing_media == 1 {
                        " was"
                    } else {
                        "s were"
                    },
                    if out.missing_media == 1 {
                        "its"
                    } else {
                        "their"
                    }
                ),
            );
        }
        if out.commands.is_empty() {
            return Ok(());
        }
        self.batch("Paste", out.commands)?;
        self.selection.select_clips(&made, SelectMode::Replace);
        if !out.tracks.is_empty() {
            self.selection
                .select_tracks(&out.tracks, SelectMode::Replace);
        }
        // The insertion point moves on: pasting again goes after this.
        let end = at + cb.length;
        self.set_edit_range(Some(EditRange::new(end, end)))
    }

    /// Copies of the selection right after it, `times` times (1:
    /// Duplicate): the range on the selected tracks when it has a length,
    /// else the selected clips (the last copy becomes the selection).
    pub(crate) fn repeat(&mut self, times: u32) -> Result<()> {
        if self.selection.range.is_some_and(|r| !r.is_empty()) {
            return self.repeat_range(times);
        }
        let clips = self.selected_arranger_clips();
        if clips.is_empty() {
            return Ok(());
        }
        let cb = self.clips_clipboard(&clips);
        let len = cb.length.ticks().max(1);
        let positions: Vec<MusicalTime> = (1..=i64::from(times.max(1)))
            .map(|k| MusicalTime(cb.origin.ticks() + k * len))
            .collect();
        let plan: Vec<Option<TrackId>> = cb.lanes.iter().map(|l| Some(l.origin)).collect();
        let out = self.paste_commands(&cb, &plan, &positions);
        if out.commands.is_empty() {
            return Ok(());
        }
        self.batch(
            if times <= 1 { "Duplicate" } else { "Repeat" },
            out.commands,
        )?;
        let last = out.clips.last().cloned().unwrap_or_default();
        self.selection.select_clips(&last, SelectMode::Replace);
        self.revision += 1;
        Ok(())
    }

    /// Copies of the range right after it, `times` times; the selection
    /// moves onto the last copy.
    pub(crate) fn repeat_range(&mut self, times: u32) -> Result<()> {
        let Some(r) = self.selection.range.filter(|r| !r.is_empty()) else {
            return Ok(());
        };
        let tracks = self.edit_tracks();
        let cb = self.range_clipboard(r, &tracks);
        let len = r.length();
        let positions: Vec<MusicalTime> = (0..i64::from(times.max(1)))
            .map(|k| MusicalTime(r.end.ticks() + k * len.ticks()))
            .collect();
        let plan: Vec<Option<TrackId>> = cb.lanes.iter().map(|l| Some(l.origin)).collect();
        let out = self.paste_commands(&cb, &plan, &positions);
        if out.commands.is_empty() {
            return Ok(());
        }
        self.batch(
            if times <= 1 { "Duplicate" } else { "Repeat" },
            out.commands,
        )?;
        let last = positions.last().copied().unwrap_or(r.end);
        self.set_edit_range(Some(EditRange::new(last, last + len)))
    }

    /// Select All in the arranger: every clip on the tracks it shows (not
    /// those in closed folders, not the launcher's), and their tracks. The
    /// edit range goes, so the clips are what edits then work on.
    pub(crate) fn select_all_clips(&mut self) {
        let rows = self.arranger_rows();
        let launcher = self.project.launcher_clips();
        let mut clips = Vec::new();
        let mut tracks = Vec::new();
        for t in &rows {
            let on: Vec<ClipId> = self
                .project
                .clips_of(*t)
                .into_iter()
                .map(|c| c.id)
                .filter(|c| !launcher.contains(c))
                .collect();
            if !on.is_empty() {
                tracks.push(*t);
                clips.extend(on);
            }
        }
        self.selection.select_clips(&clips, SelectMode::Replace);
        self.selection.select_tracks(&tracks, SelectMode::Replace);
        self.selection.range = None;
        self.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: f64) -> MusicalTime {
        MusicalTime::from_quarters(n)
    }

    fn curve(points: &[(f64, f64)]) -> AutomationCurve {
        AutomationCurve::from_points(
            points
                .iter()
                .map(|&(t, value)| AutomationPoint {
                    time: q(t),
                    value,
                    shape: Default::default(),
                })
                .collect(),
        )
    }

    fn near(v: Option<f64>, want: f64) -> bool {
        v.is_some_and(|v| (v - want).abs() < 1e-3)
    }

    #[test]
    fn inserting_time_holds_the_value_over_the_gap() {
        let c = curve(&[(0.0, 0.0), (4.0, 1.0)]);
        let out = curve_inserted(&c, q(2.0), q(8.0));
        assert!(near(out.value_at(q(1.0)), 0.25));
        assert!(near(out.value_at(q(2.0)), 0.5));
        assert!(near(out.value_at(q(6.0)), 0.5));
        assert!(near(out.value_at(q(10.0)), 0.5));
        assert!(near(out.value_at(q(12.0)), 1.0));
    }

    #[test]
    fn removing_time_closes_the_gap() {
        let c = curve(&[(0.0, 0.0), (4.0, 1.0), (8.0, 0.0)]);
        let out = curve_removed(&c, q(2.0), q(6.0));
        assert!(near(out.value_at(q(1.0)), 0.25));
        // What played at 6 now plays from 2 on.
        assert!(near(out.value_at(q(2.0) + TICK), 0.5));
        assert!(near(out.value_at(q(3.0)), 0.25));
        assert!(near(out.value_at(q(4.0)), 0.0));
    }

    #[test]
    fn clearing_keeps_the_values_outside_even_at_the_curves_ends() {
        // The first point is inside: the value before the range stays.
        let c = curve(&[(2.0, 0.2), (5.0, 0.8)]);
        let out = curve_cleared(&c, q(1.0), q(3.0));
        assert!(near(out.value_at(q(0.0)), 0.2));
        assert!(near(out.value_at(q(1.0)), 0.2));
        assert!(near(out.value_at(q(3.0)), 0.4));
        assert!(near(out.value_at(q(4.0)), 0.6));
        // The last point is inside: the value after the range stays.
        let out = curve_cleared(&c, q(4.0), q(6.0));
        assert!(near(out.value_at(q(3.0)), 0.4));
        assert!(near(out.value_at(q(6.0)), 0.8));
        assert!(near(out.value_at(q(9.0)), 0.8));
        // No point inside: nothing changes.
        assert_eq!(curve_cleared(&c, q(3.0), q(4.0)), c);
    }

    #[test]
    fn a_range_keeps_its_values_at_both_ends() {
        let c = curve(&[(0.0, 0.0), (8.0, 1.0)]);
        let pts = range_points(&c, q(2.0), q(4.0));
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].time, q(0.0));
        assert!((pts[0].value - 0.25).abs() < 1e-9);
        assert_eq!(pts[1].time, q(2.0));
        assert!((pts[1].value - 0.5).abs() < 1e-3);
    }

    #[test]
    fn spans_merge() {
        assert_eq!(
            merged(vec![(q(4.0), q(6.0)), (q(0.0), q(2.0)), (q(1.0), q(3.0))]),
            vec![(q(0.0), q(3.0)), (q(4.0), q(6.0))]
        );
    }
}
