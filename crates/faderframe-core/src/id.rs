//! Strongly typed identifiers.
//!
//! IDs are opaque, project-unique `u64` values. They are persisted in project
//! files, so they must never be derived from memory addresses or runtime
//! indices. Use [`IdAllocator`] (owned by the project) to create new ones.

use serde::{Deserialize, Serialize};
use std::fmt;

pub mod translate;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);

        // A plain number in files, as `#[serde(transparent)]` would write
        // it, read and written through [`translate`] (a merge's ids).
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                translate::wrote(self.0);
                s.serialize_u64(self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                u64::deserialize(d).map(|v| Self(translate::read(v)))
            }
        }

        impl $name {
            /// Raw numeric value (stable across save/load).
            #[inline]
            pub const fn raw(self) -> u64 {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self {
                Self(v)
            }
        }
    };
}

define_id!(
    /// A track, bus, aux or master channel.
    TrackId,
    "track#"
);
define_id!(
    /// An audio or MIDI clip on the timeline.
    ClipId,
    "clip#"
);
define_id!(
    /// An aux/bus send on a track.
    SendId,
    "send#"
);
define_id!(
    /// A hosted plugin instance (insert or instrument).
    PluginInstanceId,
    "plugin#"
);

impl PluginInstanceId {
    /// The bit marking a plugin's *cue shadow*: the second instance of a
    /// bus's device that plays the solo-safe, mute-safe cue path of the
    /// bus (never in a project file; ids are allocated well below it).
    const CUE_SHADOW: u64 = 1 << 62;

    /// The id of this slot's cue shadow instance.
    pub const fn cue_shadow(self) -> Self {
        Self(self.0 | Self::CUE_SHADOW)
    }

    pub const fn is_cue_shadow(self) -> bool {
        self.0 & Self::CUE_SHADOW != 0
    }

    /// The slot's own id (a shadow's base, else itself).
    pub const fn base(self) -> Self {
        Self(self.0 & !Self::CUE_SHADOW)
    }
}
define_id!(
    /// An audio source (file or generated material) referenced by clips.
    AudioSourceId,
    "source#"
);
define_id!(
    /// A group of tracks whose controls are linked.
    GroupId,
    "group#"
);
define_id!(
    /// A named section of the arrangement (Intro, Verse, Chorus …).
    SectionId,
    "section#"
);
define_id!(
    /// A timeline marker.
    MarkerId,
    "marker#"
);
define_id!(
    /// A cue of dialogue to replace (ADR).
    AdrCueId,
    "cue#"
);
define_id!(
    /// A song of the album.
    SongId,
    "song#"
);
define_id!(
    /// An automation lane.
    AutomationLaneId,
    "lane#"
);
define_id!(
    /// A controller mapping (MIDI learn).
    MidiMappingId,
    "mapping#"
);
define_id!(
    /// A note inside a MIDI clip.
    NoteId,
    "note#"
);
define_id!(
    /// Clips sharing their content (aliases).
    ClipLinkId,
    "link#"
);
define_id!(
    /// A modulator of a track (LFO, follower, steps, random, macro).
    ModulatorId,
    "mod#"
);
define_id!(
    /// A scene (row) of the clip launcher.
    SceneId,
    "scene#"
);
define_id!(
    /// A video file the project shows.
    VideoSourceId,
    "video#"
);
define_id!(
    /// A placed stretch of a video on a video track.
    VideoClipId,
    "vclip#"
);
define_id!(
    /// A video track.
    VideoTrackId,
    "vtrack#"
);
define_id!(
    /// A cue mix (a performer's headphone mix on its own outputs).
    CueId,
    "cuemix#"
);
define_id!(
    /// A scratch pad: another arrangement of the project's tracks.
    ScratchPadId,
    "pad#"
);
define_id!(
    /// A reference track (a finished record compared with the mix).
    ReferenceId,
    "ref#"
);
define_id!(
    /// A mix scene: a snapshot of the console, stored and recalled.
    MixSceneId,
    "mixscene#"
);
define_id!(
    /// An articulation (expression) map: what each articulation of an
    /// instrument sends (keyswitches, programs, controllers, a channel).
    ArticulationMapId,
    "artmap#"
);
define_id!(
    /// A drum map: names, order and output notes of a track's drum sounds.
    DrumMapId,
    "drummap#"
);
define_id!(
    /// A state of the interactive score (music for a game): a section or a
    /// launcher scene that loops until the game asks for another.
    MusicStateId,
    "musicstate#"
);
define_id!(
    /// A layer of the interactive score: tracks heard together, faded in
    /// and out by parameters (intensity).
    MusicLayerId,
    "musiclayer#"
);
define_id!(
    /// A transition rule of the interactive score.
    TransitionId,
    "transition#"
);
define_id!(
    /// A stinger of the interactive score: a short phrase played over the
    /// music on a sync point.
    StingerId,
    "stinger#"
);
define_id!(
    /// A parameter of the interactive score (intensity, danger …).
    ScoreParamId,
    "scoreparam#"
);

/// Identifier of a parameter *within* a processor or plugin.
///
/// Plugin formats define their own parameter id spaces (CLAP: `u32`, VST3:
/// `u32`), so this is intentionally not project-unique.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParameterId(pub u32);

