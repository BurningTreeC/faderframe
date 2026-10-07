//! Audio sources, waveform peak caches and generated material.
//!
//! Current state: sources are fully decoded into memory ([`AudioData`]),
//! which is fine for generated material and short files. The planned
//! architecture for long recordings is a disk-streaming source (decoder
//! worker → lock-free read-ahead ring → realtime clip player) behind the
//! same source identity; the realtime side must never perform file I/O.
//!
//! [`PeakCache`] stores min/max pairs at several resolutions so the arranger
//! can draw any zoom level by touching only a few values per pixel column.

#![forbid(unsafe_code)]

pub mod bw64;
pub mod decode;
pub mod dither;
mod generate;
pub mod import;
pub mod onsets;
mod peaks;
pub mod resample;
pub mod stream;
pub mod wav;
pub mod wavstream;

pub use dither::Dither;
pub use generate::{GeneratorSpec, generate};
pub use peaks::{PeakBuilder, PeakCache, PeakLevel};
pub use stream::{PAGE_FRAMES, Page, StreamSource};
pub use wav::{WavData, WavFormat, read_wav, write_wav, write_wav_mask, write_wav_with};

/// Decoded, non-interleaved audio in memory. Immutable once shared, so the
/// realtime thread may read it through an `Arc` without synchronisation.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioData {
    sample_rate: u32,
    channels: Vec<Box<[f32]>>,
}

impl AudioData {
    /// Build from per-channel sample vectors (all must have equal length).
    pub fn from_channels(sample_rate: u32, channels: Vec<Vec<f32>>) -> Self {
        let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
        Self {
            sample_rate,
            channels: channels
                .into_iter()
                .map(|mut c| {
                    c.truncate(frames);
                    c.into_boxed_slice()
                })
                .collect(),
        }
    }

    pub fn silence(sample_rate: u32, channels: usize, frames: usize) -> Self {
        Self::from_channels(sample_rate, vec![vec![0.0; frames]; channels])
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[inline]
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    #[inline]
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }

    #[inline]
    pub fn channel(&self, ch: usize) -> &[f32] {
        &self.channels[ch]
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    /// Largest absolute sample value.
    pub fn peak(&self) -> f32 {
        self.channels
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_channels_truncates_to_shortest() {
        let d = AudioData::from_channels(48_000, vec![vec![0.5; 10], vec![-0.25; 8]]);
        assert_eq!(d.frames(), 8);
        assert_eq!(d.num_channels(), 2);
        assert_eq!(d.peak(), 0.5);
        assert!((d.duration_seconds() - 8.0 / 48_000.0).abs() < 1e-12);
    }
}
