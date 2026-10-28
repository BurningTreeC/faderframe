//! Strongly typed identifiers.
//!
//! IDs are opaque, project-unique `u64` values. They are persisted in project
//! files, so they must never be derived from memory addresses or runtime
//! indices. Use [`IdAllocator`] (owned by the project) to create new ones.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

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
    TrackId, "track#"
);
define_id!(
    /// An audio or MIDI clip on the timeline.
    ClipId, "clip#"
);
define_id!(
    /// An aux/bus send on a track.
    SendId, "send#"
);
define_id!(
    /// A hosted plugin instance (insert or instrument).
    PluginInstanceId, "plugin#"
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
    AudioSourceId, "source#"
);
define_id!(
    /// A group of tracks whose controls are linked.
    GroupId, "group#"
);
define_id!(
    /// A named section of the arrangement (Intro, Verse, Chorus …).
    SectionId, "section#"
);
define_id!(
    /// A timeline marker.
    MarkerId, "marker#"
);
define_id!(
    /// A cue of dialogue to replace (ADR).
    AdrCueId, "cue#"
);
define_id!(
    /// A song of the album.
    SongId, "song#"
);
define_id!(
    /// An automation lane.
    AutomationLaneId, "lane#"
);
define_id!(
    /// A controller mapping (MIDI learn).
    MidiMappingId, "mapping#"
);
define_id!(
    /// A note inside a MIDI clip.
    NoteId, "note#"
);
define_id!(
    /// Clips sharing their content (aliases).
    ClipLinkId, "link#"
);
define_id!(
    /// A modulator of a track (LFO, follower, steps, random, macro).
    ModulatorId, "mod#"
);
define_id!(
    /// A scene (row) of the clip launcher.
    SceneId, "scene#"
);
define_id!(
    /// A video file the project shows.
    VideoSourceId, "video#"
);
define_id!(
    /// A placed stretch of a video on a video track.
    VideoClipId, "vclip#"
);
define_id!(
    /// A video track.
    VideoTrackId, "vtrack#"
);
define_id!(
    /// A cue mix (a performer's headphone mix on its own outputs).
    CueId, "cuemix#"
);
define_id!(
    /// A scratch pad: another arrangement of the project's tracks.
    ScratchPadId, "pad#"
);
define_id!(
    /// A reference track (a finished record compared with the mix).
    ReferenceId, "ref#"
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdAllocator {
    next: u64,
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl IdAllocator {
    /// Allocate a fresh ID of any kind.
    pub fn allocate<T: From<u64>>(&mut self) -> T {
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
    fn ids_format_with_kind_prefix() {
        assert_eq!(format!("{}", TrackId(7)), "track#7");
        assert_eq!(format!("{:?}", ClipId(3)), "clip#3");
    }
}