/// Monotonic allocator for project-unique IDs.
///
/// One counter is shared by all ID kinds; this keeps the persisted state
/// trivial and makes IDs unique even across kinds, which simplifies debugging.
///
/// While a project is shared live by several sessions (collaboration), each
/// session hands out ids from ranges set aside for it alone, its
/// [`IdLease`]: the counter itself moves only when a range is set aside,
/// the same way in every copy, so the copies stay alike and no two
/// sessions make the same id. The lease is no part of the project: files
/// never hold it and it never makes two allocators differ.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdAllocator {
    next: u64,
    #[serde(skip)]
    lease: Option<std::sync::Arc<IdLease>>,
}

impl PartialEq for IdAllocator {
    fn eq(&self, other: &Self) -> bool {
        self.next == other.next
    }
}

impl Eq for IdAllocator {}

impl Default for IdAllocator {
    fn default() -> Self {
        Self {
            next: 1,
            lease: None,
        }
    }
}

impl IdAllocator {
    /// Allocate a fresh ID of any kind (from the lease while there is one
    /// with ids left).
    pub fn allocate<T: From<u64>>(&mut self) -> T {
        if let Some(id) = self.lease.as_deref().and_then(IdLease::take) {
            return T::from(id);
        }
        let id = self.next;
        self.next += 1;
        T::from(id)
    }

    /// Make sure future allocations never collide with `used`.
    ///
    /// Called after loading/merging projects that may contain IDs the
    /// allocator has not seen.
    pub fn reserve_through(&mut self, used: u64) {
        if used >= self.next {
            self.next = used + 1;
        }
    }

    /// The next value that will be handed out.
    pub fn peek(&self) -> u64 {
        self.next
    }

    /// Set `count` ids aside past every id handed out so far (for a
    /// session's lease): the range `[start, end)`.
    pub fn set_aside(&mut self, count: u64) -> (u64, u64) {
        let start = self.next;
        self.next = self.next.saturating_add(count);
        (start, self.next)
    }

    /// The lease ids are handed out from (copies of the allocator share
    /// it, so ids drawn by a copy are never drawn again).
    pub fn lease(&self) -> Option<&std::sync::Arc<IdLease>> {
        self.lease.as_ref()
    }

    pub fn set_lease(&mut self, lease: Option<std::sync::Arc<IdLease>>) {
        self.lease = lease;
    }
}

/// Ranges of ids set aside for one session of a project shared live (see
/// [`IdAllocator`]), handed out in order.
#[derive(Debug, Default)]
pub struct IdLease {
    ranges: std::sync::Mutex<std::collections::VecDeque<(u64, u64)>>,
}

impl IdLease {
    fn ranges(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<(u64, u64)>> {
        self.ranges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Add the ids `[start, end)` (after the ranges already held).
    pub fn grant(&self, start: u64, end: u64) {
        if start < end {
            self.ranges().push_back((start, end));
        }
    }

    /// How many ids are left.
    pub fn remaining(&self) -> u64 {
        self.ranges().iter().map(|(a, b)| b - a).sum()
    }

    /// How many ranges are held (the current one and those after it).
    pub fn held(&self) -> usize {
        self.ranges().len()
    }

    fn take(&self) -> Option<u64> {
        let mut r = self.ranges();
        while let Some((start, end)) = r.front_mut() {
            if start < end {
                let id = *start;
                *start += 1;
                return Some(id);
            }
            r.pop_front();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_is_monotonic_and_reservable() {
        let mut alloc = IdAllocator::default();
        let a: TrackId = alloc.allocate();
        let b: ClipId = alloc.allocate();
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
        alloc.reserve_through(10);
        let c: TrackId = alloc.allocate();
        assert_eq!(c.raw(), 11);
        alloc.reserve_through(3);
        let d: TrackId = alloc.allocate();
        assert_eq!(d.raw(), 12);
    }

    #[test]
    fn a_lease_hands_out_its_ranges_and_then_the_counter() {
        let mut alloc = IdAllocator::default();
        let (a, b) = alloc.set_aside(3);
        assert_eq!((a, b), (1, 4));
        let lease = std::sync::Arc::new(IdLease::default());
        lease.grant(a, b);
        lease.grant(10, 11);
        alloc.set_lease(Some(lease.clone()));
        let mut copy = alloc.clone();
        let x: TrackId = copy.allocate();
        let y: TrackId = alloc.allocate();
        assert_eq!((x.raw(), y.raw()), (1, 2), "copies share the lease");
        assert_eq!(lease.remaining(), 2);
        let ids: Vec<u64> = (0..3).map(|_| alloc.allocate::<TrackId>().raw()).collect();
        assert_eq!(ids, [3, 10, 4], "then the counter");
        assert_eq!(lease.held(), 0);
        let mut bare = alloc.clone();
        bare.set_lease(None);
        assert_eq!(alloc, bare, "only the counter counts");
        assert_ne!(alloc, copy);
    }

    #[test]
    fn ids_format_with_kind_prefix() {
        assert_eq!(format!("{}", TrackId(7)), "track#7");
        assert_eq!(format!("{:?}", ClipId(3)), "clip#3");
    }
}
