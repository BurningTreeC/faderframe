//! Cue mixes in the session (see `faderframe_project::cue`): making one (a
//! copy of the main mix on the next free outputs), removing, renaming,
//! moving it to other outputs, pre/post-fader (its levels converted so it
//! sounds the same), copying the main mix again, the mixer's cue mode
//! (which mix its faders, pans and mutes set), new tracks joining every
//! cue mix, and the talkback microphone (a preference: its input, level
//! and how much the cue mixes dim while the engineer talks; which cue
//! mixes hear it is the project's). Every change to the project is an
//! undoable edit.

use crate::{Action, InputChoice, NoticeLevel, Result, Session, SessionError};
use faderframe_core::gain::SILENCE_DB;
use faderframe_core::{ChannelLayout, CueId, TrackId, db_to_gain};
use faderframe_project::cue::{CUE_COLORS, CueMix, CueSend, MAX_CUE_MIXES, feeds_cues};
use faderframe_project::{Command, MAX_LEVEL_DB, SendTap, Track};
use serde::{Deserialize, Serialize};

/// The talkback microphone (Preferences → Audio, the Cue Mixes window).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TalkbackSettings {
    /// The device input it comes from (0: input 1; `None`: no talkback).
    pub input: Option<u16>,
    /// Its level into the cue mixes that hear it (dB).
    pub level_db: f32,
    /// How much those cue mixes dim while it talks (dB, ≤ 0).
    pub dim_db: f32,
}

impl Default for TalkbackSettings {
    fn default() -> Self {
        Self {
            input: None,
            level_db: 0.0,
            dim_db: -20.0,
        }
    }
}

/// The TALK button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkOp {
    /// Pressed: talking while held.
    Press,
    /// Let go (a latched button stays on).
    Release,
    /// Latched on or off (Shift-click, a double-click).
    Latch(bool),
}

/// What can be done to the cue mixes (besides their levels, pans and
/// switches, which are plain edits: `Command::SetCue*`).
#[derive(Clone, Debug, PartialEq)]
pub enum CueOp {
    /// A new cue mix: a copy of the main mix on the next free outputs (the
    /// mixer shows it).
    New,
    Remove(CueId),
    Rename {
        cue: CueId,
        name: String,
    },
    /// Its outputs: from `first_channel` (0: output 1), stereo or mono.
    SetOutput {
        cue: CueId,
        first_channel: u16,
        layout: ChannelLayout,
    },
    /// Before or after the fader (the levels follow, so it sounds the
    /// same).
    SetTap {
        cue: CueId,
        tap: SendTap,
    },
    /// Every send as the main mix has it again.
    CopyMainMix(CueId),
    /// The mixer's faders, pans and mutes set this cue mix's sends
    /// (`None`: the main mix).
    Show(Option<CueId>),
    /// Whether the cue mix hears the talkback microphone.
    SetTalkback {
        cue: CueId,
        on: bool,
    },
}

impl Session {
    pub fn cue_mixes(&self) -> &[CueMix] {
        &self.project.cue_mixes
    }

    /// The cue mix the mixer shows (and its faders set), if any.
    pub fn mixer_cue(&self) -> Option<&CueMix> {
        self.mixer_cue.and_then(|c| self.project.cue(c))
    }

    /// `track`'s send to `cue` as the mixer shows it (a track without one
    /// is not in it: off, silent).
    pub fn cue_send(&self, track: &Track, cue: CueId) -> CueSend {
        track.cue_send(cue).copied().unwrap_or(CueSend {
            cue,
            level_db: SILENCE_DB,
            pan: 0.0,
            on: false,
        })
    }

    /// The cue mix's own channel's meters (after its master level).
    pub fn cue_meter(&self, cue: CueId) -> crate::MeterDisplay {
        self.project
            .cue(cue)
            .map(|c| self.meter(c.channel()))
            .unwrap_or_default()
    }

    pub(crate) fn cue_op(&mut self, op: CueOp) -> Result<()> {
        match op {
            CueOp::New => {
                self.new_cue_mix()?;
            }
            CueOp::Remove(cue) => {
                self.edit(Command::RemoveCueMix { cue })?;
                if self.mixer_cue == Some(cue) {
                    self.mixer_cue = None;
                }
            }
            CueOp::Rename { cue, name } => {
                let mut c = self.cue_or_err(cue)?.clone();
                c.name = name;
                self.edit(Command::SetCueMix { cue: Box::new(c) })?;
            }
            CueOp::SetOutput {
                cue,
                first_channel,
                layout,
            } => {
                let mut c = self.cue_or_err(cue)?.clone();
                c.first_channel = first_channel;
                c.layout = faderframe_project::cue::cue_layout(layout);
                self.edit(Command::SetCueMix { cue: Box::new(c) })?;
                self.say_where(cue);
            }
            CueOp::SetTap { cue, tap } => self.set_cue_tap(cue, tap)?,
            CueOp::CopyMainMix(cue) => self.copy_main_mix(cue)?,
            CueOp::Show(cue) => {
                self.mixer_cue = cue.filter(|c| self.project.cue(*c).is_some());
                self.revision += 1;
            }
            CueOp::SetTalkback { cue, on } => {
                self.cue_or_err(cue)?;
                self.edit(Command::SetCueTalkback { cue, on })?;
            }
        }
        Ok(())
    }

