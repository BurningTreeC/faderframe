//! Scratch pads (see [`faderframe_project::scratch`]): other arrangements
//! of the project's own tracks to try ideas in, and the way material goes
//! into them and back into the song.
//!
//! * The arranger shows the song or one pad ([`ScratchOp::Show`]); the
//!   editors, the clipboard, playback and recording work on what is shown.
//!   Showing is an undo step of its own, so every edit is undone in the
//!   arrangement it was made in. Each arrangement keeps its own playhead
//!   (a pad seen for the first time starts at bar 1).
//! * Material: a section with everything in it (tempo, metre, markers, the
//!   section itself, key, chords, lyrics, automation), the edit selection
//!   (its range on the selected tracks, else the selected clips — whole
//!   clips without automation, as copies of clips are), given clips, or
//!   all of an arrangement. Clips keep their place in the bar, tracks
//!   their clips: a pad works on the same tracks, so nothing is mapped.
//! * Into a pad: a new pad holding a copy (shown), or appended after what
//!   a pad holds, from the next bar line ([`ScratchOp::CopyTo`]).
//! * Back to the song ([`ScratchOp::ToSong`], one step, the song shown
//!   after): Insert (time opens, what follows moves later), Overwrite
//!   (laid over the song on its tracks, time does not move) or Replace a
//!   section — when the lengths differ that is ambiguous and the caller
//!   says which: the section takes the material's length (what follows
//!   moves by the difference) or keeps its own (the material is cut or
//!   padded to it, what follows stays).
//! * Renders, freezes and exports take the song whatever is shown
//!   (`Session::render_copy`); a frozen track is silent in a pad.

use crate::{NoticeLevel, Result, SelectMode, Session, SessionError, TransportAction};
use faderframe_core::{ClipId, ScratchPadId, SectionId, TrackId};
use faderframe_project::arrange::{self, Arrangement, TimeSlice};
use faderframe_project::scratch::ScratchPad;
use faderframe_project::{Command, Project, Section};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeSet;

/// What goes into a pad or back into the song.
#[derive(Clone, Debug, PartialEq)]
pub enum PadMaterial {
    /// The edit selection: its range on the selected tracks when it has a
    /// length, else the selected clips (of the arrangement shown).
    Selection,
    /// These clips, whole.
    Clips(Vec<ClipId>),
    /// A section with everything in it.
    Section(SectionId),
    /// Everything the arrangement holds.
    All,
}

