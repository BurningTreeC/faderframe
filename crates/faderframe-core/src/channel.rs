//! Channel layouts.
//!
//! Tracks, buses and graph ports carry an explicit layout. Nothing in the DAW
//! may assume "everything is stereo". Surround beds are
//! [`ChannelLayout::Surround`] (see [`crate::surround`]), ambisonic sound
//! fields [`ChannelLayout::Ambisonic`] (see [`crate::ambisonics`]);
//! [`ChannelLayout::Discrete`] covers arbitrary multichannel routing.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    Mono,
    Stereo,
    /// `n` unrelated channels (hardware I/O blocks, multichannel stems).
    Discrete(u16),
    /// A surround bed (5.1, 7.1.4, …).
    Surround(crate::surround::SurroundFormat),
    /// An ambisonic sound field (AmbiX: ACN order, SN3D), first to third
    /// order.
    Ambisonic(crate::ambisonics::AmbiOrder),
}

impl ChannelLayout {
    /// Number of audio channels carried by this layout.
    #[inline]
    pub const fn channel_count(self) -> usize {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::Discrete(n) => n as usize,
            ChannelLayout::Surround(f) => f.channels(),
            ChannelLayout::Ambisonic(o) => o.channels(),
        }
    }

    /// Whether it is spatial: a surround bed or an ambisonic field (what
    /// mono and stereo sources are panned into, and what folds or decodes
    /// into other layouts).
    pub fn is_spatial(self) -> bool {
        matches!(
            self,
            ChannelLayout::Surround(_) | ChannelLayout::Ambisonic(_)
        )
    }

    /// Short label for UI display.
    pub fn short_name(self) -> String {
        match self {
            ChannelLayout::Mono => "Mono".into(),
            ChannelLayout::Stereo => "Stereo".into(),
            ChannelLayout::Discrete(n) => format!("{n}ch"),
            ChannelLayout::Surround(f) => f.name().into(),
            ChannelLayout::Ambisonic(o) => o.short_name().into(),
        }
    }

    /// Layout with exactly `n` channels, preferring the named layouts.
    pub fn from_channel_count(n: usize) -> Self {
        match n {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            n => ChannelLayout::Discrete(n.min(u16::MAX as usize) as u16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_counts() {
        assert_eq!(ChannelLayout::Mono.channel_count(), 1);
        assert_eq!(ChannelLayout::Stereo.channel_count(), 2);
        assert_eq!(ChannelLayout::Discrete(6).channel_count(), 6);
        assert_eq!(ChannelLayout::from_channel_count(2), ChannelLayout::Stereo);
        assert_eq!(
            ChannelLayout::from_channel_count(8),
            ChannelLayout::Discrete(8)
        );
        let third = ChannelLayout::Ambisonic(crate::AmbiOrder::Third);
        assert_eq!(third.channel_count(), 16);
        assert!(third.is_spatial() && !ChannelLayout::Discrete(16).is_spatial());
    }

    /// Ambisonic layouts are written by their order and read back.
    #[test]
    fn ambisonic_layouts_in_files() {
        let first = ChannelLayout::Ambisonic(crate::AmbiOrder::First);
        let json = serde_json::to_string(&first).unwrap();
        assert_eq!(json, r#"{"ambisonic":1}"#);
        assert_eq!(serde_json::from_str::<ChannelLayout>(&json).unwrap(), first);
        assert!(serde_json::from_str::<ChannelLayout>(r#"{"ambisonic":5}"#).is_err());
    }
}
