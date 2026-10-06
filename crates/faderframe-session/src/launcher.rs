//! The clip launcher: scenes (rows) of clips per track (columns) that are
//! launched while the song plays, each waiting for the next quantised
//! position (see `faderframe_engine::launch`). A launched clip loops until
//! another clip of its track, a stop or "back to the arrangement"; a scene
//! launches its row (tracks without a clip in it stop). Launching while
//! stopped starts playback.
//!
//! The clips live in `Project::clips` keyed by slot in
//! `Project::launcher` (`Command::SetLauncherSlot`), in no track's clip
//! list. With "Record to Arrangement" on, what the launcher plays is
//! written into the arrangement when the transport stops (one undo step,
//! "Record Launches"): each run of a clip as copies over its span (the
//! last one cut where it stopped), clearing what the track had there.

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, SceneId, TrackId};
use faderframe_engine::launch::{LaunchCommand, TrackStatus};
use faderframe_project::launcher::{LaunchQuantize, Scene, SlotKey};
use faderframe_project::{Clip, ClipContent, Command, MidiClip, Project, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;

/// A launcher edit or action.
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherOp {
    /// Launch a slot's clip (an empty slot stops its track).
    Launch {
        track: TrackId,
        scene: SceneId,
    },
    /// Launch a scene's row.
    LaunchScene(SceneId),
    StopTrack(TrackId),
    StopAll,
    /// Every track plays the arrangement again.
    BackToArrangement,
    /// A new scene after `after` (at the end: `None`).
    AddScene {
        after: Option<SceneId>,
    },
    /// Remove a scene and its clips.
    RemoveScene(SceneId),
    RenameScene {
        scene: SceneId,
        name: String,
    },
    /// A copy of a scene (with copies of its clips) after it.
    DuplicateScene(SceneId),
    /// An empty MIDI clip (a bar) in a slot of a MIDI or instrument track.
    CreateClip {
        track: TrackId,
        scene: SceneId,
    },
    /// Copies of arrangement clips into the launcher: each in its track's
    /// first free slot (new scenes as needed).
    SendClips(Vec<ClipId>),
    /// Move (or copy) a slot's clip to another slot (of a track that can
    /// hold it); a clip there goes.
    MoveClip {
        from: SlotKey,
        to: SlotKey,
        copy: bool,
    },
    ClearSlot {
        track: TrackId,
        scene: SceneId,
    },
    SetQuantize(LaunchQuantize),
    /// Write what the launcher plays into the arrangement (see the module
    /// docs).
    SetRecord(bool),
    /// Record into an empty slot of an armed track from the next launch
    /// position; again (or launching or stopping its track) ends it at the
    /// next one, and the new clip plays on in time.
    Record {
        track: TrackId,
        scene: SceneId,
    },
    /// A slot's follow action (`None`: none).
    SetFollow {
        track: TrackId,
        scene: SceneId,
        follow: Option<faderframe_project::launcher::FollowAction>,
    },
}

/// A recording into a launcher slot (engine samples).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SlotRecording {
    pub track: TrackId,
    pub scene: SceneId,
    pub from: i64,
    /// Where it ends, once asked to.
    pub end: Option<i64>,
}

/// One stretch a launched clip played: its clip, from/to (engine samples).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Run {
    track: TrackId,
    clip: ClipId,
    start: i64,
    end: i64,
}

/// The launcher's session state (not saved).
#[derive(Debug, Default)]
pub struct LauncherState {
    record: bool,
    /// The engine's state at the last tick.
    status: Vec<TrackStatus>,
    /// Runs that ended, waiting for the transport to stop.
    runs: Vec<Run>,
    /// Clips playing (for recording): slot's clip and start, by track.
    open: HashMap<TrackId, (ClipId, i64)>,
    /// The playhead at the last tick while playing.
    last_pos: i64,
}

/// Can a track hold launcher clips?
pub fn holds_clips(kind: TrackKind) -> bool {
    matches!(
        kind,
        TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
    )
}

