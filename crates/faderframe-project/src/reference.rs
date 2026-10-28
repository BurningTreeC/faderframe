//! Reference tracks: finished records the mix is compared with — heard
//! instead of the mix in the monitor path (loudness-matched), their
//! spectral balance set beside the mix's and handed to EQ Match.
//!
//! A reference is a file of the project's media (its source in
//! [`crate::Project::sources`], imported and decoded like any other, so it
//! moves with the project on save and is never "unused"), but it is on no
//! track: nothing renders, exports, bounces or freezes it.

use faderframe_core::{AudioSourceId, ReferenceId};
use serde::{Deserialize, Serialize};

/// How a reference plays beside the song.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ReferenceSync {
    /// Its region plays along the timeline, starting at `at` (project
    /// samples): a matched arrangement, locates and loops taken along.
    Follow { at: i64 },
    /// Its region over and over while the transport plays, on its own.
    Loop,
}

impl Default for ReferenceSync {
    fn default() -> Self {
        ReferenceSync::Follow { at: 0 }
    }
}

fn yes() -> bool {
    true
}

/// One reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReferenceTrack {
    pub id: ReferenceId,
    pub name: String,
    /// The decoded file (at the project's rate).
    pub source: AudioSourceId,
    /// The file frames that play, `[start, end)` (`None`: all of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<(i64, i64)>,
    #[serde(default)]
    pub sync: ReferenceSync,
    /// Played as loud as the mix (integrated loudness of its region
    /// against the mix's, EBU R128).
    #[serde(default = "yes")]
    pub match_loudness: bool,
    /// A trim on top of the match (dB).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub trim_db: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl ReferenceTrack {
    pub fn new(id: ReferenceId, name: impl Into<String>, source: AudioSourceId) -> Self {
        Self {
            id,
            name: name.into(),
            source,
            region: None,
            sync: ReferenceSync::default(),
            match_loudness: true,
            trim_db: 0.0,
        }
    }

    /// The frames that play of a file of `frames`.
    pub fn span(&self, frames: i64) -> (i64, i64) {
        match self.region {
            Some((a, b)) => {
                let a = a.clamp(0, frames.max(0));
                (a, b.clamp(a, frames.max(0)))
            }
            None => (0, frames.max(0)),
        }
    }
}

/// The project's references, in the order the Reference view lists them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct References {
    #[serde(default)]
    pub tracks: Vec<ReferenceTrack>,
}

impl References {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn get(&self, id: ReferenceId) -> Option<&ReferenceTrack> {
        self.tracks.iter().find(|r| r.id == id)
    }

    pub fn get_mut(&mut self, id: ReferenceId) -> Option<&mut ReferenceTrack> {
        self.tracks.iter_mut().find(|r| r.id == id)
    }

    /// Regions kept sane (start before end, not before the file), each id
    /// once, trims finite.
    pub fn tidy(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.tracks.retain(|r| seen.insert(r.id));
        for r in &mut self.tracks {
            if let Some((a, b)) = r.region {
                let a = a.max(0);
                r.region = (b > a).then_some((a, b));
            }
            if !r.trim_db.is_finite() {
                r.trim_db = 0.0;
            }
            r.trim_db = r.trim_db.clamp(-24.0, 24.0);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{Command, History, Project};

    #[test]
    fn the_list_is_one_undoable_command_and_survives_a_file() {
        let mut p = Project::new("R", 48_000);
        let mut h = History::default();
        let id: ReferenceId = p.ids.allocate();
        let source: AudioSourceId = p.ids.allocate();
        let mut list = References::default();
        let mut r = ReferenceTrack::new(id, "Record", source);
        r.region = Some((48_000, 10));
        r.sync = ReferenceSync::Loop;
        list.tracks.push(r.clone());
        list.tracks.push(r);
        h.apply(
            &mut p,
            Command::SetReferences {
                references: Box::new(list),
            },
        )
        .unwrap();
        // Tidied: one entry, the empty region gone.
        assert_eq!(p.references.tracks.len(), 1);
        assert_eq!(p.references.tracks[0].region, None);
        assert_eq!(h.undo_labels(), ["Edit References"]);
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.references, p.references);
        h.undo(&mut p).unwrap();
        assert!(p.references.is_empty());
        // An empty list is not written at all.
        assert!(!serde_json::to_string(&p).unwrap().contains("references"));
    }
}
