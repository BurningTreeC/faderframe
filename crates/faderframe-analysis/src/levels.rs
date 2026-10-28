//! Levels of a finished file, fed piece by piece as it is read back (the
//! render statistics): per channel the sample peak, true peak (4×
//! oversampled, BS.1770 Annex 2), RMS, samples at full scale and overs;
//! over all channels BS.1770 loudness with each channel's weight
//! ([`WeightedLoudness`]: integrated, range, maxima).
//!
//! Nothing is estimated: a quantity the file cannot tell (loudness of
//! object audio, which only a renderer places) is left out by the caller
//! (`weights: None`), never filled in.

use crate::loudness::{TP_LATENCY, TruePeak, WeightedLoudness};

/// Consecutive samples at full scale that make an "over" (the usual
/// digital meter rule).
pub const OVER_RUN: usize = 3;

fn db(v: f64) -> f64 {
    if v > 0.0 {
        20.0 * v.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The largest magnitude a sample format holds: an integer format's
/// largest positive code, 1.0 (0 dBFS) for floating point.
pub fn full_scale(bits: u16, float: bool) -> f32 {
    if float || bits < 2 {
        1.0
    } else {
        let half = 2f64.powi(i32::from(bits) - 1);
        ((half - 1.0) / half) as f32
    }
}

#[derive(Clone, Debug)]
struct Channel {
    peak: f32,
    sum_square: f64,
    true_peak: TruePeak,
    max_true_peak: f64,
    at_full_scale: u64,
    above_full_scale: u64,
    run: usize,
    overs: u64,
}

/// One channel's levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelLevels {
    /// Highest sample magnitude (dBFS).
    pub sample_peak: f64,
    /// Highest interpolated peak (dBTP).
    pub true_peak: f64,
    /// RMS, unweighted (dBFS; a full-scale sine reads −3.0).
    pub rms: f64,
    /// Samples at (or past) full scale: clipped, or about to clip.
    pub full_scale_samples: u64,
    /// Samples past 0 dBFS (floating point only; they clip when the file
    /// is converted to a fixed-point format).
    pub above_full_scale: u64,
    /// Runs of [`OVER_RUN`] or more samples at full scale.
    pub overs: u64,
}

/// BS.1770 loudness of the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileLoudness {
    /// Integrated loudness (LUFS; `-inf` for silence or under 400 ms).
    pub integrated: f64,
    /// Loudness range (LU; 0 under 3 s).
    pub range: f64,
    /// Highest momentary (400 ms) loudness (LUFS).
    pub max_momentary: f64,
    /// Highest short-term (3 s) loudness (LUFS; `-inf` under 3 s).
    pub max_short_term: f64,
}

/// What a file measures.
#[derive(Clone, Debug, PartialEq)]
pub struct FileLevels {
    pub frames: u64,
    pub channels: Vec<ChannelLevels>,
    /// The highest of the channels' (dBFS / dBTP).
    pub sample_peak: f64,
    pub true_peak: f64,
    /// RMS of all channels together (dBFS).
    pub rms: f64,
    /// `None` when the caller did not ask for it (object audio).
    pub loudness: Option<FileLoudness>,
    pub full_scale_samples: u64,
    pub above_full_scale: u64,
    pub overs: u64,
}

/// Measures a file's channels as they are read (see the module).
#[derive(Clone, Debug)]
pub struct FileMeter {
    full_scale: f32,
    channels: Vec<Channel>,
    loudness: Option<WeightedLoudness>,
    frames: u64,
}

impl FileMeter {
    /// `channels` channels at `sample_rate`; `weights` their BS.1770
    /// weights for the loudness (`None`: not measured); `full_scale` the
    /// format's largest magnitude ([`full_scale`]).
    pub fn new(
        sample_rate: u32,
        channels: usize,
        weights: Option<&[f64]>,
        full_scale: f32,
    ) -> Self {
        Self {
            full_scale,
            channels: (0..channels)
                .map(|_| Channel {
                    peak: 0.0,
                    sum_square: 0.0,
                    true_peak: TruePeak::new(),
                    max_true_peak: 0.0,
                    at_full_scale: 0,
                    above_full_scale: 0,
                    run: 0,
                    overs: 0,
                })
                .collect(),
            loudness: weights.map(|w| WeightedLoudness::new(sample_rate, w)),
            frames: 0,
        }
    }

    /// Feed planar audio (`audio[c]`, equal lengths).
    pub fn add(&mut self, audio: &[&[f32]]) {
        let frames = audio.iter().map(|c| c.len()).min().unwrap_or(0);
        for (ch, samples) in self.channels.iter_mut().zip(audio) {
            for &x in &samples[..frames] {
                let a = x.abs();
                ch.peak = ch.peak.max(a);
                ch.sum_square += f64::from(x) * f64::from(x);
                ch.max_true_peak = ch.max_true_peak.max(ch.true_peak.run(x));
                if a >= self.full_scale {
                    ch.at_full_scale += 1;
                    ch.run += 1;
                    if ch.run == OVER_RUN {
                        ch.overs += 1;
                    }
                } else {
                    ch.run = 0;
                }
                if a > 1.0 {
                    ch.above_full_scale += 1;
                }
            }
        }
        if let Some(l) = &mut self.loudness {
            l.add(audio);
        }
        self.frames += frames as u64;
    }

    /// The loudness meter (`None` when the loudness is not measured): its
    /// short-term and momentary values over time.
    pub fn loudness_meter(&self) -> Option<&WeightedLoudness> {
        self.loudness.as_ref()
    }

