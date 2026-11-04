//! Hearing the master as listeners will: through a lossy codec, at the
//! loudness a streaming platform plays it.
//!
//! * [`CodecChain`] codes and decodes the master through Opus (libopus via
//!   the `opus` crate, 48 kHz, 20 ms frames, any bitrate from 16 to 510
//!   kb/s a stereo pair, VBR or CBR) in real time, with the original
//!   beside it delayed by exactly as much ([`resample`] goes to 48 kHz and
//!   back with a delay known to the sample), so the decoded signal, the
//!   original and their difference — the artefacts alone — line up.
//! * [`CodecMeter`] measures what the codec does: the true peak after
//!   decoding with the frames over full scale, the level of what it
//!   removed or added.
//! * [`PLATFORMS`] holds the streaming platforms' loudness normalisation
//!   (target, whether quiet masters are turned up, peak handling) and
//!   usual codec; where that codec has no permissive encoder, Opus at the
//!   same bitrate stands in, and says so.
//! * [`preview`] reports what each platform does to a rendered master.
//!
//! The engine's listening node runs a chain on the audio thread (see
//! `faderframe_engine::nodes::codec`); everything here allocates only when
//! made. Builds without libopus (`--cfg faderframe_check_only`) refuse
//! [`CoderKind::Opus`].

#![forbid(unsafe_code)]

pub mod chain;
pub mod meter;
pub mod platform;
pub mod preview;
pub mod resample;
pub mod stream;

pub use chain::{CHUNK, CodecChain, FRAME, MAX_CHANNELS};
pub use meter::CodecMeter;
pub use platform::{PLATFORMS, Platform, PlatformGain, TurnUp, platform};
pub use preview::{Master, PlatformPreview, Risk, Source};

use faderframe_core::ChannelLayout;

/// The bitrates menus offer (kb/s a stereo pair).
pub const BITRATES: [u32; 8] = [48, 64, 96, 128, 160, 192, 256, 320];
/// Opus' range (kb/s a stereo pair; a mono substream gets half).
pub const MIN_KBPS: u32 = 16;
pub const MAX_KBPS: u32 = 510;

/// A bitrate within Opus' range.
pub fn clamp_kbps(kbps: u32) -> u32 {
    kbps.clamp(MIN_KBPS, MAX_KBPS)
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("Opus is not in this build")]
    Unavailable,
    #[error("Opus: {0}")]
    Opus(String),
    #[error("the codec monitor does not run at {0} Hz")]
    Rate(u32),
    #[error("the codec monitor does not take {0} channels")]
    Channels(usize),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Read(String),
}

/// What codes the frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoderKind {
    /// Opus (libopus).
    Opus,
    /// Frames passed through unchanged with Opus' framing and lookahead:
    /// the same delay, nothing coded (alignment checks, builds without
    /// libopus).
    Identity,
}

/// How a chain codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecSettings {
    pub kind: CoderKind,
    /// Kilobits a second for a stereo pair (a mono substream gets half).
    pub kbps: u32,
    /// Variable bitrate (unconstrained, as files are coded), else constant.
    pub vbr: bool,
}

impl CodecSettings {
    pub fn opus(kbps: u32, vbr: bool) -> Self {
        Self {
            kind: CoderKind::Opus,
            kbps: clamp_kbps(kbps),
            vbr,
        }
    }
}

/// Opus is in this build.
pub fn opus_available() -> bool {
    cfg!(not(faderframe_check_only))
}

/// How a layout's channels are coded: `(first, count)` per substream —
/// a speaker and its mirror on the other side as a coupled pair
/// (L/R, Ls/Rs, the heights), the rest (centre, LFE, a lone speaker) mono.
pub fn substreams(layout: ChannelLayout) -> Vec<(usize, usize)> {
    match layout {
        ChannelLayout::Mono => vec![(0, 1)],
        ChannelLayout::Stereo => vec![(0, 2)],
        ChannelLayout::Surround(f) => {
            let speakers = f.speakers();
            let mut out = Vec::new();
            let mut i = 0;
            while i < speakers.len() {
                let a = &speakers[i];
                let pair = speakers.get(i + 1).is_some_and(|b| {
                    a.side() == Some(faderframe_core::surround::Side::Left)
                        && b.side() == Some(faderframe_core::surround::Side::Right)
                        && (a.azimuth + b.azimuth).abs() < 1.0
                        && (a.elevation - b.elevation).abs() < 1.0
                });
                if pair {
                    out.push((i, 2));
                    i += 2;
                } else {
                    out.push((i, 1));
                    i += 1;
                }
            }
            out
        }
        other => {
            // Any other layout: consecutive pairs, a mono one left over.
            let n = other.channel_count();
            (0..n)
                .step_by(2)
                .map(|i| (i, if i + 1 < n { 2 } else { 1 }))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::SurroundFormat;

    #[test]
    fn beds_are_coded_in_mirrored_pairs() {
        assert_eq!(substreams(ChannelLayout::Stereo), vec![(0, 2)]);
        assert_eq!(substreams(ChannelLayout::Mono), vec![(0, 1)]);
        // 5.1: L R | C | LFE | Ls Rs.
        let s = substreams(ChannelLayout::Surround(SurroundFormat::S51));
        assert_eq!(s, vec![(0, 2), (2, 1), (3, 1), (4, 2)]);
        let covered: usize = substreams(ChannelLayout::Surround(SurroundFormat::S714))
            .iter()
            .map(|g| g.1)
            .sum();
        assert_eq!(covered, 12);
    }
}
