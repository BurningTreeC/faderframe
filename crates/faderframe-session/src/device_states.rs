//! A device's settings from its editor window's header: an A/B compare
//! (two complete settings, one heard; [`faderframe_project::device_compare`]),
//! the side heard copied to the other, level-matched listening while
//! comparing, and an undo history of the device's own.
//!
//! **A/B.** Switching sides is one undoable step: the slot's settings
//! become the other side's ([`Command::SetPluginState`]) and the compare
//! records the side heard and keeps the settings left
//! ([`Command::SetDeviceCompare`]). Recorded rather than applied unrecorded
//! so that undo, redo, saving and opening keep the slot and its compare
//! together whatever happens in between; the price is a step per switch.
//! A hosted plugin's live state is taken into its slot first (its own
//! window may have changed it since). Built-ins hold everything in the
//! slot; a state of `None` is how the device was made, and the engine
//! loads that back (`plugins::Hosted::initial`), so a side without a state
//! of its own switches back exactly too.
//!
//! **Level matching** (listening only, never in a render or a meter): while
//! it is on and the transport plays, the RMS level at the device's track
//! output is followed for each side as it is heard (`LEVEL_SECONDS`
//! averaging; quiet passages left out; `SETTLE_SECONDS` after a switch
//! ignored), and side B is heard brought to side A's level with the
//! monitor path's mix-side listening gain ([`Session::listening_gain`]) —
//! which a Mix Intelligence audition, when one runs, keeps for itself.
//!
//! **The device's undo history** is kept while an editor window shows it
//! ([`DeviceOp::Watch`]): its settings (and compare) each time they change
//! outside a gesture — edits from its editor, the generic window, presets,
//! A/B, the project's undo and redo — and, for hosted plugins, after moves
//! in the plugin's own window end (the live state taken into the slot).
//! Undo in the device brings back the settings before as one step of the
//! project's history ("Undo in EQ 1: …"), whatever was done elsewhere
//! since; the project's own undo of a device step is seen as a move back in
//! the device's history, not a new change.

use crate::{Result, Session, SessionError};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_project::device_compare::{DeviceCompare, DeviceSettings, Side};
use faderframe_project::{Command, PluginFormat, PluginSlot};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The most steps a device's history keeps.
pub const MAX_DEVICE_STEPS: usize = 64;
/// A hosted plugin's window is taken as done this long after its last
/// move (plugins that send no gesture ends).
pub const EDITOR_IDLE: Duration = Duration::from_millis(400);
/// After a switch (or play starting), levels are not measured this long
/// (the switch's crossfade, tails of the other side).
pub const SETTLE_SECONDS: f32 = 0.4;
/// Each side's level follows what it plays over about this long.
pub const LEVEL_SECONDS: f32 = 3.0;
/// Each side is measured this long before matching applies.
pub const MATCH_AFTER_SECONDS: f32 = 1.0;
/// Quieter than this (dBFS RMS) is not measured.
const QUIET_DB: f32 = -60.0;
/// The most a side is brought up or down (dB).
const MAX_MATCH_DB: f32 = 24.0;

/// What a device's header asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceOp {
    /// Hear the other side (A ⇄ B).
    Switch(PluginInstanceId),
    /// Hear `side`.
    Hear {
        plugin: PluginInstanceId,
        side: Side,
    },
    /// The side heard copied to the other (A → B, or B → A).
    CopyToOther(PluginInstanceId),
    /// Hear side B at side A's level while comparing (listening only).
    MatchLevels {
        plugin: PluginInstanceId,
        on: bool,
    },
    /// The device's own undo and redo.
    Undo(PluginInstanceId),
    Redo(PluginInstanceId),
    /// An editor window shows the device (`on`) or closed: its history is
    /// kept while one does.
    Watch {
        plugin: PluginInstanceId,
        on: bool,
    },
}

/// What a device's header shows.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceStatus {
    /// The side heard.
    pub heard: Side,
    /// A B exists (else the first switch makes one, a copy of A).
    pub compared: bool,
    /// The sides' settings differ.
    pub differ: bool,
    /// What the device's undo and redo would do (their labels).
    pub undo: Option<String>,
    pub redo: Option<String>,
    /// Level matching is on for this device.
    pub matching: bool,
    /// The levels measured and the gain matching applies now.
    pub levels: Option<LevelMatch>,
}

