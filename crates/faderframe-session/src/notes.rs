//! Note editing for the piano roll: operations on selected notes, copy and
//! paste, duplication, splitting, chords, controller lanes and the editor's
//! musical settings. Every action is one undoable command (a labelled batch
//! around `SetClipContent` or note commands); ids are allocated here, never
//! by the view.

use crate::{Result, SelectMode, Session, SessionError};
use faderframe_core::{ClipId, NoteId};
use faderframe_project::midi_ops::{self, ChordKind, QuantizeSettings, Scale};
use faderframe_project::midi_tools::{self, Tool, ToolContext, ToolSettings};
use faderframe_project::{
    ClipContent, Command, ControllerPoint, ExpressionKind, ExpressionPoint, MidiClip,
    MidiController, MidiNote, NoteExpression,
};
use faderframe_timeline::{GridDivision, MusicalTime};
use std::collections::HashSet;

/// Something done to a set of notes (or all notes of the clip).
#[derive(Clone, Debug, PartialEq)]
pub enum NoteOp {
    Quantize(QuantizeSettings),
    /// Random offsets up to `timing` and `velocity` steps.
    Humanize {
        timing: MusicalTime,
        velocity: u8,
    },
    Legato,
    Transpose(i32),
    /// Transpose by scale degrees.
    TransposeInScale(i32, Scale),
    SetLength(MusicalTime),
    ScaleLength(f64),
    Reverse,
    Invert,
    SetVelocity(u8),
    /// `v * factor + offset`.
    ScaleVelocity {
        factor: f32,
        offset: i32,
    },
    RampVelocity {
        from: u8,
        to: u8,
    },
    FoldToScale(Scale),
    RemoveOverlaps,
    SetMuted(bool),
    ToggleMuted,
    SetChannel(u8),
    /// Move and/or transpose (drags, nudges).
    Move {
        by: MusicalTime,
        keys: i32,
    },
    /// Change starts and/or ends (resizing from either edge).
    Resize {
        start: MusicalTime,
        end: MusicalTime,
    },
}

impl NoteOp {
    pub fn label(&self) -> &'static str {
        match self {
            NoteOp::Quantize(_) => "Quantize",
            NoteOp::Humanize { .. } => "Humanize",
            NoteOp::Legato => "Legato",
            NoteOp::Transpose(_) | NoteOp::TransposeInScale(..) => "Transpose",
            NoteOp::SetLength(_) | NoteOp::ScaleLength(_) => "Change Length",
            NoteOp::Reverse => "Reverse",
            NoteOp::Invert => "Invert",
            NoteOp::SetVelocity(_) | NoteOp::ScaleVelocity { .. } | NoteOp::RampVelocity { .. } => {
                "Change Velocity"
            }
            NoteOp::FoldToScale(_) => "Fold to Scale",
            NoteOp::RemoveOverlaps => "Remove Overlaps",
            NoteOp::SetMuted(_) | NoteOp::ToggleMuted => "Mute Notes",
            NoteOp::SetChannel(_) => "Change Channel",
            NoteOp::Move { .. } => "Move Notes",
            NoteOp::Resize { .. } => "Resize Notes",
        }
    }
}

/// Length of newly drawn notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoteLength {
    /// One grid step.
    #[default]
    Grid,
    /// Like the last note drawn or resized.
    Last,
    Fixed(GridDivision),
}

/// What the piano roll shows on the key axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyFold {
    /// All 128 keys.
    #[default]
    Off,
    /// Only keys of the scale.
    Scale,
    /// Only keys the clip uses.
    Used,
}

