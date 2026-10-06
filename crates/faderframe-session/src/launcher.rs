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
//! written into the arrangement as it plays — each loop once it has
//! played, the rest where the clip stopped or another took over — as
//! copies in time with the clip's loop, clearing what the track had there
//! (one undo step a transport run, "Record Launches"); moves of the
//! mixer and of device parameters are written as automation meanwhile
//! (latched, into new lanes where there are none).

use crate::{Result, Session, SessionError};
use faderframe_core::{ClipId, SceneId, TrackId};
use faderframe_engine::launch::Quantize;
use faderframe_engine::launch::{LaunchCommand, TrackStatus};
use faderframe_project::launcher::{LaunchMode, LaunchQuantize, Scene, SlotKey};
use faderframe_project::{Clip, ClipContent, Command, MidiClip, Project, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;

/// A launcher edit or action.
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherOp {
    /// A slot's launch button pressed: its clip launches as its launch
    /// mode says (a toggle playing stops); an empty slot stops its track.
    Launch {
        track: TrackId,
        scene: SceneId,
    },
    /// The launch button let go (gate and repeat clips stop).
    Release {
        track: TrackId,
        scene: SceneId,
    },
    /// A slot's launch settings (`None`: the defaults).
    SetClipLaunch {
        track: TrackId,
        scene: SceneId,
        launch: Option<faderframe_project::launcher::ClipLaunch>,
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
    /// position (for the launcher's fixed length, if it has one); again
    /// (or launching or stopping its track) ends it at the next one, and
    /// the new clip plays on in time. On a slot with a MIDI clip it
    /// overdubs: the clip plays and the notes played join its loop.
    Record {
        track: TrackId,
        scene: SceneId,
    },
    /// Copies of launcher clips (dragged to the arranger) into the
    /// arrangement at `at` on `track` (as they play: following the tempo);
    /// clips of other tracks go to the tracks after it, in their order,
    /// one after another on each.
    ToArrangement {
        clips: Vec<ClipId>,
        track: TrackId,
        at: MusicalTime,
    },
    /// Copies of clips (dragged from the arrangement) into a slot and the
    /// slots below it on its track (new scenes as needed; clips that do not
    /// fit the track are left out); clips of other tracks go to the
    /// columns after it, in the tracks' order.
    PlaceClips {
        clips: Vec<ClipId>,
        track: TrackId,
        scene: SceneId,
    },
    /// Slot recordings' length (bars, 0: until ended) and count-in.
    SetRecordOptions {
        bars: u16,
        count_in: u8,
    },
    /// A slot's follow action (`None`: none).
    SetFollow {
        track: TrackId,
        scene: SceneId,
        follow: Option<faderframe_project::launcher::FollowAction>,
    },
}

/// The payload of arrangement clips dragged to other views.
pub fn clips_payload(clips: &[ClipId]) -> String {
    let ids: Vec<String> = clips.iter().map(|c| c.raw().to_string()).collect();
    format!("clips:{}", ids.join(","))
}

/// The clips in a [`clips_payload`].
pub fn parse_clips_payload(payload: &str) -> Option<Vec<ClipId>> {
    let ids = payload.strip_prefix("clips:")?;
    ids.split(',')
        .map(|s| s.parse::<u64>().ok().map(ClipId))
        .collect()
}

/// A recording into a launcher slot (engine samples).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SlotRecording {
    pub track: TrackId,
    pub scene: SceneId,
    pub from: i64,
    /// Where it ends, once asked to (or from the start, with a fixed
    /// length).
    pub end: Option<i64>,
    /// Overdubbing the slot's MIDI clip, which started playing here: the
    /// notes are merged into its loop.
    pub overdub: Option<i64>,
}

/// One stretch a launched clip played: its clip, where its loop starts
/// (its phase), from/to (engine samples).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Run {
    track: TrackId,
    clip: ClipId,
    phase: i64,
    start: i64,
    end: i64,
}

/// A clip playing while recorded: its phase, where it began and how far
/// it is written into the arrangement.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OpenRun {
    clip: ClipId,
    phase: i64,
    written: i64,
}