/// Level matching's measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelMatch {
    /// Each side's RMS level at the track's output (dBFS), once measured.
    pub a_db: Option<f32>,
    pub b_db: Option<f32>,
    /// The listening gain applied now (dB; side B only).
    pub gain_db: f32,
}

/// A device's settings at one point of its history.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    settings: DeviceSettings,
    compare: Option<DeviceCompare>,
    /// What led to it.
    label: String,
}

impl Snapshot {
    fn same(&self, other: &Snapshot) -> bool {
        self.settings == other.settings && self.compare == other.compare
    }

    /// Whether it is what `slot` and `compare` hold now (nothing copied).
    fn is(&self, slot: &PluginSlot, compare: Option<&DeviceCompare>) -> bool {
        self.settings.state == slot.state
            && self.settings.parameters == slot.parameters
            && self.compare.as_ref() == compare
    }
}

/// A device's own history.
struct DeviceHistory {
    states: Vec<Snapshot>,
    /// The present (an index into `states`).
    at: usize,
    /// Editor windows showing it.
    windows: u32,
    /// Its own window moved something (hosted plugins): taken in once it
    /// has been quiet for [`EDITOR_IDLE`], or at once when a gesture ended.
    touched: Option<Instant>,
}

/// Level matching for one device.
struct Matching {
    plugin: PluginInstanceId,
    /// Mean power heard on each side (A, B) and for how long.
    power: [f64; 2],
    seconds: [f32; 2],
    /// Time since the side heard changed (or play started).
    settled: f32,
    last_side: Side,
    /// The gain this sets on the listening path (dB).
    gain_db: f32,
}

/// The session's part.
#[derive(Default)]
pub(crate) struct DeviceStates {
    histories: HashMap<PluginInstanceId, DeviceHistory>,
    /// The project's history as last looked at (current node, revision).
    seen: (u64, u64),
    matching: Option<Matching>,
}

impl Session {
    /// A device the header's A/B and undo work on: a track's or an album
    /// song's (a clip effect's own window has its card's settings).
    fn compared_owner(&self, plugin: PluginInstanceId) -> Option<(TrackId, &PluginSlot)> {
        if self.clip_fx_slot(plugin).is_some() {
            return None;
        }
        self.plugin_owner(plugin)
    }

    /// [`Self::compared_owner`]'s settings.
    fn compared_slot(&self, plugin: PluginInstanceId) -> Option<(TrackId, DeviceSettings)> {
        self.compared_owner(plugin)
            .map(|(t, s)| (t, DeviceSettings::of(s)))
    }

    /// Take a hosted plugin's live state into its slot (its own window may
    /// have changed it) — a step of its history when it did; built-ins
    /// keep everything in the slot already.
    fn take_live_settings(&mut self, plugin: PluginInstanceId) {
        let hosted = self
            .plugin_owner(plugin)
            .is_some_and(|(_, s)| s.plugin.format != PluginFormat::Builtin);
        if hosted && self.clip_fx_slot(plugin).is_none() {
            self.capture_plugin_state(plugin);
            if let Some(h) = self.device_states.histories.get_mut(&plugin) {
                h.touched = None;
            }
            self.note_device(plugin, Some("Plugin Window Edit".into()));
        }
    }

    fn device_snapshot(&self, plugin: PluginInstanceId, label: String) -> Option<Snapshot> {
        let (_, settings) = self.compared_slot(plugin)?;
        Some(Snapshot {
            settings,
            compare: self.project.device_compare(plugin).cloned(),
            label,
        })
    }

    /// What a device's header shows (`None`: a device it does not work
    /// on).
    pub fn device_status(&self, plugin: PluginInstanceId) -> Option<DeviceStatus> {
        let (_, slot) = self.compared_owner(plugin)?;
        let compare = self.project.device_compare(plugin);
        let h = self.device_states.histories.get(&plugin);
        let undo = h
            .filter(|h| h.at > 0)
            .and_then(|h| h.states.get(h.at))
            .map(|s| s.label.clone());
        let redo = h
            .and_then(|h| h.states.get(h.at + 1))
            .map(|s| s.label.clone());
        let matching = self
            .device_states
            .matching
            .as_ref()
            .filter(|m| m.plugin == plugin);
        let level = |m: &Matching, i: usize| {
            (m.seconds[i] >= MATCH_AFTER_SECONDS)
                .then(|| 10.0 * m.power[i].max(1e-12).log10() as f32)
        };
        Some(DeviceStatus {
            heard: compare.map_or(Side::A, |c| c.heard),
            compared: compare.is_some(),
            differ: compare.is_some_and(|c| {
                c.other.state != slot.state || c.other.parameters != slot.parameters
            }),
            undo,
            redo,
            matching: matching.is_some(),
            levels: matching.map(|m| LevelMatch {
                a_db: level(m, 0),
                b_db: level(m, 1),
                gain_db: m.gain_db,
            }),
        })
    }