    // --- talkback ----------------------------------------------------------------------

    pub fn talkback_settings(&self) -> &TalkbackSettings {
        &self.talkback
    }

    /// The talkback microphone's input, level and dim (a preference);
    /// another input reopens the device for it when needed.
    pub fn set_talkback_settings(&mut self, settings: TalkbackSettings) -> Result<()> {
        let settings = TalkbackSettings {
            level_db: settings.level_db.clamp(SILENCE_DB, MAX_LEVEL_DB),
            dim_db: settings.dim_db.clamp(SILENCE_DB, 0.0),
            ..settings
        };
        if settings == self.talkback {
            return Ok(());
        }
        self.talkback = settings;
        self.revision += 1;
        self.apply_talkback()?;
        self.check_channels();
        Ok(())
    }

    /// The TALK button.
    pub(crate) fn talk(&mut self, op: TalkOp) -> Result<()> {
        let was = self.talking();
        match op {
            TalkOp::Press => self.talk_held = true,
            TalkOp::Release => self.talk_held = false,
            TalkOp::Latch(on) => {
                self.talk_latched = on;
                self.talk_held = false;
            }
        }
        if self.talking() != was {
            self.revision += 1;
            if self.talkback.input.is_none() {
                self.notify(
                    NoticeLevel::Info,
                    "Talkback has no input: choose one in the Cue Mixes window",
                );
            }
        }
        self.apply_talkback()
    }

    /// The engineer is talking to the performers (the button held, or
    /// latched).
    pub fn talking(&self) -> bool {
        self.talk_held || self.talk_latched
    }

    pub fn talkback_latched(&self) -> bool {
        self.talk_latched
    }

    /// The engine's talkback state from the settings and the button (also
    /// for a new engine).
    pub(crate) fn apply_talkback(&mut self) -> Result<()> {
        let t = faderframe_engine::Talkback {
            on: self.talking(),
            input: self.talkback.input,
            level: db_to_gain(self.talkback.level_db),
            dim: db_to_gain(self.talkback.dim_db),
        };
        if self.engine.set_talkback(&self.project, t)? {
            self.sync(faderframe_project::Impact::Graph)?;
        }
        Ok(())
    }

    /// `t`'s send to `cue` as the main mix has it now — from what the
    /// controls show (automation playing), by the console rule
    /// (`Project::main_mix_send`).
    pub fn main_mix_send(&self, t: &Track, cue: &CueMix) -> CueSend {
        self.project.main_mix_send_with(t, cue, |x| {
            (
                self.shown_volume_db(x),
                self.shown_pan(x),
                self.shown_mute(x),
            )
        })
    }

    /// Every track's send to `cue` as the main mix has it now.
    fn shown_main_mix(&self, cue: &CueMix) -> Vec<(TrackId, CueSend)> {
        self.project
            .tracks
            .iter()
            .filter(|t| feeds_cues(t.kind))
            .map(|t| (t.id, self.main_mix_send(t, cue)))
            .collect()
    }

    fn cue_or_err(&self, cue: CueId) -> Result<&CueMix> {
        self.project
            .cue(cue)
            .ok_or_else(|| SessionError::Other("no such cue mix".into()))
    }

    /// A new cue mix (a copy of the main mix on the next free outputs),
    /// shown in the mixer.
    pub fn new_cue_mix(&mut self) -> Result<CueId> {
        if self.project.cue_mixes.len() >= MAX_CUE_MIXES {
            return Err(SessionError::Other(format!(
                "a project has at most {MAX_CUE_MIXES} cue mixes"
            )));
        }
        let n = (1..)
            .find(|n| {
                let name = format!("Cue {n}");
                !self.project.cue_mixes.iter().any(|c| c.name == name)
            })
            .unwrap_or(1);
        let layout = ChannelLayout::Stereo;
        let first = self.project.free_cue_outputs(layout, None);
        let id: CueId = self.project.ids.allocate();
        let color = CUE_COLORS[self.project.cue_mixes.len() % CUE_COLORS.len()];
        let cue = CueMix::new(id, format!("Cue {n}"), color, first, layout);
        let sends = self.shown_main_mix(&cue);
        self.edit(Command::AddCueMix {
            cue: Box::new(cue),
            index: None,
            sends,
        })?;
        self.mixer_cue = Some(id);
        self.say_where(id);
        Ok(id)
    }

