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
    /// The track's clip in the scene with this index (0: the first); the
    /// track stops if it has none there.
    Jump(u16),
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

    pub fn label(self) -> String {
        match self {
            FollowKind::Again => "Again".into(),
            FollowKind::Next => "Next".into(),
            FollowKind::Previous => "Previous".into(),
            FollowKind::First => "First".into(),
            FollowKind::Last => "Last".into(),
            FollowKind::Any => "Any".into(),
            FollowKind::Other => "Other".into(),
            FollowKind::Stop => "Stop".into(),
            FollowKind::Jump(n) => format!("Jump to Scene {}", u32::from(n) + 1),
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
            // By scene, not among the clips (see `Launcher::follow_targets`).
            FollowKind::Stop | FollowKind::Jump(_) => (Vec::new(), false),
        }
    }
}

/// A slot's follow action: after `bars` bars (0: the clip's length) the
/// track goes on as `kind` says — or, with a second action, as `kind` with
/// a `chance` in a hundred and as `other` otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FollowAction {
    pub kind: FollowKind,
    #[serde(default)]
    pub bars: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<FollowKind>,
    /// Percent (0–100) for `kind` when there is an `other`.
    #[serde(default = "full_chance", skip_serializing_if = "is_full")]
    pub chance: u8,
}

fn full_chance() -> u8 {
    100
}

fn is_full(c: &u8) -> bool {
    *c == 100
}

impl Default for FollowAction {
    fn default() -> Self {
        Self {
            kind: FollowKind::default(),
            bars: 0,
            other: None,
            chance: 100,
        }
    }
}

impl FollowAction {
    pub fn new(kind: FollowKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }
}

/// How a slot's clip answers its launch button (pad, key, mouse).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    /// A press launches it.
    #[default]
    Trigger,
    /// It plays while held; letting go stops it.
    Gate,
    /// A press launches it, the next one stops it.
    Toggle,
    /// While held it starts again at every launch position; letting go
    /// stops it.
    Repeat,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [
        LaunchMode::Trigger,
        LaunchMode::Gate,
        LaunchMode::Toggle,
        LaunchMode::Repeat,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LaunchMode::Trigger => "Trigger",
            LaunchMode::Gate => "Gate",
            LaunchMode::Toggle => "Toggle",
            LaunchMode::Repeat => "Repeat",
        }
    }
}

/// A slot's launch settings (the default: a trigger at the launcher's
/// quantisation, from the clip's start).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipLaunch {
    #[serde(default)]
    pub mode: LaunchMode,
    /// Its own quantisation (`None`: the launcher's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<LaunchQuantize>,
    /// Takes over the position of the clip playing on its track (in time,
    /// not from the start).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legato: bool,
    /// An audio clip follows the project's tempo: its audio is in time at
    /// this tempo (BPM) and is stretched to the tempo it plays at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<f64>,
}

impl ClipLaunch {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
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
    /// Launch settings by slot (slots without are triggers).
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        with = "launch_list"
    )]
    pub launch: BTreeMap<SlotKey, ClipLaunch>,
    /// Recording into slots: bars a recording lasts (0: until ended) and
    /// bars counted in when the transport starts for it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub record_bars: u16,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub count_in: u8,
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

impl Launcher {
    /// A slot's launch settings.
    pub fn launch_of(&self, key: SlotKey) -> ClipLaunch {
        self.launch.get(&key).copied().unwrap_or_default()
    }

    /// The quantisation a slot's launch waits for.
    pub fn quantize_of(&self, key: SlotKey) -> LaunchQuantize {
        self.launch_of(key).quantize.unwrap_or(self.quantize)
    }

    /// The filled slots of `track` in scene order.
    pub fn track_slots(&self, track: TrackId) -> Vec<SlotKey> {
        self.scenes
            .iter()
            .map(|s| SlotKey { track, scene: s.id })
            .filter(|k| self.slots.contains_key(k))
            .collect()
    }

    /// Where `kind` goes from `key` (slots; empty: the track stops) and
    /// whether one of them is picked at random.
    pub fn follow_targets(&self, key: SlotKey, kind: FollowKind) -> (Vec<SlotKey>, bool) {
        let list = self.track_slots(key.track);
        if let FollowKind::Jump(n) = kind {
            let target = self.scenes.get(usize::from(n)).map(|s| SlotKey {
                track: key.track,
                scene: s.id,
            });
            return (
                target
                    .filter(|k| self.slots.contains_key(k))
                    .into_iter()
                    .collect(),
                false,
            );
        }
        let Some(at) = list.iter().position(|k| *k == key) else {
            return (Vec::new(), false);
        };
        let (targets, random) = kind.targets(at, list.len());
        (targets.into_iter().map(|i| list[i]).collect(), random)
    }

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

/// Launch settings as a list in files.
mod launch_list {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Entry {
        track: TrackId,
        scene: SceneId,
        launch: ClipLaunch,
    }