    pub(crate) fn device_op(&mut self, op: DeviceOp) -> Result<()> {
        match op {
            DeviceOp::Switch(plugin) => {
                let heard = self
                    .project
                    .device_compare(plugin)
                    .map_or(Side::A, |c| c.heard);
                self.hear_side(plugin, heard.other())?;
            }
            DeviceOp::Hear { plugin, side } => self.hear_side(plugin, side)?,
            DeviceOp::CopyToOther(plugin) => self.copy_to_other(plugin)?,
            DeviceOp::MatchLevels { plugin, on } => self.match_levels(plugin, on)?,
            DeviceOp::Undo(plugin) => self.device_undo(plugin, true)?,
            DeviceOp::Redo(plugin) => self.device_undo(plugin, false)?,
            DeviceOp::Watch { plugin, on } => self.watch_device(plugin, on),
        }
        self.revision += 1;
        Ok(())
    }

    fn compare_target(&self, plugin: PluginInstanceId) -> Result<(TrackId, String)> {
        let (track, _) = self.compared_owner(plugin).ok_or_else(|| {
            SessionError::Other(
                "this device has no A/B compare (a clip effect's settings are its card's)".into(),
            )
        })?;
        let name = self
            .device_name(plugin)
            .unwrap_or_else(|| "the device".into());
        Ok((track, name))
    }

    /// Hear `side` of a device (one step; nothing when it is heard).
    fn hear_side(&mut self, plugin: PluginInstanceId, side: Side) -> Result<()> {
        let (track, name) = self.compare_target(plugin)?;
        self.take_live_settings(plugin);
        let compare = self.project.device_compare(plugin).cloned();
        let heard = compare.as_ref().map_or(Side::A, |c| c.heard);
        if heard == side {
            return Ok(());
        }
        let Some((_, now)) = self.compared_slot(plugin) else {
            return Ok(());
        };
        let mut commands = Vec::new();
        // The first switch: B starts as a copy of A.
        if let Some(c) = &compare {
            commands.push(c.other.command(track, plugin));
        }
        commands.push(Command::SetDeviceCompare {
            plugin,
            compare: Some(DeviceCompare {
                heard: side,
                other: now,
            }),
        });
        self.batch(&format!("Compare {} ({name})", side.label()), commands)?;
        self.note_device(plugin, None);
        if let Some(m) = self
            .device_states
            .matching
            .as_mut()
            .filter(|m| m.plugin == plugin)
        {
            m.settled = 0.0;
        }
        Ok(())
    }

    /// The side heard copied to the other (one step).
    fn copy_to_other(&mut self, plugin: PluginInstanceId) -> Result<()> {
        let (_, name) = self.compare_target(plugin)?;
        self.take_live_settings(plugin);
        let Some((_, now)) = self.compared_slot(plugin) else {
            return Ok(());
        };
        let heard = self
            .project
            .device_compare(plugin)
            .map_or(Side::A, |c| c.heard);
        let compare = DeviceCompare { heard, other: now };
        if self.project.device_compare(plugin) == Some(&compare) {
            return Ok(());
        }
        self.batch(
            &format!(
                "Copy {} to {} ({name})",
                heard.label(),
                heard.other().label()
            ),
            vec![Command::SetDeviceCompare {
                plugin,
                compare: Some(compare),
            }],
        )?;
        self.note_device(plugin, None);
        Ok(())
    }

    fn match_levels(&mut self, plugin: PluginInstanceId, on: bool) -> Result<()> {
        if on {
            self.compare_target(plugin)?;
            let side = self
                .project
                .device_compare(plugin)
                .map_or(Side::A, |c| c.heard);
            self.device_states.matching = Some(Matching {
                plugin,
                power: [0.0; 2],
                seconds: [0.0; 2],
                settled: 0.0,
                last_side: side,
                gain_db: 0.0,
            });
        } else if self
            .device_states
            .matching
            .as_ref()
            .is_some_and(|m| m.plugin == plugin)
        {
            self.stop_matching();
        }
        Ok(())
    }

    /// Level matching ends: its gain goes.
    fn stop_matching(&mut self) {
        if let Some(m) = self.device_states.matching.take()
            && m.gain_db != 0.0
            && !self.mix.auditioning()
        {
            self.set_listening_mix_gain(0.0);
        }
    }

