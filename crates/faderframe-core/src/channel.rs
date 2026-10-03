//! Channel layouts.
//!
//! Tracks, buses and graph ports carry an explicit layout. Nothing in the DAW
//! may assume "everything is stereo". New layouts (surround, ambisonics) are
//! added as variants; [`ChannelLayout::Discrete`] covers arbitrary
//! multichannel routing in the meantime.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    Mono,
    Stereo,
    /// `n` unrelated channels (hardware I/O blocks, multichannel stems).
    Discrete(u16),
}

impl ChannelLayout {
    /// Number of audio channels carried by this layout.
    #[inline]
    pub const fn channel_count(self) -> usize {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
            ChannelLayout::Discrete(n) => n as usize,
        }
    }

    /// Short label for UI display.
    pub fn short_name(self) -> String {
        match self {
            ChannelLayout::Mono => "Mono".into(),
            ChannelLayout::Stereo => "Stereo".into(),
            ChannelLayout::Discrete(n) => format!("{n}ch"),
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
    }
}
