use crate::AudioData;

/// Min/max pairs at one resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct PeakLevel {
    pub frames_per_peak: usize,
    /// `[channel][peak] = (min, max)`.
    pub peaks: Vec<Vec<(f32, f32)>>,
}

/// Multi-resolution waveform overview.
///
/// Level 0 aggregates [`PeakCache::BASE_FRAMES_PER_PEAK`] frames per peak;
/// every further level aggregates [`PeakCache::LEVEL_FACTOR`] peaks of the
/// previous one. Memory is about 1/24 of the source (f32 min+max per 64
/// frames, ×4/3 for the pyramid).
#[derive(Clone, Debug, PartialEq)]
pub struct PeakCache {
    frames: usize,
    channels: usize,
    levels: Vec<PeakLevel>,
}

impl PeakCache {
    pub const BASE_FRAMES_PER_PEAK: usize = 64;
    pub const LEVEL_FACTOR: usize = 4;

    pub fn build(data: &AudioData) -> Self {
        let frames = data.frames();
        let channels = data.num_channels();
        let mut levels = Vec::new();
        let base = PeakLevel {
            frames_per_peak: Self::BASE_FRAMES_PER_PEAK,
            peaks: (0..channels)
                .map(|c| {
                    data.channel(c)
                        .chunks(Self::BASE_FRAMES_PER_PEAK)
                        .map(|chunk| {
                            chunk
                                .iter()
                                .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)))
                        })
                        .collect()
                })
                .collect(),
        };
        levels.push(base);
        loop {
            let prev = levels.last().expect("at least the base level exists");
            if prev.peaks.first().is_none_or(|p| p.len() <= 2) {
                break;
            }
            let next = PeakLevel {
                frames_per_peak: prev.frames_per_peak * Self::LEVEL_FACTOR,
                peaks: prev
                    .peaks
                    .iter()
                    .map(|ch| {
                        ch.chunks(Self::LEVEL_FACTOR)
                            .map(|c| {
                                c.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| {
                                    (lo.min(a), hi.max(b))
                                })
                            })
                            .collect()
                    })
                    .collect(),
            };
            levels.push(next);
        }
        Self {
            frames,
            channels,
            levels,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn levels(&self) -> &[PeakLevel] {
        &self.levels
    }

    /// Coarsest level that still has at least one peak per `span` frames.
    fn level_for_span(&self, span: usize) -> &PeakLevel {
        let mut best = &self.levels[0];
        for l in &self.levels {
            if l.frames_per_peak <= span.max(1) {
                best = l;
            }
        }
        best
    }

    /// Min/max of `channel` over frames `[start, end)`; `None` when the
    /// range lies outside the source. Uses the coarsest adequate level, so
    /// cost is O(LEVEL_FACTOR) per call regardless of the span.
    pub fn min_max(&self, channel: usize, start: i64, end: i64) -> Option<(f32, f32)> {
        if channel >= self.channels || end <= 0 || start >= self.frames as i64 || end <= start {
            return None;
        }
        let start = start.max(0) as usize;
        let end = (end as usize).min(self.frames);
        let level = self.level_for_span(end - start);
        let fpp = level.frames_per_peak;
        let peaks = &level.peaks[channel];
        let a = start / fpp;
        let b = end.div_ceil(fpp).min(peaks.len());
        if a >= b {
            return None;
        }
        Some(
            peaks[a..b]
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &(x, y)| {
                    (lo.min(x), hi.max(y))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> AudioData {
        let ch: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
        let neg: Vec<f32> = ch.iter().map(|v| -v).collect();
        AudioData::from_channels(48_000, vec![ch, neg])
    }

    #[test]
    fn pyramid_levels_shrink() {
        let cache = PeakCache::build(&ramp(100_000));
        assert!(cache.levels().len() > 3);
        for w in cache.levels().windows(2) {
            assert_eq!(w[1].frames_per_peak, w[0].frames_per_peak * 4);
            assert!(w[1].peaks[0].len() < w[0].peaks[0].len());
        }
    }

    #[test]
    fn min_max_matches_brute_force_within_peak_granularity() {
        let data = ramp(100_000);
        let cache = PeakCache::build(&data);
        let (lo, hi) = cache.min_max(0, 0, 100_000).unwrap();
        assert_eq!(lo, 0.0);
        assert!((hi - data.channel(0)[99_999]).abs() < 1e-6);
        let (lo, hi) = cache.min_max(1, 10_000, 20_000).unwrap();
        // Peaks are aligned to their level, so allow one coarse peak of slack.
        let slack = 4096.0 / 100_000.0;
        assert!(hi <= -0.1 + slack && hi >= -0.1 - 1e-6, "{hi}");
        assert!(lo >= -0.2 - slack && lo <= -0.2 + 1e-6, "{lo}");
        assert_eq!(cache.min_max(0, 200_000, 300_000), None);
        assert_eq!(cache.min_max(5, 0, 10), None);
    }
}
