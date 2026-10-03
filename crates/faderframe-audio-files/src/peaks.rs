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
        let mut b = PeakBuilder::new(data.num_channels());
        let chans: Vec<&[f32]> = (0..data.num_channels()).map(|c| data.channel(c)).collect();
        b.push(&chans, data.frames());
        b.finish()
    }

    /// Build the pyramid above an existing base level.
    pub fn from_base(frames: usize, base: Vec<Vec<(f32, f32)>>) -> Self {
        let channels = base.len();
        let mut levels = vec![PeakLevel {
            frames_per_peak: Self::BASE_FRAMES_PER_PEAK,
            peaks: base,
        }];
        loop {
            let prev = &levels[levels.len() - 1];
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

    const MAGIC: &'static [u8; 4] = b"FFPK";
    const VERSION: u32 = 1;

    /// Save the base level (`.ffpk`); higher levels are rebuilt on load.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(Self::MAGIC)?;
        w.write_all(&Self::VERSION.to_le_bytes())?;
        w.write_all(&(self.channels as u32).to_le_bytes())?;
        w.write_all(&(self.frames as u64).to_le_bytes())?;
        w.write_all(&(Self::BASE_FRAMES_PER_PEAK as u32).to_le_bytes())?;
        for ch in &self.levels[0].peaks {
            w.write_all(&(ch.len() as u64).to_le_bytes())?;
            for &(lo, hi) in ch {
                w.write_all(&lo.to_le_bytes())?;
                w.write_all(&hi.to_le_bytes())?;
            }
        }
        w.flush()
    }

    /// Load a `.ffpk`; fails if it does not describe `frames` × `channels`.
    pub fn load(path: &std::path::Path, frames: usize, channels: usize) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let bad = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid or stale peak file",
            )
        };
        let mut at = 0usize;
        let mut take = |n: usize| -> std::io::Result<&[u8]> {
            let s = bytes.get(at..at + n).ok_or_else(bad)?;
            at += n;
            Ok(s)
        };
        let u32_of = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let u64_of =
            |b: &[u8]| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        if take(4)? != Self::MAGIC || u32_of(take(4)?) != Self::VERSION {
            return Err(bad());
        }
        let ch = u32_of(take(4)?) as usize;
        let fr = u64_of(take(8)?) as usize;
        let fpp = u32_of(take(4)?) as usize;
        if ch != channels || fr != frames || fpp != Self::BASE_FRAMES_PER_PEAK {
            return Err(bad());
        }
        let mut base = Vec::with_capacity(ch);
        for _ in 0..ch {
            let n = u64_of(take(8)?) as usize;
            if n != frames.div_ceil(fpp) {
                return Err(bad());
            }
            let raw = take(n * 8)?;
            base.push(
                raw.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|p| {
                        (
                            f32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                            f32::from_le_bytes([p[4], p[5], p[6], p[7]]),
                        )
                    })
                    .collect(),
            );
        }
        Ok(Self::from_base(frames, base))
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

/// Builds a [`PeakCache`] incrementally from streamed audio (import,
/// recording) without holding the audio in memory.
///
/// While building it can already be queried ([`PeakBuilder::min_max`],
/// including the newest partial peak), which is what draws the waveform of
/// a take while it is being recorded. A coarse level is kept alongside the
/// base level so zoomed-out live drawing stays cheap for long takes.
#[derive(Clone, Debug)]
pub struct PeakBuilder {
    base: Vec<Vec<(f32, f32)>>,
    acc: Vec<(f32, f32)>,
    in_peak: usize,
    coarse: Vec<Vec<(f32, f32)>>,
    coarse_acc: Vec<(f32, f32)>,
    in_coarse: usize,
    frames: usize,
}

const EMPTY: (f32, f32) = (f32::MAX, f32::MIN);

fn merge(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0.min(b.0), a.1.max(b.1))
}

impl PeakBuilder {
    /// Base peaks per coarse peak.
    pub const COARSE_FACTOR: usize = 64;

    pub fn new(channels: usize) -> Self {
        Self {
            base: vec![Vec::new(); channels],
            acc: vec![EMPTY; channels],
            in_peak: 0,
            coarse: vec![Vec::new(); channels],
            coarse_acc: vec![EMPTY; channels],
            in_coarse: 0,
            frames: 0,
        }
    }

    pub fn channels(&self) -> usize {
        self.base.len()
    }

