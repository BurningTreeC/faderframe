//! Automation in the session: what can be automated, lanes (shown,
//! modes), the values controls display while automation plays, and
//! writing automation (Touch, Latch, Write) from control moves.
//!
//! Writing: while the transport plays, moving a control whose lane is in
//! Touch or Latch mode — or any control of a lane in Write mode — records
//! points at the playhead. The lane stops driving its parameter while it is
//! being written (the engine skips suspended lanes), so what you hear is the
//! control. Touch ends when the gesture ends (the curve takes over again),
//! Latch and Write when playback stops. The recorded points are thinned and
//! replace the curve over the written range in one undo step.

use crate::{NoticeLevel, Result, Session};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
    thin,
};
use faderframe_core::{AutomationLaneId, FaderLaw, TrackId};
use faderframe_plugin_host::ParameterUnit;
use faderframe_project::{Command, Track, TrackKind};
use faderframe_timeline::MusicalTime;
use std::collections::HashMap;

/// How a parameter's values map to a lane's height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    /// dB on the console fader law (volume, send levels).
    Gain,
    /// -1 (left) … +1 (right).
    Pan,
    /// Off/on (mute, bypass, stepped switches).
    Toggle,
    /// Linear between min and max.
    Linear,
    /// Logarithmic between min and max (frequencies, times).
    Log,
}

/// One automatable parameter of a track.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationParam {
    pub target: AutomationTarget,
    pub name: String,
    /// Plain-unit range and default.
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub kind: ParamKind,
    /// Unit suffix for display ("dB", "ms", "Hz", "%").
    pub unit: &'static str,
}

impl AutomationParam {
    /// Plain value → lane position 0..=1.
    pub fn to_normal(&self, v: f64) -> f64 {
        match self.kind {
            ParamKind::Gain => FaderLaw::console().db_to_position(v as f32) as f64,
            ParamKind::Pan => (v.clamp(-1.0, 1.0) + 1.0) * 0.5,
            ParamKind::Toggle => {
                if v >= (self.min + self.max) * 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            ParamKind::Linear => {
                ((v - self.min) / (self.max - self.min).max(1e-12)).clamp(0.0, 1.0)
            }
            ParamKind::Log => {
                let (a, b) = (self.min.max(1e-9).ln(), self.max.max(1e-9).ln());
                ((v.max(1e-9).ln() - a) / (b - a).max(1e-12)).clamp(0.0, 1.0)
            }
        }
    }

    /// Lane position 0..=1 → plain value.
    pub fn from_normal(&self, n: f64) -> f64 {
        let n = n.clamp(0.0, 1.0);
        match self.kind {
            ParamKind::Gain => FaderLaw::console().position_to_db(n as f32) as f64,
            ParamKind::Pan => n * 2.0 - 1.0,
            ParamKind::Toggle => {
                if n >= 0.5 {
                    self.max
                } else {
                    self.min
                }
            }
            ParamKind::Linear => self.min + (self.max - self.min) * n,
            ParamKind::Log => {
                let (a, b) = (self.min.max(1e-9).ln(), self.max.max(1e-9).ln());
                (a + (b - a) * n).exp()
            }
        }
    }

    /// "−6.0 dB", "L25", "On", "350 ms" …
    pub fn format(&self, v: f64) -> String {
        match self.kind {
            ParamKind::Gain => format!("{} dB", faderframe_core::gain::format_db(v as f32)),
            ParamKind::Pan => faderframe_core::pan::format_pan(v as f32),
            ParamKind::Toggle => {
                if self.to_normal(v) >= 0.5 {
                    "On".into()
                } else {
                    "Off".into()
                }
            }
            ParamKind::Linear | ParamKind::Log => {
                if self.unit == "%" {
                    // Percent parameters are fractions.
                    format!("{:.0} %", v * 100.0)
                } else if self.unit.is_empty() {
                    format!("{v:.2}")
                } else {
                    format!("{v:.1} {}", self.unit)
                }
            }
        }
    }

    /// New points snap to steps for toggles.
    pub fn shape(&self) -> CurveShape {
        if self.kind == ParamKind::Toggle {
            CurveShape::Step
        } else {
            CurveShape::Linear
        }
    }
}

/// A lane being written.
#[derive(Clone, Debug)]
pub(crate) struct WriteState {
    track: TrackId,
    mode: AutomationMode,
    start: MusicalTime,
    points: Vec<AutomationPoint>,
}

#[derive(Debug, Default)]
pub(crate) struct AutomationWriter {
    writing: HashMap<AutomationLaneId, WriteState>,
    /// Lanes written from a plugin's own editor and when it last moved
    /// them (Touch ends at the gesture's end, or after a pause for plugins
    /// that report no gestures).
    plugin_touch: HashMap<AutomationLaneId, std::time::Instant>,
}

/// A plugin editor's Touch ends this long after its last move when the
/// plugin reports no gesture end.
const PLUGIN_TOUCH_IDLE: std::time::Duration = std::time::Duration::from_millis(750);

fn unit_label(u: ParameterUnit) -> &'static str {
    match u {
        ParameterUnit::None => "",
        ParameterUnit::Decibels => "dB",
        ParameterUnit::Milliseconds => "ms",
        ParameterUnit::Hertz => "Hz",
        ParameterUnit::Percent => "%",
        ParameterUnit::Samples => "smp",
    }
}

