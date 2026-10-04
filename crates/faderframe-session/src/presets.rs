//! Plugin presets: the user's own (`.ffpreset`, any format, kept per plugin
//! under `$XDG_DATA_HOME/faderframe/presets`) and the plugin format's own
//! files (VST3 `.vstpreset` from the standard folders). Loading one is an
//! undoable [`Command::SetPluginState`].

use crate::{Result, Session, SessionError};
use faderframe_core::PluginInstanceId;
use faderframe_project::{Command, PluginRef, SavedParameter};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One preset a plugin can load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPreset {
    pub name: String,
    pub path: PathBuf,
    /// From the plugin format's own folders (not saved by FaderFrame).
    pub factory: bool,
}

/// What a `.ffpreset` holds.
#[derive(Serialize, Deserialize)]
struct PresetFile {
    plugin: PluginRef,
    /// The plugin's complete state (hosted plugins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    /// Parameter values (plugins without a state, e.g. built-ins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parameters: Vec<SavedParameter>,
}

/// Where the user's presets of `plugin` live.
pub fn user_dir(plugin: &PluginRef) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    crate::media::data_dir()
        .join("presets")
        .join(format!("{:?}", plugin.format).to_lowercase())
        .join(safe(&plugin.id))
}

fn file_name(name: &str) -> String {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if "/\\\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!(
        "{}.ffpreset",
        if n.is_empty() { "Preset".into() } else { n }
    )
}

impl Session {
    fn plugin_ref(&self, plugin: PluginInstanceId) -> Result<PluginRef> {
        self.plugin_owner(plugin)
            .map(|(_, s)| s.plugin.clone())
            .ok_or_else(|| SessionError::Other(format!("no plugin {plugin}")))
    }

    /// The plugin's presets: the user's first, then the format's own, each
    /// by name.
    pub fn plugin_presets(&self, plugin: PluginInstanceId) -> Vec<PluginPreset> {
        let Ok(r) = self.plugin_ref(plugin) else {
            return Vec::new();
        };
        let mut user: Vec<PluginPreset> = std::fs::read_dir(user_dir(&r))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ffpreset"))
            .map(|path| PluginPreset {
                name: path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().to_string()),
                path,
                factory: false,
            })
            .collect();
        user.sort_by_key(|p| p.name.to_lowercase());
        let factory = self
            .engine
            .plugin_preset_files(plugin)
            .into_iter()
            .map(|path| PluginPreset {
                name: path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().to_string()),
                path,
                factory: true,
            });
        user.into_iter().chain(factory).collect()
    }

    /// Does the plugin have a sidechain input?
    pub fn plugin_has_sidechain(&self, plugin: PluginInstanceId) -> bool {
        self.engine.plugin_has_sidechain(plugin)
    }

    /// Tracks that can feed the plugin's sidechain (audio after their
    /// inserts, without creating a feedback loop).
    pub fn sidechain_sources(
        &self,
        plugin: PluginInstanceId,
    ) -> Vec<(faderframe_core::TrackId, String)> {
        let Some((track, _)) = self.plugin_slot(plugin) else {
            return Vec::new();
        };
        let p = &self.project;
        p.tracks
            .iter()
            .filter(|t| {
                t.kind.has_audio()
                    && t.kind != faderframe_project::TrackKind::Midi
                    && t.kind != faderframe_project::TrackKind::Master
                    && !p.would_cycle(t.id, track.id)
            })
            .map(|t| (t.id, t.name.clone()))
            .collect()
    }

    /// Save the plugin's current settings as a user preset.
    pub fn save_plugin_preset(&mut self, plugin: PluginInstanceId, name: &str) -> Result<PathBuf> {
        self.capture_plugin_states();
        let (_, slot) = self
            .plugin_owner(plugin)
            .ok_or_else(|| SessionError::Other(format!("no plugin {plugin}")))?;
        let slot = slot.clone();
        let parameters = if slot.state.is_some() {
            slot.parameters.clone()
        } else {
            // No state (built-ins): every parameter's current value.
            let ids: Vec<faderframe_core::ParameterId> = self
                .engine
                .plugin_parameters(plugin)
                .unwrap_or(&[])
                .iter()
                .map(|p| p.id)
                .collect();
            ids.into_iter()
                .filter_map(|id| {
                    self.engine
                        .plugin_parameter_value(plugin, id)
                        .map(|value| SavedParameter { id, value })
                })
                .collect()
        };
        let file = PresetFile {
            plugin: slot.plugin.clone(),
            state: slot.state.clone(),
            parameters,
        };
        let dir = user_dir(&slot.plugin);
        std::fs::create_dir_all(&dir)
            .map_err(|e| SessionError::Other(format!("{}: {e}", dir.display())))?;
        let path = dir.join(file_name(name));
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| SessionError::Other(e.to_string()))?;
        std::fs::write(&path, json)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        self.notify(
            crate::NoticeLevel::Info,
            format!("saved preset '{}' for {}", name.trim(), slot.plugin.name),
        );
        Ok(path)
    }

    /// Load a preset (one undo step).
    pub fn load_plugin_preset(&mut self, plugin: PluginInstanceId, path: &Path) -> Result<()> {
        let (track, plugin_ref) = self
            .plugin_owner(plugin)
            .map(|(t, s)| (t, s.plugin.clone()))
            .ok_or_else(|| SessionError::Other(format!("no plugin {plugin}")))?;
        let bytes = std::fs::read(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let (state, parameters) = if path.extension().is_some_and(|e| e == "ffpreset") {
            let f: PresetFile = serde_json::from_slice(&bytes)
                .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
            if f.plugin.id != plugin_ref.id || f.plugin.format != plugin_ref.format {
                return Err(SessionError::Other(format!(
                    "the preset is for {}, not {}",
                    f.plugin.name, plugin_ref.name
                )));
            }
            (f.state, f.parameters)
        } else {
            let state = self
                .engine
                .plugin_state_from_preset(plugin, &bytes)
                .map_err(|e| SessionError::Other(e.to_string()))?;
            (Some(state), Vec::new())
        };
        self.edit(Command::SetPluginState {
            track,
            plugin,
            state,
            parameters,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_file_names_are_safe() {
        assert_eq!(file_name(" Warm / Bright "), "Warm _ Bright.ffpreset");
        assert_eq!(file_name(""), "Preset.ffpreset");
    }
}
