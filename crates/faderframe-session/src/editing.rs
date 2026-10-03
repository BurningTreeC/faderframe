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
//!   follows it. Operations — separate, trim to selection, clear, copy/cut/
//!   paste, duplicate, repeat, insert silence, nudge, Tab — work on it.
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
    Samples,
}

impl CounterUnit {
    pub const ALL: [CounterUnit; 3] = [Self::BarsBeats, Self::MinSecs, Self::Samples];

    pub fn label(self) -> &'static str {
        match self {
            Self::BarsBeats => "Bars|Beats",
            Self::MinSecs => "Min:Sec",
            Self::Samples => "Samples",
        }
    }
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
}

/// A time range (start ≤ end; a cursor when equal).
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

/// A copied time range: clip parts per track (index among the copied
/// tracks), positions relative to the range start.
#[derive(Clone, Debug, Default)]
pub(crate) struct RangeClipboard {
    pub(crate) length: MusicalTime,
    pub(crate) parts: Vec<(usize, Clip)>,
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

    /// Tracks the range operations work on: the selected tracks in project
    /// order (else the tracks of the selected clips).
    pub(crate) fn edit_tracks(&self) -> Vec<TrackId> {
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
        out
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
            self.transport.position = s;
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
                if c.start >= b {
                    after.push((c.id, track, c.start));
                    continue;
                }
                if end <= a {
                    continue;
                }
                match (c.start < a, end > b) {
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
        self.batch("Clear", cmds)?;
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
        for track in tracks {
            let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
            for c in clips {
                let end = self.clip_end(&c);
                let cut_a = c.start < r.start && end > r.start;
                let cut_b = c.start < r.end && end > r.end;
                match (cut_a, cut_b) {
                    (true, true) => {
                        let mid: ClipId = self.project.ids.allocate();
                        let right: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at: r.start,
                            new_clip: mid,
                        });
                        cmds.push(Command::SplitClip {
                            clip: mid,
                            at: r.end,
                            new_clip: right,
                        });
                    }
                    (true, false) | (false, true) => {
                        let at = if cut_a { r.start } else { r.end };
                        let new_clip: ClipId = self.project.ids.allocate();
                        cmds.push(Command::SplitClip {
                            clip: c.id,
                            at,
                            new_clip,
                        });
                    }
                    (false, false) => {}
                }
            }
        }
        self.batch("Separate", cmds)
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
                let mut keep = c.id;
                if c.start < r.start {
                    let mid: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: c.id,
                        at: r.start,
                        new_clip: mid,
                    });
                    cmds.push(Command::RemoveClip { clip: c.id });
                    keep = mid;
                }
                if end > r.end {
                    let right: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: keep,
                        at: r.end,
                        new_clip: right,
                    });
                    cmds.push(Command::RemoveClip { clip: right });
                }
            }
        }
        self.batch("Trim to Selection", cmds)
    }

    /// Copy the range on the selected tracks.
    pub(crate) fn copy_range(&mut self) -> Result<()> {
        let Some(r) = self.range() else {
            return Ok(());
        };
        let tracks = self.edit_tracks();
        let mut parts = Vec::new();
        for (i, track) in tracks.iter().enumerate() {
            for c in self.project.clips_of(*track) {
                if let Some(mut part) = c.slice(
                    r.start,
                    r.end,
                    c.id,
                    &self.project.timeline,
                    self.project.sample_rate,
                ) {
                    part.start -= r.start;
                    parts.push((i, part));
                }
            }
        }
        self.range_clipboard = RangeClipboard {
            length: r.length(),
            parts,
        };
        Ok(())
    }

    /// Commands placing the clipboard at `at` on `tracks` (overwriting).
    fn paste_commands(&mut self, at: MusicalTime, tracks: &[TrackId]) -> Vec<Command> {
        let clip = self.range_clipboard.clone();
        if clip.parts.is_empty() {
            return Vec::new();
        }
        let used: Vec<TrackId> = (0..tracks.len())
            .filter(|i| clip.parts.iter().any(|(t, _)| t == i))
            .map(|i| tracks[i])
            .collect();
        let (mut cmds, _) = self.clear_commands(at, at + clip.length, &used);
        for (i, part) in clip.parts {
            let Some(&track) = tracks.get(i) else {
                continue;
            };
            let fits = self
                .project
                .track(track)
                .is_some_and(|t| match part.content {
                    ClipContent::Audio(_) | ClipContent::Takes(_) => {
                        t.kind == faderframe_project::TrackKind::Audio
                    }
                    ClipContent::Midi(_) => matches!(
                        t.kind,
                        faderframe_project::TrackKind::Instrument
                            | faderframe_project::TrackKind::Midi
                    ),
                });
            if !fits {
                continue;
            }
            let id: ClipId = self.project.ids.allocate();
            cmds.push(Command::AddClip {
                clip: Box::new(Clip {
                    id,
                    track,
                    start: at + part.start,
                    ..part
                }),
            });
        }
        cmds
    }

    /// Paste the copied range at the playhead on the selected tracks.
    pub(crate) fn paste_range(&mut self) -> Result<()> {
        let at = self.playhead();
        let tracks = self.edit_tracks();
        let cmds = self.paste_commands(at, &tracks);
        self.batch("Paste", cmds)?;
        let len = self.range_clipboard.length;
        self.selection.range = Some(EditRange::new(at, at + len));
        self.revision += 1;
        Ok(())
    }

    /// Copies of the range right after it, `times` times; the selection
    /// moves onto the last copy.
    pub(crate) fn repeat_range(&mut self, times: u32) -> Result<()> {
        let Some(r) = self.range() else {
            return Ok(());
        };
        let saved = std::mem::take(&mut self.range_clipboard);
        self.copy_range()?;
        let tracks = self.edit_tracks();
        let len = r.length();
        let mut cmds = Vec::new();
        let mut at = r.end;
        for _ in 0..times.max(1) {
            cmds.extend(self.paste_commands(at, &tracks));
            at += len;
        }
        self.range_clipboard = saved;
        self.batch(if times <= 1 { "Duplicate" } else { "Repeat" }, cmds)?;
        self.set_edit_range(Some(EditRange::new(at - len, at)))
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
        for track in tracks {
            let clips: Vec<Clip> = self.project.clips_of(track).into_iter().cloned().collect();
            for c in clips {
                let end = self.clip_end(&c);
                if c.start >= r.start {
                    cmds.push(Command::MoveClip {
                        clip: c.id,
                        track,
                        start: c.start + len,
                    });
                } else if end > r.start {
                    let right: ClipId = self.project.ids.allocate();
                    cmds.push(Command::SplitClip {
                        clip: c.id,
                        at: r.start,
                        new_clip: right,
                    });
                    cmds.push(Command::MoveClip {
                        clip: right,
                        track,
                        start: r.start + len,
                    });
                }
            }
        }
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
            .selection
            .clips
            .iter()
            .filter_map(|c| self.project.clip(*c).cloned())
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
        for c in clips {
            let d = self.nudge_delta(c.start);
            match target {
                NudgeTarget::Move => {
                    let start = if forward {
                        c.start + d
                    } else {
                        (c.start - d).max(MusicalTime::ZERO)
                    };
                    cmds.push(Command::MoveClip {
                        clip: c.id,
                        track: c.track,
                        start,
                    });
                }
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
                    cmds.extend(self.trim_commands(&c, edge, to)?);
                }
            }
        }
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
        let shuffle = self.editor.edit_mode == EditMode::Shuffle;
        if shuffle {
            // The clip stays where it is; what follows closes up or opens.
            start = c.start;
        }
        let new_end = self.clip_end(&Clip {
            content: content.clone(),
            start,
            ..c.clone()
        });
        let mut cmds = vec![Command::SetClipContent {
            clip: c.id,
            start,
            content: Box::new(content),
        }];
        if shuffle {
            cmds.extend(self.shuffle_later(c, end, new_end)?);
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
        self.transport.position = s;
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
        let lanes: Vec<&faderframe_project::Track> = self
            .project
            .tracks
            .iter()
            // The arranger's rows: every track but the master.
            .filter(|t| t.kind != faderframe_project::TrackKind::Master)
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
        let mut cmds = Vec::new();
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
                cmds.extend(self.stretch_commands(*id, edge, to)?);
            } else {
                cmds.extend(self.trim_commands(&c, edge, to)?);
            }
        }
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

/// A typed position in `unit` ("5.3.480", "1:02.500", "96000"), or `None`.
pub fn parse_position(
    text: &str,
    unit: CounterUnit,
    timeline: &faderframe_timeline::Timeline,
    rate: u32,
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
        assert_eq!(
            parse_position("3", CounterUnit::BarsBeats, &tl, 48_000),
            Some(q(8.0))
        );
        assert_eq!(
            parse_position("2.3.480", CounterUnit::BarsBeats, &tl, 48_000),
            Some(q(6.5))
        );
        assert_eq!(
            parse_position("0.1", CounterUnit::BarsBeats, &tl, 48_000),
            None
        );
        assert_eq!(
            parse_position("0:01.5", CounterUnit::MinSecs, &tl, 48_000),
            Some(q(3.0))
        );
        assert_eq!(
            parse_position("2", CounterUnit::MinSecs, &tl, 48_000),
            Some(q(4.0))
        );
        assert_eq!(
            parse_position("24_000", CounterUnit::Samples, &tl, 48_000),
            Some(q(1.0))
        );
        assert_eq!(parse_position("x", CounterUnit::Samples, &tl, 48_000), None);
    }
}
