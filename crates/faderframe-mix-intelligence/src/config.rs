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

/// Thresholds of the health check ([`crate::health`]).
#[derive(Clone, Debug, PartialEq)]
pub struct HealthRule {
    /// A channel whose mean is at least this (dBFS) has a DC offset worth
    /// reporting (heuristic: −50 dBFS is 0.3 % of full scale — a level
    /// that costs headroom through a sum of tracks and thumps at edits and
    /// mutes; well-behaved converters and devices stay far below).
    pub dc_dbfs: f64,
    /// … over at least this much audio (seconds): the mean of a shorter
    /// stretch is swayed by low frequencies.
    pub dc_min_seconds: f64,
    /// A device "changes nothing" when what it adds or takes away is this
    /// many dB under its input (heuristic: 90 dB is far below anything
    /// heard, above the rounding noise of floating-point processing).
    pub no_change_db: f64,
    /// A device's input counts as signal from this RMS (dBFS): quieter
    /// and nothing can be said about what it does.
    pub signal_dbfs: f64,
    /// Its output counts as silent under this RMS (dBFS).
    pub silent_dbfs: f64,
    /// A device flattens peaks when its output has at least this many runs
    /// of equal-magnitude samples …
    pub min_flat_runs: u64,
    /// … and this many times as many as its input (so material that has
    /// them already — a constant, a square wave — does not count).
    pub flat_ratio: f64,
    /// A device's processing time (share of real time) at which a device
    /// that changes nothing counts fully in the severity (heuristic: 2 %
    /// of one core).
    pub cost_share: f64,
    /// The deepest trim an experiment puts before a device (dB).
    pub max_trim_db: f64,
}

impl Default for HealthRule {
    fn default() -> Self {
        Self {
            dc_dbfs: -50.0,
            dc_min_seconds: 2.0,
            no_change_db: 90.0,
            signal_dbfs: -80.0,
            silent_dbfs: -120.0,
            min_flat_runs: 10,
            flat_ratio: 4.0,
            cost_share: 0.02,
            max_trim_db: 12.0,
        }
    }
}

/// Which pairs of tracks are compared for phase and timing
/// ([`crate::phase`]), on what, and the thresholds of the phase rules.
/// Levels are band levels as in [`MaskingRule`].
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseRule {
    /// At most this many pairs are measured per analysis (each needs both
    /// tracks rendered over an excerpt and compared): the cost stays
    /// bounded whatever the number of tracks.
    pub max_pairs: usize,
    /// How long an excerpt is (seconds): about what the delay estimator
    /// compares at most (160 frames of 0.17 s at half overlap).
    pub excerpt_seconds: f64,
    /// Pairs playing together for less than this (seconds) in their best
    /// excerpt are not compared.
    pub min_together_seconds: f64,
    /// The furthest delay searched either way (ms): Auto-Align's default,
    /// about ten metres of air.
    pub max_lag_ms: f64,
    /// The delay estimator's confidence (0 to 1) from which a delay or a
    /// polarity is taken as measured (its own floor, 0.25, only says
    /// "some common signal"; heuristic).
    pub min_confidence: f64,
    /// How far the correlation's peak has to stand over the next one
    /// (from 0 to 1) before a delay or polarity is suggested: low clarity
    /// means two sources heard by both (bleed), or periodic material
    /// (heuristic).
    pub min_clarity: f64,
    /// Delays under this (ms) count as none: the first cancellation would
    /// lie above 10 kHz (heuristic).
    pub min_offset_ms: f64,
    /// Delays over this (ms) are heard as an echo, a doubling or a timing
    /// feel rather than as a colouring comb filter: no delay is suggested
    /// for them from the correlation (heuristic; comb filtering is
    /// usually discussed up to 10–20 ms).
    pub max_comb_ms: f64,
    /// A band counts as cancelling when the two tracks' sum holds this
    /// many dB less than the sum of their energies (heuristic; 3 dB is
    /// half the energy gone).
    pub cancel_db: f64,
    /// A correction (delay, polarity) is suggested only when it would
    /// bring back at least this much (dB, over the bands both tracks are
    /// relevant in; heuristic).
    pub min_gain_db: f64,
    /// Bands where the magnitude-squared coherence (0 to 1, debiased) is
    /// at least this are taken as sharing a signal (heuristic).
    pub min_coherence: f64,
    /// A band is relevant to the pair when both tracks are within this
    /// many dB of their loudest band and above `floor_db`.
    pub relevance_db: f64,
    pub floor_db: f64,
    /// Pairs whose band levels rise and fall together over time at least
    /// this much (a correlation, 0 to 1) are compared even without roles
    /// or a masking finding: a screening for "may share a source"
    /// (heuristic; the coherence measured afterwards decides).
    pub covary: f64,
}

