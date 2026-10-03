//! Note editing for the piano roll: operations on selected notes, copy and
//! paste, duplication, splitting, chords, controller lanes and the editor's
//! musical settings. Every action is one undoable command (a labelled batch
//! around `SetClipContent` or note commands); ids are allocated here, never
//! by the view.

use crate::{Result, SelectMode, Session, SessionError};
use faderframe_core::{ClipId, NoteId};
use faderframe_project::midi_ops::{self, ChordKind, QuantizeSettings, Scale};
use faderframe_project::{
    ClipContent, Command, ControllerPoint, MidiClip, MidiController, MidiNote,
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
    pub scale: Scale,
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
    pub quantize: QuantizeSettings,
    /// The lane under the notes: velocity (`None`) or a controller on a
    /// channel.
    pub lane: Option<(MidiController, u8)>,
}

impl Default for PianoRollSettings {
    fn default() -> Self {
        Self {
            scale: Scale::default(),
            scale_snap: false,
            chord: ChordKind::Single,
            note_length: NoteLength::Grid,
            velocity: 100,
            ghost_notes: true,
            audition: true,
            fold: KeyFold::Off,
            quantize: QuantizeSettings::default(),
            lane: None,
        }
    }
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
        let (_, mut m) = self.midi_clip(clip)?;
        let mut ids = Vec::with_capacity(notes.len());
        for n in notes {
            let id: NoteId = self.project.ids.allocate();
            m.notes.push(MidiNote { id, ..*n });
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

    /// A chord of the editor's chord kind and scale on `key`.
    pub fn add_chord(
        &mut self,
        clip: ClipId,
        start: MusicalTime,
        length: MusicalTime,
        key: u8,
        velocity: u8,
    ) -> Result<Vec<NoteId>> {
        let pr = self.editor.piano;
        let root = if pr.scale_snap {
            pr.scale.nearest(key)
        } else {
            key
        };
        let notes: Vec<MidiNote> = pr
            .chord
            .keys(root, &pr.scale)
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
        self.add_notes(clip, &copies)
    }

    /// Split the notes crossing `at` (clip-relative) into two.
    pub fn split_notes(&mut self, clip: ClipId, notes: &[NoteId], at: MusicalTime) -> Result<()> {
        let (_, mut m) = self.midi_clip(clip)?;
        let mut added = Vec::new();
        for n in m.notes.iter_mut() {
            if (notes.is_empty() || notes.contains(&n.id)) && n.start < at && n.end() > at {
                let right = MidiNote {
                    id: self.project.ids.allocate(),
                    start: at,
                    length: n.end() - at,
                    ..*n
                };
                n.length = at - n.start;
                added.push(right);
            }
        }
        if added.is_empty() {
            return Ok(());
        }
        m.notes.extend(added);
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
        self.add_notes(clip, &notes)
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
