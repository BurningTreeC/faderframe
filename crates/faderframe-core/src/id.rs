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