impl Default for PhaseRule {
    fn default() -> Self {
        Self {
            max_pairs: 12,
            excerpt_seconds: 12.0,
            min_together_seconds: 2.0,
            max_lag_ms: 30.0,
            min_confidence: 0.5,
            min_clarity: 0.3,
            min_offset_ms: 0.05,
            max_comb_ms: 15.0,
            cancel_db: 3.0,
            min_gain_db: 1.0,
            min_coherence: 0.5,
            relevance_db: 30.0,
            floor_db: -60.0,
            covary: 0.8,
        }
    }
}

/// Thresholds of the onset timing rule ([`crate::phase::OnsetTiming`]).
#[derive(Clone, Debug, PartialEq)]
pub struct TimingRule {
    /// Onsets of the two tracks this close (ms) are taken as one played
    /// together (heuristic; wider takes in neighbouring notes).
    pub match_ms: f64,
    /// Matched onsets needed at least.
    pub min_onsets: usize,
    /// …and the share of the sparser track's onsets they make.
    pub min_share: f64,
    /// A median offset from this (ms) is reported (heuristic: tighter
    /// than this is heard as together; the onset detector itself is good
    /// to about a millisecond).
    pub flam_ms: f64,
    /// An offset is consistent when the spread (median absolute
    /// deviation) is under `consistent_ms` plus this share of the offset.
    pub consistent_ms: f64,
    pub consistent_share: f64,
    /// A spread from this (ms) between doubled parts is reported as loose
    /// timing (heuristic).
    pub loose_ms: f64,
    /// Offsets beyond this (ms) are not offered as a delay experiment
    /// (larger moves change the part, not its alignment).
    pub max_shift_ms: f64,
}

impl Default for TimingRule {
    fn default() -> Self {
        Self {
            match_ms: 50.0,
            min_onsets: 6,
            min_share: 0.5,
            flam_ms: 5.0,
            consistent_ms: 1.5,
            consistent_share: 0.35,
            loose_ms: 10.0,
            max_shift_ms: 30.0,
        }
    }
}

/// Thresholds of the width rules ([`crate::width`]).
#[derive(Clone, Debug, PartialEq)]
pub struct WidthRule {
    /// Under this frequency (Hz) a wide band is reported (the low end many
    /// systems play in mono: subwoofers, clubs, cutting lathes).
    pub low_end_hz: f64,
    /// A low band is wide when its side signal comes within this many dB
    /// of its mid signal (heuristic: −6 dB is a correlation of about 0.6
    /// between equal channels, or a source panned well off centre).
    pub wide_db: f64,
    /// …for at least this long (seconds).
    pub min_seconds: f64,
    /// Bands within this many dB of the loudest band (and above −60 dB)
    /// are judged.
    pub relevance_db: f64,
    /// The master's balance in mono: a band range whose mono fold-down
    /// changes by this many dB more than the whole (heuristic: panning
    /// and decorrelation alone move a band by at most 3 dB against a
    /// centred one, so more takes cancellation).
    pub mono_shift_db: f64,
}

impl Default for WidthRule {
    fn default() -> Self {
        Self {
            low_end_hz: 150.0,
            wide_db: -6.0,
            min_seconds: 1.0,
            relevance_db: 30.0,
            mono_shift_db: 4.0,
        }
    }
}

/// Thresholds of the dynamics rules ([`crate::dynamics`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicsRule {
    /// An envelope event: the 5 ms RMS rising this many dB over its lowest
    /// in the 50 ms before (heuristic; a soft swell does not count)…
    pub event_rise_db: f64,
    /// …with a peak above this (dBFS)…
    pub event_floor_db: f64,
    /// …at least this long after the previous one (seconds).
    pub event_gap_seconds: f64,
    /// Events needed for a transient-to-sustain figure.
    pub min_events: usize,
    /// Possible transient flattening: the source's attacks stand at least
    /// this far over their sustain (dB; there were transients to lose)…
    pub min_source_tsr_db: f64,
    /// …and the processed signal's stand this much less far (dB;
    /// heuristic).
    pub flatten_db: f64,
    /// A track's loudness range is reported from this (LU; heuristic:
    /// mixed tracks commonly measure 3–12 LU)…
    pub track_lra_lu: f64,
    /// …when it is also this much wider than the mix's (LU)…
    pub lra_margin_lu: f64,
    /// …over at least this much active time (seconds: the LRA is a
    /// programme measure and inflates on short, sparse material).
    pub min_active_seconds: f64,
    /// A step: the momentary loudness 2 s after a moment this many LU
    /// from the 2 s before, both playing (heuristic).
    pub step_lu: f64,
    /// A step the mix makes too (by at least this share of the track's)
    /// is the arrangement's, not the track's: left out.
    pub mirrored_share: f64,
}