    /// Where a cue mix plays, and whether the device has those outputs.
    fn say_where(&mut self, cue: CueId) {
        let Some(c) = self.project.cue(cue) else {
            return;
        };
        let msg = match self.stream_info() {
            Some(info) if c.outputs().end > info.output_channels => format!(
                "{} plays on {} — the audio device has {} outputs open",
                c.name,
                c.output_label(),
                info.output_channels
            ),
            _ => format!("{} plays on {}", c.name, c.output_label()),
        };
        self.notify(NoticeLevel::Info, msg);
    }

    /// Pre- or post-fader: every send's level moves by its track's fader
    /// (and VCAs) the other way, so the cue mix sounds as it did (a track
    /// whose fader is down keeps its level).
    fn set_cue_tap(&mut self, cue: CueId, tap: SendTap) -> Result<()> {
        let c = self.cue_or_err(cue)?.clone();
        if tap == SendTap::PreFx {
            return Err(SessionError::Other(
                "cue mixes tap before or after the fader".into(),
            ));
        }
        if c.tap == tap {
            return Ok(());
        }
        let to_post = tap == SendTap::PostFader;
        let mut commands = vec![Command::SetCueMix {
            cue: Box::new(CueMix { tap, ..c }),
        }];
        for t in &self.project.tracks {
            let Some(s) = t.cue_send(cue) else { continue };
            let fader = t.volume_db
                + self
                    .project
                    .vca_chain(t)
                    .iter()
                    .map(|v| v.volume_db)
                    .sum::<f32>();
            if fader <= SILENCE_DB || s.level_db <= SILENCE_DB {
                continue;
            }
            let level = if to_post {
                s.level_db - fader
            } else {
                s.level_db + fader
            };
            commands.push(Command::SetCueSend {
                track: t.id,
                cue,
                send: Some(CueSend {
                    level_db: level.clamp(SILENCE_DB, MAX_LEVEL_DB),
                    ..*s
                }),
            });
        }
        self.batch(
            if to_post {
                "Cue Mix Post-Fader"
            } else {
                "Cue Mix Pre-Fader"
            },
            commands,
        )
    }

    /// Every send to `cue` as the main mix has it now — automated and
    /// live levels, not the static faders (one undo step).
    fn copy_main_mix(&mut self, cue: CueId) -> Result<()> {
        let c = self.cue_or_err(cue)?.clone();
        let commands = self
            .shown_main_mix(&c)
            .into_iter()
            .map(|(track, send)| Command::SetCueSend {
                track,
                cue,
                send: Some(send),
            })
            .collect();
        self.batch("Copy Main Mix", commands)
    }

    /// Tracks a command adds join every cue mix: a new one as the main mix
    /// has it (its fader level), a copy (duplicate, preset) with what it
    /// brings to the cue mixes there are.
    pub(crate) fn cue_sends_for_new_tracks(&self, cmd: Command) -> Command {
        if self.project.cue_mixes.is_empty() {
            return cmd;
        }
        match cmd {
            Command::AddTrack { mut track, index } => {
                if feeds_cues(track.kind) {
                    let cues = &self.project.cue_mixes;
                    track
                        .cue_sends
                        .retain(|s| cues.iter().any(|c| c.id == s.cue));
                    for c in cues {
                        if track.cue_send(c.id).is_none() {
                            let send = self.main_mix_send(&track, c);
                            track.cue_sends.push(send);
                        }
                    }
                } else {
                    track.cue_sends.clear();
                }
                Command::AddTrack { track, index }
            }
            Command::Batch { label, commands } => Command::Batch {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| self.cue_sends_for_new_tracks(c))
                    .collect(),
            },
            other => other,
        }
    }

    /// The outputs a cue mix can move to (pairs and single outputs up to
    /// the device's, or 16), checked where it is.
    pub fn cue_output_choices(&self, cue: CueId) -> Vec<InputChoice> {
        let Some(c) = self.project.cue(cue) else {
            return Vec::new();
        };
        let outputs = self
            .stream_info()
            .map_or(16, |i| i.output_channels)
            .max(c.outputs().end)
            .max(2);
        let used = self.project.used_outputs(Some(cue));
        let mut out = Vec::new();
        for (layout, step) in [(ChannelLayout::Stereo, 2u16), (ChannelLayout::Mono, 1)] {
            let mut first = 0u16;
            let mut start = true;
            while first + step <= outputs {
                let busy = (first..first + step).any(|ch| used.contains(&ch));
                let label = faderframe_project::cue::output_label(first, layout);
                out.push(InputChoice {
                    label: if busy {
                        format!("{label} (in use)")
                    } else {
                        label
                    },
                    action: Action::Cue(CueOp::SetOutput {
                        cue,
                        first_channel: first,
                        layout,
                    }),
                    checked: c.first_channel == first && c.layout == layout,
                    group_start: start,
                });
                start = false;
                first += step;
            }
        }
        out
    }

    /// The tracks feeding cue mixes, in the mixer's order.
    pub fn cue_tracks(&self) -> Vec<TrackId> {
        self.project
            .folder_order()
            .into_iter()
            .filter(|t| feeds_cues(t.kind))
            .map(|t| t.id)
            .collect()
    }
}