/// The piano roll's musical settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PianoRollSettings {
    /// The scale where the project has no key, or when not following it.
    pub scale: Scale,
    /// The scale is the key track's key at each position.
    pub follow_key: bool,
    /// Drawn and moved notes land on scale keys.
    pub scale_snap: bool,
    pub chord: ChordKind,
    pub note_length: NoteLength,
    pub velocity: u8,
    /// Other clips of the track shown behind.
    pub ghost_notes: bool,
    /// Play notes while drawing, moving and clicking keys.
    pub audition: bool,
    pub fold: KeyFold,
    /// The lane under the notes: velocity (`None`) or a controller on a
    /// channel.
    pub lane: Option<(MidiController, u8)>,
    /// Per-note expression in the lane instead (overrides `lane`).
    pub expression: Option<ExpressionKind>,
    /// The MIDI Tools panel's tool (`None`: the panel is closed).
    pub tool: Option<Tool>,
    /// Every tool's settings.
    pub tools: ToolSettings,
}

impl Default for PianoRollSettings {
    fn default() -> Self {
        Self {
            scale: Scale::default(),
            follow_key: true,
            scale_snap: false,
            chord: ChordKind::Single,
            note_length: NoteLength::Grid,
            velocity: 100,
            ghost_notes: true,
            audition: true,
            fold: KeyFold::Off,
            lane: None,
            expression: None,
            tool: None,
            tools: ToolSettings::default(),
        }
    }
}

/// What the MIDI Tools panel's tool would do to the open clip.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolPreview {
    pub tool: Tool,
    /// Notes it takes away (by id).
    pub removed: Vec<NoteId>,
    /// The notes it leaves or adds (clip time; id 0: a new note).
    pub notes: Vec<MidiNote>,
    /// A generator's range (clip time).
    pub range: Option<(MusicalTime, MusicalTime)>,
}

impl Session {
    fn midi_clip(&self, clip: ClipId) -> Result<(MusicalTime, MidiClip)> {
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        let m = c
            .as_midi()
            .ok_or_else(|| SessionError::Other("not a MIDI clip".into()))?;
        Ok((c.start, m.clone()))
    }

    fn set_midi_clip(&mut self, clip: ClipId, label: &str, mut m: MidiClip) -> Result<()> {
        let start = self
            .project
            .clip(clip)
            .map(|c| c.start)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        for n in &mut m.notes {
            n.start = n.start.max(MusicalTime::ZERO);
            n.length = n.length.max(MusicalTime(1));
            n.velocity = n.velocity.clamp(1, 127);
        }
        m.sort_notes();
        m.prune_expressions();
        self.edit(Command::Batch {
            label: label.into(),
            commands: vec![Command::SetClipContent {
                clip,
                start,
                content: Box::new(ClipContent::Midi(m)),
            }],
        })
    }

