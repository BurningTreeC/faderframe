//! ARA (Audio Random Access): which inserts render their track's clips
//! through ARA, and each ARA plug-in's document archive.
//!
//! A side table so `PluginSlot` literals stay as they are. An insert bound
//! to ARA takes on all three roles for its track: it renders the track's
//! audio clips as playback regions (its input is ignored), previews edits,
//! and its editor shows the whole document. The archive is the plug-in's
//! own (Melodyne's notes and edits), captured before every save and render
//! like plugin states — bookkeeping, not an undoable edit.

use crate::Project;
use faderframe_core::{PluginInstanceId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ara {
    /// Inserts bound to ARA. Ids of slots that are gone are kept (an undone
    /// removal brings the slot back bound) and ignored.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub slots: BTreeSet<PluginInstanceId>,
    /// Each ARA plug-in's document state, by factory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archives: Vec<AraArchive>,
}

/// An ARA plug-in's document state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AraArchive {
    /// The factory's ID (versioned; the same through every companion API).
    pub factory: String,
    /// The plug-in as the user knows it (to say what is missing).
    pub plugin: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    /// The archive ID the plug-in wrote it under.
    pub archive_id: String,
    /// The plug-in's bytes, base64.
    pub data: String,
}

impl Ara {
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty() && self.archives.is_empty()
    }

    pub fn archive(&self, factory: &str) -> Option<&AraArchive> {
        self.archives.iter().find(|a| a.factory == factory)
    }

    /// Store (or replace) a factory's archive.
    pub fn set_archive(&mut self, archive: AraArchive) {
        match self
            .archives
            .iter_mut()
            .find(|a| a.factory == archive.factory)
        {
            Some(a) => *a = archive,
            None => self.archives.push(archive),
        }
    }
}

impl Project {
    /// Whether `slot` is an insert bound to ARA (and exists).
    pub fn is_ara_slot(&self, slot: PluginInstanceId) -> bool {
        self.ara.slots.contains(&slot) && self.ara_slot_track(slot).is_some()
    }

    /// The track an ARA insert renders.
    pub fn ara_slot_track(&self, slot: PluginInstanceId) -> Option<TrackId> {
        if !self.ara.slots.contains(&slot) {
            return None;
        }
        self.tracks
            .iter()
            .find(|t| t.slots().iter().any(|s| s.id == slot))
            .map(|t| t.id)
    }

    /// A track's ARA insert, if it has one (the first).
    pub fn ara_slot_of(&self, track: TrackId) -> Option<&crate::PluginSlot> {
        self.track(track)?
            .slots()
            .into_iter()
            .find(|s| self.ara.slots.contains(&s.id))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{Command, PluginFormat, PluginRef, PluginSlot, TrackKind};

    #[test]
    fn an_insert_is_bound_to_ara_by_an_undoable_command() {
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
                format: PluginFormat::Vst3,
                id: "melodyne".into(),
                name: "Melodyne".into(),
            },
            bypass: false,
            parameters: Vec::new(),
            state: None,
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
        let cmd = Command::SetAraSlot { slot: id, on: true };
        assert_eq!(cmd.label(), "Open with ARA");
        let undo = cmd.apply(&mut p).unwrap();
        assert!(p.is_ara_slot(id));
        assert_eq!(p.ara_slot_track(id), Some(track));
        assert_eq!(p.ara_slot_of(track).map(|s| s.id), Some(id));
        undo.apply(&mut p).unwrap();
        assert!(!p.is_ara_slot(id));
        assert!(
            Command::SetAraSlot {
                slot: PluginInstanceId(9999),
                on: true
            }
            .apply(&mut p)
            .is_err()
        );
        // Archives: one per factory, replaced.
        let a = AraArchive {
            factory: "f.1".into(),
            plugin: "Melodyne".into(),
            vendor: "Celemony".into(),
            version: "5".into(),
            archive_id: "a.1".into(),
            data: "AAAA".into(),
        };
        p.ara.set_archive(a.clone());
        p.ara.set_archive(AraArchive {
            data: "BBBB".into(),
            ..a
        });
        assert_eq!(p.ara.archives.len(), 1);
        assert_eq!(p.ara.archive("f.1").unwrap().data, "BBBB");
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ara, p.ara);
    }
}
