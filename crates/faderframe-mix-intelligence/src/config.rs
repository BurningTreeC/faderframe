//! What an analysis measures with (the spectral resolution) and what the
//! diagnostic rules consider worth reporting (their thresholds).
//!
//! The thresholds are heuristics, named and documented where they are
//! defined, and part of every result's fingerprint: a result says which
//! settings (and which [`crate::ALGORITHM_VERSION`]) it came from.

use thiserror::Error;

/// The spectral analysis: FFT size and hop (samples) and the band layout.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisConfig {
    /// Power of two from [`MIN_FFT`] to [`MAX_FFT`].
    pub fft_size: usize,
    /// Samples between frames (1 to `fft_size`).
    pub hop: usize,
    /// Bands per octave (1 to 6; 3 = third octaves).
    pub bands_per_octave: u32,
    /// Lowest and highest band centres (Hz).
    pub lowest_hz: f64,
    pub highest_hz: f64,
}

pub const MIN_FFT: usize = 1024;
pub const MAX_FFT: usize = 32_768;

/// How long a frame is by default (seconds): long enough for bins of about
/// 6 Hz at 44.1/48 kHz (the bass), short enough to tell a kick's hit from
/// the bass note under it at the hop's 85 ms.
pub const DEFAULT_FRAME_SECONDS: f64 = 0.17;

#[derive(Debug, Error, PartialEq)]
pub enum ConfigError {
    #[error("the FFT size must be a power of two from {MIN_FFT} to {MAX_FFT} (not {0})")]
    FftSize(usize),
    #[error("the hop must be from 1 to the FFT size (not {0})")]
    Hop(usize),
    #[error("bands per octave must be from 1 to 6 (not {0})")]
    Bands(u32),
    #[error("the band range {0} to {1} Hz is not inside 10 Hz to Nyquist")]
    Range(f64, f64),
    #[error("the sample rate {0} Hz is not usable")]
    Rate(u32),
}

impl AnalysisConfig {
    /// The default for `rate`: frames of about [`DEFAULT_FRAME_SECONDS`]
    /// (8192 samples at 44.1 and 48 kHz, 16384 at 88.2 and 96 kHz),
    /// half overlapping (Hann windows at half overlap sum to a constant,
    /// so every sample counts the same), third octaves from 25 Hz to
    /// 20 kHz.
    pub fn for_rate(rate: u32) -> Self {
        let want = (f64::from(rate.max(8000)) * DEFAULT_FRAME_SECONDS) as usize;
        let fft = want.next_power_of_two().clamp(MIN_FFT, MAX_FFT);
        // Round to the nearer power of two (0.17 s at 48 kHz is 8160).
        let fft = if fft / 2 >= MIN_FFT && (fft - want) > (want - fft / 2) {
            fft / 2
        } else {
            fft
        };
        Self {
            fft_size: fft,
            hop: fft / 2,
            bands_per_octave: 3,
            lowest_hz: 25.0,
            highest_hz: 20_000.0,
        }
    }

    pub fn validate(&self, rate: u32) -> Result<(), ConfigError> {
        if !(8000..=768_000).contains(&rate) {
            return Err(ConfigError::Rate(rate));
        }
        if !self.fft_size.is_power_of_two() || !(MIN_FFT..=MAX_FFT).contains(&self.fft_size) {
            return Err(ConfigError::FftSize(self.fft_size));
        }
        if self.hop == 0 || self.hop > self.fft_size {
            return Err(ConfigError::Hop(self.hop));
        }
        if !(1..=6).contains(&self.bands_per_octave) {
            return Err(ConfigError::Bands(self.bands_per_octave));
        }
        let nyquist = f64::from(rate) / 2.0;
        if !(self.lowest_hz >= 10.0 && self.highest_hz > self.lowest_hz && self.lowest_hz < nyquist)
            || !self.highest_hz.is_finite()
        {
            return Err(ConfigError::Range(self.lowest_hz, self.highest_hz));
        }
        Ok(())
    }

    /// Seconds between frames at `rate`.
    pub fn hop_seconds(&self, rate: u32) -> f64 {
        self.hop as f64 / f64::from(rate.max(1))
    }

    /// The band layout at `rate`.
    pub fn layout(&self, rate: u32) -> crate::bands::BandLayout {
        crate::bands::BandLayout::fractional_octave(
            self.bands_per_octave,
            self.lowest_hz,
            self.highest_hz,
            f64::from(rate) / 2.0,
        )
    }

    /// A stable number for everything that changes measurements (cache
    /// keys).
    pub fn fingerprint(&self) -> u64 {
        let mut h = crate::Fnv::new();
        h.write_u64(self.fft_size as u64);
        h.write_u64(self.hop as u64);
        h.write_u64(u64::from(self.bands_per_octave));
        h.write_u64(self.lowest_hz.to_bits());
        h.write_u64(self.highest_hz.to_bits());
        h.write_u64(u64::from(crate::ALGORITHM_VERSION));
        h.finish()
    }
}