    /// Apply `op` to `notes` of `clip` (all notes when empty).
    pub fn note_operation(&mut self, clip: ClipId, notes: &[NoteId], op: &NoteOp) -> Result<()> {
        let (clip_start, mut m) = self.midi_clip(clip)?;
        let ids: HashSet<NoteId> = if notes.is_empty() {
            m.notes.iter().map(|n| n.id).collect()
        } else {
            notes.iter().copied().collect()
        };
        let mut sel: Vec<MidiNote> = m
            .notes
            .iter()
            .filter(|n| ids.contains(&n.id))
            .copied()
            .collect();
        if sel.is_empty() {
            return Ok(());
        }
        let meter = &self.project.timeline.meter;
        match op {
            NoteOp::Quantize(q) => midi_ops::quantize(&mut sel, clip_start, q, meter),
            NoteOp::Humanize { timing, velocity } => {
                let seed = self.history.revision() ^ (sel.len() as u64).wrapping_mul(0x9e37_79b9);
                midi_ops::humanize(&mut sel, *timing, *velocity, seed);
            }
            NoteOp::Legato => midi_ops::legato(&mut sel),
            NoteOp::Transpose(n) => midi_ops::transpose(&mut sel, *n),
            NoteOp::TransposeInScale(n, s) => midi_ops::transpose_in_scale(&mut sel, *n, s),
            NoteOp::SetLength(l) => midi_ops::set_length(&mut sel, *l),
            NoteOp::ScaleLength(f) => midi_ops::scale_length(&mut sel, *f),
            NoteOp::Reverse => midi_ops::reverse(&mut sel),
            NoteOp::Invert => midi_ops::invert(&mut sel),
            NoteOp::SetVelocity(v) => midi_ops::set_velocity(&mut sel, *v),
            NoteOp::ScaleVelocity { factor, offset } => {
                midi_ops::scale_velocity(&mut sel, *factor, *offset)
            }
            NoteOp::RampVelocity { from, to } => midi_ops::ramp_velocity(&mut sel, *from, *to),
            NoteOp::FoldToScale(s) => midi_ops::fold_to_scale(&mut sel, s),
            NoteOp::RemoveOverlaps => midi_ops::remove_overlaps(&mut sel),
            NoteOp::SetMuted(on) => sel.iter_mut().for_each(|n| n.muted = *on),
            NoteOp::ToggleMuted => {
                let all = sel.iter().all(|n| n.muted);
                sel.iter_mut().for_each(|n| n.muted = !all);
            }
            NoteOp::SetChannel(c) => sel.iter_mut().for_each(|n| n.channel = (*c).min(15)),
            NoteOp::Move { by, keys } => {
                for n in &mut sel {
                    n.start = (n.start + *by).max(MusicalTime::ZERO);
                    n.key = (n.key as i32 + keys).clamp(0, 127) as u8;
                }
            }
            NoteOp::Resize { start, end } => {
                for n in &mut sel {
                    let s = (n.start + *start).max(MusicalTime::ZERO);
                    let e = (n.end() + *end).max(s + MusicalTime(1));
                    n.start = s;
                    n.length = e - s;
                }
            }
        }
        for n in &mut m.notes {
            if let Some(new) = sel.iter().find(|s| s.id == n.id) {
                *n = *new;
            }
        }
        self.set_midi_clip(clip, op.label(), m)
    }

    /// Add notes (ids allocated); they become the selection.
    pub fn add_notes(&mut self, clip: ClipId, notes: &[MidiNote]) -> Result<Vec<NoteId>> {
        self.add_notes_with(clip, notes, &[])
    }

    /// Add notes; `expressions` (keyed by the given notes' ids) are copied
    /// to the new notes.
    fn add_notes_with(
        &mut self,
        clip: ClipId,
        notes: &[MidiNote],
        expressions: &[NoteExpression],
    ) -> Result<Vec<NoteId>> {
        let (_, mut m) = self.midi_clip(clip)?;
        let mut ids = Vec::with_capacity(notes.len());
        for n in notes {
            let id: NoteId = self.project.ids.allocate();
            m.notes.push(MidiNote { id, ..*n });
            if let Some(e) = expressions.iter().find(|e| e.note == n.id) {
                m.expressions.push(NoteExpression {
                    note: id,
                    ..e.clone()
                });
            }
            ids.push(id);
        }
        self.set_midi_clip(
            clip,
            if notes.len() > 1 {
                "Add Notes"
            } else {
                "Add Note"
            },
            m,
        )?;
        self.selection.select_notes(&ids, SelectMode::Replace);
        Ok(ids)
    }