    pub fn serialize<S: Serializer>(
        m: &BTreeMap<SlotKey, ClipLaunch>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let v: Vec<Entry> = m
            .iter()
            .map(|(k, l)| Entry {
                track: k.track,
                scene: k.scene,
                launch: *l,
            })
            .collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<SlotKey, ClipLaunch>, D::Error> {
        let v: Vec<Entry> = Vec::deserialize(d)?;
        Ok(v.into_iter()
            .map(|e| {
                (
                    SlotKey {
                        track: e.track,
                        scene: e.scene,
                    },
                    e.launch,
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

/// A launcher clip as it plays: an audio clip that follows the tempo (see
/// [`ClipLaunch::tempo`]) stretched — through its warp map, or a uniform
/// one — to its musical length (at its own tempo) under `timeline`, from
/// the song's start; anything else as it is.
pub fn as_played(
    clip: &crate::Clip,
    launch: &ClipLaunch,
    timeline: &faderframe_timeline::Timeline,
    project_rate: u32,
) -> crate::Clip {
    let (Some(bpm), crate::ClipContent::Audio(a)) = (launch.tempo, &clip.content) else {
        return clip.clone();
    };
    let rate = f64::from(project_rate.max(1));
    if !bpm.is_finite() || bpm <= 0.0 || a.length <= 0 || a.reversed {
        return clip.clone();
    }
    let quarters = a.length as f64 / rate * bpm / 60.0;
    let length = timeline
        .to_samples(
            faderframe_timeline::MusicalTime::from_quarters(quarters),
            rate,
        )
        .max(1);
    if (length - a.length).abs() <= 1 {
        return clip.clone();
    }
    let mut warp = a
        .warp
        .clone()
        .unwrap_or_else(|| crate::Warp::uniform(a.length));
    let scale = length as f64 / a.length as f64;
    let mut last = 0;
    warp.markers.retain_mut(|m| {
        m.at = (m.at as f64 * scale).round() as i64;
        let keep = m.at > last && m.at < length;
        if keep {
            last = m.at;
        }
        keep
    });
    let mut out = clip.clone();
    if let crate::ClipContent::Audio(b) = &mut out.content {
        b.length = length;
        b.warp = Some(warp);
    }
    out
}

impl crate::Project {
    /// A launcher clip as it plays (see [`as_played`]).
    pub fn launcher_clip_as_played(&self, key: SlotKey) -> Option<crate::Clip> {
        let clip = self.clips.get(self.launcher.slots.get(&key)?)?;
        Some(as_played(
            clip,
            &self.launcher.launch_of(key),
            &self.timeline,
            self.sample_rate,
        ))
    }

    /// The clips in launcher slots (to leave out of the arrangement).
    pub fn launcher_clips(&self) -> BTreeSet<ClipId> {
        self.launcher.slots.values().copied().collect()
    }

    pub fn is_launcher_clip(&self, clip: ClipId) -> bool {
        self.launcher.slots.values().any(|c| *c == clip)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{AudioClip, Clip, ClipContent, ClipFades, StretchSettings, Warp, WarpMarker};
    use faderframe_timeline::{MusicalTime, Timeline};

    #[test]
    fn audio_that_follows_the_tempo_is_stretched_to_its_musical_length() {
        // Two seconds of audio in time at 120 BPM: four beats.
        let mut warp = Warp::uniform(96_000);
        warp.markers.push(WarpMarker {
            at: 48_000,
            source: 40_000,
        });
        let clip = Clip {
            id: ClipId(1),
            track: TrackId(1),
            name: "loop".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: faderframe_core::AudioSourceId(1),
                source_offset: 0,
                length: 96_000,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: Some(warp),
                pitch: None,
                effects: None,
            }),
        };
        let mut tl = Timeline::default();
        tl.tempo.set_initial_bpm(60.0);
        let follow = ClipLaunch {
            tempo: Some(120.0),
            ..ClipLaunch::default()
        };
        let played = as_played(&clip, &follow, &tl, 48_000);
        let ClipContent::Audio(a) = &played.content else {
            panic!()
        };
        // At 60 BPM four beats last four seconds; the marker moves along.
        assert_eq!(a.length, 192_000);
        let w = a.warp.as_ref().unwrap();
        assert_eq!(w.source_length, 96_000);
        assert_eq!(w.markers[0].at, 96_000);
        assert_eq!(w.markers[0].source, 40_000);
        // At its own tempo, or without following: as it is.
        tl.tempo.set_initial_bpm(120.0);
        assert_eq!(as_played(&clip, &follow, &tl, 48_000), clip);
        assert_eq!(as_played(&clip, &ClipLaunch::default(), &tl, 48_000), clip);
    }
}