    /// The levels of everything fed.
    pub fn finish(mut self) -> FileLevels {
        let frames = self.frames.max(1) as f64;
        let mut all_square = 0.0;
        let channels: Vec<ChannelLevels> = self
            .channels
            .iter_mut()
            .map(|ch| {
                // The interpolator's latency: the last samples' peaks.
                for _ in 0..TP_LATENCY {
                    ch.max_true_peak = ch.max_true_peak.max(ch.true_peak.run(0.0));
                }
                all_square += ch.sum_square;
                ChannelLevels {
                    sample_peak: db(f64::from(ch.peak)),
                    true_peak: db(ch.max_true_peak),
                    rms: db((ch.sum_square / frames).sqrt()),
                    full_scale_samples: ch.at_full_scale,
                    above_full_scale: ch.above_full_scale,
                    overs: ch.overs,
                }
            })
            .collect();
        let n = channels.len().max(1) as f64;
        FileLevels {
            frames: self.frames,
            sample_peak: channels
                .iter()
                .map(|c| c.sample_peak)
                .fold(f64::NEG_INFINITY, f64::max),
            true_peak: channels
                .iter()
                .map(|c| c.true_peak)
                .fold(f64::NEG_INFINITY, f64::max),
            rms: db((all_square / (frames * n)).sqrt()),
            loudness: self.loudness.as_ref().map(|l| FileLoudness {
                integrated: l.integrated(),
                range: l.range(),
                max_momentary: l.max_momentary(),
                max_short_term: l.max_short_term(),
            }),
            full_scale_samples: channels.iter().map(|c| c.full_scale_samples).sum(),
            above_full_scale: channels.iter().map(|c| c.above_full_scale).sum(),
            overs: channels.iter().map(|c| c.overs).sum(),
            channels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, amp: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| amp * (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin())
            .collect()
    }

    #[test]
    fn a_sine_measures_its_level() {
        let rate = 48_000;
        // −6 dBFS peak at 997 Hz on both channels.
        let amp = 10f32.powf(-6.0 / 20.0);
        let s = sine(rate, 997.0, amp, 10.0);
        let mut m = FileMeter::new(rate, 2, Some(&[1.0, 1.0]), full_scale(24, false));
        for c in s.chunks(5_000) {
            m.add(&[c, c]);
        }
        let l = m.finish();
        assert_eq!(l.frames, s.len() as u64);
        assert!((l.sample_peak + 6.0).abs() < 0.01, "{}", l.sample_peak);
        assert!(l.true_peak >= l.sample_peak - 1e-9 && l.true_peak < -5.9);
        assert!((l.rms + 9.01).abs() < 0.02, "{}", l.rms);
        assert!((l.channels[1].rms + 9.01).abs() < 0.02);
        let loud = l.loudness.unwrap();
        // Stereo 997 Hz at −6 dBFS reads −6 LUFS (0 dBFS in both: 0 LUFS).
        assert!((loud.integrated + 6.0).abs() < 0.1, "{}", loud.integrated);
        assert!(loud.range < 0.1);
        assert_eq!(
            (l.full_scale_samples, l.overs, l.above_full_scale),
            (0, 0, 0)
        );
    }

    #[test]
    fn clipped_samples_and_overs_are_counted() {
        let rate = 48_000;
        // A sine at +3.5 dBFS clipped at full scale (as a 16-bit file
        // holds it): long runs at the rails.
        let fs = full_scale(16, false);
        let hot: Vec<f32> = sine(rate, 100.0, 1.5, 1.0)
            .into_iter()
            .map(|v| v.clamp(-1.0, fs))
            .collect();
        let mut m = FileMeter::new(rate, 1, Some(&[1.0]), fs);
        m.add(&[&hot]);
        let l = m.finish();
        assert!(l.full_scale_samples > 1000, "{}", l.full_scale_samples);
        // Every half cycle sits on a rail once: 200 runs.
        assert_eq!(l.overs, 200);
        assert_eq!(l.above_full_scale, 0);
        // A float file keeps them: past 0 dBFS, counted too.
        let unclipped = sine(rate, 100.0, 1.5, 1.0);
        let mut f = FileMeter::new(rate, 1, None, full_scale(32, true));
        f.add(&[&unclipped]);
        let l = f.finish();
        assert!(l.above_full_scale > 1000 && l.overs == 200);
        assert!((l.sample_peak - 3.52).abs() < 0.01, "{}", l.sample_peak);
        assert!(l.loudness.is_none(), "not asked for");
        // A lone sample at the rail is no over.
        let mut one = vec![0.0f32; 1000];
        one[500] = 1.0;
        let mut m = FileMeter::new(rate, 1, None, 1.0);
        m.add(&[&one]);
        let l = m.finish();
        assert_eq!((l.full_scale_samples, l.overs), (1, 0));
    }

    #[test]
    fn full_scale_is_the_largest_code() {
        assert_eq!(full_scale(16, false), 32_767.0 / 32_768.0);
        assert_eq!(full_scale(24, false), 8_388_607.0 / 8_388_608.0);
        assert_eq!(full_scale(32, true), 1.0);
        let silent = FileMeter::new(48_000, 2, Some(&[1.0, 1.0]), 1.0).finish();
        assert_eq!(silent.sample_peak, f64::NEG_INFINITY);
        assert_eq!(
            silent.loudness.unwrap().integrated,
            f64::NEG_INFINITY,
            "nothing to measure"
        );
    }
}