    fn watch_device(&mut self, plugin: PluginInstanceId, on: bool) {
        if on {
            if let Some(h) = self.device_states.histories.get_mut(&plugin) {
                h.windows += 1;
                return;
            }
            self.take_live_settings(plugin);
            let Some(first) = self.device_snapshot(plugin, String::new()) else {
                return;
            };
            self.device_states.histories.insert(
                plugin,
                DeviceHistory {
                    states: vec![first],
                    at: 0,
                    windows: 1,
                    touched: None,
                },
            );
            self.device_states.seen = (self.history.current(), self.history.revision());
        } else if let Some(h) = self.device_states.histories.get_mut(&plugin) {
            h.windows = h.windows.saturating_sub(1);
            if h.windows == 0 {
                self.device_states.histories.remove(&plugin);
                if self
                    .device_states
                    .matching
                    .as_ref()
                    .is_some_and(|m| m.plugin == plugin)
                {
                    self.stop_matching();
                }
            }
        }
    }

    /// Whether a device's history is kept (an editor window shows it).
    pub fn device_watched(&self, plugin: PluginInstanceId) -> bool {
        self.device_states.histories.contains_key(&plugin)
    }

    /// Look at a watched device's settings now: a change becomes a step of
    /// its history (labelled `label`, else by the project's last step),
    /// the settings before or after it are a move back or on.
    fn note_device(&mut self, plugin: PluginInstanceId, label: Option<String>) {
        let (Some(h), Some((_, slot))) = (
            self.device_states.histories.get(&plugin),
            self.compared_owner(plugin),
        ) else {
            return;
        };
        // Unchanged (the usual case): nothing copied.
        if h.states[h.at].is(slot, self.project.device_compare(plugin)) {
            return;
        }
        let label = label
            .or_else(|| self.history.undo_label().map(str::to_string))
            .unwrap_or_else(|| "Change".into());
        let Some(now) = self.device_snapshot(plugin, label) else {
            return;
        };
        let Some(h) = self.device_states.histories.get_mut(&plugin) else {
            return;
        };
        if h.states[h.at].same(&now) {
            return;
        }
        if h.at > 0 && h.states[h.at - 1].same(&now) {
            h.at -= 1;
            return;
        }
        if h.states.get(h.at + 1).is_some_and(|s| s.same(&now)) {
            h.at += 1;
            return;
        }
        h.states.truncate(h.at + 1);
        h.states.push(now);
        if h.states.len() > MAX_DEVICE_STEPS {
            h.states.remove(0);
        }
        h.at = h.states.len() - 1;
    }

    /// Undo (or redo) in a device: its settings before (after) as one step
    /// of the project's history.
    fn device_undo(&mut self, plugin: PluginInstanceId, back: bool) -> Result<()> {
        let (track, name) = self.compare_target(plugin)?;
        if !self.device_states.histories.contains_key(&plugin) {
            return Err(SessionError::Other(format!(
                "{name}'s own undo works while its window is open"
            )));
        }
        // What its own window did is a step first.
        self.take_live_settings(plugin);
        let Some(h) = self.device_states.histories.get(&plugin) else {
            return Ok(());
        };
        let (target, what) = if back {
            if h.at == 0 {
                return Err(SessionError::Other(format!("nothing to undo in {name}")));
            }
            (h.states[h.at - 1].clone(), h.states[h.at].label.clone())
        } else {
            match h.states.get(h.at + 1) {
                Some(s) => (s.clone(), s.label.clone()),
                None => return Err(SessionError::Other(format!("nothing to redo in {name}"))),
            }
        };
        let mut commands = Vec::new();
        if self.compared_slot(plugin).map(|(_, s)| s) != Some(target.settings.clone()) {
            commands.push(target.settings.command(track, plugin));
        }
        if self.project.device_compare(plugin) != target.compare.as_ref() {
            commands.push(Command::SetDeviceCompare {
                plugin,
                compare: target.compare.clone(),
            });
        }
        if let Some(h) = self.device_states.histories.get_mut(&plugin) {
            h.at = if back { h.at - 1 } else { h.at + 1 };
        }
        let label = format!("{} in {name}: {what}", if back { "Undo" } else { "Redo" });
        if let Err(e) = self.batch(&label, commands) {
            // Nothing changed: the history stays where it was.
            if let Some(h) = self.device_states.histories.get_mut(&plugin) {
                h.at = if back { h.at + 1 } else { h.at - 1 };
            }
            return Err(e);
        }
        self.device_states.seen = (self.history.current(), self.history.revision());
        Ok(())
    }