    /// Min/max of `channel` over frames `[start, end)` of what has been
    /// pushed so far (cost bounded by the coarse level for long spans).
    pub fn min_max(&self, channel: usize, start: i64, end: i64) -> Option<(f32, f32)> {
        if channel >= self.base.len() || end <= start || end <= 0 || start >= self.frames as i64 {
            return None;
        }
        let (start, end) = (start.max(0) as usize, (end as usize).min(self.frames));
        let base_fpp = PeakCache::BASE_FRAMES_PER_PEAK;
        let coarse_fpp = base_fpp * Self::COARSE_FACTOR;
        let mut out = EMPTY;
        if end - start >= coarse_fpp {
            let c = &self.coarse[channel];
            let (a, b) = (start / coarse_fpp, end.div_ceil(coarse_fpp));
            for p in &c[a.min(c.len())..b.min(c.len())] {
                out = merge(out, *p);
            }
            if b > c.len() {
                out = merge(merge(out, self.coarse_acc[channel]), self.acc[channel]);
            }
        } else {
            let v = &self.base[channel];
            let (a, b) = (start / base_fpp, end.div_ceil(base_fpp));
            for p in &v[a.min(v.len())..b.min(v.len())] {
                out = merge(out, *p);
            }
            if b > v.len() {
                out = merge(out, self.acc[channel]);
            }
        }
        (out.0 <= out.1).then_some(out)
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Add `frames` frames of non-interleaved audio.
    pub fn push(&mut self, channels: &[&[f32]], frames: usize) {
        let fpp = PeakCache::BASE_FRAMES_PER_PEAK;
        for i in 0..frames {
            for (c, acc) in self.acc.iter_mut().enumerate() {
                let s = channels
                    .get(c)
                    .and_then(|ch| ch.get(i))
                    .copied()
                    .unwrap_or(0.0);
                acc.0 = acc.0.min(s);
                acc.1 = acc.1.max(s);
            }
            self.in_peak += 1;
            if self.in_peak == fpp {
                for (c, acc) in self.acc.iter_mut().enumerate() {
                    self.base[c].push(*acc);
                    self.coarse_acc[c] = merge(self.coarse_acc[c], *acc);
                    *acc = EMPTY;
                }
                self.in_peak = 0;
                self.in_coarse += 1;
                if self.in_coarse == Self::COARSE_FACTOR {
                    for (c, acc) in self.coarse_acc.iter_mut().enumerate() {
                        self.coarse[c].push(*acc);
                        *acc = EMPTY;
                    }
                    self.in_coarse = 0;
                }
            }
        }
        self.frames += frames;
    }

    pub fn finish(mut self) -> PeakCache {
        if self.in_peak > 0 {
            for (c, acc) in self.acc.iter().enumerate() {
                self.base[c].push(*acc);
            }
        }
        PeakCache::from_base(self.frames, self.base)
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

    #[test]
    fn builder_matches_batch_and_file_round_trip() {
        let data = ramp(100_003);
        let batch = PeakCache::build(&data);
        let mut b = PeakBuilder::new(2);
        let l = data.channel(0);
        let r = data.channel(1);
        let mut i = 0;
        while i < data.frames() {
            let n = 777.min(data.frames() - i);
            b.push(&[&l[i..i + n], &r[i..i + n]], n);
            i += n;
        }
        let streamed = b.finish();
        assert_eq!(streamed, batch);
        let path = std::env::temp_dir().join(format!("ff-peaks-{}.ffpk", std::process::id()));
        batch.save(&path).unwrap();
        assert_eq!(PeakCache::load(&path, 100_003, 2).unwrap(), batch);
        assert!(
            PeakCache::load(&path, 100_004, 2).is_err(),
            "stale file rejected"
        );
        std::fs::remove_file(&path).unwrap();
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn live_queries_match_the_finished_cache_and_include_partial_peaks() {
        let n = 300_000;
        let data: Vec<f32> = (0..n)
            .map(|i| ((i as f32) * 0.001).sin() * (i as f32 / n as f32))
            .collect();
        let mut b = PeakBuilder::new(1);
        // Push in odd-sized blocks, like the record writer.
        let mut pos = 0;
        while pos < n {
            let len = 333.min(n - pos);
            b.push(&[&data[pos..pos + len]], len);
            pos += len;
        }
        // The newest frames (not a whole peak yet) are visible, at peak
        // granularity: the partial peak holds the last n % 64 frames.
        let tail = b.min_max(0, n as i64 - 10, n as i64).unwrap();
        let partial = n % PeakCache::BASE_FRAMES_PER_PEAK;
        assert!(partial > 10);
        let exact = data[n - partial..]
            .iter()
            .fold(EMPTY, |a, &s| merge(a, (s, s)));
        assert_eq!(tail, exact);
        // Results cover the range exactly up to bucket granularity.
        let exact = |a: usize, e: usize| data[a..e].iter().fold(EMPTY, |m, &s| merge(m, (s, s)));
        for (a, e) in [
            (0usize, 100_000usize),
            (1_000, 1_500),
            (250_000, 300_000),
            (0, n),
        ] {
            let live = b.min_max(0, a as i64, e as i64).unwrap();
            let inner = exact(a, e);
            let bucket = if e - a >= 4096 { 4096 } else { 64 };
            let outer = exact(a / bucket * bucket, (e.div_ceil(bucket) * bucket).min(n));
            assert!(
                live.0 <= inner.0 && live.1 >= inner.1,
                "{a}..{e}: {live:?} misses {inner:?}"
            );
            assert!(
                live.0 >= outer.0 && live.1 <= outer.1,
                "{a}..{e}: {live:?} beyond {outer:?}"
            );
        }
        assert_eq!(b.clone().finish().frames(), n);
        assert!(b.min_max(0, n as i64, n as i64 + 5).is_none());
    }
}