impl Session {
    /// The tracks the launcher shows (in the editors' order).
    pub fn launcher_tracks(&self) -> Vec<&faderframe_project::Track> {
        self.project
            .folder_order()
            .into_iter()
            .filter(|t| holds_clips(t.kind))
            .collect()
    }

    /// The launcher's state per track as the engine last published it.
    pub fn launch_status(&self) -> &[TrackStatus] {
        &self.launcher.status
    }

    /// What `track` plays: its slot's clip playing, or queued (and
    /// whether that stops it).
    pub fn launch_state(&self, track: TrackId) -> Option<&TrackStatus> {
        self.launcher.status.iter().find(|s| s.track == track)
    }

    /// Does "Record to Arrangement" run?
    pub fn launcher_records(&self) -> bool {
        self.launcher.record
    }

    /// Where the clip playing on `track` is: 0…1 of its length.
    pub fn launch_progress(&self, track: TrackId) -> Option<f32> {
        let (slot, start) = self.launch_state(track)?.playing?;
        let key = self
            .project
            .launcher
            .slots
            .keys()
            .find(|k| k.hash() == slot)?;
        let clip = self.project.clips.get(&self.project.launcher.slots[key])?;
        let length = self.launch_length(clip).max(1);
        let pos = self.transport.position;
        Some(((pos - start).rem_euclid(length)) as f32 / length as f32)
    }

    /// A launcher clip's loop length in engine samples (as the engine
    /// plays it: from the song's start).
    fn launch_length(&self, clip: &Clip) -> i64 {
        let at_zero = Clip {
            start: MusicalTime::ZERO,
            ..clip.clone()
        };
        let end = at_zero.end(&self.project.timeline, self.project.sample_rate);
        self.engine.musical_to_samples(&self.project, end).max(1)
    }