    /// A hosted plugin's window moved something: take its state in (as a
    /// step of the device's history) once the moves end.
    pub(crate) fn note_device_editor_edits(
        &mut self,
        edits: &[(PluginInstanceId, faderframe_plugin_host::EditorEdit)],
    ) {
        use faderframe_plugin_host::EditorEdit as E;
        for (plugin, edit) in edits {
            if let Some(h) = self.device_states.histories.get_mut(plugin) {
                let now = Instant::now();
                h.touched = Some(match edit {
                    // Done: taken in at the next look.
                    E::End(_) => now.checked_sub(EDITOR_IDLE).unwrap_or(now),
                    E::Begin(_) | E::Value(..) => now,
                });
            }
        }
    }

    /// After a change of the project (its sync) and in the tick: watched
    /// devices' histories follow what the project's history did (not
    /// inside a gesture: a drag is one step once it ends).
    pub(crate) fn follow_device_histories(&mut self) {
        if self.device_states.histories.is_empty() || self.history.in_gesture() {
            return;
        }
        let now = (self.history.current(), self.history.revision());
        if now == self.device_states.seen {
            return;
        }
        self.device_states.seen = now;
        let plugins: Vec<PluginInstanceId> = self.device_states.histories.keys().copied().collect();
        for plugin in plugins {
            self.note_device(plugin, None);
        }
    }

    /// The tick's part: watched devices' histories follow the project's
    /// (a gesture ended) and their own windows; level matching measures
    /// and sets its gain.
    pub(crate) fn tick_device_states(&mut self, dt: f32) {
        self.follow_device_histories();
        if !self.history.in_gesture() {
            let due: Vec<PluginInstanceId> = self
                .device_states
                .histories
                .iter()
                .filter(|(_, h)| h.touched.is_some_and(|t| t.elapsed() >= EDITOR_IDLE))
                .map(|(p, _)| *p)
                .collect();
            for plugin in due {
                self.take_live_settings(plugin);
            }
        }
        self.tick_level_match(dt);
    }

    fn tick_level_match(&mut self, dt: f32) {
        let Some(plugin) = self.device_states.matching.as_ref().map(|m| m.plugin) else {
            return;
        };
        let Some((track, _)) = self.compared_slot(plugin) else {
            self.stop_matching();
            return;
        };
        let side = self
            .project
            .device_compare(plugin)
            .map_or(Side::A, |c| c.heard);
        let playing = self.transport.playing;
        let meter = self.meter(track);
        let shown = meter.shown();
        let power = shown
            .iter()
            .map(|c| f64::from(10f32.powf(c.rms_db / 10.0)))
            .sum::<f64>()
            / shown.len().max(1) as f64;
        let quiet = shown.iter().all(|c| c.rms_db < QUIET_DB);
        let auditioning = self.mix.auditioning();
        let Some(m) = self.device_states.matching.as_mut() else {
            return;
        };
        if side != m.last_side || !playing {
            m.last_side = side;
            m.settled = 0.0;
        }
        m.settled += dt;
        if playing && m.settled >= SETTLE_SECONDS && !quiet {
            let i = usize::from(side == Side::B);
            let a = (f64::from(dt) / f64::from(LEVEL_SECONDS)).min(1.0);
            m.power[i] = if m.seconds[i] == 0.0 {
                power
            } else {
                m.power[i] + (power - m.power[i]) * a
            };
            m.seconds[i] += dt;
        }
        let measured = m.seconds.iter().all(|s| *s >= MATCH_AFTER_SECONDS);
        let gain = if side == Side::B && measured {
            let db = 10.0 * (m.power[0].max(1e-12) / m.power[1].max(1e-12)).log10();
            (db as f32).clamp(-MAX_MATCH_DB, MAX_MATCH_DB)
        } else {
            0.0
        };
        // Whole hundredths of a dB: no new gain every tick.
        let gain = (gain * 100.0).round() / 100.0;
        if gain != m.gain_db && !auditioning {
            m.gain_db = gain;
            self.set_listening_mix_gain(gain);
            self.revision += 1;
        }
    }

    /// A project replaced: devices' histories and level matching go.
    pub(crate) fn device_states_project_replaced(&mut self) {
        self.stop_matching();
        self.device_states.histories.clear();
    }
}