/// The launcher's session state (not saved).
#[derive(Debug, Default)]
pub struct LauncherState {
    record: bool,
    /// The engine's state at the last tick.
    status: Vec<TrackStatus>,
    /// Runs played, waiting to be written (when no gesture is open).
    runs: Vec<Run>,
    /// Clips playing (for recording), by track.
    open: HashMap<TrackId, OpenRun>,
    /// The undo step the recording writes into (see
    /// `History::apply_amending`).
    amend: Option<u64>,
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
        let clip = self.project.launcher_clip_as_played(*key)?;
        let length = self.launch_length(&clip).max(1);
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
            LauncherOp::Release { .. } | LauncherOp::SetClipLaunch { .. } => {}
            LauncherOp::LaunchScene(_) | LauncherOp::StopAll => self.end_slot_recording(None),
            _ => {}
        }
        match op {
            LauncherOp::Record { track, scene } => self.record_slot(track, scene)?,
            LauncherOp::Launch { track, scene } => match self.project.launcher.clip(track, scene) {
                Some(_) => {
                    let key = SlotKey { track, scene };
                    let settings = self.project.launcher.launch_of(key);
                    let q: Quantize = self.project.launcher.quantize_of(key).into();
                    let slot = key.hash();
                    let state = self.launch_state(track);
                    let busy = state.is_some_and(|s| {
                        s.playing.is_some_and(|(p, _)| p == slot)
                            || s.queued.is_some_and(|(n, _)| n == Some(slot))
                    });
                    if settings.mode == LaunchMode::Toggle && busy {
                        self.engine
                            .launch(LaunchCommand::Stop { track, quantize: q })?;
                    } else {
                        let repeat = match settings.mode {
                            LaunchMode::Repeat => self.repeat_every(q),
                            _ => 0,
                        };
                        self.engine.launch(LaunchCommand::Launch {
                            track,
                            slot,
                            quantize: q,
                            legato: settings.legato,
                            repeat,
                        })?;
                        self.start_for_launch()?;
                    }
                }
                None => self
                    .engine
                    .launch(LaunchCommand::Stop { track, quantize })?,
            },
            LauncherOp::Release { track, scene } => {
                let key = SlotKey { track, scene };
                if self.project.launcher.slots.contains_key(&key)
                    && matches!(
                        self.project.launcher.launch_of(key).mode,
                        LaunchMode::Gate | LaunchMode::Repeat
                    )
                {
                    self.engine.launch(LaunchCommand::Release {
                        track,
                        slot: key.hash(),
                        quantize: self.project.launcher.quantize_of(key).into(),
                    })?;
                }
            }
            LauncherOp::SetClipLaunch {
                track,
                scene,
                launch,
            } => {
                self.edit(Command::SetClipLaunch {
                    track,
                    scene,
                    launch,
                })?;
            }
            LauncherOp::LaunchScene(scene) => {
                let tracks: Vec<TrackId> = self.launcher_tracks().iter().map(|t| t.id).collect();
                for track in tracks {
                    let key = SlotKey { track, scene };
                    let cmd = match self.project.launcher.clip(track, scene) {
                        Some(_) => LaunchCommand::Launch {
                            track,
                            slot: key.hash(),
                            quantize: self.project.launcher.quantize_of(key).into(),
                            legato: self.project.launcher.launch_of(key).legato,
                            repeat: 0,
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
                commands.extend(
                    self.project
                        .launcher
                        .launch
                        .keys()
                        .filter(|k| k.scene == scene)
                        .map(|k| Command::SetClipLaunch {
                            track: k.track,
                            scene,
                            launch: None,
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
                    if let Some(l) = self.project.launcher.launch.get(&k) {
                        commands.push(Command::SetClipLaunch {
                            track: k.track,
                            scene: id,
                            launch: Some(*l),
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
                let launch = self.project.launcher.launch.get(&from).copied();
                if !copy && launch.is_some() {
                    commands.push(Command::SetClipLaunch {
                        track: from.track,
                        scene: from.scene,
                        launch: None,
                    });
                }
                commands.push(Command::SetClipLaunch {
                    track: to.track,
                    scene: to.scene,
                    launch,
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
                            Command::SetClipLaunch {
                                track,
                                scene,
                                launch: None,
                            },
                        ],
                    })?;
                }
            }
            LauncherOp::PlaceClips {
                clips,
                track,
                scene,
            } => self.place_clips(&clips, track, scene)?,
            LauncherOp::ToArrangement { clips, track, at } => {
                self.copy_to_arrangement(&clips, track, at)?;
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
            LauncherOp::SetRecordOptions { bars, count_in } => {
                self.edit(Command::SetLaunchRecording { bars, count_in })?;
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
        let key = SlotKey { track, scene };
        let overdub = match self.project.launcher.clip(track, scene) {
            Some(c) if self.project.clip(c).is_some_and(|c| c.as_midi().is_some()) => true,
            Some(_) => return Err(SessionError::Other("the slot has a clip".into())),
            None => false,
        };
        let quantize: Quantize = if overdub {
            self.project.launcher.quantize_of(key).into()
        } else {
            self.project.launcher.quantize.into()
        };
        let pos = self.engine.transport_snapshot().position;
        let playing = self.transport.playing;
        let rate = self.engine.sample_rate() as f64;
        let boundary = |q| {
            if playing {
                faderframe_engine::launch::next_boundary(q, pos, &self.project.timeline, rate)
            } else {
                pos
            }
        };
        let slot_hash = key.hash();
        // Overdubbing: the clip plays (launched now if it does not), the
        // recording starts with it or on the next launch position.
        let playing_start = self
            .launch_state(track)
            .and_then(|s| s.playing)
            .filter(|(s, _)| *s == slot_hash)
            .map(|(_, start)| start);
        let from = boundary(quantize);
        let overdub_start = overdub.then(|| playing_start.unwrap_or(from));
        self.start_recording_only(from, Some(track))?;
        let Some(r) = self.recording.as_mut() else {
            // Nothing could be recorded (a notice says why).
            return Ok(());
        };
        // A fixed length ends it that many bars on.
        let bars = self.project.launcher.record_bars;
        let end = (bars > 0).then(|| {
            let tl = &self.project.timeline;
            let at = tl.to_musical(from, rate);
            let bar = tl.meter.bar_at(at);
            let length = tl.meter.bar_start(bar + i32::from(bars)) - tl.meter.bar_start(bar);
            tl.to_samples(at + length, rate)
        });
        r.slot = Some(SlotRecording {
            track,
            scene,
            from,
            end,
            overdub: overdub_start,
        });
        if overdub {
            if playing_start.is_none() {
                self.engine.launch(LaunchCommand::Launch {
                    track,
                    slot: slot_hash,
                    quantize,
                    legato: false,
                    repeat: 0,
                })?;
            }
        } else {
            // What the track plays stops where the recording starts.
            self.engine
                .launch(LaunchCommand::Stop { track, quantize })?;
        }
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
        if let Some(play_start) = slot.overdub {
            return self.finish_overdub(slot, play_start, end, midi);
        }
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
        let at = self.engine.samples_to_musical(&self.project, slot.from);
        let follow = self.follow_tempo_command(
            &clip,
            SlotKey {
                track: slot.track,
                scene: slot.scene,
            },
            at,
        );
        commands.push(Command::SetLauncherSlot {
            track: slot.track,
            scene: slot.scene,
            clip: Some(Box::new(clip)),
        });
        commands.extend(follow);
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

    /// An overdub's notes into the slot's MIDI clip: each where it falls in
    /// the loop (from where the clip started playing), as long as it was
    /// held (a loop at most). One undo step.
    fn finish_overdub(
        &mut self,
        slot: SlotRecording,
        play_start: i64,
        end: i64,
        midi: Option<&crate::midi::MidiTake>,
    ) -> Result<()> {
        let key = SlotKey {
            track: slot.track,
            scene: slot.scene,
        };
        let Some(m) = midi else { return Ok(()) };
        let Some(i) = m.tracks.iter().position(|t| *t == slot.track) else {
            return Ok(());
        };
        let Some(id) = self.project.launcher.slots.get(&key).copied() else {
            return Ok(());
        };
        let Some(clip) = self.project.clips.get(&id).cloned() else {
            return Ok(());
        };
        let Some(took) = self.midi_take_clip(m, i, Some((slot.from, end))) else {
            return Ok(());
        };
        let (Some(mine), Some(new)) = (clip.as_midi(), took.as_midi()) else {
            return Ok(());
        };
        let loop_len = self.launch_length(&clip).max(1);
        let from = self.engine.samples_to_musical(&self.project, slot.from);
        let mut merged = mine.clone();
        for n in &new.notes {
            let at = self
                .engine
                .musical_to_samples(&self.project, from + n.start);
            let in_loop = (at - play_start).rem_euclid(loop_len);
            let start = self.engine.samples_to_musical(&self.project, in_loop);
            let mut note = *n;
            note.id = self.project.ids.allocate();
            note.start = start;
            note.length = n
                .length
                .min(mine.length)
                .max(faderframe_timeline::MusicalTime(1));
            merged.notes.push(note);
        }
        if new.notes.is_empty() {
            self.notify(crate::NoticeLevel::Warning, "nothing was overdubbed");
            return Ok(());
        }
        merged.notes.sort_by_key(|n| (n.start, n.key));
        self.edit(Command::Batch {
            label: "Overdub".into(),
            commands: vec![Command::SetClipContent {
                clip: id,
                start: clip.start,
                content: Box::new(ClipContent::Midi(merged)),
            }],
        })
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

    /// An audio clip entering a slot follows the tempo from the one it was
    /// in time with (at `at`).
    fn follow_tempo_command(&self, clip: &Clip, key: SlotKey, at: MusicalTime) -> Option<Command> {
        clip.is_audio().then(|| Command::SetClipLaunch {
            track: key.track,
            scene: key.scene,
            launch: Some(faderframe_project::launcher::ClipLaunch {
                tempo: Some(self.project.timeline.tempo.bpm_at(at)),
                ..self.project.launcher.launch_of(key)
            }),
        })
    }

    /// How often a repeating clip starts again: its launch quantum where
    /// the playhead is (a sixteenth without quantisation).
    fn repeat_every(&self, q: Quantize) -> i64 {
        let tl = &self.project.timeline;
        let rate = self.engine.sample_rate() as f64;
        let pos = self.engine.transport_snapshot().position.max(0);
        let here = tl.to_musical(pos, rate);
        let length = match q {
            Quantize::None => MusicalTime::from_quarters(0.25),
            Quantize::Beat => {
                let sig = tl.meter.signature_of_bar(tl.meter.bar_at(here));
                MusicalTime::from_quarters(4.0 / f64::from(sig.denominator.max(1)))
            }
            Quantize::Bars(n) => {
                let bar = tl.meter.bar_at(here);
                tl.meter.bar_start(bar + n as i32) - tl.meter.bar_start(bar)
            }
        };
        (tl.to_samples(here + length, rate) - tl.to_samples(here, rate)).max(1)
    }

    /// Launching while stopped starts playback.
    fn start_for_launch(&mut self) -> Result<()> {
        if !self.transport.playing {
            self.play()?;
        }
        Ok(())
    }

    /// Copies of `clips` into `track`'s slot in `scene` and the ones below;
    /// clips of other tracks into the columns after it.
    fn place_clips(&mut self, clips: &[ClipId], track: TrackId, scene: SceneId) -> Result<()> {
        let columns: Vec<TrackId> = self.launcher_tracks().iter().map(|t| t.id).collect();
        let Some(first_col) = columns.iter().position(|t| *t == track) else {
            return Err(SessionError::Other("no such track".into()));
        };
        // The clips by their own track, in the editors' order.
        let order = self.project.folder_order();
        let rank = |t: TrackId| order.iter().position(|x| x.id == t).unwrap_or(usize::MAX);
        let mut list: Vec<Clip> = clips
            .iter()
            .filter_map(|c| self.project.clips.get(c))
            .cloned()
            .collect();
        list.sort_by_key(|c| (rank(c.track), c.start, c.id));
        let mut groups: Vec<Vec<Clip>> = Vec::new();
        for c in list {
            match groups.last_mut() {
                Some(g) if g[0].track == c.track => g.push(c),
                _ => groups.push(vec![c]),
            }
        }
        let mut scenes = self.project.launcher.scenes.clone();
        let Some(first) = scenes.iter().position(|s| s.id == scene) else {
            return Err(SessionError::Other("no such scene".into()));
        };
        let mut commands = Vec::new();
        let mut placed = 0;
        for (g, group) in groups.into_iter().enumerate() {
            let Some(&column) = columns.get(first_col + g) else {
                break;
            };
            let Some(kind) = self.project.track(column).map(|t| t.kind) else {
                continue;
            };
            let fits = |c: &Clip| match c.content {
                ClipContent::Midi(_) => matches!(kind, TrackKind::Instrument | TrackKind::Midi),
                _ => kind == TrackKind::Audio,
            };
            for (k, c) in group.into_iter().filter(|c| fits(c)).enumerate() {
                let i = first + k;
                while i >= scenes.len() {
                    let id: SceneId = self.project.ids.allocate();
                    scenes.push(Scene {
                        id,
                        name: format!("Scene {}", scenes.len() + 1),
                    });
                }
                let at = c.start;
                let mut copy = c;
                copy.id = self.project.ids.allocate();
                copy.track = column;
                copy.start = MusicalTime::ZERO;
                let key = SlotKey {
                    track: column,
                    scene: scenes[i].id,
                };
                let follow = self.follow_tempo_command(&copy, key, at);
                commands.push(Command::SetLauncherSlot {
                    track: column,
                    scene: scenes[i].id,
                    clip: Some(Box::new(copy)),
                });
                commands.extend(follow);
                placed += 1;
            }
        }
        if placed == 0 {
            return Err(SessionError::Other(
                "those clips do not fit this track".into(),
            ));
        }
        commands.insert(0, Command::SetScenes { scenes });
        self.edit(Command::Batch {
            label: "Clips to Launcher".into(),
            commands,
        })
    }

    /// Copies of launcher clips into the arrangement (see
    /// [`LauncherOp::ToArrangement`]).
    fn copy_to_arrangement(
        &mut self,
        clips: &[ClipId],
        track: TrackId,
        at: MusicalTime,
    ) -> Result<()> {
        let rows: Vec<TrackId> = self.project.folder_order().iter().map(|t| t.id).collect();
        let Some(first_row) = rows.iter().position(|t| *t == track) else {
            return Err(SessionError::Other("no such track".into()));
        };
        // The clips as they play, by their track (in order), then scene.
        let launcher = &self.project.launcher;
        let mut list: Vec<(usize, usize, Clip)> = clips
            .iter()
            .filter_map(|c| {
                let key = launcher.slot_of(*c)?;
                let row = rows.iter().position(|t| *t == key.track)?;
                let scene = launcher.scene_index(key.scene)?;
                Some((row, scene, self.project.launcher_clip_as_played(key)?))
            })
            .collect();
        if list.is_empty() {
            return Err(SessionError::Other("no launcher clips".into()));
        }
        list.sort_by_key(|(row, scene, _)| (*row, *scene));
        let base_row = list[0].0;
        let mut commands = Vec::new();
        let mut placed = 0;
        let mut next_at: HashMap<TrackId, MusicalTime> = HashMap::new();
        for (row, _, clip) in list {
            let Some(&target) = rows.get(first_row + (row - base_row)) else {
                continue;
            };
            let Some(kind) = self.project.track(target).map(|t| t.kind) else {
                continue;
            };
            let fits = match clip.content {
                ClipContent::Midi(_) => matches!(kind, TrackKind::Instrument | TrackKind::Midi),
                _ => kind == TrackKind::Audio,
            };
            if !fits {
                continue;
            }
            let start = *next_at.get(&target).unwrap_or(&at);
            let mut copy = clip;
            copy.id = self.project.ids.allocate();
            copy.track = target;
            copy.start = start;
            next_at.insert(
                target,
                copy.end(&self.project.timeline, self.project.sample_rate),
            );
            commands.push(Command::AddClip {
                clip: Box::new(copy),
            });
            placed += 1;
        }
        if placed == 0 {
            return Err(SessionError::Other(
                "those clips do not fit this track".into(),
            ));
        }
        self.edit(Command::Batch {
            label: "Clips to Arrangement".into(),
            commands,
        })
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
            let follow = self.follow_tempo_command(
                &copy,
                SlotKey {
                    track: c.track,
                    scene,
                },
                c.start,
            );
            commands.push(Command::SetLauncherSlot {
                track: c.track,
                scene,
                clip: Some(Box::new(copy)),
            });
            commands.extend(follow);
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
        if self.transport.playing && !was_playing {
            // A new transport run: a new undo step.
            self.launcher.amend = None;
        }
        if self.launcher.record {
            let pos = self.transport.position;
            for s in &status {
                let now = s.playing.and_then(|(slot, phase)| {
                    let key = self
                        .project
                        .launcher
                        .slots
                        .keys()
                        .find(|k| k.hash() == slot)?;
                    Some((self.project.launcher.slots[key], phase))
                });
                let was = self.launcher.open.get(&s.track).copied();
                if was.map(|w| (w.clip, w.phase)) == now {
                    continue;
                }
                // When it changed: at the launch (or stop) the last tick
                // saw waiting, else where the new clip's loop starts, else
                // about now.
                let waited = self
                    .launch_state(s.track)
                    .and_then(|o| o.queued)
                    .map(|(_, at)| at)
                    .filter(|at| *at <= pos);
                let switch = waited
                    .or(now.map(|(_, phase)| phase))
                    .unwrap_or(self.launcher.last_pos);
                if let Some(w) = was {
                    self.launcher.runs.push(Run {
                        track: s.track,
                        clip: w.clip,
                        phase: w.phase,
                        start: w.written,
                        end: switch.max(w.written),
                    });
                }
                match now {
                    Some((clip, phase)) => self.launcher.open.insert(
                        s.track,
                        OpenRun {
                            clip,
                            phase,
                            written: switch.max(phase),
                        },
                    ),
                    None => self.launcher.open.remove(&s.track),
                };
            }
            // Clips playing on: every loop played goes in now.
            let open: Vec<(TrackId, OpenRun)> =
                self.launcher.open.iter().map(|(t, o)| (*t, *o)).collect();
            for (track, o) in open {
                let Some(clip) = self
                    .project
                    .launcher
                    .slot_of(o.clip)
                    .and_then(|k| self.project.launcher_clip_as_played(k))
                else {
                    continue;
                };
                let length = self.launch_length(&clip);
                let looped = o.phase + (pos - o.phase).div_euclid(length) * length;
                if looped > o.written {
                    self.launcher.runs.push(Run {
                        track,
                        clip: o.clip,
                        phase: o.phase,
                        start: o.written,
                        end: looped,
                    });
                    if let Some(w) = self.launcher.open.get_mut(&track) {
                        w.written = looped;
                    }
                }
            }
        }
        self.launcher.status = status;
        if was_playing && !self.transport.playing {
            self.close_launch_runs(self.launcher.last_pos);
        }
        // Written when no gesture is open (they would join it).
        if !self.launcher.runs.is_empty()
            && !self.history.in_gesture()
            && let Err(e) = self.write_launch_runs()
        {
            self.notify(crate::NoticeLevel::Error, e.to_string());
        }
    }

    /// Clips still playing end at `at`.
    fn close_launch_runs(&mut self, at: i64) {
        for (track, o) in self.launcher.open.drain() {
            self.launcher.runs.push(Run {
                track,
                clip: o.clip,
                phase: o.phase,
                start: o.written,
                end: at.max(o.written),
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
            // As it played (following the tempo).
            let Some(clip) = self
                .project
                .launcher
                .slot_of(run.clip)
                .and_then(|k| self.project.launcher_clip_as_played(k))
                .or_else(|| self.project.clips.get(&run.clip).cloned())
            else {
                continue;
            };
            let length = self.launch_length(&clip);
            for cmd in run_commands(&mut scratch, run, &clip, length, |p, s| {
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
        // One undo step a recording, however often it writes.
        let (impact, token) = self.history.apply_amending(
            &mut self.project,
            Command::Batch {
                label: "Record Launches".into(),
                commands,
            },
            "Record Launches",
            self.launcher.amend,
        )?;
        self.launcher.amend = Some(token);
        self.sync(impact)
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
/// the track, then copies of the clip in time with its loop over it, cut
/// at the span's ends.
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
    let length = length.max(1);
    let mut at = run.phase + (run.start - run.phase).div_euclid(length) * length;
    while at < run.end {
        let mut copy = clip.clone();
        copy.id = p.ids.allocate();
        copy.track = run.track;
        copy.start = to_musical(p, at);
        if at < run.start {
            let right = p.ids.allocate();
            match copy.split_at(a, right, &p.timeline, p.sample_rate) {
                Ok((_, r)) => copy = r,
                Err(_) => {
                    at += length;
                    continue;
                }
            }
        }
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