impl Session {
    // --- what can be automated -------------------------------------------------

    /// Every automatable parameter of `track`: volume, pan, mute, sends,
    /// and the automatable parameters and bypass of its instrument and
    /// inserts.
    pub fn automatable_parameters(&self, track: TrackId) -> Vec<AutomationParam> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // VCAs automate their fader and mute.
        let vca = t.kind == TrackKind::Vca;
        if t.kind.has_audio() || vca {
            out.push(AutomationParam {
                target: AutomationTarget::TrackVolume,
                name: "Volume".into(),
                min: faderframe_core::gain::SILENCE_DB as f64,
                max: faderframe_project::MAX_LEVEL_DB as f64,
                default: 0.0,
                kind: ParamKind::Gain,
                unit: "dB",
            });
            if !vca {
                out.push(AutomationParam {
                    target: AutomationTarget::TrackPan,
                    name: "Pan".into(),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                    kind: ParamKind::Pan,
                    unit: "",
                });
            }
            out.push(AutomationParam {
                target: AutomationTarget::TrackMute,
                name: "Mute".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                kind: ParamKind::Toggle,
                unit: "",
            });
        }
        for s in &t.sends {
            let dst = self
                .project
                .track(s.target)
                .map_or("?", |d| d.name.as_str());
            out.push(AutomationParam {
                target: AutomationTarget::SendLevel(s.id),
                name: format!("Send → {dst}"),
                min: faderframe_core::gain::SILENCE_DB as f64,
                max: faderframe_project::MAX_LEVEL_DB as f64,
                default: 0.0,
                kind: ParamKind::Gain,
                unit: "dB",
            });
        }
        for slot in t.instrument.iter().chain(t.inserts.iter()) {
            let Some(params) = self.engine.plugin_parameters(slot.id) else {
                continue;
            };
            for p in params.iter().filter(|p| p.automatable) {
                let kind = if p.stepped && p.max - p.min <= 1.0 {
                    ParamKind::Toggle
                } else if matches!(p.unit, ParameterUnit::Hertz | ParameterUnit::Milliseconds)
                    && p.min > 0.0
                {
                    ParamKind::Log
                } else {
                    ParamKind::Linear
                };
                out.push(AutomationParam {
                    target: AutomationTarget::PluginParameter {
                        plugin: slot.id,
                        parameter: p.id,
                    },
                    name: format!("{}: {}", slot.plugin.name, p.name),
                    min: p.min,
                    max: p.max,
                    default: p.default,
                    kind,
                    unit: unit_label(p.unit),
                });
            }
            out.push(AutomationParam {
                target: AutomationTarget::PluginBypass(slot.id),
                name: format!("{}: Bypass", slot.plugin.name),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                kind: ParamKind::Toggle,
                unit: "",
            });
        }
        out
    }

    pub fn automation_param(
        &self,
        track: TrackId,
        target: AutomationTarget,
    ) -> Option<AutomationParam> {
        self.automatable_parameters(track)
            .into_iter()
            .find(|p| p.target == target)
    }

    /// The static (non-automated) value of a parameter.
    pub fn static_value(&self, track: TrackId, target: AutomationTarget) -> Option<f64> {
        let t = self.project.track(track)?;
        Some(match target {
            AutomationTarget::TrackVolume => t.volume_db as f64,
            AutomationTarget::TrackPan => t.pan as f64,
            AutomationTarget::TrackMute => f64::from(u8::from(t.mute)),
            AutomationTarget::SendLevel(s) => t.send(s)?.level_db as f64,
            AutomationTarget::PluginParameter { plugin, parameter } => {
                let slot = t
                    .instrument
                    .iter()
                    .chain(t.inserts.iter())
                    .find(|s| s.id == plugin)?;
                match slot.parameters.iter().find(|p| p.id == parameter) {
                    Some(p) => p.value,
                    None => {
                        self.engine
                            .plugin_parameters(plugin)?
                            .iter()
                            .find(|p| p.id == parameter)?
                            .default
                    }
                }
            }
            AutomationTarget::PluginBypass(plugin) => {
                let slot = t
                    .instrument
                    .iter()
                    .chain(t.inserts.iter())
                    .find(|s| s.id == plugin)?;
                f64::from(u8::from(slot.bypass))
            }
        })
    }

    fn driving_lane<'t>(
        &self,
        t: &'t Track,
        target: AutomationTarget,
    ) -> Option<&'t AutomationLane> {
        t.automation.lane(target).filter(|l| {
            l.mode != AutomationMode::Off
                && !l.curve.is_empty()
                && !self.automation_writer.writing.contains_key(&l.id)
        })
    }

    /// What a control shows: the automated value at the playhead while a
    /// lane drives the parameter, else its static value.
    pub fn display_value(&self, track: TrackId, target: AutomationTarget) -> Option<f64> {
        let t = self.project.track(track)?;
        match self.driving_lane(t, target) {
            Some(l) => l.curve.value_at(self.playhead()),
            None => self.static_value(track, target),
        }
    }

    /// Fader level a control shows (automated while a lane drives it).
    pub fn shown_volume_db(&self, t: &Track) -> f32 {
        self.display_value(t.id, AutomationTarget::TrackVolume)
            .map_or(t.volume_db, |v| v as f32)
    }

    pub fn shown_pan(&self, t: &Track) -> f32 {
        self.display_value(t.id, AutomationTarget::TrackPan)
            .map_or(t.pan, |v| v as f32)
    }

    pub fn shown_mute(&self, t: &Track) -> bool {
        self.display_value(t.id, AutomationTarget::TrackMute)
            .map_or(t.mute, |v| v >= 0.5)
    }

    pub fn shown_send_db(&self, t: &Track, send: &faderframe_project::AuxSend) -> f32 {
        self.display_value(t.id, AutomationTarget::SendLevel(send.id))
            .map_or(send.level_db, |v| v as f32)
    }

    /// Is a lane driving this parameter right now (controls show it)?
    pub fn is_automated(&self, track: TrackId, target: AutomationTarget) -> bool {
        self.project
            .track(track)
            .is_some_and(|t| self.driving_lane(t, target).is_some())
    }

    // --- lanes ----------------------------------------------------------------------

    /// Lanes of `track` shown in the arranger, in order.
    pub fn shown_lanes(&self, track: TrackId) -> Vec<&AutomationLane> {
        self.project.track(track).map_or_else(Vec::new, |t| {
            t.automation
                .lanes
                .iter()
                .filter(|l| self.workspace.automation_shown.contains(&l.id))
                .collect()
        })
    }

    pub fn is_lane_writing(&self, lane: AutomationLaneId) -> bool {
        self.automation_writer.writing.contains_key(&lane)
    }

    /// Show the lane for `target` (creating an empty one if needed).
    pub(crate) fn show_automation(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
    ) -> Result<AutomationLaneId> {
        let existing = self
            .project
            .track(track)
            .and_then(|t| t.automation.lane(target))
            .map(|l| l.id);
        let id = match existing {
            Some(id) => id,
            None => {
                let id: AutomationLaneId = self.project.ids.allocate();
                self.edit(Command::AddAutomationLane {
                    track,
                    lane: Box::new(AutomationLane {
                        id,
                        target,
                        curve: AutomationCurve::new(),
                        mode: AutomationMode::Read,
                        visible: true,
                    }),
                })?;
                id
            }
        };
        self.workspace.automation_shown.insert(id);
        self.revision += 1;
        Ok(id)
    }

    pub(crate) fn hide_automation(&mut self, lane: AutomationLaneId) {
        self.workspace.automation_shown.remove(&lane);
        self.revision += 1;
    }

    /// Show/hide the most useful lane of a track (volume, or the first
    /// existing lane): the header's automation button.
    pub(crate) fn toggle_track_automation(&mut self, track: TrackId) -> Result<()> {
        let shown: Vec<AutomationLaneId> = self.shown_lanes(track).iter().map(|l| l.id).collect();
        if !shown.is_empty() {
            for l in shown {
                self.workspace.automation_shown.remove(&l);
            }
            self.revision += 1;
            return Ok(());
        }
        let lanes: Vec<AutomationLaneId> = self
            .project
            .track(track)
            .map(|t| {
                t.automation
                    .lanes
                    .iter()
                    .filter(|l| !l.curve.is_empty())
                    .map(|l| l.id)
                    .collect()
            })
            .unwrap_or_default();
        if lanes.is_empty() {
            let target = self
                .automatable_parameters(track)
                .first()
                .map(|p| p.target)
                .ok_or_else(|| {
                    crate::SessionError::Other("this track has nothing to automate".into())
                })?;
            self.show_automation(track, target)?;
        } else {
            self.workspace.automation_shown.extend(lanes);
            self.revision += 1;
        }
        Ok(())
    }

    pub(crate) fn set_lane_mode(
        &mut self,
        track: TrackId,
        lane: AutomationLaneId,
        mode: AutomationMode,
    ) -> Result<()> {
        let Some(mut l) = self
            .project
            .track(track)
            .and_then(|t| t.automation.lanes.iter().find(|l| l.id == lane))
            .cloned()
        else {
            return Ok(());
        };
        if l.mode == mode {
            return Ok(());
        }
        l.mode = mode;
        self.edit(Command::SetAutomationLane {
            track,
            lane: Box::new(l),
        })
    }

    // --- writing -----------------------------------------------------------------------

    /// The lane (and its mode) a command would write, if writing applies.
    fn write_target(&self, cmd: &Command) -> Option<(TrackId, AutomationTarget, f64)> {
        let (track, target, value) = match cmd {
            Command::SetTrackVolume { track, db } => {
                (*track, AutomationTarget::TrackVolume, *db as f64)
            }
            Command::SetTrackPan { track, pan } => {
                (*track, AutomationTarget::TrackPan, *pan as f64)
            }
            Command::SetTrackMute { track, on } => (
                *track,
                AutomationTarget::TrackMute,
                f64::from(u8::from(*on)),
            ),
            Command::SetSendLevel { track, send, db } => {
                (*track, AutomationTarget::SendLevel(*send), *db as f64)
            }
            Command::SetPluginParameter {
                track,
                plugin,
                parameter,
                value: Some(v),
            } => (
                *track,
                AutomationTarget::PluginParameter {
                    plugin: *plugin,
                    parameter: *parameter,
                },
                *v,
            ),
            _ => return None,
        };
        Some((track, target, value))
    }

    /// Called before every edit: record a point if the edit moves a control
    /// whose lane is being (or should start being) written.
    pub(crate) fn capture_automation(&mut self, cmd: &Command) {
        let Some((track, target, value)) = self.write_target(cmd) else {
            return;
        };
        let written = self.write_point(track, target, value);
        // A click (mute toggle) is not a gesture: Touch ends right away.
        if let Some((_, AutomationMode::Touch)) = written
            && target == AutomationTarget::TrackMute
        {
            self.commit_writes(|_| true);
        }
    }

    /// Record `value` for `target` now if its lane writes (playing, mode
    /// Touch/Latch/Write); returns the lane and its mode.
    fn write_point(
        &mut self,
        track: TrackId,
        target: AutomationTarget,
        value: f64,
    ) -> Option<(AutomationLaneId, AutomationMode)> {
        if !self.transport.playing {
            return None;
        }
        let lane = self
            .project
            .track(track)
            .and_then(|t| t.automation.lane(target))
            .filter(|l| {
                matches!(
                    l.mode,
                    AutomationMode::Touch | AutomationMode::Latch | AutomationMode::Write
                )
            })
            .map(|l| (l.id, l.mode))?;
        let now = self.playhead();
        let started = !self.automation_writer.writing.contains_key(&lane.0);
        let shape = if target == AutomationTarget::TrackMute {
            CurveShape::Step
        } else {
            CurveShape::Linear
        };
        let state = self
            .automation_writer
            .writing
            .entry(lane.0)
            .or_insert(WriteState {
                track,
                mode: lane.1,
                start: now,
                points: Vec::new(),
            });
        state.points.push(AutomationPoint {
            time: now,
            value,
            shape,
        });
        if started {
            self.update_suspended();
        }
        Some(lane)
    }

    /// Parameter moves made in plugins' own editors: written like moves of
    /// FaderFrame's controls (Touch ends with the plugin's gesture).
    pub(crate) fn plugin_editor_edits(
        &mut self,
        edits: Vec<(
            faderframe_core::PluginInstanceId,
            faderframe_plugin_host::EditorEdit,
        )>,
    ) {
        use faderframe_plugin_host::EditorEdit as E;
        for (plugin, edit) in edits {
            let Some(track) = self.plugin_slot(plugin).map(|(t, _)| t.id) else {
                continue;
            };
            let parameter = match edit {
                E::Begin(p) | E::End(p) | E::Value(p, _) => p,
            };
            let target = AutomationTarget::PluginParameter { plugin, parameter };
            match edit {
                E::Value(_, v) => {
                    if let Some((lane, AutomationMode::Touch)) = self.write_point(track, target, v)
                    {
                        self.automation_writer
                            .plugin_touch
                            .insert(lane, std::time::Instant::now());
                    }
                }
                E::End(_) => {
                    let lane = self
                        .project
                        .track(track)
                        .and_then(|t| t.automation.lane(target))
                        .map(|l| l.id);
                    if let Some(lane) = lane
                        && self.automation_writer.plugin_touch.remove(&lane).is_some()
                    {
                        self.commit_lanes(vec![lane]);
                    }
                }
                E::Begin(_) => {}
            }
        }
        // Plugins without gesture events: Touch ends after a pause.
        let idle: Vec<AutomationLaneId> = self
            .automation_writer
            .plugin_touch
            .iter()
            .filter(|(_, t)| t.elapsed() >= PLUGIN_TOUCH_IDLE)
            .map(|(id, _)| *id)
            .collect();
        if !idle.is_empty() {
            for id in &idle {
                self.automation_writer.plugin_touch.remove(id);
            }
            self.commit_lanes(idle);
        }
    }

    /// Playback started: lanes in Write mode write from here.
    pub(crate) fn automation_play_started(&mut self) {
        let now = self.playhead();
        let lanes: Vec<(TrackId, AutomationLaneId, AutomationTarget)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| {
                t.automation
                    .lanes
                    .iter()
                    .filter(|l| l.mode == AutomationMode::Write)
                    .map(move |l| (t.id, l.id, l.target))
            })
            .collect();
        for (track, id, target) in lanes {
            let Some(v) = self.static_value(track, target) else {
                continue;
            };
            self.automation_writer.writing.insert(
                id,
                WriteState {
                    track,
                    mode: AutomationMode::Write,
                    start: now,
                    points: vec![AutomationPoint {
                        time: now,
                        value: v,
                        shape: CurveShape::Linear,
                    }],
                },
            );
        }
        self.update_suspended();
    }

    /// A gesture ended: Touch lanes stop writing.
    pub(crate) fn automation_gesture_ended(&mut self) {
        self.commit_writes(|s| s.mode == AutomationMode::Touch);
    }

    /// Playback stopped: everything stops writing.
    pub(crate) fn automation_play_stopped(&mut self) {
        self.commit_writes(|_| true);
    }

    fn update_suspended(&mut self) {
        let lanes = self.automation_writer.writing.keys().copied().collect();
        self.engine.set_suspended_lanes(lanes);
        if let Err(e) = self.sync(faderframe_project::Impact::Timeline) {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    fn commit_writes(&mut self, which: impl Fn(&WriteState) -> bool) {
        let ids: Vec<AutomationLaneId> = self
            .automation_writer
            .writing
            .iter()
            .filter(|(_, s)| which(s))
            .map(|(id, _)| *id)
            .collect();
        self.commit_lanes(ids);
    }

    /// Stop writing these lanes and store what was written (one undo step).
    fn commit_lanes(&mut self, ids: Vec<AutomationLaneId>) {
        if ids.is_empty() {
            return;
        }
        let end = self.playhead();
        let mut commands = Vec::new();
        for id in ids {
            let Some(state) = self.automation_writer.writing.remove(&id) else {
                continue;
            };
            let Some(param) = self
                .project
                .track(state.track)
                .and_then(|t| t.automation.lanes.iter().find(|l| l.id == id))
                .map(|l| l.target)
                .and_then(|target| self.automation_param(state.track, target))
            else {
                continue;
            };
            let Some(mut lane) = self
                .project
                .track(state.track)
                .and_then(|t| t.automation.lanes.iter().find(|l| l.id == id))
                .cloned()
            else {
                continue;
            };
            let mut points = state.points;
            // Latch/Write hold the last value until the end of the run.
            if state.mode != AutomationMode::Touch
                && let Some(last) = points.last().copied()
                && end > last.time
            {
                points.push(AutomationPoint { time: end, ..last });
            }
            let tolerance = match param.kind {
                ParamKind::Gain => 0.1,
                ParamKind::Toggle => 0.0,
                _ => (param.max - param.min).abs() * 0.002,
            };
            let points = if param.kind == ParamKind::Toggle {
                points
            } else {
                thin(&points, tolerance)
            };
            let to = points.last().map_or(end, |p| p.time.max(state.start));
            lane.curve.replace_range(state.start, to, &points);
            commands.push(Command::SetAutomationLane {
                track: state.track,
                lane: Box::new(lane),
            });
        }
        self.update_suspended();
        if !commands.is_empty()
            && let Err(e) = self.edit(Command::Batch {
                label: "Write Automation".into(),
                commands,
            })
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }
}

/// Tracks that can hold automation lanes in the arranger.
pub fn has_automation(kind: TrackKind) -> bool {
    kind != TrackKind::Midi
}
