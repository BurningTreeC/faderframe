//! Pro-style editing in the arranger.
//!
//! * **Edit modes** (as in Pro Tools): *Grid* snaps to the grid (absolute:
//!   to grid lines; relative: moves by whole grid steps), *Slip* moves
//!   freely, *Shuffle* keeps clips butted together (moving reorders,
//!   trimming and clearing close or open gaps), *Spot* asks for exact
//!   positions.
//! * **Tools**: the Smart tool combines selector (upper half of a clip),
//!   grabber (lower half), trimmer (edges) and fades (upper corners); the
//!   single tools do one thing everywhere.
//! * **The edit selection** is a time range on the selected tracks
//!   ([`EditRange`]); with *Link Timeline and Edit Selection* the playhead
//!   follows it. Operations — separate, trim to selection, clear, insert
//!   silence, nudge, Tab — work on it; copy/cut/paste, duplicate and repeat
//!   take it when it has a length, else the selected clips
//!   ([`crate::clipboard`]).
//!
//! Every operation is one undoable [`Command::Batch`] built from the
//! project's clip commands (split, move, remove, add, set content), with
//! clip ids allocated up front.

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_project::{Clip, ClipContent, Command};
use faderframe_timeline::{GridDivision, MusicalTime};

/// How clips move and trim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EditMode {
    /// Clips stay butted together.
    Shuffle,
    /// Free movement.
    Slip,
    /// Positions are typed in.
    Spot,
    /// Snap to the grid.
    #[default]
    Grid,
}

impl EditMode {
    pub const ALL: [EditMode; 4] = [Self::Shuffle, Self::Slip, Self::Spot, Self::Grid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Shuffle => "Shuffle",
            Self::Slip => "Slip",
            Self::Spot => "Spot",
            Self::Grid => "Grid",
        }
    }
}

/// Grid mode: to grid lines, or by grid steps keeping the offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GridMode {
    #[default]
    Absolute,
    Relative,
}

/// The arranger's edit tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EditTool {
    /// Selector, grabber, trimmer and fades by position in the clip.
    #[default]
    Smart,
    Trim,
    /// Trimming time-stretches the clip.
    TrimStretch,
    Select,
    Grab,
    /// Dragging a selection lifts that part out and moves it.
    GrabSeparation,
    Scrub,
    Pencil,
    Zoom,
}

impl EditTool {
    pub const ALL: [EditTool; 9] = [
        Self::Smart,
        Self::Zoom,
        Self::Trim,
        Self::TrimStretch,
        Self::Select,
        Self::Grab,
        Self::GrabSeparation,
        Self::Scrub,
        Self::Pencil,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Smart => "Smart",
            Self::Trim => "Trim",
            Self::TrimStretch => "Stretch",
            Self::Select => "Select",
            Self::Grab => "Grab",
            Self::GrabSeparation => "Separate",
            Self::Scrub => "Scrub",
            Self::Pencil => "Pencil",
            Self::Zoom => "Zoom",
        }
    }
}

/// How far nudging moves things.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NudgeValue {
    Grid(GridDivision),
    Millis(u32),
}

impl Default for NudgeValue {
    fn default() -> Self {
        Self::Grid(GridDivision::Note(16))
    }
}

impl NudgeValue {
    pub fn label(self) -> String {
        match self {
            Self::Grid(g) => g.label(),
            Self::Millis(ms) if ms >= 1000 => format!("{} s", ms as f64 / 1000.0),
            Self::Millis(ms) => format!("{ms} ms"),
        }
    }

    /// Every nudge value offered.
    pub fn all() -> Vec<NudgeValue> {
        let mut out: Vec<NudgeValue> = GridDivision::all().into_iter().map(Self::Grid).collect();
        out.extend([1, 5, 10, 50, 100, 500, 1000].map(Self::Millis));
        out
    }
}

/// Units of the selection counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CounterUnit {
    #[default]
    BarsBeats,
    MinSecs,
    /// SMPTE timecode at the project's rate and start.
    Timecode,
    Samples,
}

impl CounterUnit {
    pub const ALL: [CounterUnit; 4] = [
        Self::BarsBeats,
        Self::MinSecs,
        Self::Timecode,
        Self::Samples,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::BarsBeats => "Bars|Beats",
            Self::MinSecs => "Min:Sec",
            Self::Timecode => "Timecode",
            Self::Samples => "Samples",
        }
    }
}

/// The position `pos` (samples) as `unit` shows it.
pub fn format_position(
    project: &faderframe_project::Project,
    pos: i64,
    unit: CounterUnit,
) -> String {
    let rate = project.sample_rate;
    let r = rate.max(1) as f64;
    match unit {
        CounterUnit::BarsBeats => project
            .timeline
            .format_bbt(project.timeline.to_musical(pos, r)),
        CounterUnit::MinSecs => faderframe_timeline::format_seconds(pos as f64 / r),
        CounterUnit::Timecode => {
            let tc = project.timecode.unwrap_or_default();
            tc.at(pos, rate).display(tc.rate)
        }
        CounterUnit::Samples => pos.to_string(),
    }
}

/// A duration of `frames` samples as timecode ("00:00:02:12").
pub fn format_timecode_length(project: &faderframe_project::Project, frames: i64) -> String {
    let tc = project.timecode.unwrap_or_default();
    let n = tc
        .rate
        .frame_at(frames.max(0) as f64 / project.sample_rate.max(1) as f64);
    faderframe_core::timecode::Timecode::from_frames(n, tc.rate).display(tc.rate)
}