    pub(crate) fn launcher_op(&mut self, op: LauncherOp) -> Result<()> {
        let quantize = self.project.launcher.quantize.into();
        // Launching or stopping a recording track ends its recording.
        match &op {
            LauncherOp::Launch { track, .. } | LauncherOp::StopTrack(track) => {
                self.end_slot_recording(Some(*track));
            }
            LauncherOp::LaunchScene(_) | LauncherOp::StopAll => self.end_slot_recording(None),
            _ => {}
        }
        match op {
            LauncherOp::Record { track, scene } => self.record_slot(track, scene)?,
            LauncherOp::Launch { track, scene } => match self.project.launcher.clip(track, scene) {
                Some(_) => {
                    let slot = SlotKey { track, scene }.hash();
                    self.engine.launch(LaunchCommand::Launch {
                        track,
                        slot,
                        quantize,
                    })?;
                    self.start_for_launch()?;
                }
                None => self
                    .engine
                    .launch(LaunchCommand::Stop { track, quantize })?,
            },
            LauncherOp::LaunchScene(scene) => {
                let tracks: Vec<TrackId> = self.launcher_tracks().iter().map(|t| t.id).collect();
                for track in tracks {
                    let cmd = match self.project.launcher.clip(track, scene) {
                        Some(_) => LaunchCommand::Launch {
                            track,
                            slot: SlotKey { track, scene }.hash(),
                            quantize,
                        },
                        None => LaunchCommand::Stop { track, quantize },
                    };
                    self.engine.launch(cmd)?;
                }
                self.start_for_launch()?;
            }
            LauncherOp::StopTrack(track) => {
                self.engine
                    .launch(LaunchCommand::Stop { track, quantize })?;
            }
            LauncherOp::StopAll => self.engine.launch(LaunchCommand::StopAll { quantize })?,
            LauncherOp::BackToArrangement => {
                self.engine.launch(LaunchCommand::BackToArrangement)?;
            }
            LauncherOp::AddScene { after } => {
                let mut scenes = self.project.launcher.scenes.clone();
                let at = after
                    .and_then(|a| scenes.iter().position(|s| s.id == a))
                    .map_or(scenes.len(), |i| i + 1);
                let id: SceneId = self.project.ids.allocate();
                scenes.insert(
                    at,
                    Scene {
                        id,
                        name: format!("Scene {}", scenes.len() + 1),
                    },
                );
                self.edit(Command::SetScenes { scenes })?;
            }
            LauncherOp::RemoveScene(scene) => {
                let mut commands: Vec<Command> = self
                    .project
                    .launcher
                    .slots
                    .keys()
                    .filter(|k| k.scene == scene)
                    .map(|k| Command::SetLauncherSlot {
                        track: k.track,
                        scene,
                        clip: None,
                    })
                    .collect();
                commands.extend(
                    self.project
                        .launcher
                        .follow
                        .keys()
                        .filter(|k| k.scene == scene)
                        .map(|k| Command::SetFollowAction {
                            track: k.track,
                            scene,
                            follow: None,
                        }),
                );
                let mut scenes = self.project.launcher.scenes.clone();
                scenes.retain(|s| s.id != scene);
                commands.push(Command::SetScenes { scenes });
                self.edit(Command::Batch {
                    label: "Remove Scene".into(),
                    commands,
                })?;
            }
            LauncherOp::RenameScene { scene, name } => {
                let mut scenes = self.project.launcher.scenes.clone();
                let Some(s) = scenes.iter_mut().find(|s| s.id == scene) else {
                    return Err(SessionError::Other("no such scene".into()));
                };
                s.name = name;
                self.edit(Command::SetScenes { scenes })?;
            }
            LauncherOp::DuplicateScene(scene) => {
                let mut scenes = self.project.launcher.scenes.clone();
                let Some(i) = scenes.iter().position(|s| s.id == scene) else {
                    return Err(SessionError::Other("no such scene".into()));
                };
                let id: SceneId = self.project.ids.allocate();
                scenes.insert(
                    i + 1,
                    Scene {
                        id,
                        name: format!("{} (copy)", scenes[i].name),
                    },
                );
                let mut commands = vec![Command::SetScenes { scenes }];
                let slots: Vec<(SlotKey, ClipId)> = self
                    .project
                    .launcher
                    .slots
                    .iter()
                    .filter(|(k, _)| k.scene == scene)
                    .map(|(k, c)| (*k, *c))
                    .collect();
                for (k, c) in slots {
                    let Some(clip) = self.project.clips.get(&c) else {
                        continue;
                    };
                    let mut copy = clip.clone();
                    copy.id = self.project.ids.allocate();
                    commands.push(Command::SetLauncherSlot {
                        track: k.track,
                        scene: id,
                        clip: Some(Box::new(copy)),
                    });
                    if let Some(f) = self.project.launcher.follow.get(&k) {
                        commands.push(Command::SetFollowAction {
                            track: k.track,
                            scene: id,
                            follow: Some(*f),
                        });
                    }
                }
                self.edit(Command::Batch {
                    label: "Duplicate Scene".into(),
                    commands,
                })?;
            }
            LauncherOp::CreateClip { track, scene } => {
                let t = self
                    .project
                    .track(track)
                    .ok_or(SessionError::Other("no such track".into()))?;
                if !matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) {
                    return Err(SessionError::Other(
                        "only MIDI and instrument tracks make empty clips".into(),
                    ));
                }
                let color = Some(t.color);
                let bar = self
                    .project
                    .timeline
                    .meter
                    .bar_start(1)
                    .max(MusicalTime::from_quarters_i(1));
                let n = self.project.launcher.slots.len() + 1;
                let id: ClipId = self.project.ids.allocate();
                let clip = Clip {
                    id,
                    track,
                    name: format!("Clip {n}"),
                    color,
                    start: MusicalTime::ZERO,
                    muted: false,
                    content: ClipContent::Midi(MidiClip {
                        length: bar,
                        ..MidiClip::default()
                    }),
                };
                self.edit(Command::SetLauncherSlot {
                    track,
                    scene,
                    clip: Some(Box::new(clip)),
                })?;
                self.selection
                    .select_clips(&[id], crate::SelectMode::Replace);
                self.editor_clip = Some(id);
                self.revision += 1;
            }
            LauncherOp::SendClips(clips) => self.send_to_launcher(&clips)?,
            LauncherOp::MoveClip { from, to, copy } => {
                if from == to {
                    return Ok(());
                }
                let Some(c) = self
                    .project
                    .launcher
                    .slots
                    .get(&from)
                    .and_then(|c| self.project.clips.get(c))
                else {
                    return Ok(());
                };
                let target = self
                    .project
                    .track(to.track)
                    .ok_or(SessionError::Other("no such track".into()))?;
                let fits = match &c.content {
                    ClipContent::Midi(_) => {
                        matches!(target.kind, TrackKind::Instrument | TrackKind::Midi)
                    }
                    _ => target.kind == TrackKind::Audio,
                };
                if !fits {
                    return Err(SessionError::Other(format!(
                        "'{}' cannot hold this clip",
                        target.name
                    )));
                }
                let mut moved = c.clone();
                moved.track = to.track;
                moved.id = self.project.ids.allocate();
                let mut commands = Vec::new();
                if !copy {
                    commands.push(Command::SetLauncherSlot {
                        track: from.track,
                        scene: from.scene,
                        clip: None,
                    });
                }
                commands.push(Command::SetLauncherSlot {
                    track: to.track,
                    scene: to.scene,
                    clip: Some(Box::new(moved)),
                });
                // The follow action goes (or is copied) with the clip.
                let follow = self.project.launcher.follow.get(&from).copied();
                if !copy && follow.is_some() {
                    commands.push(Command::SetFollowAction {
                        track: from.track,
                        scene: from.scene,
                        follow: None,
                    });
                }
                commands.push(Command::SetFollowAction {
                    track: to.track,
                    scene: to.scene,
                    follow,
                });
                self.edit(Command::Batch {
                    label: if copy { "Copy Clip" } else { "Move Clip" }.into(),
                    commands,
                })?;
            }
            LauncherOp::ClearSlot { track, scene } => {
                if self.project.launcher.clip(track, scene).is_some() {
                    self.edit(Command::Batch {
                        label: "Delete Launcher Clip".into(),
                        commands: vec![
                            Command::SetLauncherSlot {
                                track,
                                scene,
                                clip: None,
                            },
                            Command::SetFollowAction {
                                track,
                                scene,
                                follow: None,
                            },
                        ],
                    })?;
                }
            }
            LauncherOp::SetFollow {
                track,
                scene,
                follow,
            } => {
                self.edit(Command::SetFollowAction {
                    track,
                    scene,
                    follow,
                })?;
            }
            LauncherOp::SetQuantize(quantize) => {
                self.edit(Command::SetLaunchQuantize { quantize })?;
            }
            LauncherOp::SetRecord(on) => {
                if !on {
                    self.close_launch_runs(self.launcher.last_pos);
                    self.write_launch_runs()?;
                }
                self.launcher.record = on;
                self.launcher.open.clear();
                self.revision += 1;
            }
        }
        Ok(())
    }

    /// The slot being recorded into: track, scene, and whether it is
    /// ending.
    pub fn launcher_recording(&self) -> Option<(TrackId, SceneId, bool)> {
        let s = self.recording.as_ref()?.slot?;
        Some((s.track, s.scene, s.end.is_some()))
    }

    /// Start recording into a slot, or end the recording there.
    fn record_slot(&mut self, track: TrackId, scene: SceneId) -> Result<()> {
        if let Some(s) = self.recording.as_ref().and_then(|r| r.slot) {
            if s.track == track && s.scene == scene {
                self.end_slot_recording(Some(track));
                return Ok(());
            }
            return Err(SessionError::Other("already recording into a slot".into()));
        }
        if self.recording.is_some() {
            return Err(SessionError::Other(
                "stop recording the arrangement first".into(),
            ));
        }
        let Some(t) = self.project.track(track) else {
            return Err(SessionError::Other("no such track".into()));
        };
        if !t.record_arm {
            return Err(SessionError::Other(format!(
                "arm '{}' to record into the launcher",
                t.name
            )));
        }
        if self.project.launcher.clip(track, scene).is_some() {
            return Err(SessionError::Other("the slot has a clip".into()));
        }
        let quantize = self.project.launcher.quantize.into();
        let pos = self.engine.transport_snapshot().position;
        let playing = self.transport.playing;
        let from = if playing {
            faderframe_engine::launch::next_boundary(
                quantize,
                pos,
                &self.project.timeline,
                self.engine.sample_rate() as f64,
            )
        } else {
            pos
        };
        self.start_recording_only(from, Some(track))?;
        let Some(r) = self.recording.as_mut() else {
            // Nothing could be recorded (a notice says why).
            return Ok(());
        };
        r.slot = Some(SlotRecording {
            track,
            scene,
            from,
            end: None,
        });
        // What the track plays stops where the recording starts.
        self.engine
            .launch(LaunchCommand::Stop { track, quantize })?;
        if !playing {
            self.play()?;
        }
        self.revision += 1;
        Ok(())
    }

    /// The slot recording (on `track`, or any) ends at the next launch
    /// position.
    fn end_slot_recording(&mut self, track: Option<TrackId>) {
        let quantize = self.project.launcher.quantize.into();
        let pos = self.engine.transport_snapshot().position;
        let rate = self.engine.sample_rate() as f64;
        let tl = &self.project.timeline;
        if let Some(s) = self.recording.as_mut().and_then(|r| r.slot.as_mut())
            && s.end.is_none()
            && track.is_none_or(|t| t == s.track)
        {
            let end = faderframe_engine::launch::next_boundary(quantize, pos, tl, rate);
            s.end = Some(end.max(s.from + 1));
            self.revision += 1;
        }
    }

    /// A slot recording's take becomes the slot's clip, playing on in time.
    pub(crate) fn finish_slot_recording(
        &mut self,
        slot: SlotRecording,
        outcome: crate::record::RecordOutcome,
        midi: Option<&crate::midi::MidiTake>,
        latency: i64,
    ) -> Result<()> {
        let end = slot.end.unwrap_or(slot.from + 1);
        let mut commands = Vec::new();
        let mut clip = None;
        if let Some(m) = midi
            && let Some(i) = m.tracks.iter().position(|t| *t == slot.track)
        {
            // Notes are where they were heard already.
            clip = self.midi_take_clip(m, i, Some((slot.from, end)));
        }
        let mut opened = None;
        if let Some(take) = outcome.takes.into_iter().find(|t| t.track == slot.track) {
            let p = &mut self.project;
            let take_rate = take.sample_rate.max(1) as f64;
            let to_project = |f: i64| (f as f64 * p.sample_rate as f64 / take_rate).round() as i64;
            let name = take.path.file_stem().map_or_else(
                || "Launcher Take".to_string(),
                |s| s.to_string_lossy().to_string(),
            );
            if let Some(seg) = take.segments.first() {
                let offset =
                    seg.file_offset as i64 + latency + (slot.from - seg.timeline_start).max(0);
                let length = (end - slot.from).min(take.frames as i64 - offset);
                if length > 0 {
                    let source = faderframe_project::AudioSource {
                        id: p.ids.allocate(),
                        name: name.clone(),
                        spec: faderframe_project::SourceSpec::File {
                            path: take.path.clone(),
                            channels: take.channels as u16,
                            frames: take.frames as i64,
                            sample_rate: take.sample_rate,
                        },
                    };
                    clip = Some(Clip {
                        id: p.ids.allocate(),
                        track: slot.track,
                        name,
                        color: None,
                        start: MusicalTime::ZERO,
                        muted: false,
                        content: ClipContent::Audio(faderframe_project::AudioClip {
                            source: source.id,
                            source_offset: to_project(offset),
                            length: to_project(length),
                            gain_db: 0.0,
                            fades: faderframe_project::ClipFades::default(),
                            stretch: faderframe_project::StretchSettings::Off,
                            reversed: false,
                            warp: None,
                            pitch: None,
                            effects: None,
                        }),
                    });
                    opened = Some((source.id, take.path.clone(), take.peaks));
                    commands.push(Command::AddSource {
                        source: Box::new(source),
                    });
                }
            }
        }
        let Some(mut clip) = clip else {
            self.notify(
                crate::NoticeLevel::Warning,
                "nothing was recorded into the slot",
            );
            return Ok(());
        };
        clip.start = MusicalTime::ZERO;
        if let Some((id, path, peaks)) = opened {
            match crate::media::open_stream(&path) {
                Ok(s) => {
                    self.sources
                        .insert(id, faderframe_engine::Source::Stream(s));
                }
                Err(e) => self.notify(
                    crate::NoticeLevel::Error,
                    format!("{}: {e}", path.display()),
                ),
            }
            self.peaks.insert(id, std::sync::Arc::new(peaks));
        }
        commands.push(Command::SetLauncherSlot {
            track: slot.track,
            scene: slot.scene,
            clip: Some(Box::new(clip)),
        });
        self.edit(Command::Batch {
            label: "Record Clip".into(),
            commands,
        })?;
        // It plays on from where the recording started, in time.
        if self.transport.playing {
            self.engine.launch(LaunchCommand::Resume {
                track: slot.track,
                slot: SlotKey {
                    track: slot.track,
                    scene: slot.scene,
                }
                .hash(),
                start: slot.from,
            })?;
        }
        Ok(())
    }

    /// A slot recording past its end stops (every tick).
    pub(crate) fn poll_slot_recording(&mut self) {
        let due = self.recording.as_ref().and_then(|r| {
            let end = r.slot?.end?;
            // Captured that much after it was heard.
            let late = r.latency.max(r.midi.as_ref().map_or(0, |m| m.shift));
            Some(end + late)
        });
        if let Some(due) = due
            && self.engine.transport_snapshot().position >= due
            && let Err(e) = self.stop_recording()
        {
            self.notify(crate::NoticeLevel::Error, e.to_string());
        }
    }

    /// Launching while stopped starts playback.
    fn start_for_launch(&mut self) -> Result<()> {
        if !self.transport.playing {
            self.play()?;
        }
        Ok(())
    }

    /// Copies of arrangement clips into their tracks' first free slots.
    fn send_to_launcher(&mut self, clips: &[ClipId]) -> Result<()> {
        let mut scenes = self.project.launcher.scenes.clone();
        let mut taken: Vec<SlotKey> = self.project.launcher.slots.keys().copied().collect();
        let mut commands = Vec::new();
        let mut list: Vec<&Clip> = clips
            .iter()
            .filter_map(|c| self.project.clips.get(c))
            .filter(|c| !self.project.is_launcher_clip(c.id))
            .collect();
        list.sort_by_key(|c| (c.start, c.id));
        let list: Vec<Clip> = list.into_iter().cloned().collect();
        for c in list {
            let free = scenes.iter().map(|s| s.id).find(|s| {
                !taken.contains(&SlotKey {
                    track: c.track,
                    scene: *s,
                })
            });
            let scene = match free {
                Some(s) => s,
                None => {
                    let id: SceneId = self.project.ids.allocate();
                    scenes.push(Scene {
                        id,
                        name: format!("Scene {}", scenes.len() + 1),
                    });
                    id
                }
            };
            taken.push(SlotKey {
                track: c.track,
                scene,
            });
            let mut copy = c.clone();
            copy.id = self.project.ids.allocate();
            copy.start = MusicalTime::ZERO;
            commands.push(Command::SetLauncherSlot {
                track: c.track,
                scene,
                clip: Some(Box::new(copy)),
            });
        }
        if commands.is_empty() {
            return Ok(());
        }
        commands.insert(0, Command::SetScenes { scenes });
        self.edit(Command::Batch {
            label: "Send to Launcher".into(),
            commands,
        })
    }

    /// After the transport moved on (every tick): the engine's launcher
    /// state, and recording what it plays.
    pub(crate) fn poll_launcher(&mut self, was_playing: bool) {
        let status = self.engine.launch_status();
        if status != self.launcher.status {
            self.revision += 1;
        }
        if self.transport.playing {
            // Everything moves while a clip plays.
            if status
                .iter()
                .any(|s| s.playing.is_some() || s.queued.is_some())
            {
                self.revision += 1;
            }
            self.launcher.last_pos = self.transport.position;
        }
        if self.launcher.record {
            for s in &status {
                let now = s.playing.and_then(|(slot, start)| {
                    let key = self
                        .project
                        .launcher
                        .slots
                        .keys()
                        .find(|k| k.hash() == slot)?;
                    Some((self.project.launcher.slots[key], start))
                });
                let was = self.launcher.open.get(&s.track).copied();
                if was == now {
                    continue;
                }
                if let Some((clip, start)) = was {
                    // It ended where the next started, or where it was told
                    // to stop, or about now.
                    let end = match (now, self.launch_state(s.track).and_then(|o| o.queued)) {
                        (Some((_, next)), _) => next,
                        (None, Some((None, at))) if at <= self.launcher.last_pos => at,
                        _ => self.launcher.last_pos,
                    };
                    self.launcher.runs.push(Run {
                        track: s.track,
                        clip,
                        start,
                        end,
                    });
                }
                match now {
                    Some(n) => self.launcher.open.insert(s.track, n),
                    None => self.launcher.open.remove(&s.track),
                };
            }
        }
        self.launcher.status = status;
        if was_playing && !self.transport.playing {
            self.close_launch_runs(self.launcher.last_pos);
            if let Err(e) = self.write_launch_runs() {
                self.notify(crate::NoticeLevel::Error, e.to_string());
            }
        }
    }

    /// Clips still playing end at `at`.
    fn close_launch_runs(&mut self, at: i64) {
        for (track, (clip, start)) in self.launcher.open.drain() {
            self.launcher.runs.push(Run {
                track,
                clip,
                start,
                end: at,
            });
        }
    }

    /// The runs into the arrangement (one undo step).
    fn write_launch_runs(&mut self) -> Result<()> {
        let runs = std::mem::take(&mut self.launcher.runs);
        if runs.is_empty() {
            return Ok(());
        }
        // Built on a copy so each run sees the ones before it.
        let mut scratch = self.project.clone();
        let mut commands = Vec::new();
        for run in runs.iter().filter(|r| r.end > r.start) {
            let Some(clip) = self.project.clips.get(&run.clip) else {
                continue;
            };
            let length = self.launch_length(clip);
            for cmd in run_commands(&mut scratch, run, clip, length, |p, s| {
                self.engine.samples_to_musical(p, s)
            }) {
                let applied = cmd.clone().apply(&mut scratch);
                if applied.is_ok() {
                    commands.push(cmd);
                }
            }
        }
        // The ids handed out on the copy.
        self.project.ids = scratch.ids.clone();
        if commands.is_empty() {
            return Ok(());
        }
        self.edit(Command::Batch {
            label: "Record Launches".into(),
            commands,
        })
    }

    /// The transport stops on the project being replaced: launches end.
    pub(crate) fn reset_launcher(&mut self) -> Result<()> {
        self.launcher.runs.clear();
        self.launcher.open.clear();
        self.engine.launch(LaunchCommand::BackToArrangement)?;
        Ok(())
    }
}

/// The commands writing one run into the arrangement: its span cleared on
/// the track, then copies of the clip over it, the last cut at its end.
fn run_commands(
    p: &mut Project,
    run: &Run,
    clip: &Clip,
    length: i64,
    to_musical: impl Fn(&Project, i64) -> MusicalTime,
) -> Vec<Command> {
    let a = to_musical(p, run.start);
    let b = to_musical(p, run.end);
    let mut out = crate::record::carve_any(p, run.track, a, b);
    let mut at = run.start;
    while at < run.end {
        let mut copy = clip.clone();
        copy.id = p.ids.allocate();
        copy.track = run.track;
        copy.start = to_musical(p, at);
        let end = copy.end(&p.timeline, p.sample_rate);
        if end > b {
            let right = p.ids.allocate();
            match copy.split_at(b, right, &p.timeline, p.sample_rate) {
                Ok((left, _)) => copy = left,
                Err(_) => break,
            }
        }
        out.push(Command::AddClip {
            clip: Box::new(copy),
        });
        at += length;
    }
    out
}
