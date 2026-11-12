//! A device's A/B compare: two complete settings of a plugin slot, one
//! heard — the slot's own state and parameter values — and the other kept
//! here, by the slot's id.
//!
//! A side table so `PluginSlot` literals stay as they are. Switching sides
//! is one undo step that swaps the slot's settings with the side kept
//! ([`Command::SetPluginState`]) and records which side is heard
//! ([`Command::SetDeviceCompare`]), so undo, redo, saving and opening keep
//! the two together. A slot without an entry is heard on side A and has no
//! B yet (the first switch makes B a copy of A). Entries of slots that are
//! gone are kept while the session runs (an undone removal brings the slot
//! back with its B) and dropped when a project is opened
//! ([`Project::repair`]).

use crate::{Command, Project, SavedParameter};
use faderframe_core::{PluginInstanceId, TrackId};
use serde::{Deserialize, Serialize};

/// One of the two sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    #[default]
    A,
    B,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::A => "A",
            Side::B => "B",
        }
    }
}

/// A plugin slot's complete settings: its state and the parameter values
/// applied over it (what [`Command::SetPluginState`] sets).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<SavedParameter>,
}

impl DeviceSettings {
    /// The settings a slot holds.
    pub fn of(slot: &crate::PluginSlot) -> Self {
        Self {
            state: slot.state.clone(),
            parameters: slot.parameters.clone(),
        }
    }

    /// The edit giving the slot these settings.
    pub fn command(&self, track: TrackId, plugin: PluginInstanceId) -> Command {
        Command::SetPluginState {
            track,
            plugin,
            state: self.state.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// A slot's A/B compare.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceCompare {
    /// The side the slot plays (its own settings).
    #[serde(default)]
    pub heard: Side,
    /// The other side's settings.
    #[serde(default)]
    pub other: DeviceSettings,
}

impl DeviceCompare {
    /// The settings of `side` when the slot holds `now` (the heard side's).
    pub fn settings_of(&self, side: Side, now: &DeviceSettings) -> DeviceSettings {
        if side == self.heard {
            now.clone()
        } else {
            self.other.clone()
        }
    }
}

impl Project {
    /// A slot's A/B compare (`None`: side A heard, no B yet).
    pub fn device_compare(&self, plugin: PluginInstanceId) -> Option<&DeviceCompare> {
        self.device_compare.get(&plugin)
    }

    /// Drop the compares of slots the project has not got (on opening).
    pub(crate) fn repair_device_compare(&mut self) -> Vec<String> {
        let slots: std::collections::BTreeSet<PluginInstanceId> = self
            .tracks
            .iter()
            .flat_map(|t| t.slots())
            .chain(self.album.inserts())
            .map(|s| s.id)
            .collect();
        let before = self.device_compare.len();
        self.device_compare.retain(|id, _| slots.contains(id));
        let gone = before - self.device_compare.len();
        if gone > 0 {
            vec![format!(
                "dropped the A/B compare of {gone} device(s) that are gone"
            )]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{PluginFormat, PluginRef, PluginSlot, TrackKind};
    use faderframe_core::ParameterId;

    fn project_with_slot() -> (Project, TrackId, PluginInstanceId) {
        let mut p = Project::new("p", 48_000);
        let track = p.ids.allocate();
        p.tracks.push(crate::Track::new(
            track,
            TrackKind::Audio,
            "Vocals",
            crate::TrackColor::rgb(1, 2, 3),
        ));
        let slot = PluginSlot {
            id: p.ids.allocate(),
            plugin: PluginRef {
                format: PluginFormat::Clap,
                id: "eq".into(),
                name: "EQ".into(),
            },
            bypass: false,
            parameters: vec![SavedParameter {
                id: ParameterId(1),
                value: 0.5,
            }],
            state: Some("A".into()),
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        };
        let id = slot.id;
        Command::InsertPlugin {
            track,
            index: 0,
            slot,
        }
        .apply(&mut p)
        .unwrap();
        (p, track, id)
    }

    /// A switch is one step: the slot gets B's settings and the compare
    /// keeps A's; undoing it brings both back.
    #[test]
    fn a_switch_swaps_the_settings_and_undo_swaps_them_back() {
        let (mut p, track, plugin) = project_with_slot();
        let a = DeviceSettings::of(p.track(track).unwrap().plugin(plugin).unwrap());
        let b = DeviceSettings {
            state: Some("B".into()),
            parameters: Vec::new(),
        };
        let switch = Command::Batch {
            label: "Compare B".into(),
            commands: vec![
                b.command(track, plugin),
                Command::SetDeviceCompare {
                    plugin,
                    compare: Some(DeviceCompare {
                        heard: Side::B,
                        other: a.clone(),
                    }),
                },
            ],
        };
        assert_eq!(switch.impact(), crate::Impact::Params);
        let undo = switch.apply(&mut p).unwrap();
        let slot = p.track(track).unwrap().plugin(plugin).unwrap();
        assert_eq!(DeviceSettings::of(slot), b);
        let c = p.device_compare(plugin).unwrap();
        assert_eq!(c.heard, Side::B);
        assert_eq!(c.settings_of(Side::A, &b), a);
        undo.apply(&mut p).unwrap();
        assert_eq!(
            DeviceSettings::of(p.track(track).unwrap().plugin(plugin).unwrap()),
            a
        );
        assert!(p.device_compare(plugin).is_none());
    }

    #[test]
    fn compares_of_slots_that_are_gone_are_dropped_on_opening() {
        let (mut p, _, plugin) = project_with_slot();
        p.device_compare.insert(plugin, DeviceCompare::default());
        p.device_compare
            .insert(PluginInstanceId(9_999), DeviceCompare::default());
        let json = serde_json::to_string(&p).unwrap();
        let mut back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.device_compare.len(), 2, "saved as it is");
        let notes = back.repair_device_compare();
        assert_eq!(notes.len(), 1);
        assert!(back.device_compare(plugin).is_some());
        assert_eq!(back.device_compare.len(), 1);
        // A project without compares writes none.
        let plain = Project::new("q", 48_000);
        assert!(
            !serde_json::to_string(&plain)
                .unwrap()
                .contains("device_compare")
        );
    }
}