    /// What the MIDI Tools panel's tool would do to `clip`: a transformation
    /// to `notes` (all of the clip's without a selection), a generator in
    /// the bars the selection spans (the whole clip without one).
    pub fn preview_midi_tool(&self, clip: ClipId, notes: &[NoteId]) -> Option<ToolPreview> {
        let tool = self.editor.piano.tool?;
        let (clip_start, m) = self.midi_clip(clip).ok()?;
        let s = self.editor.piano.tools;
        let chosen: Vec<MidiNote> = if notes.is_empty() {
            m.notes.clone()
        } else {
            m.notes
                .iter()
                .filter(|n| notes.contains(&n.id))
                .copied()
                .collect()
        };
        let scale_at = |t: MusicalTime| self.piano_scale_at(t);
        let key_at = |t: MusicalTime| self.project.key_at(t);
        let ctx = ToolContext {
            clip_start,
            meter: &self.project.timeline.meter,
            scale_at: &scale_at,
            key_at: &key_at,
            chords: &self.project.chords,
        };
        if tool.is_generator() {
            let meter = &self.project.timeline.meter;
            let range = if notes.is_empty() || chosen.is_empty() {
                (MusicalTime::ZERO, m.length)
            } else {
                // The bars the selection spans.
                let a = chosen
                    .iter()
                    .map(|n| n.start)
                    .min()
                    .unwrap_or(MusicalTime::ZERO);
                let b = chosen.iter().map(|n| n.end()).max().unwrap_or(m.length);
                let first = meter.bar_start(meter.bar_at(clip_start + a)) - clip_start;
                let mut last = meter.bar_at(clip_start + b);
                if meter.bar_start(last) < clip_start + b {
                    last += 1;
                }
                (
                    first.max(MusicalTime::ZERO),
                    (meter.bar_start(last) - clip_start).min(m.length),
                )
            };
            let made = midi_tools::generate(tool, &s, range.0, range.1, &ctx);
            let removed = if s.replace {
                m.notes
                    .iter()
                    .filter(|n| n.start >= range.0 && n.start < range.1)
                    .map(|n| n.id)
                    .collect()
            } else {
                Vec::new()
            };
            return Some(ToolPreview {
                tool,
                removed,
                notes: made,
                range: Some(range),
            });
        }
        if chosen.is_empty() {
            return None;
        }
        let made = midi_tools::transform(tool, &s, &chosen, &ctx);
        Some(ToolPreview {
            tool,
            removed: chosen.iter().map(|n| n.id).collect(),
            notes: made,
            range: None,
        })
    }

    /// Apply the MIDI Tools panel's tool (one undo step); what it made
    /// becomes the selection.
    pub fn apply_midi_tool(&mut self, clip: ClipId, notes: &[NoteId]) -> Result<()> {
        let Some(p) = self.preview_midi_tool(clip, notes) else {
            return Ok(());
        };
        let (_, mut m) = self.midi_clip(clip)?;
        let kept: HashSet<NoteId> = p.notes.iter().map(|n| n.id).collect();
        m.notes
            .retain(|n| !p.removed.contains(&n.id) || kept.contains(&n.id));
        let mut selected = Vec::new();
        for n in p.notes {
            if n.id.0 != 0
                && let Some(old) = m.notes.iter_mut().find(|x| x.id == n.id)
            {
                *old = n;
                selected.push(n.id);
                continue;
            }
            let id: NoteId = self.project.ids.allocate();
            m.notes.push(MidiNote { id, ..n });
            selected.push(id);
        }
        let present: HashSet<NoteId> = m.notes.iter().map(|n| n.id).collect();
        m.expressions.retain(|e| present.contains(&e.note));
        m.notes.sort_by_key(|n| (n.start, n.key));
        self.set_midi_clip(clip, p.tool.label(), m)?;
        self.selection.select_notes(&selected, SelectMode::Replace);
        Ok(())
    }

    /// Does the piano roll's scale come from the key track (following it,
    /// and the project has keys)?
    pub fn piano_follows_key(&self) -> bool {
        self.editor.piano.follow_key && !self.project.keys.is_empty()
    }

    /// The piano roll's scale at `at` (project time): the key track's key
    /// there when following it, else the chosen scale.
    pub fn piano_scale_at(&self, at: MusicalTime) -> Scale {
        if self.editor.piano.follow_key
            && let Some(key) = self.project.key_at(at)
        {
            return Scale::of_key(key);
        }
        self.editor.piano.scale
    }