impl Default for DynamicsRule {
    fn default() -> Self {
        Self {
            event_rise_db: 9.0,
            event_floor_db: -50.0,
            event_gap_seconds: 0.05,
            min_events: 8,
            min_source_tsr_db: 8.0,
            flatten_db: 4.0,
            track_lra_lu: 15.0,
            lra_margin_lu: 6.0,
            min_active_seconds: 20.0,
            step_lu: 10.0,
            mirrored_share: 0.5,
        }
    }
}

/// Thresholds of the comparisons with a reference track
/// ([`crate::reference`]). Every level difference is taken after matching
/// the loudness; a reference is a comparison target, never a standard.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceRule {
    /// A frequency range is reported where the mix and the (loudness-
    /// matched) reference differ by at least this much (dB, smoothed over
    /// neighbouring bands) …
    pub spectrum_db: f64,
    /// … over at least this many adjacent bands (third octaves: 3 = one
    /// octave; a narrower difference is more likely the arrangement — a
    /// note, an instrument — than the balance).
    pub min_bands: usize,
    /// Only bands within this many dB of the louder signal's loudest band
    /// are compared (the noise floor of either says nothing).
    pub relevance_db: f64,
    /// At most this many ranges are reported.
    pub max_ranges: usize,
    /// A section is reported when a broad region's balance there differs
    /// from the reference's by this much (dB) and by half this from the
    /// sections' typical difference (a difference the whole song has is the
    /// spectrum rule's).
    pub section_db: f64,
    /// Sections shorter than this (seconds) are left out.
    pub min_section_seconds: f64,
    /// Loudness differences reported from (LU): integrated, and the
    /// loudness range.
    pub loudness_lu: f64,
    pub range_lu: f64,
    /// A broad region is reported when the mix loses this much more in a
    /// mono fold-down than the reference (dB) …
    pub fold_db: f64,
    /// … or is this much wider or narrower (side against mid, dB).
    pub width_db: f64,
}

impl Default for ReferenceRule {
    fn default() -> Self {
        Self {
            spectrum_db: 4.0,
            min_bands: 3,
            relevance_db: 40.0,
            max_ranges: 3,
            section_db: 4.0,
            min_section_seconds: 4.0,
            loudness_lu: 2.0,
            range_lu: 4.0,
            fold_db: 3.0,
            width_db: 6.0,
        }
    }
}

/// When two tracks carry the same audio ([`crate::same`]): masking is not
/// judged there, and a finding says so.
#[derive(Clone, Debug, PartialEq)]
pub struct SameRule {
    /// The windows coherence is judged over (seconds, sliding by half):
    /// the time resolution of "the same in this section, not in that".
    pub window_seconds: f64,
    /// Same audio from this coherence (0 to 1; both its power-weighted and
    /// its plain mean over the bands that matter). Heuristic: independent
    /// sounds stay near 0, copies through gain, pan, EQ or a small delay
    /// near 1; time-varying processing (compression) a little under.
    pub coherence: f64,
    /// At least this many bands must matter (one steady tone could keep
    /// step with another at the frame rate by chance).
    pub min_bands: usize,
    /// Delays allowed for between copies (ms; band sums lose coherence
    /// beyond, see the module docs).
    pub max_lag_ms: f64,
    /// Frames both play in needed to judge a window.
    pub min_frames: usize,
    /// A band matters when both tracks are within this many dB of their
    /// loudest band in the window and above `floor_db`.
    pub relevance_db: f64,
    pub floor_db: f64,
    /// The same audio is reported from this long in all (seconds).
    pub min_seconds: f64,
}

impl Default for SameRule {
    fn default() -> Self {
        Self {
            window_seconds: 4.0,
            coherence: 0.85,
            min_bands: 2,
            max_lag_ms: 1.0,
            min_frames: 8,
            relevance_db: 30.0,
            floor_db: -60.0,
            min_seconds: 2.0,
        }
    }
}

/// Every rule's thresholds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    pub masking: MaskingRule,
    pub levels: LevelRule,
    pub stereo: StereoRule,
    pub health: HealthRule,
    pub reference: ReferenceRule,
    pub phase: PhaseRule,
    pub timing: TimingRule,
    pub width: WidthRule,
    pub dynamics: DynamicsRule,
    pub same: SameRule,
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
