//! Plugins that stop working, and sandboxed plugins' state.
//!
//! A plugin whose processing fails — in a sandbox: its helper process
//! crashed or stopped answering — is bypassed by the engine; the user gets
//! one notice and can start it again ([`Action::ReloadPlugin`]): its
//! instance is dropped and created anew from its slot. So that a reload
//! after a crash restores recent settings, the state of sandboxed plugins
//! that report changes is taken into their slots every few seconds.
//!
//! [`Action::ReloadPlugin`]: crate::Action::ReloadPlugin

use crate::{NoticeLevel, Result, Session};
use faderframe_core::PluginInstanceId;
use faderframe_project::Impact;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How often changed state of sandboxed plugins is taken into the slots.
const CAPTURE_EVERY: Duration = Duration::from_secs(5);

pub(crate) struct PluginCare {
    /// Failures already reported.
    noticed: HashSet<PluginInstanceId>,
    /// Sandboxed plugins with state not yet in their slots.
    dirty: HashSet<PluginInstanceId>,
    last_capture: Instant,
}

impl Default for PluginCare {
    fn default() -> Self {
        Self {
            noticed: HashSet::new(),
            dirty: HashSet::new(),
            last_capture: Instant::now(),
        }
    }
}

impl Session {
    fn slot_name(&self, id: PluginInstanceId) -> String {
        self.plugin_owner(id)
            .map_or_else(|| "A plugin".into(), |(_, s)| s.plugin.name.clone())
    }

    /// Per tick: report new failures, keep sandboxed plugins' state.
    pub(crate) fn care_for_plugins(&mut self) {
        for id in self.engine.failed_plugins() {
            if self.plugin_care.noticed.insert(id) {
                let name = self.slot_name(id);
                let text = if self.engine.plugin_sandboxed(id) {
                    format!(
                        "{name} stopped working — its process crashed or stopped answering. \
                         The track plays on without it; Reload Plugin in its menu starts it again."
                    )
                } else {
                    format!(
                        "{name} failed while processing and is bypassed; Reload Plugin in its menu starts it again."
                    )
                };
                self.notify(NoticeLevel::Warning, text);
                self.revision += 1;
            }
        }
        for id in self.engine.take_dirty_plugins() {
            if self.engine.plugin_sandboxed(id) {
                self.plugin_care.dirty.insert(id);
            }
        }
        if !self.plugin_care.dirty.is_empty()
            && self.plugin_care.last_capture.elapsed() >= CAPTURE_EVERY
        {
            let dirty: Vec<_> = self.plugin_care.dirty.drain().collect();
            for id in dirty {
                if !self.engine.plugin_failed(id) {
                    self.capture_plugin_state(id);
                }
            }
            self.plugin_care.last_capture = Instant::now();
        }
    }

    /// The plugin stopped working (crashed, hung or failed processing).
    pub fn plugin_failed(&self, id: PluginInstanceId) -> bool {
        self.engine.plugin_failed(id)
    }

    /// The plugin runs in a helper process.
    pub fn plugin_sandboxed(&self, id: PluginInstanceId) -> bool {
        self.engine.plugin_sandboxed(id)
    }

    /// Start plugins again from their slots; the state of those still
    /// working is taken first.
    pub(crate) fn reload_plugins(&mut self, ids: &[PluginInstanceId]) -> Result<()> {
        let mut reloaded = Vec::new();
        for &id in ids {
            if !self.engine.plugin_failed(id) {
                self.capture_plugin_state(id);
            }
            if self.engine.reload_plugin(id) {
                self.plugin_care.noticed.remove(&id);
                self.plugin_care.dirty.remove(&id);
                reloaded.push(id);
            }
        }
        if reloaded.is_empty() {
            return Ok(());
        }
        self.sync(Impact::Graph)?;
        let text = match reloaded.as_slice() {
            [one] => format!("{} started again", self.slot_name(*one)),
            many => format!("{} plugins started again", many.len()),
        };
        self.notify(NoticeLevel::Info, text);
        self.revision += 1;
        Ok(())
    }
}
