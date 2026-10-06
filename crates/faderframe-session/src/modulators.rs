//! Modulators (see `faderframe_project::modulation`): adding, changing and
//! removing them (each one undoable `SetModulators`; drags coalesce inside
//! their gesture), mapping one by touching a parameter, what they can
//! move, and what they put out now.
//!
//! Per-note modulators (velocity, key, note envelope, note LFO, note
//! random) move the devices that get the notes: each voice its own where
//! the parameter takes it ([`ModTargetChoice::per_note`]), else as the
//! newest note has it.
//!
//! Mapping: while a modulator maps, the next parameter touched on its
//! track's devices becomes one of its targets. A FaderFrame control's move
//! is taken for that (the value stays as it was) until the gesture ends; a
//! plugin's own editor has moved its value already, which stays.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ModulatorId, ParameterId, PluginInstanceId, TrackId};
use faderframe_project::Command;
use faderframe_project::modulation::{MAX_MODULATORS, ModRoute, ModSource, ModTarget, Modulator};

/// The depth a mapped route starts with.
pub const DEFAULT_DEPTH: f32 = 0.25;

/// A target a modulator can move.
#[derive(Clone, Debug, PartialEq)]
pub struct ModTargetChoice {
    pub target: ModTarget,
    /// "Track", or the device's name.
    pub group: String,
    pub name: String,
    /// The device gets the track's notes (per-note modulators move it).
    pub takes_notes: bool,
    /// The parameter takes modulation per note (each voice its own).
    pub per_note: bool,
}

/// A modulator mapping (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ModLearn {
    pub track: TrackId,
    pub modulator: ModulatorId,
    /// The parameter mapped in this gesture (its moves are taken).
    pub mapped: Option<(PluginInstanceId, ParameterId)>,
}

impl Session {
    fn modulators_of(&self, track: TrackId) -> Result<Vec<Modulator>> {
        let t = self
            .project
            .track(track)
            .ok_or_else(|| SessionError::Other("no such track".into()))?;
        if !faderframe_engine::modulation::modulated_kind(t.kind) {
            return Err(SessionError::Other(
                "modulators are for tracks with audio (not MIDI tracks, folders or VCAs)".into(),
            ));
        }
        Ok(t.modulators.clone())
    }

    /// A new modulator on `track`, named after its kind ("LFO 2" next to
    /// an LFO).
    pub(crate) fn add_modulator(
        &mut self,
        track: TrackId,
        source: ModSource,
    ) -> Result<ModulatorId> {
        let mut list = self.modulators_of(track)?;
        if list.len() >= MAX_MODULATORS {
            return Err(SessionError::Other(format!(
                "a track has at most {MAX_MODULATORS} modulators"
            )));
        }
        let id: ModulatorId = self.project.ids.allocate();
        let mut m = Modulator::new(id, source);
        let kind = m.source.kind_label();
        let same = list
            .iter()
            .filter(|x| x.source.kind_label() == kind)
            .count();
        if same > 0 {
            m.name = format!("{kind} {}", same + 1);
        }
        list.push(m);
        self.edit(Command::SetModulators {
            track,
            modulators: list,
        })?;
        Ok(id)
    }

    pub(crate) fn remove_modulator(
        &mut self,
        track: TrackId,
        modulator: ModulatorId,
    ) -> Result<()> {
        let mut list = self.modulators_of(track)?;
        let before = list.len();
        list.retain(|m| m.id != modulator);
        if list.len() == before {
            return Ok(());
        }
        if self.mod_learn.is_some_and(|l| l.modulator == modulator) {
            self.mod_learn = None;
        }
        self.edit(Command::SetModulators {
            track,
            modulators: list,
        })
    }

    /// Replace the modulator with `modulator`'s id (its settings, routes
    /// and depths).
    pub(crate) fn set_modulator(&mut self, track: TrackId, modulator: Modulator) -> Result<()> {
        let mut list = self.modulators_of(track)?;
        let Some(m) = list.iter_mut().find(|m| m.id == modulator.id) else {
            return Ok(());
        };
        if *m == modulator {
            return Ok(());
        }
        *m = modulator;
        self.edit(Command::SetModulators {
            track,
            modulators: list,
        })
    }

    /// Start mapping `modulator` (`None`: stop).
    pub(crate) fn learn_modulation(&mut self, learn: Option<(TrackId, ModulatorId)>) {
        self.mod_learn = learn.map(|(track, modulator)| ModLearn {
            track,
            modulator,
            mapped: None,
        });
        self.revision += 1;
    }

    /// The modulator mapping now, if one is.
    pub fn modulation_learning(&self) -> Option<(TrackId, ModulatorId)> {
        self.mod_learn.map(|l| (l.track, l.modulator))
    }