    /// The piano roll's scales over `from..to` (project time), as
    /// `(start, end, scale)` spans.
    pub fn piano_scales(
        &self,
        from: MusicalTime,
        to: MusicalTime,
    ) -> Vec<(MusicalTime, MusicalTime, Scale)> {
        let mut spans = vec![(from, to, self.piano_scale_at(from))];
        if !self.piano_follows_key() {
            return spans;
        }
        for k in self
            .project
            .keys
            .iter()
            .filter(|k| k.at > from && k.at < to)
        {
            if let Some(last) = spans.last_mut() {
                last.1 = k.at;
            }
            spans.push((k.at, to, Scale::of_key(k.key)));
        }
        spans
    }

    /// The keys a note drawn on `key` at `at` (project time) becomes: the
    /// editor's chord kind (the chord track's chord, voiced round the key)
    /// on the key, moved into the scale with scale snap.
    pub fn piano_chord_keys(&self, key: u8, at: MusicalTime) -> Vec<u8> {
        let pr = &self.editor.piano;
        let scale = self.piano_scale_at(at);
        let root = if pr.scale_snap && !scale.is_chromatic() {
            scale.nearest(key)
        } else {
            key
        };
        if pr.chord == ChordKind::ChordTrack
            && let Some(c) = self.project.chord_at(at)
        {
            return c
                .chord
                .voicing(i32::from(key))
                .into_iter()
                .filter(|k| (0..=127).contains(k))
                .map(|k| k as u8)
                .collect();
        }
        pr.chord.keys(root, &scale)
    }

    /// A chord of the editor's chord kind and scale on `key`.
    pub fn add_chord(
        &mut self,
        clip: ClipId,
        start: MusicalTime,
        length: MusicalTime,
        key: u8,
        velocity: u8,
    ) -> Result<Vec<NoteId>> {
        let (clip_start, _) = self.midi_clip(clip)?;
        let notes: Vec<MidiNote> = self
            .piano_chord_keys(key, clip_start + start)
            .into_iter()
            .map(|k| MidiNote {
                id: NoteId(0),
                start,
                length,
                key: k,
                velocity,
                channel: 0,
                muted: false,
            })
            .collect();
        self.add_notes(clip, &notes)
    }

    /// Copies of `notes` moved by `offset` / `keys` (offset `None`: right
    /// after the notes' span); the copies become the selection.
    pub fn duplicate_notes(
        &mut self,
        clip: ClipId,
        notes: &[NoteId],
        offset: Option<MusicalTime>,
        keys: i32,
    ) -> Result<Vec<NoteId>> {
        let (clip_start, m) = self.midi_clip(clip)?;
        let sel: Vec<MidiNote> = m
            .notes
            .iter()
            .filter(|n| notes.contains(&n.id))
            .copied()
            .collect();
        let (Some(a), Some(b)) = (
            sel.iter().map(|n| n.start).min(),
            sel.iter().map(|n| n.end()).max(),
        ) else {
            return Ok(Vec::new());
        };
        let by = offset.unwrap_or_else(|| {
            // Whole grid steps, so a duplicated bar lands on the next bar.
            let step = self
                .editor
                .step(clip_start + a, &self.project.timeline.meter)
                .ticks()
                .max(1);
            MusicalTime(((b - a).ticks() + step - 1) / step * step)
        });
        let copies: Vec<MidiNote> = sel
            .iter()
            .map(|n| MidiNote {
                start: n.start + by,
                key: (n.key as i32 + keys).clamp(0, 127) as u8,
                ..*n
            })
            .collect();
        self.add_notes_with(clip, &copies, &m.expressions)
    }