/// Thresholds of the spectral masking rule ([`crate::masking`]). All
/// levels are band levels in dB (mean-square power of the band, dBFS: a
/// full-scale sine reads −3 dB in its band).
#[derive(Clone, Debug, PartialEq)]
pub struct MaskingRule {
    /// Under this a band is treated as silent (heuristic: about where a
    /// band stops being heard in a mix played at a normal level).
    pub floor_db: f64,
    /// A target band counts when it is within this many dB of the
    /// target's loudest band in the same frame: a prominent part of what
    /// the target sounds like then, not its noise floor or a far
    /// harmonic (heuristic; the EQ's collision display uses 30 dB).
    pub prominence_db: f64,
    /// How the competing level counts: not at all when the masker is
    /// `cover_lo_db` or more under the target in the band, fully when it
    /// is at `cover_hi_db` relative to the target or louder, linearly in
    /// between (heuristic level-proximity weighting, not a psychoacoustic
    /// masking threshold).
    pub cover_lo_db: f64,
    pub cover_hi_db: f64,
    /// How much a fully different stereo placement still counts (0 to 1):
    /// separation lessens masking but does not remove it, and a mono
    /// playback takes it away (heuristic).
    pub spatial_floor: f64,
    /// A frame-band counts as covered from this conflict value (0 to 1).
    pub strong: f64,
    /// A range is reported when it is covered for at least this long in
    /// all (seconds): shorter overlaps are too brief to matter at this
    /// time resolution.
    pub min_seconds: f64,
    /// Covered stretches closer than this (seconds) are one event.
    pub merge_gap_seconds: f64,
    /// At most this many frequency ranges per track pair and direction.
    pub max_ranges: usize,
    /// Pairs whose combined severity is under this are not reported
    /// (0 to 1).
    pub min_severity: f64,
}

impl Default for MaskingRule {
    fn default() -> Self {
        Self {
            floor_db: -60.0,
            prominence_db: 24.0,
            cover_lo_db: -12.0,
            cover_hi_db: 0.0,
            spatial_floor: 0.5,
            strong: 0.5,
            min_seconds: 1.0,
            merge_gap_seconds: 1.0,
            max_ranges: 2,
            min_severity: 0.1,
        }
    }
}

/// Thresholds of the level rules ([`crate::levels`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LevelRule {
    /// The highest true peak a master should reach (dBTP; EBU R128 and
    /// most streaming services ask for −1 dBTP).
    pub master_true_peak_dbtp: f64,
    /// Adjacent sections whose integrated loudness differs by this much
    /// (LU) are reported as a large step (heuristic; such steps are often
    /// intentional, which the finding says).
    pub section_step_lu: f64,
    /// Sections shorter than this (seconds) are left out of the section
    /// comparison (BS.1770 integration needs time).
    pub min_section_seconds: f64,
}

impl Default for LevelRule {
    fn default() -> Self {
        Self {
            master_true_peak_dbtp: -1.0,
            section_step_lu: 8.0,
            min_section_seconds: 4.0,
        }
    }
}

/// Thresholds of the stereo rules ([`crate::stereo`]).
#[derive(Clone, Debug, PartialEq)]
pub struct StereoRule {
    /// A band whose mono fold-down ((L+R)/2) is this many dB under the
    /// mean of its channels is reported (a fully uncorrelated band loses
    /// 3 dB, which is normal for a wide sound; 6 dB means the side signal
    /// is as strong as three times the mid).
    pub fold_loss_db: f64,
    /// Only bands within this many dB of the loudest band are judged (a
    /// quiet band's loss is not heard).
    pub relevance_db: f64,
    /// Under this frequency (Hz) a fold-down loss counts double (low end
    /// meant to be mono: cutting, clubs, mono playback).
    pub low_end_hz: f64,
    /// An overall correlation at or under this, with the two channels at
    /// similar levels, suggests one channel's polarity is inverted.
    pub inverted_correlation: f64,
    /// Seconds the condition has to hold at least.
    pub min_seconds: f64,
}

impl Default for StereoRule {
    fn default() -> Self {
        Self {
            fold_loss_db: 6.0,
            relevance_db: 30.0,
            low_end_hz: 150.0,
            inverted_correlation: -0.6,
            min_seconds: 1.0,
        }
    }
}

/// Every rule's thresholds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    pub masking: MaskingRule,
    pub levels: LevelRule,
    pub stereo: StereoRule,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_rate_and_validate() {
        let c = AnalysisConfig::for_rate(48_000);
        assert_eq!((c.fft_size, c.hop), (8192, 4096));
        assert_eq!(AnalysisConfig::for_rate(44_100).fft_size, 8192);
        assert_eq!(AnalysisConfig::for_rate(96_000).fft_size, 16_384);
        assert!(c.validate(48_000).is_ok());
        let bad = AnalysisConfig {
            fft_size: 1000,
            ..c.clone()
        };
        assert_eq!(bad.validate(48_000), Err(ConfigError::FftSize(1000)));
        let bad = AnalysisConfig {
            hop: 0,
            ..c.clone()
        };
        assert_eq!(bad.validate(48_000), Err(ConfigError::Hop(0)));
        assert_eq!(c.validate(100), Err(ConfigError::Rate(100)));
        assert_ne!(
            c.fingerprint(),
            AnalysisConfig {
                hop: 2048,
                ..c.clone()
            }
            .fingerprint()
        );
        assert_eq!(
            c.fingerprint(),
            AnalysisConfig::for_rate(48_000).fingerprint()
        );
    }
}