    /// While mapping: `plugin`'s `parameter` was touched. True when the
    /// touch was taken for the mapping (the edit is not applied).
    pub(crate) fn map_touched(
        &mut self,
        plugin: PluginInstanceId,
        parameter: ParameterId,
    ) -> Result<bool> {
        let Some(learn) = self.mod_learn else {
            return Ok(false);
        };
        if let Some(mapped) = learn.mapped {
            return Ok(mapped == (plugin, parameter));
        }
        if self.plugin_slot(plugin).map(|(t, _)| t.id) != Some(learn.track) {
            return Ok(false);
        }
        let target = ModTarget::Plugin { plugin, parameter };
        let name = self
            .modulation_target_name(learn.track, target)
            .unwrap_or_else(|| "that parameter".into());
        if !self.engine.plugin_modulatable(plugin, parameter) {
            self.notify(NoticeLevel::Warning, format!("{name} cannot be modulated"));
            return Ok(false);
        }
        let mut list = self.modulators_of(learn.track)?;
        let Some(m) = list.iter_mut().find(|m| m.id == learn.modulator) else {
            self.mod_learn = None;
            return Ok(false);
        };
        if m.source.per_note() && !self.engine.plugin_takes_notes(plugin) {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "‘{}’ follows notes: it moves the devices that get them, not {name}",
                    m.name
                ),
            );
            return Ok(false);
        }
        let modulator = m.name.clone();
        if m.routes.iter().any(|r| r.target == target) {
            self.notify(
                NoticeLevel::Info,
                format!("‘{modulator}’ moves {name} already"),
            );
        } else {
            m.routes.push(ModRoute {
                target,
                depth: DEFAULT_DEPTH,
            });
            self.edit(Command::SetModulators {
                track: learn.track,
                modulators: list,
            })?;
            self.notify(NoticeLevel::Info, format!("‘{modulator}’ moves {name}"));
        }
        self.mod_learn = Some(ModLearn {
            mapped: Some((plugin, parameter)),
            ..learn
        });
        Ok(true)
    }

    /// A gesture ended: a mapping that took its parameter is done.
    pub(crate) fn mapping_gesture_ended(&mut self) {
        if self.mod_learn.is_some_and(|l| l.mapped.is_some()) {
            self.mod_learn = None;
        }
    }

    /// What `track`'s modulators can move: its fader and pan, and every
    /// device parameter that takes modulation.
    pub fn modulation_targets(&self, track: TrackId) -> Vec<ModTargetChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut out = vec![
            ModTargetChoice {
                target: ModTarget::Volume,
                group: "Track".into(),
                name: "Volume".into(),
                takes_notes: false,
                per_note: false,
            },
            ModTargetChoice {
                target: ModTarget::Pan,
                group: "Track".into(),
                name: "Pan".into(),
                takes_notes: false,
                per_note: false,
            },
        ];
        for slot in t.slots() {
            let Some(infos) = self.engine.plugin_parameters(slot.id) else {
                continue;
            };
            let takes_notes = self.engine.plugin_takes_notes(slot.id);
            for p in infos {
                if self.engine.plugin_modulatable(slot.id, p.id) {
                    out.push(ModTargetChoice {
                        target: ModTarget::Plugin {
                            plugin: slot.id,
                            parameter: p.id,
                        },
                        group: slot.plugin.name.clone(),
                        name: p.name.clone(),
                        takes_notes,
                        per_note: takes_notes
                            && self.engine.plugin_modulatable_per_note(slot.id, p.id),
                    });
                }
            }
        }
        out
    }

    /// A target's name ("Volume", "Utility · Gain"); `None` when it is gone.
    pub fn modulation_target_name(&self, track: TrackId, target: ModTarget) -> Option<String> {
        match target {
            ModTarget::Volume => Some("Volume".into()),
            ModTarget::Pan => Some("Pan".into()),
            ModTarget::Plugin { plugin, parameter } => {
                let (t, slot) = self.plugin_slot(plugin)?;
                if t.id != track {
                    return None;
                }
                let p = self
                    .engine
                    .plugin_parameters(plugin)?
                    .iter()
                    .find(|p| p.id == parameter)?;
                Some(format!("{} · {}", slot.plugin.name, p.name))
            }
        }
    }

    /// The outputs of `track`'s modulators now (bipolar ones −1..1,
    /// unipolar ones 0..1; 0 when off).
    pub fn modulator_values(&self, track: TrackId) -> Vec<(ModulatorId, f32)> {
        self.engine.modulator_values(track)
    }

    /// Is `plugin`'s `parameter` moved by a modulator?
    pub fn is_modulated(&self, plugin: PluginInstanceId, parameter: ParameterId) -> bool {
        let target = ModTarget::Plugin { plugin, parameter };
        self.plugin_slot(plugin).is_some_and(|(t, _)| {
            t.modulators
                .iter()
                .any(|m| m.enabled && m.routes.iter().any(|r| r.target == target))
        })
    }
}
