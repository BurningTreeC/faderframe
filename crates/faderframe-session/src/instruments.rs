//! What the built-in instruments load beyond their parameters: DX7 voices
//! for the FM (a SysEx file read into a bank the session keeps per
//! instance — outside the project and its history — and a voice of it
//! loaded as one undo step, its whole state replaced), and the frame size
//! of a Wavetable's user tables (part of its sample document, so it saves,
//! undoes and travels with presets; one undo step).

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{PluginInstanceId, builtin};
use faderframe_plugin_host::ParamValues;
use faderframe_plugin_host::devices::fm::{self, dx7::Dx7Voice};
use faderframe_project::{Command, PluginFormat};
use std::path::{Path, PathBuf};

/// The voices read from a DX7 SysEx file for one FM instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct Dx7Bank {
    pub path: PathBuf,
    pub voices: Vec<Dx7Voice>,
    /// The voice loaded last (from this bank).
    pub loaded: Option<usize>,
}

impl Dx7Bank {
    /// The voices' names, in order.
    pub fn names(&self) -> Vec<&str> {
        self.voices.iter().map(|v| v.name.as_str()).collect()
    }
}

impl Session {
    /// Whether `plugin` is a built-in of kind `id`.
    fn is_builtin(&self, plugin: PluginInstanceId, id: &str) -> bool {
        self.plugin_owner(plugin).is_some_and(|(_, slot)| {
            slot.plugin.format == PluginFormat::Builtin && slot.plugin.id == id
        })
    }

    /// Read a DX7 SysEx file for the FM instrument `plugin`: its voices
    /// are its bank (a file of one voice loads it at once).
    pub(crate) fn import_dx7(&mut self, plugin: PluginInstanceId, path: &Path) -> Result<()> {
        if !self.is_builtin(plugin, builtin::FM) {
            return Err(SessionError::Other("not an FM instrument".into()));
        }
        let bytes = std::fs::read(path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let voices = match fm::dx7::parse(&bytes) {
            Ok(v) => v,
            Err(e) => {
                self.notify(NoticeLevel::Warning, format!("{}: {e}", path.display()));
                return Ok(());
            }
        };
        let single = voices.len() == 1;
        self.dx7_banks.insert(
            plugin,
            Dx7Bank {
                path: path.to_path_buf(),
                voices,
                loaded: None,
            },
        );
        self.revision += 1;
        if single {
            self.load_dx7_voice(plugin, 0)?;
        }
        Ok(())
    }

    /// Load voice `index` of the FM instrument's bank: its whole state, one
    /// undo step named after the voice.
    pub(crate) fn load_dx7_voice(&mut self, plugin: PluginInstanceId, index: usize) -> Result<()> {
        let Some(voice) = self
            .dx7_banks
            .get(&plugin)
            .and_then(|b| b.voices.get(index))
            .cloned()
        else {
            return Err(SessionError::Other(format!("no DX7 voice {index}")));
        };
        let Some((track, _)) = self.plugin_owner(plugin) else {
            return Err(SessionError::Other("no such plugin".into()));
        };
        let params = ParamValues::new(fm::parameters());
        for (id, v) in &voice.values {
            params
                .set_by_id(*id, *v)
                .map_err(|e| SessionError::Other(e.to_string()))?;
        }
        let state = faderframe_engine::encode_state(&params.save());
        // The state to go back to (a fresh instance's slot has none).
        self.capture_plugin_state(plugin);
        self.edit(Command::Batch {
            label: format!("Load DX7 Voice \"{}\"", voice.name),
            commands: vec![Command::SetPluginState {
                track,
                plugin,
                state: Some(state),
                parameters: Vec::new(),
            }],
        })?;
        if let Some(b) = self.dx7_banks.get_mut(&plugin) {
            b.loaded = Some(index);
        }
        Ok(())
    }

    /// The DX7 bank read for an FM instrument.
    pub fn dx7_bank(&self, plugin: PluginInstanceId) -> Option<&Dx7Bank> {
        self.dx7_banks.get(&plugin)
    }

    /// Set the samples a frame of the Wavetable's user table in `slot`
    /// has (0: from the file).
    pub(crate) fn set_wavetable_cycle(
        &mut self,
        plugin: PluginInstanceId,
        slot: usize,
        samples: u32,
    ) -> Result<()> {
        let Some((_, owner)) = self.plugin_owner(plugin) else {
            return Err(SessionError::Other("no such plugin".into()));
        };
        if !self.is_builtin(plugin, builtin::WAVETABLE) {
            return Err(SessionError::Other("not a Wavetable".into()));
        }
        let mut doc = crate::samples::doc_of(owner);
        if doc.cycle(slot) == samples {
            return Ok(());
        }
        doc.set_cycle(slot, samples);
        self.apply_samples_as(plugin, &doc, "Wavetable Frame Size")
    }

    /// The frame size a Wavetable's user table in `slot` is read with (0:
    /// from the file).
    pub fn wavetable_cycle(&self, plugin: PluginInstanceId, slot: usize) -> u32 {
        self.plugin_owner(plugin)
            .map_or(0, |(_, s)| crate::samples::doc_of(s).cycle(slot))
    }
}