/// On/off editing options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditFlag {
    /// Tab moves to the next transient instead of the next clip boundary.
    TabToTransients,
    /// The playhead follows the edit selection (clicks in clips locate).
    LinkTimeline,
    /// After stopping, the playhead stays where playback stopped (off:
    /// it returns to where playback started).
    InsertionFollowsPlayback,
    /// The editors scroll to keep the playhead in view while playing.
    FollowPlayhead,
    /// Draw detected transients in audio clips.
    ShowTransients,
    /// Warp view: warp markers on audio clips, dragging them stretches.
    Warp,
    /// The edit toolbar under the transport.
    EditToolbar,
    /// Positions off the grid (free edits, Alt, Slip) land on whole samples.
    SnapToSamples,
    /// Cuts and trims of audio clips move to the nearest zero crossing.
    SnapToZeroCrossings,
    /// Drags snap to clip starts and ends, markers, section bounds, the
    /// loop and selection bounds and the playhead within a few pixels,
    /// ahead of the grid ([`crate::snapping`]).
    SnapToObjects,
    /// The Pitch editor finds every note, chords too.
    PolyphonicPitch,
    /// Stop puts the playhead at the start of the project (off: back to
    /// where playback started, or where it stopped with
    /// [`EditFlag::InsertionFollowsPlayback`]).
    StopToStart,
}

/// How far a cut looks for a zero crossing, each side (seconds).
pub const ZERO_CROSSING_REACH: f64 = 0.005;

/// A time range (start ≤ end; a cursor when equal).
/// A clip with one edge trimmed (or stretched), before it is placed:
/// [`Session::trims_commands`] places several at once.
#[derive(Clone, Debug)]
pub(crate) struct Trimmed {
    /// The clip as the gesture first saw it.
    pub base: Clip,
    /// Its start after the trim (Shuffle: as it was).
    pub start: MusicalTime,
    pub content: ClipContent,
    /// Whether, in Shuffle mode, what follows moves with its end.
    pub shuffles: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EditRange {
    pub start: MusicalTime,
    pub end: MusicalTime,
}

impl EditRange {
    pub fn new(a: MusicalTime, b: MusicalTime) -> Self {
        Self {
            start: a.min(b),
            end: a.max(b),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    pub fn length(&self) -> MusicalTime {
        self.end - self.start
    }
}

/// A zoom request for the arranger (from the edit toolbar or keys).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ZoomRequest {
    In,
    Out,
    /// The edit selection fills the view.
    Selection,
    /// The whole project fits.
    #[default]
    Fit,
}

/// Which edge of a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipEdge {
    Start,
    End,
}

/// What a nudge moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NudgeTarget {
    /// The selected clips (or the selection range when no clip is
    /// selected).
    Move,
    TrimStart,
    TrimEnd,
}

impl Session {
    /// `t` on the nearest whole sample when Snap to Samples is on.
    pub fn on_sample(&self, t: MusicalTime) -> MusicalTime {
        if !self.editor.snap_samples {
            return t;
        }
        let p = &self.project;
        let rate = f64::from(p.sample_rate.max(1));
        p.timeline.to_musical(p.timeline.to_samples(t, rate), rate)
    }

    /// Where a cut of clip `c` at `at` goes: the nearest zero crossing of
    /// its audio within [`ZERO_CROSSING_REACH`] when Snap to Zero
    /// Crossings is on (audio clips played forwards), else `at`.
    pub fn cut_at(&self, c: &faderframe_project::Clip, at: MusicalTime) -> MusicalTime {
        if !self.editor.zero_crossings {
            return at;
        }
        let ClipContent::Audio(a) = &c.content else {
            return at;
        };
        if a.reversed {
            return at;
        }
        let p = &self.project;
        let rate = f64::from(p.sample_rate.max(1));
        let start = p.timeline.to_samples(c.start, rate);
        let rel = p.timeline.to_samples(at, rate) - start;
        let source = if a.warp.is_some() {
            a.source_at(rel as f64).round() as i64
        } else {
            a.source_offset + rel
        };
        let reach = (ZERO_CROSSING_REACH * rate) as i64;
        let Some(frames) = self.source_frames(a.source, source - reach, (2 * reach + 1) as usize)
        else {
            return at;
        };
        // The channels summed; the crossing nearest the cut (the sample of
        // the pair nearer zero).
        let mono: Vec<f32> = (0..frames.first().map_or(0, Vec::len))
            .map(|i| frames.iter().map(|ch| ch[i]).sum())
            .collect();
        let best = (1..mono.len())
            .filter(|&i| (mono[i - 1] <= 0.0) != (mono[i] <= 0.0) || mono[i] == 0.0)
            .map(|i| {
                if mono[i - 1].abs() < mono[i].abs() {
                    i - 1
                } else {
                    i
                }
            })
            .min_by_key(|&i| (i as i64 - reach).abs());
        match best {
            Some(i) => p
                .timeline
                .to_musical(start + rel + (i as i64 - reach), rate),
            None => at,
        }
    }
}

impl Session {
    pub(crate) fn frames_between(&self, a: MusicalTime, b: MusicalTime) -> i64 {
        let p = &self.project;
        let rate = p.sample_rate as f64;
        p.timeline.to_samples(b, rate) - p.timeline.to_samples(a, rate)
    }

    pub(crate) fn clip_end(&self, c: &Clip) -> MusicalTime {
        c.end(&self.project.timeline, self.project.sample_rate)
    }