    /// Split the notes crossing `at` (clip-relative) into two.
    pub fn split_notes(&mut self, clip: ClipId, notes: &[NoteId], at: MusicalTime) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        let mut added = Vec::new();
        let mut split_expr = Vec::new();
        for n in m.notes.iter_mut() {
            if (notes.is_empty() || notes.contains(&n.id)) && n.start < at && n.end() > at {
                let right = MidiNote {
                    id: self.project.ids.allocate(),
                    start: at,
                    length: n.end() - at,
                    ..*n
                };
                split_expr.push((n.id, at - n.start, right.id));
                n.length = at - n.start;
                added.push(right);
            }
        }
        if added.is_empty() {
            return Ok(());
        }
        m.notes.extend(added);
        // Expression is cut with its note.
        for (left, cut, right) in split_expr {
            if let Some(e) = m.expressions.iter_mut().find(|e| e.note == left) {
                let r = e.split_off(cut, right);
                m.expressions.push(r);
            }
        }
        self.set_midi_clip(clip, "Split Notes", m)
    }

    /// Remove notes.
    pub fn remove_notes(&mut self, clip: ClipId, notes: &[NoteId]) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        let before = m.notes.len();
        m.notes.retain(|n| !notes.contains(&n.id));
        if m.notes.len() == before {
            return Ok(());
        }
        self.set_midi_clip(clip, "Delete Notes", m)?;
        self.selection.select_notes(&[], SelectMode::Replace);
        Ok(())
    }

    /// Notes on the clipboard (relative to the earliest).
    pub fn note_clipboard(&self) -> &[MidiNote] {
        &self.note_clipboard
    }

    pub fn copy_notes(&mut self, clip: ClipId, notes: &[NoteId]) -> Result<()> {
        let (_, m) = self.midi_clip(clip)?;
        let mut sel: Vec<MidiNote> = m
            .notes
            .iter()
            .filter(|n| notes.contains(&n.id))
            .copied()
            .collect();
        let Some(a) = sel.iter().map(|n| n.start).min() else {
            return Ok(());
        };
        for n in &mut sel {
            n.start -= a;
        }
        self.note_clipboard_expressions = m
            .expressions
            .iter()
            .filter(|e| sel.iter().any(|n| n.id == e.note))
            .cloned()
            .collect();
        self.note_clipboard = sel;
        Ok(())
    }

    pub fn cut_notes(&mut self, clip: ClipId, notes: &[NoteId]) -> Result<()> {
        self.copy_notes(clip, notes)?;
        self.remove_notes(clip, notes)
    }

    /// Paste the clipboard at `at` (clip-relative).
    pub fn paste_notes(&mut self, clip: ClipId, at: MusicalTime) -> Result<Vec<NoteId>> {
        let notes: Vec<MidiNote> = self
            .note_clipboard
            .iter()
            .map(|n| MidiNote {
                start: n.start + at,
                ..*n
            })
            .collect();
        if notes.is_empty() {
            return Ok(Vec::new());
        }
        let expressions = self.note_clipboard_expressions.clone();
        self.add_notes_with(clip, &notes, &expressions)
    }

    /// Replace `kind` of a note's expression in `from..to` (relative to the
    /// note's start).
    pub fn set_note_expression(
        &mut self,
        clip: ClipId,
        note: NoteId,
        kind: ExpressionKind,
        from: MusicalTime,
        to: MusicalTime,
        points: &[ExpressionPoint],
    ) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        if m.note(note).is_none() {
            return Err(SessionError::Other(format!("no note {note}")));
        }
        m.expression_mut(note).replace_range(kind, from, to, points);
        self.set_midi_clip(clip, "Edit Expression", m)
    }

    /// Replace a controller lane's points in `from..to` (lane created on
    /// first use, removed when it ends up empty).
    pub fn set_controller_points(
        &mut self,
        clip: ClipId,
        controller: MidiController,
        channel: u8,
        from: MusicalTime,
        to: MusicalTime,
        points: &[ControllerPoint],
    ) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        m.lane_mut(controller, channel)
            .replace_range(from, to, points);
        m.controllers.retain(|l| !l.points.is_empty());
        self.set_midi_clip(clip, "Edit Controller", m)
    }

    /// Change a MIDI clip's length (notes past the end stay, silent).
    pub fn set_midi_clip_length(&mut self, clip: ClipId, length: MusicalTime) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        m.length = length.max(MusicalTime(1));
        self.set_midi_clip(clip, "Change Clip Length", m)
    }
}
