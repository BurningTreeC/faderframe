//! The clip launcher: clips in slots, a column a track and a row a scene,
//! started and stopped live (quantised to the bar or beat) next to the
//! arrangement.
//!
//! A slot's clip is an ordinary [`crate::Clip`] in `Project::clips` (so
//! every clip editor works on it) that no track lists in `Track::clips`:
//! the arrangement, the timeline snapshot and time edits leave it alone
//! ([`Project::launcher_clips`] tells them which to skip). Its start is
//! unused; it plays from its own start, looping over its length while it
//! is launched. Scenes only name rows; their slots are found by
//! [`SlotKey`].

use faderframe_core::{ClipId, SceneId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Where a launch waits for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchQuantize {
    /// At once.
    None,
    /// The next beat.
    Beat,
    /// The next bar.
    #[default]
    Bar,
    /// The next 2, 4 or 8 bars (from the song's start).
    Bars(u8),
}

impl LaunchQuantize {
    pub const ALL: [LaunchQuantize; 6] = [
        LaunchQuantize::None,
        LaunchQuantize::Beat,
        LaunchQuantize::Bar,
        LaunchQuantize::Bars(2),
        LaunchQuantize::Bars(4),
        LaunchQuantize::Bars(8),
    ];

    pub fn label(self) -> String {
        match self {
            LaunchQuantize::None => "None".into(),
            LaunchQuantize::Beat => "1 Beat".into(),
            LaunchQuantize::Bar => "1 Bar".into(),
            LaunchQuantize::Bars(n) => format!("{n} Bars"),
        }
    }
}

/// Where a clip goes after it has played for a while.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowKind {
    /// From its start again.
    Again,
    /// The track's clip in the next scene that has one (the first after
    /// the last).
    #[default]
    Next,
    Previous,
    First,
    Last,
    /// Any of the track's clips (this one too), at random.
    Any,
    /// Another of the track's clips, at random.
    Other,
    /// The track stops.
    Stop,
}

impl FollowKind {
    pub const ALL: [FollowKind; 8] = [
        FollowKind::Again,
        FollowKind::Next,
        FollowKind::Previous,
        FollowKind::First,
        FollowKind::Last,
        FollowKind::Any,
        FollowKind::Other,
        FollowKind::Stop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FollowKind::Again => "Again",
            FollowKind::Next => "Next",
            FollowKind::Previous => "Previous",
            FollowKind::First => "First",
            FollowKind::Last => "Last",
            FollowKind::Any => "Any",
            FollowKind::Other => "Other",
            FollowKind::Stop => "Stop",
        }
    }

    /// The slots it goes to among the track's clips (in scene order), from
    /// `at` in them; empty: stop. `random`: one of them at random.
    pub fn targets(self, at: usize, count: usize) -> (Vec<usize>, bool) {
        if count == 0 {
            return (Vec::new(), false);
        }
        match self {
            FollowKind::Again => (vec![at], false),
            FollowKind::Next => (vec![(at + 1) % count], false),
            FollowKind::Previous => (vec![(at + count - 1) % count], false),
            FollowKind::First => (vec![0], false),
            FollowKind::Last => (vec![count - 1], false),
            FollowKind::Any => ((0..count).collect(), true),
            FollowKind::Other if count == 1 => (vec![at], false),
            FollowKind::Other => ((0..count).filter(|i| *i != at).collect(), true),
            FollowKind::Stop => (Vec::new(), false),
        }
    }
}

/// A slot's follow action: after `bars` bars (0: the clip's length) the
/// track goes on as `kind` says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FollowAction {
    pub kind: FollowKind,
    #[serde(default)]
    pub bars: u16,
}

/// A row of slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
}

/// A slot: a track's column in a scene's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotKey {
    pub track: TrackId,
    pub scene: SceneId,
}

impl SlotKey {
    /// A stable number for the engine (below 2^63: the engine's status
    /// flags take the top bit).
    pub fn hash(self) -> u64 {
        (self.track.raw().wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.scene.raw().rotate_left(29))
            >> 1
    }
}

/// The launcher (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Launcher {
    pub scenes: Vec<Scene>,
    /// The clip in each filled slot.
    #[serde(default, with = "slot_list")]
    pub slots: BTreeMap<SlotKey, ClipId>,
    #[serde(default)]
    pub quantize: LaunchQuantize,
    /// Follow actions by slot.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        with = "follow_list"
    )]
    pub follow: BTreeMap<SlotKey, FollowAction>,
}

impl Launcher {
    pub fn is_empty(&self) -> bool {
        self.scenes.is_empty() && self.slots.is_empty()
    }

    pub fn scene_index(&self, scene: SceneId) -> Option<usize> {
        self.scenes.iter().position(|s| s.id == scene)
    }

    /// The clip in a slot.
    pub fn clip(&self, track: TrackId, scene: SceneId) -> Option<ClipId> {
        self.slots.get(&SlotKey { track, scene }).copied()
    }

    /// The slot holding `clip`.
    pub fn slot_of(&self, clip: ClipId) -> Option<SlotKey> {
        self.slots
            .iter()
            .find(|(_, c)| **c == clip)
            .map(|(k, _)| *k)
    }
}

/// Follow actions as a list in files.
mod follow_list {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        track: TrackId,
        scene: SceneId,
        follow: FollowAction,
    }

    pub fn serialize<S: Serializer>(
        m: &BTreeMap<SlotKey, FollowAction>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let v: Vec<Entry> = m
            .iter()
            .map(|(k, f)| Entry {
                track: k.track,
                scene: k.scene,
                follow: *f,
            })
            .collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<SlotKey, FollowAction>, D::Error> {
        let v: Vec<Entry> = Vec::deserialize(d)?;
        Ok(v.into_iter()
            .map(|e| {
                (
                    SlotKey {
                        track: e.track,
                        scene: e.scene,
                    },
                    e.follow,
                )
            })
            .collect())
    }
}

/// Slots as a list in files (JSON maps need string keys).
mod slot_list {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        track: TrackId,
        scene: SceneId,
        clip: ClipId,
    }

    pub fn serialize<S: Serializer>(
        m: &BTreeMap<SlotKey, ClipId>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let v: Vec<Entry> = m
            .iter()
            .map(|(k, c)| Entry {
                track: k.track,
                scene: k.scene,
                clip: *c,
            })
            .collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<SlotKey, ClipId>, D::Error> {
        let v: Vec<Entry> = Vec::deserialize(d)?;
        Ok(v.into_iter()
            .map(|e| {
                (
                    SlotKey {
                        track: e.track,
                        scene: e.scene,
                    },
                    e.clip,
                )
            })
            .collect())
    }
}

impl crate::Project {
    /// The clips in launcher slots (to leave out of the arrangement).
    pub fn launcher_clips(&self) -> BTreeSet<ClipId> {
        self.launcher.slots.values().copied().collect()
    }

    pub fn is_launcher_clip(&self, clip: ClipId) -> bool {
        self.launcher.slots.values().any(|c| *c == clip)
    }
}