/// Where material from a pad goes in the song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongPlacement {
    /// Open time at `at` and put it there: what follows moves later.
    Insert { at: MusicalTime },
    /// Lay it over the song from `at`, replacing what lies under it on its
    /// tracks; time does not move.
    Overwrite { at: MusicalTime },
    /// Replace a section of the song: with `keep_length` the section keeps
    /// its length (the material is cut or padded to it, what follows
    /// stays), else it takes the material's (what follows moves by the
    /// difference).
    Replace {
        section: SectionId,
        keep_length: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScratchOp {
    /// A new pad (named so, else "Scratch Pad n") holding a copy of
    /// `from` (`None`: nothing), shown.
    New {
        name: Option<String>,
        from: Option<PadMaterial>,
    },
    /// Copy material of the arrangement shown after what a pad holds (the
    /// arrangement shown stays).
    CopyTo {
        pad: ScratchPadId,
        what: PadMaterial,
    },
    /// Show the song (`None`) or a pad.
    Show(Option<ScratchPadId>),
    Rename {
        pad: ScratchPadId,
        name: String,
    },
    /// Ask for a pad's new name (a dialog of the shell).
    PromptRename(ScratchPadId),
    /// A copy of a pad after it (not shown).
    Duplicate(ScratchPadId),
    /// Remove a pad (the song is shown if it was).
    Delete(ScratchPadId),
    /// Bring a pad's material into the song (`what` is the pad's; the
    /// selection only while it is shown). One step; the song is shown
    /// after, at the material.
    ToSong {
        pad: ScratchPadId,
        what: PadMaterial,
        place: SongPlacement,
    },
}

/// A song section, as the scratch pad menus offer it.
#[derive(Clone, Debug, PartialEq)]
pub struct SongSection {
    pub id: SectionId,
    pub name: String,
    pub start: MusicalTime,
    pub end: MusicalTime,
}

impl Session {
    /// The scratch pads, in order.
    pub fn scratch_pads(&self) -> &[ScratchPad] {
        &self.project.scratch_pads.pads
    }

    /// The pad shown (`None`: the song).
    pub fn shown_pad(&self) -> Option<ScratchPadId> {
        self.project.shown_pad()
    }

    /// The name of the arrangement shown ("Song" for the song).
    pub fn shown_arrangement_name(&self) -> String {
        let pads = &self.project.scratch_pads;
        pads.name_of(pads.shown)
    }

    /// The song's sections (put aside while a pad is shown).
    pub fn song_sections(&self) -> Vec<SongSection> {
        let p = &self.project;
        let sections = match &p.scratch_pads.song {
            Some(song) => &song.sections,
            None => &p.sections,
        };
        sections
            .iter()
            .map(|s| SongSection {
                id: s.id,
                name: s.name.clone(),
                start: s.start,
                end: s.end,
            })
            .collect()
    }

    /// The song's playhead: where it is, or where it was left while a pad
    /// is shown.
    pub fn song_position(&self) -> MusicalTime {
        if self.project.shown_pad().is_none() {
            self.playhead()
        } else {
            self.pad_positions
                .get(&None)
                .copied()
                .unwrap_or(MusicalTime::ZERO)
        }
    }

    /// The bar line at or before `at` in the song.
    pub fn song_bar_line(&self, at: MusicalTime) -> MusicalTime {
        let meter = &self.song_timeline().meter;
        meter.bar_start(meter.bar_at(at.max(MusicalTime::ZERO)))
    }

    /// How long `what` of `pad` is (whole bars round clips; `None`:
    /// nothing there).
    pub fn pad_material_length(
        &self,
        pad: ScratchPadId,
        what: &PadMaterial,
    ) -> Option<MusicalTime> {
        let mut q = self.project.clone();
        q.show_arrangement(Some(pad)).ok()?;
        let selection = self.project.shown_pad() == Some(pad);
        self.material(&q, what, selection, None)
            .ok()
            .map(|(slice, _)| slice.length)
    }

    pub(crate) fn scratch_op(&mut self, op: ScratchOp) -> Result<()> {
        match op {
            ScratchOp::New { name, from } => self.new_pad(name, from).map(|_| ()),
            ScratchOp::CopyTo { pad, what } => self.copy_to_pad(pad, what),
            ScratchOp::Show(pad) => self.show_pad(pad),
            ScratchOp::Rename { pad, name } => self.rename_pad(pad, &name),
            ScratchOp::PromptRename(pad) => {
                self.pad_named(pad)?;
                self.ui_requests
                    .push(crate::UiRequest::RenameScratchPad(pad));
                Ok(())
            }
            ScratchOp::Duplicate(pad) => self.duplicate_pad(pad).map(|_| ()),
            ScratchOp::Delete(pad) => self.delete_pad(pad),
            ScratchOp::ToSong { pad, what, place } => self.pad_to_song(pad, what, place),
        }
    }

    /// Switching arrangements waits for recordings and gestures to end.
    fn may_switch(&self) -> Result<()> {
        if self.recording_in_progress() {
            return Err(SessionError::Other(
                "stop recording before showing another arrangement".into(),
            ));
        }
        if self.history.in_gesture() {
            return Err(SessionError::Other(
                "finish the edit before showing another arrangement".into(),
            ));
        }
        // The launcher writes into the arrangement shown as it plays.
        if self.launcher_records() && self.transport.playing {
            return Err(SessionError::Other(
                "stop the launcher's Record to Arrangement before showing another arrangement"
                    .into(),
            ));
        }
        Ok(())
    }

    fn pad_named(&self, pad: ScratchPadId) -> Result<String> {
        self.project
            .scratch_pads
            .pad(pad)
            .map(|p| p.name.clone())
            .ok_or_else(|| SessionError::Other(format!("no scratch pad {pad}")))
    }

    /// `what` of the arrangement `q` shows, as a slice, and where it
    /// starts there. `selection`: the session's selection is of `q`'s
    /// arrangement. `length`: make it this long (from where it starts;
    /// what lies past it is cut off).
    fn material(
        &self,
        q: &Project,
        what: &PadMaterial,
        selection: bool,
        length: Option<MusicalTime>,
    ) -> Result<(TimeSlice, MusicalTime)> {
        let meter = &q.timeline.meter;
        let bars_between = |a: MusicalTime, b: MusicalTime| meter.bar_at(b) - meter.bar_at(a);
        let nothing = || SessionError::Other("there is nothing to copy there".into());
        let clips_of = |ids: &[ClipId]| -> Result<(TimeSlice, MusicalTime)> {
            let launcher = q.launcher_clips();
            let found: Vec<&faderframe_project::Clip> = ids
                .iter()
                .filter(|c| !launcher.contains(c))
                .filter_map(|c| q.clip(*c))
                .collect();
            let start = found.iter().map(|c| c.start).min().ok_or_else(nothing)?;
            let end = found
                .iter()
                .map(|c| c.end(&q.timeline, q.sample_rate))
                .max()
                .ok_or_else(nothing)?;
            let (a, b) = arrange::bar_lines(meter, start, end);
            let b = length.map_or(b, |l| a + l);
            let mut slice = arrange::copy_span(q, a, b);
            let keep: BTreeSet<ClipId> = found.iter().map(|c| c.id).collect();
            slice.retain(None, Some(&keep));
            slice.without_globals();
            Ok((slice, a))
        };
        let (slice, start) = match what {
            PadMaterial::All => {
                let (a, b) = q.arrangement_span().ok_or_else(nothing)?;
                (arrange::copy_span(q, a, length.map_or(b, |l| a + l)), a)
            }
            PadMaterial::Section(id) => {
                let s = q
                    .sections
                    .iter()
                    .find(|s| s.id == *id)
                    .ok_or_else(|| SessionError::Other("no such section there".into()))?;
                let b = length.map_or(s.end, |l| s.start + l);
                (arrange::copy_span(q, s.start, b), s.start)
            }
            PadMaterial::Clips(ids) => clips_of(ids)?,
            PadMaterial::Selection => {
                if !selection {
                    return Err(SessionError::Other(
                        "show the scratch pad to bring its selection".into(),
                    ));
                }
                match self.selection.range.filter(|r| !r.is_empty()) {
                    Some(r) => {
                        let tracks: BTreeSet<TrackId> = self.edit_tracks().into_iter().collect();
                        if tracks.is_empty() {
                            return Err(nothing());
                        }
                        let (a, b) = arrange::bar_lines(meter, r.start, r.end);
                        let b = length.map_or(b, |l| a + l);
                        let to = r.end.min(b);
                        if to <= r.start {
                            return Err(nothing());
                        }
                        let mut slice = arrange::copy_span(q, r.start, to);
                        slice.retain(Some(&tracks), None);
                        slice.delay(r.start - a, b - a, Some(bars_between(a, b)));
                        (slice, a)
                    }
                    None => {
                        let clips = self.selected_arranger_clips();
                        if clips.is_empty() {
                            return Err(SessionError::Other(
                                "select clips or a range to copy".into(),
                            ));
                        }
                        clips_of(&clips)?
                    }
                }
            }
        };
        if slice.is_empty() {
            return Err(nothing());
        }
        Ok((slice, start))
    }

    /// The song's timeline (put aside while a pad is shown); a new pad
    /// starts with its tempo and metre at its start.
    pub fn song_timeline(&self) -> &faderframe_timeline::Timeline {
        match &self.project.scratch_pads.song {
            Some(song) => &song.timeline,
            None => &self.project.timeline,
        }
    }

    /// Apply `commands` as one undo step; when they show another
    /// arrangement, the one left keeps its playhead, the one shown gets its
    /// own back (bar 1 for a pad never shown), and the selected clips,
    /// notes and range go (they are not there).
    fn switched(&mut self, label: &str, commands: Vec<Command>) -> Result<()> {
        let from = self.project.shown_pad();
        let here = self.playhead();
        self.automation_play_stopped();
        self.batch(label, commands)?;
        let to = self.project.shown_pad();
        if to != from {
            self.pad_positions.insert(from, here);
            // Clips of the other arrangement are not here.
            self.selection.clips.clear();
            self.selection.notes.clear();
            self.selection.range = None;
            let at = self
                .pad_positions
                .get(&to)
                .copied()
                .unwrap_or(MusicalTime::ZERO);
            self.transport_action(TransportAction::Locate(at))?;
            self.revision += 1;
        }
        Ok(())
    }

    fn new_pad(&mut self, name: Option<String>, from: Option<PadMaterial>) -> Result<ScratchPadId> {
        self.may_switch()?;
        let id: ScratchPadId = self.project.ids.allocate();
        let base = match (&name, &from) {
            (Some(n), _) if !n.trim().is_empty() => n.trim().to_string(),
            (_, Some(PadMaterial::Section(s))) => self
                .project
                .sections
                .iter()
                .find(|x| x.id == *s)
                .map(|x| format!("{} Idea", x.name))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let name = self.project.scratch_pads.unused_name(&base);
        let pad = ScratchPad::empty(id, name.clone(), self.song_timeline());
        let index = self.project.scratch_pads.pads.len();
        let add = Command::SetScratchPad {
            pad: id,
            value: Some(Box::new(pad)),
            index,
        };
        let show = Command::ShowArrangement { pad: Some(id) };
        let mut commands = vec![add.clone(), show.clone()];
        if let Some(what) = from {
            let mut q = self.project.clone();
            let (mut slice, _) = self.material(&q, &what, true, None)?;
            add.apply(&mut q)?;
            show.apply(&mut q)?;
            slice.adopt_lanes(&mut q);
            arrange::insert_span(&mut q, MusicalTime::ZERO, &slice, false);
            commands.push(Command::SetArrangement {
                arrangement: Box::new(Arrangement::of(&q)),
            });
        }
        self.switched(&format!("New Scratch Pad ‘{name}’"), commands)?;
        Ok(id)
    }

    fn copy_to_pad(&mut self, pad: ScratchPadId, what: PadMaterial) -> Result<()> {
        let name = self.pad_named(pad)?;
        let from = self.project.shown_pad();
        if from == Some(pad) {
            return Err(SessionError::Other(format!(
                "‘{name}’ is shown: copy and paste within it"
            )));
        }
        self.may_switch()?;
        let mut q = self.project.clone();
        let (mut slice, _) = self.material(&q, &what, true, None)?;
        q.show_arrangement(Some(pad))?;
        let at = q.arrangement_span().map_or(MusicalTime::ZERO, |(_, b)| b);
        slice.adopt_lanes(&mut q);
        arrange::insert_span(&mut q, at, &slice, false);
        self.batch(
            &format!("Copy to Scratch Pad ‘{name}’"),
            vec![
                Command::ShowArrangement { pad: Some(pad) },
                Command::SetArrangement {
                    arrangement: Box::new(Arrangement::of(&q)),
                },
                Command::ShowArrangement { pad: from },
            ],
        )?;
        self.notify(NoticeLevel::Info, format!("Copied to ‘{name}’"));
        Ok(())
    }

    fn show_pad(&mut self, pad: Option<ScratchPadId>) -> Result<()> {
        if pad == self.project.shown_pad() {
            return Ok(());
        }
        let name = match pad {
            Some(id) => self.pad_named(id)?,
            None => "Song".into(),
        };
        self.may_switch()?;
        self.switched(
            &format!("Show ‘{name}’"),
            vec![Command::ShowArrangement { pad }],
        )
    }

    fn rename_pad(&mut self, pad: ScratchPadId, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SessionError::Other("a scratch pad needs a name".into()));
        }
        let pads = &self.project.scratch_pads;
        let index = pads
            .index_of(pad)
            .ok_or_else(|| SessionError::Other(format!("no scratch pad {pad}")))?;
        let mut value = pads.pads[index].clone();
        if value.name == name {
            return Ok(());
        }
        value.name = name.to_string();
        self.batch(
            "Rename Scratch Pad",
            vec![Command::SetScratchPad {
                pad,
                value: Some(Box::new(value)),
                index,
            }],
        )
    }

    fn duplicate_pad(&mut self, pad: ScratchPadId) -> Result<ScratchPadId> {
        let name = self.pad_named(pad)?;
        let stash = if self.project.shown_pad() == Some(pad) {
            let song = self.project.song();
            song.scratch_pads
                .pad(pad)
                .and_then(|p| p.stash.as_deref().cloned())
                .unwrap_or_default()
        } else {
            self.project
                .scratch_pads
                .pad(pad)
                .and_then(|p| p.stash.as_deref().cloned())
                .unwrap_or_default()
        };
        let stash = stash.with_new_ids(&mut self.project.ids);
        let id: ScratchPadId = self.project.ids.allocate();
        let copy = ScratchPad {
            id,
            name: self
                .project
                .scratch_pads
                .unused_name(&format!("{name} Copy")),
            stash: Some(Box::new(stash)),
        };
        let index = self.project.scratch_pads.index_of(pad).map_or(0, |i| i + 1);
        self.batch(
            "Duplicate Scratch Pad",
            vec![Command::SetScratchPad {
                pad: id,
                value: Some(Box::new(copy)),
                index,
            }],
        )?;
        Ok(id)
    }

    fn delete_pad(&mut self, pad: ScratchPadId) -> Result<()> {
        let name = self.pad_named(pad)?;
        let index = self.project.scratch_pads.index_of(pad).unwrap_or(0);
        let mut commands = Vec::new();
        if self.project.shown_pad() == Some(pad) {
            self.may_switch()?;
            commands.push(Command::ShowArrangement { pad: None });
        }
        commands.push(Command::SetScratchPad {
            pad,
            value: None,
            index,
        });
        self.switched(&format!("Delete Scratch Pad ‘{name}’"), commands)?;
        self.pad_positions.remove(&Some(pad));
        Ok(())
    }

    fn pad_to_song(
        &mut self,
        pad: ScratchPadId,
        what: PadMaterial,
        place: SongPlacement,
    ) -> Result<()> {
        let name = self.pad_named(pad)?;
        let from = self.project.shown_pad();
        self.may_switch()?;
        let mut q = self.project.clone();
        q.show_arrangement(Some(pad))?;
        let selection = from == Some(pad);
        let mut song = q.clone().into_song();
        // A section keeping its length takes that much of the material.
        let length = match place {
            SongPlacement::Replace {
                section,
                keep_length: true,
            } => {
                let s = song_section(&song, section)?;
                Some(s.end - s.start)
            }
            _ => None,
        };
        let (mut slice, _) = self.material(&q, &what, selection, length)?;
        slice.adopt_lanes(&mut song);
        let before: BTreeSet<ClipId> = song.clips.keys().copied().collect();
        let tracks = slice.clip_tracks();
        let at = match place {
            SongPlacement::Insert { at } => {
                let at = at.max(MusicalTime::ZERO);
                arrange::insert_span(&mut song, at, &slice, false);
                at
            }
            SongPlacement::Overwrite { at } => {
                let at = at.max(MusicalTime::ZERO);
                arrange::overlay_span(&mut song, at, &slice);
                at
            }
            SongPlacement::Replace { section, .. } => {
                let s = song_section(&song, section)?;
                arrange::replace_span(&mut song, s.start, s.end, &slice, Some(s.clone()));
                s.start
            }
        };
        let end = at + slice.length;
        let made: Vec<ClipId> = song
            .clips
            .values()
            .filter(|c| !before.contains(&c.id) && tracks.contains(&c.track))
            .filter(|c| c.start >= at && c.start < end)
            .map(|c| c.id)
            .collect();
        let mut commands = Vec::new();
        if from.is_some() {
            commands.push(Command::ShowArrangement { pad: None });
        }
        commands.push(Command::SetArrangement {
            arrangement: Box::new(Arrangement::of(&song)),
        });
        // The song is shown at the material.
        self.pad_positions.insert(None, at);
        self.switched(&format!("Bring ‘{name}’ to Song"), commands)?;
        if from.is_none() {
            self.transport_action(TransportAction::Locate(at))?;
        }
        self.selection.select_clips(&made, SelectMode::Replace);
        self.revision += 1;
        Ok(())
    }
}

fn song_section(song: &Project, id: SectionId) -> Result<Section> {
    song.sections
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| SessionError::Other("the song has no such section".into()))
}