    /// Does a cut at `at` fall inside `c` (a whole sample from both edges,
    /// for audio), so that splitting there leaves two clips?
    pub(crate) fn cuts_inside(&self, c: &Clip, at: MusicalTime) -> bool {
        let end = self.clip_end(c);
        at > c.start
            && at < end
            && (!c.content.is_audio()
                || (self.frames_between(c.start, at) > 0 && self.frames_between(at, end) > 0))
    }

    /// Tracks the range operations work on (and the arranger paints the
    /// range on): the selected tracks in project order (else the tracks of
    /// the selected clips), then the tracks grouped with them in folder
    /// groups ([`crate::folder_groups`]).
    pub fn edit_tracks(&self) -> Vec<TrackId> {
        let sel = &self.selection;
        let mut out: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| sel.tracks.contains(&t.id))
            .map(|t| t.id)
            .collect();
        if out.is_empty() {
            for c in &sel.clips {
                if let Some(clip) = self.project.clip(*c)
                    && !out.contains(&clip.track)
                {
                    out.push(clip.track);
                }
            }
        }
        self.with_grouped_tracks(out)
    }

    /// The selection range, if it has a length.
    fn range(&self) -> Option<EditRange> {
        self.selection.range.filter(|r| !r.is_empty())
    }

    pub(crate) fn set_edit_range(&mut self, range: Option<EditRange>) -> Result<()> {
        self.selection.range = range;
        if let Some(r) = range
            && self.editor.link_timeline
            && !self.transport.playing
        {
            let s = self.engine.musical_to_samples(&self.project, r.start);
            self.engine
                .transport(faderframe_transport::TransportCommand::Locate(s))?;
            self.show_position(s);
        }
        self.revision += 1;
        Ok(())
    }

    /// Commands that cut out `[a, b)` on `tracks`; returns them and, per
    /// track, the clips that start at or after `b` afterwards (for
    /// shuffling).
    fn clear_commands(
        &mut self,
        a: MusicalTime,
        b: MusicalTime,
        tracks: &[TrackId],
    ) -> (Vec<Command>, Vec<(ClipId, TrackId, MusicalTime)>) {
        let mut cmds = Vec::new();
        let mut after = Vec::new();
        for &track in tracks {
            let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
            for c in clips {
                let end = self.clip_end(&c);
                // Audio is cut on whole samples: a clip less than a sample
                // past an edge is on that edge's side (splitting there
                // would leave nothing and refuse the whole batch).
                let audio = c.content.is_audio();
                if c.start >= b || (audio && self.frames_between(c.start, b) <= 0) {
                    after.push((c.id, track, c.start));
                    continue;
                }
                if end <= a || (audio && self.frames_between(a, end) <= 0) {
                    continue;
                }
                match (self.cuts_inside(&c, a), self.cuts_inside(&c, b)) {
                    // Entirely inside.
                    (false, false) => cmds.push(Command::RemoveClip { clip: c.id }),
                    // Across the whole range: keep both ends.
                    (true, true) => {
                        let mid: ClipId = self.project.ids.allocate();
                        let right: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at: a,
                            new_clip: mid,
                        });
                        cmds.push(Command::SplitClip {
                            clip: mid,
                            at: b,
                            new_clip: right,
                        });
                        cmds.push(Command::RemoveClip { clip: mid });
                        after.push((right, track, b));
                    }
                    // Its end is inside.
                    (true, false) => {
                        let tail: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at: a,
                            new_clip: tail,
                        });
                        cmds.push(Command::RemoveClip { clip: tail });
                    }
                    // Its start is inside.
                    (false, true) => {
                        let right: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at: b,
                            new_clip: right,
                        });
                        cmds.push(Command::RemoveClip { clip: c.id });
                        after.push((right, track, b));
                    }
                }
            }
        }
        (cmds, after)
    }

    pub(crate) fn batch(&mut self, label: &str, commands: Vec<Command>) -> Result<()> {
        if commands.is_empty() {
            return Ok(());
        }
        self.edit(Command::Batch {
            label: label.into(),
            commands,
        })
    }

    /// Delete the selection range on the selected tracks (Shuffle closes
    /// the gap).
    pub(crate) fn clear_range(&mut self) -> Result<bool> {
        self.clear_range_as("Clear")
    }

    /// [`Self::clear_range`] as the undo step `label`: the clips' parts in
    /// the range and the tracks' automation of it go (as Copy takes both).
    pub(crate) fn clear_range_as(&mut self, label: &str) -> Result<bool> {
        let Some(r) = self.range() else {
            return Ok(false);
        };
        let tracks = self.edit_tracks();
        let (mut cmds, after) = self.clear_commands(r.start, r.end, &tracks);
        if self.editor.edit_mode == EditMode::Shuffle {
            let len = r.length();
            for (clip, track, start) in after {
                cmds.push(Command::MoveClip {
                    clip,
                    track,
                    start: (start - len).max(MusicalTime::ZERO),
                });
            }
        }
        // The tracks' automation of the range goes too: the time itself in
        // Shuffle, else the points in it (the values outside stay).
        let mut lanes = crate::clipboard::LaneWork::default();
        for &track in &tracks {
            if self.editor.edit_mode == EditMode::Shuffle {
                lanes.remove_time(&mut self.project, track, r.start, r.end);
            } else {
                lanes.clear_time(&mut self.project, track, r.start, r.end);
            }
        }
        cmds.extend(lanes.commands());
        self.batch(label, cmds)?;
        self.selection.clips.clear();
        Ok(true)
    }

    /// Split the clips on the selected tracks at the range's edges (or the
    /// selected clips at the playhead without a range).
    pub(crate) fn separate(&mut self) -> Result<()> {
        let Some(r) = self.range() else {
            return self.split_at_playhead();
        };
        let tracks = self.edit_tracks();
        let mut cmds = Vec::new();
        // Pieces of selected clips stay selected.
        let mut pieces = Vec::new();
        for track in tracks {
            let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
            for c in clips {
                let selected = self.selection.clips.contains(&c.id);
                // At a zero crossing when that is on and stays inside.
                let cut = |s: &Self, at: MusicalTime| {
                    let z = s.cut_at(&c, at);
                    if s.cuts_inside(&c, z) { z } else { at }
                };
                let cut_a = self.cuts_inside(&c, r.start);
                let cut_b = self.cuts_inside(&c, r.end);
                match (cut_a, cut_b) {
                    (true, true) => {
                        let mid: ClipId = self.project.ids.allocate();
                        let right: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at: cut(self, r.start),
                            new_clip: mid,
                        });
                        cmds.push(Command::SplitClip {
                            clip: mid,
                            at: cut(self, r.end),
                            new_clip: right,
                        });
                        if selected {
                            pieces.extend([mid, right]);
                        }
                    }
                    (true, false) | (false, true) => {
                        let at = cut(self, if cut_a { r.start } else { r.end });
                        let new_clip: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at,
                            new_clip,
                        });
                        if selected {
                            pieces.push(new_clip);
                        }
                    }
                    (false, false) => {}
                }
            }
        }
        if cmds.is_empty() {
            return Ok(());
        }
        self.batch("Separate", cmds)?;
        self.selection.select_clips(&pieces, crate::SelectMode::Add);
        self.revision += 1;
        Ok(())
    }

    /// Keep only what is inside the range of the clips it touches.
    pub(crate) fn trim_to_selection(&mut self) -> Result<()> {
        let Some(r) = self.range() else {
            return Ok(());
        };
        let tracks = self.edit_tracks();
        let mut cmds = Vec::new();
        for track in tracks {
            let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
            for c in clips {
                let end = self.clip_end(&c);
                if end <= r.start || c.start >= r.end {
                    continue;
                }
                // At a zero crossing that stays inside the clip, else at
                // the edge itself; an edge less than a sample inside it
                // cuts nothing.
                let cut = |s: &Self, at: MusicalTime| {
                    let z = s.cut_at(&c, at);
                    if s.cuts_inside(&c, z) {
                        Some(z)
                    } else {
                        s.cuts_inside(&c, at).then_some(at)
                    }
                };
                let mut keep = c.id;
                if c.start < r.start
                    && let Some(at) = cut(self, r.start)
                {
                    let mid: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: c.id,
                        at,
                        new_clip: mid,
                    });
                    cmds.push(Command::RemoveClip { clip: c.id });
                    keep = mid;
                }
                if end > r.end
                    && let Some(at) = cut(self, r.end)
                {
                    let right: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: keep,
                        at,
                        new_clip: right,
                    });
                    cmds.push(Command::RemoveClip { clip: right });
                }
            }
        }
        self.batch("Trim to Selection", cmds)
    }

    /// Move everything at or after the range start on the selected tracks
    /// later by the range's length.
    pub(crate) fn insert_silence(&mut self) -> Result<()> {
        let Some(r) = self.range() else {
            return Ok(());
        };
        let tracks = self.edit_tracks();
        let len = r.length();
        let mut cmds = Vec::new();
        // The tracks' automation makes room too (holding the value there).
        let mut lanes = crate::clipboard::LaneWork::default();
        for track in tracks {
            cmds.extend(self.insert_time_commands(track, r.start, len));
            lanes.insert_time(&mut self.project, track, r.start, len);
        }
        cmds.extend(lanes.commands());
        self.batch("Insert Silence", cmds)
    }

    /// The nudge distance at `at`.
    fn nudge_delta(&self, at: MusicalTime) -> MusicalTime {
        let p = &self.project;
        match self.editor.nudge {
            NudgeValue::Grid(g) => g.step(
                p.timeline
                    .meter
                    .signature_of_bar(p.timeline.meter.bar_at(at)),
            ),
            NudgeValue::Millis(ms) => {
                let rate = p.sample_rate as f64;
                let s = p.timeline.to_samples(at, rate);
                let frames = (ms as f64 / 1000.0 * rate).round() as i64;
                p.timeline.to_musical(s + frames, rate) - at
            }
        }
    }

    /// Nudge the selected clips (move or trim), or the range.
    pub(crate) fn nudge(&mut self, forward: bool, target: NudgeTarget) -> Result<()> {
        let clips: Vec<Clip> = self
            .selected_arranger_clips()
            .into_iter()
            .filter_map(|c| self.project.clip(c).cloned())
            .collect();
        if clips.is_empty() {
            // The range moves.
            if let Some(r) = self.selection.range {
                let d = self.nudge_delta(r.start);
                let r2 = if forward {
                    EditRange::new(r.start + d, r.end + d)
                } else {
                    let d = d.min(r.start);
                    EditRange::new(r.start - d, r.end - d)
                };
                return self.set_edit_range(Some(r2));
            }
            return Ok(());
        }
        let mut cmds = Vec::new();
        let mut trims = Vec::new();
        // Backwards, the group moves together and stops at zero (its
        // spacing kept).
        let room = clips
            .iter()
            .map(|c| c.start)
            .min()
            .unwrap_or(MusicalTime::ZERO);
        for c in clips {
            let d = self.nudge_delta(c.start);
            match target {
                NudgeTarget::Move => {
                    let start = if forward {
                        c.start + d
                    } else {
                        c.start - d.min(room)
                    };
                    cmds.push(Command::MoveClip {
                        clip: c.id,
                        track: c.track,
                        start,
                    });
                }
                // Take folders are not trimmed (as by a drag).
                NudgeTarget::TrimStart | NudgeTarget::TrimEnd
                    if matches!(c.content, ClipContent::Takes(_)) => {}
                NudgeTarget::TrimStart | NudgeTarget::TrimEnd => {
                    let (edge, base) = if target == NudgeTarget::TrimStart {
                        (ClipEdge::Start, c.start)
                    } else {
                        (ClipEdge::End, self.clip_end(&c))
                    };
                    let to = if forward {
                        base + d
                    } else {
                        (base - d).max(MusicalTime::ZERO)
                    };
                    trims.push(self.trim_part(&c, edge, to)?);
                }
            }
        }
        cmds.extend(self.trims_commands(trims)?);
        self.batch("Nudge", cmds)
    }

    /// Commands that move one edge of `c` to `to` (clamped to the audio
    /// available and a minimum length). In Shuffle mode the clip keeps its
    /// start and the clips after it move along with its end.
    pub(crate) fn trim_commands(
        &mut self,
        c: &Clip,
        edge: ClipEdge,
        to: MusicalTime,
    ) -> Result<Vec<Command>> {
        let t = self.trim_part(c, edge, to)?;
        self.trims_commands(vec![t])
    }

    /// `c` (as the gesture first saw it) with one edge moved to `to`, not
    /// yet placed (see [`Self::trims_commands`]).
    pub(crate) fn trim_part(
        &mut self,
        c: &Clip,
        edge: ClipEdge,
        to: MusicalTime,
    ) -> Result<Trimmed> {
        let rate = self.project.sample_rate as f64;
        let end = self.clip_end(c);
        let min_len = MusicalTime::from_quarters(1.0 / 64.0);
        let mut content = c.content.clone();
        let mut start = c.start;
        match (&mut content, edge) {
            (ClipContent::Audio(a), ClipEdge::Start) => {
                let to = to.min(end - min_len);
                let delta = self.frames_between(c.start, to);
                let before = a.length;
                crate::warping::trim_audio_start(a, delta);
                let moved = before - a.length;
                let tl = &self.project.timeline;
                start = tl.to_musical(tl.to_samples(c.start, rate) + moved, rate);
                a.fades.fade_in = a.fades.fade_in.min(a.length);
                a.fades.fade_out = a.fades.fade_out.min(a.length - a.fades.fade_in);
            }
            (ClipContent::Audio(a), ClipEdge::End) => {
                let p = &self.project;
                let source_frames = p
                    .sources
                    .get(&a.source)
                    .map_or(i64::MAX, |s| s.frames(p.sample_rate));
                let len = self.frames_between(c.start, to.max(c.start + min_len));
                crate::warping::trim_audio_end(a, len, source_frames);
                a.fades.fade_out = a.fades.fade_out.min(a.length);
                a.fades.fade_in = a.fades.fade_in.min(a.length - a.fades.fade_out);
            }
            (ClipContent::Midi(m), ClipEdge::Start) => {
                let to = to.min(end - min_len).max(MusicalTime::ZERO);
                let shift = to - c.start;
                start = to;
                m.length -= shift;
                m.notes.retain(|n| n.end() > shift);
                for n in &mut m.notes {
                    if n.start < shift {
                        n.length = n.end() - shift;
                        n.start = MusicalTime::ZERO;
                    } else {
                        n.start -= shift;
                    }
                }
                for l in &mut m.controllers {
                    let carried = l.value_at(shift);
                    l.points.retain(|pt| pt.time >= shift);
                    for pt in &mut l.points {
                        pt.time -= shift;
                    }
                    if let Some(v) = carried
                        && l.points
                            .first()
                            .is_none_or(|pt| pt.time > MusicalTime::ZERO)
                    {
                        l.points.insert(
                            0,
                            faderframe_project::ControllerPoint {
                                time: MusicalTime::ZERO,
                                value: v,
                            },
                        );
                    }
                }
                m.sysex.retain(|e| e.time >= shift);
                for e in &mut m.sysex {
                    e.time -= shift;
                }
                m.prune_expressions();
            }
            (ClipContent::Midi(m), ClipEdge::End) => {
                m.length = (to - c.start).max(min_len);
            }
            (ClipContent::Takes(_), _) => {
                return Err(SessionError::Other(
                    "take folders cannot be trimmed; flatten the comp first".into(),
                ));
            }
        }
        if self.editor.edit_mode == EditMode::Shuffle {
            // The clip stays where it is; what follows closes up or opens.
            start = c.start;
        }
        Ok(Trimmed {
            base: c.clone(),
            start,
            content,
            shuffles: true,
        })
    }

    /// The commands placing trimmed clips. In Shuffle mode every clip
    /// after a trimmed one's end (on its track, as the gesture first saw
    /// them) moves by as much as that end moved — the trimmed ones too, so
    /// several trims on one track add up instead of overlapping.
    pub(crate) fn trims_commands(&mut self, trims: Vec<Trimmed>) -> Result<Vec<Command>> {
        let shuffle = self.editor.edit_mode == EditMode::Shuffle;
        // (track, old end, how far it moved) of each shuffling trim.
        let mut moves: Vec<(TrackId, MusicalTime, i64)> = Vec::new();
        if shuffle {
            for t in trims.iter().filter(|t| t.shuffles) {
                let old_end = self.clip_end(&t.base);
                let new_end = self.clip_end(&Clip {
                    content: t.content.clone(),
                    start: t.start,
                    ..t.base.clone()
                });
                moves.push((t.base.track, old_end, (new_end - old_end).ticks()));
            }
        }
        let shift = |track: TrackId, start: MusicalTime, own: ClipId, trims: &[Trimmed]| {
            moves
                .iter()
                .zip(trims.iter().filter(|t| t.shuffles))
                .filter(|((t, old_end, _), x)| *t == track && *old_end <= start && x.base.id != own)
                .map(|((_, _, d), _)| *d)
                .sum::<i64>()
        };
        let mut cmds = Vec::new();
        for t in &trims {
            let by = shift(t.base.track, t.base.start, t.base.id, &trims);
            cmds.push(Command::SetClipContent {
                clip: t.base.id,
                start: MusicalTime((t.start.ticks() + by).max(0)),
                content: Box::new(t.content.clone()),
            });
        }
        if shuffle && !moves.is_empty() {
            let mut tracks: Vec<TrackId> = moves.iter().map(|m| m.0).collect();
            tracks.sort_unstable();
            tracks.dedup();
            for track in tracks {
                let ids: Vec<ClipId> = self
                    .project
                    .clips_of(track)
                    .into_iter()
                    .map(|o| o.id)
                    .filter(|id| !trims.iter().any(|t| t.base.id == *id))
                    .collect();
                for id in ids {
                    let o = self.gesture_clip(id)?;
                    let by = shift(track, o.start, id, &trims);
                    if by != 0 {
                        cmds.push(Command::MoveClip {
                            clip: id,
                            track: o.track,
                            start: MusicalTime((o.start.ticks() + by).max(0)),
                        });
                    }
                }
            }
        }
        Ok(cmds)
    }

    /// Move one edge of a clip.
    pub fn trim_clip(&mut self, clip: ClipId, edge: ClipEdge, to: MusicalTime) -> Result<()> {
        let c = self.gesture_clip(clip)?;
        let cmds = self.trim_commands(&c, edge, to)?;
        self.batch("Trim Clip", cmds)
    }

    /// Fade lengths of an audio clip in frames (clamped to its length).
    pub fn set_clip_fades(&mut self, clip: ClipId, fade_in: i64, fade_out: i64) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let ClipContent::Audio(mut a) = c.content else {
            return Ok(());
        };
        let fade_in = fade_in.clamp(0, a.length);
        a.fades.fade_in = fade_in;
        a.fades.fade_out = fade_out.clamp(0, a.length - fade_in);
        self.batch(
            "Fade",
            vec![Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            }],
        )
    }

    /// Clip gain of an audio clip.
    pub fn set_clip_gain(&mut self, clip: ClipId, db: f32) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let ClipContent::Audio(mut a) = c.content else {
            return Ok(());
        };
        a.gain_db = db.clamp(-60.0, 24.0);
        self.batch(
            "Clip Gain",
            vec![Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Audio(a)),
            }],
        )
    }

    /// Shuffle a clip to `at` on `track`: the gap it leaves closes, and it
    /// lands at the clip boundary nearest `at`, pushing later clips along.
    pub fn shuffle_clip(&mut self, clip: ClipId, track: TrackId, at: MusicalTime) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        // (The arranger previews shuffle drags without moving the clip.)
        let original = c.clone();
        let len = self.clip_end(&original) - original.start;
        // Others on the old track after the gap move up; then make room.
        let mut positions: Vec<(ClipId, TrackId, MusicalTime)> = Vec::new();
        for o in self.project.clips_of(original.track) {
            if o.id == clip {
                continue;
            }
            let s = if o.start > original.start {
                (o.start - len).max(MusicalTime::ZERO)
            } else {
                o.start
            };
            positions.push((o.id, o.track, s));
        }
        if track != original.track {
            for o in self.project.clips_of(track) {
                positions.push((o.id, o.track, o.start));
            }
        }
        // Nearest boundary on the target track.
        let mut bounds = vec![MusicalTime::ZERO];
        for &(id, t, s) in &positions {
            if t == track
                && let Some(o) = self.project.clip(id)
            {
                bounds.push(s);
                bounds.push(s + (self.clip_end(o) - o.start));
            }
        }
        let dest = bounds
            .into_iter()
            .min_by_key(|b| (b.ticks() - at.ticks()).abs())
            .unwrap_or(at);
        let mut cmds = Vec::new();
        for (id, t, s) in positions {
            let s = if t == track && s >= dest { s + len } else { s };
            cmds.push(Command::MoveClip {
                clip: id,
                track: t,
                start: s,
            });
        }
        cmds.push(Command::MoveClip {
            clip,
            track,
            start: dest,
        });
        self.batch("Shuffle Clip", cmds)
    }

    /// Place a clip at an exact position (Spot mode).
    pub fn spot_clip(&mut self, clip: ClipId, start: MusicalTime) -> Result<()> {
        let c = self
            .project
            .clip(clip)
            .cloned()
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        self.batch(
            "Spot Clip",
            vec![Command::MoveClip {
                clip,
                track: c.track,
                start: start.max(MusicalTime::ZERO),
            }],
        )
    }

    /// Positions Tab stops at: clip boundaries on the selected tracks (all
    /// tracks without a selection), or detected transients.
    fn tab_stops(&self) -> Vec<MusicalTime> {
        let tracks = self.edit_tracks();
        let tracks: Vec<TrackId> = if tracks.is_empty() {
            self.project.tracks.iter().map(|t| t.id).collect()
        } else {
            tracks
        };
        let mut out = vec![MusicalTime::ZERO];
        for t in &tracks {
            for c in self.project.clips_of(*t) {
                if self.editor.tab_to_transients {
                    out.extend(self.clip_transients(c));
                } else {
                    out.push(c.start);
                    out.push(self.clip_end(c));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Move the playhead to the next (or previous) stop; `extend` grows the
    /// selection to it.
    pub(crate) fn tab_to(&mut self, forward: bool, extend: bool) -> Result<()> {
        let here = self.playhead();
        let stops = self.tab_stops();
        let eps = MusicalTime(1);
        let next = if forward {
            stops.into_iter().find(|s| *s > here + eps)
        } else {
            stops.into_iter().rev().find(|s| *s + eps < here)
        };
        let Some(to) = next else { return Ok(()) };
        let s = self.engine.musical_to_samples(&self.project, to);
        self.engine
            .transport(faderframe_transport::TransportCommand::Locate(s))?;
        self.show_position(s);
        if extend {
            let anchor = match self.selection.range {
                Some(r) if r.end == here => r.start,
                Some(r) if r.start == here => r.end,
                _ => here,
            };
            self.selection.range = Some(EditRange::new(anchor, to));
        } else {
            self.selection.range = Some(EditRange::new(to, to));
        }
        self.revision += 1;
        Ok(())
    }
}

/// Can `clip` live on `track`?
fn fits(track: &faderframe_project::Track, clip: &Clip) -> bool {
    use faderframe_project::TrackKind;
    match clip.content {
        ClipContent::Audio(_) | ClipContent::Takes(_) => track.kind == TrackKind::Audio,
        ClipContent::Midi(_) => matches!(track.kind, TrackKind::Instrument | TrackKind::Midi),
    }
}

/// Edits of several clips at once (everything selected): each clip is
/// changed relative to where the running gesture found it.
impl Session {
    pub(crate) fn move_clips(&mut self, clips: &[ClipId], by: i64, tracks: i32) -> Result<()> {
        let mut base = Vec::with_capacity(clips.len());
        for id in clips {
            base.push(self.gesture_clip(*id)?);
        }
        if base.is_empty() {
            return Ok(());
        }
        // Nothing moves before the timeline start.
        let earliest = base.iter().map(|c| c.start.ticks()).min().unwrap_or(0);
        let by = by.max(-earliest);
        // The arranger's rows (folder order, closed folders' tracks left
        // out): `tracks` counts them, as the arranger shows them.
        let rows = self.arranger_rows();
        let lanes: Vec<&faderframe_project::Track> = rows
            .iter()
            .filter_map(|id| self.project.track(*id))
            .collect();
        let target = |c: &Clip| -> Option<TrackId> {
            let i = lanes.iter().position(|t| t.id == c.track)? as i32 + tracks;
            let t = lanes.get(usize::try_from(i).ok()?)?;
            fits(t, c).then_some(t.id)
        };
        let all_fit = tracks == 0 || base.iter().all(|c| target(c).is_some());
        let cmds = base
            .iter()
            .map(|c| Command::MoveClip {
                clip: c.id,
                track: if all_fit && tracks != 0 {
                    target(c).unwrap_or(c.track)
                } else {
                    c.track
                },
                start: MusicalTime(c.start.ticks() + by),
            })
            .collect();
        self.batch(
            if clips.len() > 1 {
                "Move Clips"
            } else {
                "Move Clip"
            },
            cmds,
        )
    }

    pub(crate) fn trim_clips(
        &mut self,
        clips: &[ClipId],
        edge: ClipEdge,
        by: i64,
        stretch: bool,
    ) -> Result<()> {
        let mut parts = Vec::new();
        for id in clips {
            let c = self.gesture_clip(*id)?;
            if matches!(c.content, ClipContent::Takes(_)) {
                continue;
            }
            let edge_at = match edge {
                ClipEdge::Start => c.start,
                ClipEdge::End => self.clip_end(&c),
            };
            let to = MusicalTime((edge_at.ticks() + by).max(0));
            if stretch {
                parts.push(self.stretch_part(*id, edge, to)?);
            } else {
                let to = self.cut_at(&c, to);
                parts.push(self.trim_part(&c, edge, to)?);
            }
        }
        let cmds = self.trims_commands(parts)?;
        self.batch(if stretch { "Time Stretch" } else { "Trim Clip" }, cmds)
    }

    /// Clip gain: `delta` added to the gain the gesture started from, or
    /// set to `db`.
    pub(crate) fn clip_gain(
        &mut self,
        clips: &[ClipId],
        delta: Option<f32>,
        db: Option<f32>,
    ) -> Result<()> {
        let mut cmds = Vec::new();
        for id in clips {
            let c = self.gesture_clip(*id)?;
            let gain = |g: f32| db.unwrap_or(g + delta.unwrap_or(0.0)).clamp(-60.0, 24.0);
            let content = match &c.content {
                ClipContent::Audio(a) => {
                    let mut a = a.clone();
                    a.gain_db = gain(a.gain_db);
                    ClipContent::Audio(a)
                }
                ClipContent::Takes(f) => {
                    let mut f = f.clone();
                    f.gain_db = gain(f.gain_db);
                    ClipContent::Takes(f)
                }
                ClipContent::Midi(_) => continue,
            };
            cmds.push(Command::SetClipContent {
                clip: c.id,
                start: c.start,
                content: Box::new(content),
            });
        }
        self.batch("Clip Gain", cmds)
    }

    /// Fade settings of one edge of clips (lengths clamped to each clip).
    pub(crate) fn set_fade(
        &mut self,
        clips: &[ClipId],
        edge: ClipEdge,
        length: Option<i64>,
        shape: Option<faderframe_project::FadeShape>,
        bend: Option<i16>,
    ) -> Result<()> {
        let mut cmds = Vec::new();
        for id in clips {
            let c = self.gesture_clip(*id)?;
            let mut content = c.content.clone();
            let (fades, len) = match &mut content {
                ClipContent::Audio(a) => (&mut a.fades, a.length),
                ClipContent::Takes(f) => (&mut f.fades, f.length),
                ClipContent::Midi(_) => continue,
            };
            match edge {
                ClipEdge::Start => {
                    if let Some(l) = length {
                        // Up to the other fade, never over it.
                        fades.fade_in = l.clamp(0, (len - fades.fade_out).max(0));
                    }
                    if let Some(s) = shape {
                        fades.fade_in_shape = s;
                    }
                    if let Some(b) = bend {
                        fades.fade_in_bend = b.clamp(-100, 100);
                    }
                }
                ClipEdge::End => {
                    if let Some(l) = length {
                        fades.fade_out = l.clamp(0, (len - fades.fade_in).max(0));
                    }
                    if let Some(s) = shape {
                        fades.fade_out_shape = s;
                    }
                    if let Some(b) = bend {
                        fades.fade_out_bend = b.clamp(-100, 100);
                    }
                }
            }
            cmds.push(Command::SetClipContent {
                clip: c.id,
                start: c.start,
                content: Box::new(content),
            });
        }
        self.batch("Fade", cmds)
    }
}

/// A typed position in `unit` ("5.3.480", "1:02.500", "01:00:10:12",
/// "96000"), or `None`. Timecode may leave out leading fields: they fill
/// from the right ("10:12" = 00:00:10:12).
pub fn parse_position(
    text: &str,
    unit: CounterUnit,
    timeline: &faderframe_timeline::Timeline,
    rate: u32,
    tc: faderframe_project::video::ProjectTimecode,
) -> Option<MusicalTime> {
    let text = text.trim();
    let r = rate.max(1) as f64;
    match unit {
        CounterUnit::BarsBeats => timeline.parse_bbt(text),
        CounterUnit::MinSecs => {
            let (m, s) = match text.rsplit_once(':') {
                Some((m, s)) => (m.trim().parse::<f64>().ok()?, s.trim().parse::<f64>().ok()?),
                None => (0.0, text.parse::<f64>().ok()?),
            };
            let secs = m * 60.0 + s;
            (secs.is_finite() && secs >= 0.0)
                .then(|| timeline.to_musical((secs * r).round() as i64, r))
        }
        CounterUnit::Timecode => {
            let mut fields: Vec<&str> = text.split([':', ';', '.', ',']).collect();
            if fields.len() > 4 || fields.iter().any(|f| f.trim().is_empty()) {
                return None;
            }
            while fields.len() < 4 {
                fields.insert(0, "0");
            }
            let label = faderframe_core::timecode::Timecode::parse(&fields.join(":"), tc.rate)?;
            let f = tc.position_of(label, rate);
            (f >= 0).then(|| timeline.to_musical(f, r))
        }
        CounterUnit::Samples => {
            let f: i64 = text.replace(['_', ','], "").parse().ok()?;
            (f >= 0).then(|| timeline.to_musical(f, r))
        }
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;

    #[test]
    fn typed_positions() {
        let mut tl = faderframe_timeline::Timeline::default();
        tl.tempo.set_initial_bpm(120.0);
        let q = MusicalTime::from_quarters;
        let tc = faderframe_project::video::ProjectTimecode {
            rate: faderframe_core::timecode::FrameRate::Fps25,
            start: faderframe_core::timecode::Timecode {
                hours: 1,
                minutes: 0,
                seconds: 0,
                frames: 0,
            },
        };
        assert_eq!(
            parse_position("01:00:02:00", CounterUnit::Timecode, &tl, 48_000, tc),
            Some(q(4.0))
        );
        assert_eq!(
            parse_position("1:0:3:0", CounterUnit::Timecode, &tl, 48_000, tc),
            Some(q(6.0))
        );
        assert_eq!(
            parse_position("10:00", CounterUnit::Timecode, &tl, 48_000, tc),
            None,
            "before the start"
        );
        assert_eq!(
            parse_position("3", CounterUnit::BarsBeats, &tl, 48_000, tc),
            Some(q(8.0))
        );
        assert_eq!(
            parse_position("2.3.480", CounterUnit::BarsBeats, &tl, 48_000, tc),
            Some(q(6.5))
        );
        assert_eq!(
            parse_position("0.1", CounterUnit::BarsBeats, &tl, 48_000, tc),
            None
        );
        assert_eq!(
            parse_position("0:01.5", CounterUnit::MinSecs, &tl, 48_000, tc),
            Some(q(3.0))
        );
        assert_eq!(
            parse_position("2", CounterUnit::MinSecs, &tl, 48_000, tc),
            Some(q(4.0))
        );
        assert_eq!(
            parse_position("24_000", CounterUnit::Samples, &tl, 48_000, tc),
            Some(q(1.0))
        );
        assert_eq!(
            parse_position("x", CounterUnit::Samples, &tl, 48_000, tc),
            None
        );
    }
}
