//! The plugins' own programs (VST3 program lists) and the precision they
//! process in.
//!
//! Selecting a program changes the plugin's whole state, so it is one undo
//! step like loading a preset: the state from before is taken into the
//! slot, the program is selected, and once the plugin's processor has taken
//! it (its next block, at the latest after a second) the new state goes
//! into the slot as a "Select Program" step whose undo loads the old one.
//! The program itself is not stored: reapplying it on load would overwrite
//! what the user changed after choosing it.

use crate::{Result, Session, SessionError};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_project::{Command, Impact, PluginFormat, TrackKind};
use std::time::{Duration, Instant};

/// How long a selected program may take to reach the processor before its
/// state is recorded anyway (a plugin that is not processing).
const SETTLE_LIMIT: Duration = Duration::from_secs(1);
/// After the processor took the change: a moment for the plugin to finish
/// the block it took it in.
const SETTLE_AFTER: Duration = Duration::from_millis(20);

/// A selected program whose state is not recorded yet.
pub(crate) struct PendingProgram {
    plugin: PluginInstanceId,
    track: TrackId,
    since: Instant,
    taken: Option<Instant>,
}

/// Names like "Program 3" or "ProgramChange 12" only: the slots many
/// plugins list for MIDI program changes.
fn numbered_slots(names: &[String]) -> bool {
    names.iter().all(|n| {
        let word: String = n
            .trim()
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        matches!(
            word.as_str(),
            "" | "program" | "programchange" | "prog" | "pc" | "patch" | "preset" | "midiprogram"
        )
    })
}

impl Session {
    /// The plugin's own programs, by name (empty: it has none, or only
    /// numbered slots for MIDI program changes — not worth a menu).
    pub fn plugin_programs(&self, plugin: PluginInstanceId) -> Vec<String> {
        let names = self.engine.plugin_programs(plugin);
        if numbered_slots(&names) {
            Vec::new()
        } else {
            names
        }
    }

    /// The submenu each of [`Self::plugin_programs`] goes in ("" at the
    /// top of the menu; empty: none are grouped).
    pub fn plugin_program_groups(&self, plugin: PluginInstanceId) -> Vec<String> {
        let groups = self.engine.plugin_program_groups(plugin);
        if groups.len() == self.plugin_programs(plugin).len() {
            groups
        } else {
            Vec::new()
        }
    }

    /// The program the plugin says is selected.
    pub fn plugin_current_program(&self, plugin: PluginInstanceId) -> Option<usize> {
        self.engine.plugin_current_program(plugin)
    }

    pub(crate) fn select_plugin_program(
        &mut self,
        plugin: PluginInstanceId,
        index: usize,
    ) -> Result<()> {
        let track = self
            .plugin_owner(plugin)
            .map(|(t, _)| t)
            .ok_or_else(|| SessionError::Other(format!("no plugin {plugin}")))?;
        // One at a time: an earlier selection is recorded first.
        self.finish_programs(true);
        // The state to go back to.
        self.capture_plugin_state(plugin);
        // Factory delay/reverb settings are voiced for inserts. On an
        // effect return, the sending tracks already carry the dry signal.
        let wet = self.plugin_slot(plugin).and_then(|(track, slot)| {
            if track.kind == TrackKind::Aux && slot.plugin.format == PluginFormat::Builtin {
                faderframe_plugin_host::presets::send_return_mix(&slot.plugin.id)
                    .map(|id| (id, 1.0))
            } else {
                None
            }
        });
        self.engine
            .select_plugin_program(plugin, index, wet.as_slice())
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.pending_programs.push(PendingProgram {
            plugin,
            track,
            since: Instant::now(),
            taken: None,
        });
        self.revision += 1;
        Ok(())
    }

    /// Record the selected programs whose processors have taken them.
    pub(crate) fn tick_programs(&mut self) {
        self.finish_programs(false);
    }

    /// `now`: record every pending selection without waiting.
    fn finish_programs(&mut self, now: bool) {
        let mut done = Vec::new();
        for p in &mut self.pending_programs {
            if !now && p.since.elapsed() < SETTLE_LIMIT {
                if self.engine.plugin_changes_pending(p.plugin) {
                    continue;
                }
                let taken = *p.taken.get_or_insert_with(Instant::now);
                if taken.elapsed() < SETTLE_AFTER {
                    continue;
                }
            }
            done.push((p.plugin, p.track));
        }
        self.pending_programs
            .retain(|p| !done.iter().any(|(id, _)| *id == p.plugin));
        for (plugin, track) in done {
            let Some(state) = self.engine.plugin_state(plugin) else {
                continue;
            };
            let Some(mut parameters) = self.plugin_owner(plugin).map(|(_, s)| s.parameters.clone())
            else {
                continue;
            };
            // Explicit values are applied after the state when it loads:
            // they must be the program's, not what they were before.
            for p in &mut parameters {
                if let Some(v) = self.engine.plugin_parameter_value(plugin, p.id) {
                    p.value = v;
                }
            }
            // The plugin has this state already: nothing to load.
            self.engine.note_plugin_state(plugin, &state);
            let step = Command::Batch {
                label: "Select Program".into(),
                commands: vec![Command::SetPluginState {
                    track,
                    plugin,
                    state: Some(state),
                    parameters,
                }],
            };
            if let Err(e) = self.edit(step) {
                self.notify(crate::NoticeLevel::Error, e.to_string());
            }
        }
    }

    /// Process plugins in 64-bit floating point where they can (VST3 and
    /// CLAP plugins that support it are reactivated).
    pub fn set_plugin_double_precision(&mut self, on: bool) -> Result<()> {
        if self.engine.set_plugin_double_precision(on) {
            self.sync(Impact::Graph)?;
        }
        Ok(())
    }

    pub fn plugin_double_precision(&self) -> bool {
        self.engine.plugin_double_precision()
    }
}

#[cfg(test)]
mod tests {
    use super::numbered_slots;

    #[test]
    fn numbered_slots_are_told_from_named_programs() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(numbered_slots(&names(&["Program 0", "Program 1"])));
        assert!(numbered_slots(&names(&[
            "ProgramChange 1",
            "ProgramChange 128"
        ])));
        assert!(!numbered_slots(&names(&["Soft", "Program 2"])));
        assert!(!numbered_slots(&names(&["Brass 1", "Brass 2"])));
    }
}
