//! The measurements of A/V calibration (pure: no GStreamer here): when a
//! camera saw the screen flash, when a microphone heard the beep that goes
//! with it, and from the pairs how much later the picture is than the
//! sound.
//!
//! Every time is in nanoseconds on one clock (the audio engine's; see
//! [`crate::camera`] for how camera stamps are put on it). A flash and its
//! beep are meant to start at once, so each pair gives one difference; the
//! estimate is their median-filtered mean, with the spread.
//!
//! * **Flashes, sub-frame.** A camera frame integrates light over its
//!   exposure, so the frame the flash starts in is only partly bright: the
//!   share of its exposure that saw the flash is its brightness over the
//!   flash's own (the frames fully inside it), and the flash began that
//!   share of the exposure before the exposure ended. Each row band of a
//!   grid over the picture is measured on its own: a rolling shutter
//!   exposes the rows one after another (and the screen scans out top to
//!   bottom), so the onsets lie on a line down the picture. The line is
//!   fitted per flash (one slope for all) and read at the middle row of
//!   the screen — where a viewer looks. Only cells that flash take part
//!   (the screen may fill a small part of the picture).
//! * **Beeps.** The microphone's input is demodulated at the beep's tone
//!   and summed over the beep's length (a matched filter for a tone
//!   burst): the result rises in a straight line from the beep's first
//!   sample to its last. The line through the first milliseconds of the
//!   rise (the direct sound, before the room's reflections add to it)
//!   meets the noise floor where the beep started. Sample times come from a straight line through the
//!   callbacks' stamps (late callbacks are thrown out), so scheduling
//!   jitter does not reach them.
//!
//! The assumptions (documented where they matter): a camera frame's stamp
//! is its first row's exposure start and its exposure lasts its frame
//! period; the microphone's input latency is what the audio device
//! reports. Both are the sensors' own and stay in the result; the screen's
//! and the speakers' delays are what is measured.

/// A grid laid over a camera picture: `columns` × `rows` cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
}

impl Grid {
    pub fn cells(&self) -> usize {
        self.columns * self.rows
    }
}

/// One camera frame as the calibration sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct LightFrame {
    /// When its exposure started (its first row's), ns.
    pub at: i64,
    /// How long the exposure lasted (its frame period), ns.
    pub exposure: i64,
    /// The mean brightness (0…1) of every cell of the grid, row by row
    /// from the top.
    pub cells: Vec<f32>,
}

/// A flash as the camera saw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flash {
    /// When it began at the middle row of the screen (ns).
    pub at: i64,
    /// The frames it was seen in.
    pub frames: usize,
}

/// What the camera saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flashes {
    pub flashes: Vec<Flash>,
    /// The cells that flashed (the screen), as indices into the grid.
    pub cells: Vec<usize>,
    /// How much later the onset is per picture height down the screen
    /// (ns; the display's scan less the camera's readout).
    pub slope: f64,
}

/// A cell must brighten by this much (0…1) to count as the screen.
const MIN_RESPONSE: f32 = 0.03;
/// Cells brightening less than this share of the brightest one are not the
/// screen.
const CELL_SHARE: f32 = 0.5;
/// A frame is lit at this share of the flash's level (normalised).
const LIT: f32 = 0.25;
/// A row band starts lighting at this share.
const ONSET: f32 = 0.03;
/// How much of a beep's rise its start is read from (s): shorter than
/// the way to the nearest reflecting surface and back, usually.
const RISE_SECONDS: f64 = 0.004;

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

fn quantile(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let i = ((v.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    v[i.min(v.len() - 1)]
}

/// The flashes in `frames` (in time order), seen through `grid`.
pub fn flashes(frames: &[LightFrame], grid: Grid) -> Flashes {
    let n = frames.len();
    let c = grid.cells();
    if n < 4 || c == 0 || frames.iter().any(|f| f.cells.len() != c) {
        return Flashes::default();
    }
    // Each cell's dark level (the median: the screen is dark most of the
    // time) and how far it brightens (a high quantile: inside flashes).
    let mut base = vec![0.0f32; c];
    let mut response = vec![0.0f32; c];
    let mut column = vec![0.0f32; n];
    for j in 0..c {
        for (i, f) in frames.iter().enumerate() {
            column[i] = f.cells[j];
        }
        base[j] = quantile(&mut column, 0.5);
        response[j] = quantile(&mut column, 0.97) - base[j];
    }
    let best = response.iter().copied().fold(0.0f32, f32::max);
    if best < MIN_RESPONSE {
        return Flashes::default();
    }
    let cells: Vec<usize> = (0..c)
        .filter(|&j| response[j] >= best * CELL_SHARE && response[j] >= MIN_RESPONSE)
        .collect();
    // Per row band: the flashing cells' normalised brightness (0 dark, 1
    // the flash), and the band's middle as a share of the picture height.
    let bands: Vec<(f64, Vec<usize>)> = (0..grid.rows)
        .filter_map(|r| {
            let in_row: Vec<usize> = cells
                .iter()
                .copied()
                .filter(|j| j / grid.columns == r)
                .collect();
            (!in_row.is_empty()).then(|| ((r as f64 + 0.5) / grid.rows as f64, in_row))
        })
        .collect();
    let level = |i: usize, band: &[usize]| -> f32 {
        band.iter()
            .map(|&j| (frames[i].cells[j] - base[j]) / response[j])
            .sum::<f32>()
            / band.len() as f32
    };
    let whole: Vec<f32> = (0..n).map(|i| level(i, &cells)).collect();
    // The screen's middle row (where a viewer looks).
    let middle = bands.iter().map(|b| b.0 * b.1.len() as f64).sum::<f64>()
        / bands.iter().map(|b| b.1.len() as f64).sum::<f64>().max(1.0);
    // Runs of lit frames; one touching either end is cut off.
    let mut runs = Vec::new();
    let mut i = 0;
    while i < n {
        if whole[i] > LIT {
            let k = i;
            while i < n && whole[i] > LIT {
                i += 1;
            }
            if k > 0 && i < n {
                runs.push((k, i - 1));
            }
        } else {
            i += 1;
        }
    }
    // Per flash and band: the onset as the band saw it (ignoring its own
    // place in the readout), `(height share, onset ns)`.
    let mut onsets: Vec<Vec<(f64, f64)>> = Vec::new();
    for &(k, m) in &runs {
        let first = k - 1;
        let last = (m + 1).min(n - 1);
        let mut per_band = Vec::new();
        for (y, band) in &bands {
            let Some(start) = (first..=last).find(|&i| level(i, band) > ONSET) else {
                continue;
            };
            // The band's flash level: the frames fully inside the flash
            // (after its first lit frame, before its last).
            let end = (start..=last)
                .rev()
                .find(|&i| level(i, band) > ONSET)
                .unwrap_or(start);
            let mut inside: Vec<f64> = (start + 1..end).map(|i| level(i, band) as f64).collect();
            let full = median(&mut inside).filter(|v| *v > 0.2).unwrap_or(1.0);
            let share = (level(start, band) as f64 / full).clamp(0.0, 1.0);
            let f = &frames[start];
            let onset = (f.at + f.exposure) as f64 - share * f.exposure as f64;
            per_band.push((*y, onset));
        }
        onsets.push(per_band);
    }
    // One slope for every flash: the median of their own fits.
    let mut slopes: Vec<f64> = onsets
        .iter()
        .filter_map(|o| {
            if o.len() < 2 {
                return None;
            }
            let my = o.iter().map(|p| p.0).sum::<f64>() / o.len() as f64;
            let mu = o.iter().map(|p| p.1).sum::<f64>() / o.len() as f64;
            let sxx: f64 = o.iter().map(|p| (p.0 - my).powi(2)).sum();
            let sxy: f64 = o.iter().map(|p| (p.0 - my) * (p.1 - mu)).sum();
            (sxx > 1e-9).then(|| sxy / sxx)
        })
        .collect();
    let slope = median(&mut slopes).unwrap_or(0.0);
    let flashes = runs
        .iter()
        .zip(&onsets)
        .filter_map(|(&(k, m), o)| {
            let mut at: Vec<f64> = o.iter().map(|p| p.1 + slope * (middle - p.0)).collect();
            Some(Flash {
                at: median(&mut at)?.round() as i64,
                frames: m - k + 1,
            })
        })
        .collect();
    Flashes {
        flashes,
        cells,
        slope,
    }
}

/// What the microphone heard: its samples and when they were taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heard {
    pub rate: u32,
    pub samples: Vec<f32>,
    /// `(first sample of a block, when that sample was taken in ns)`, one
    /// per device callback (its stamp less the input's latency).
    pub stamps: Vec<(usize, i64)>,
}

impl Heard {
    /// When sample `i` was taken: a straight line through the stamps
    /// (callbacks woken late are left out), else the nominal rate from the
    /// first stamp.
    pub fn clock(&self) -> SampleClock {
        let rate = f64::from(self.rate.max(1));
        let nominal = 1e9 / rate;
        let Some(&(i0, t0)) = self.stamps.first() else {
            return SampleClock {
                origin: 0.0,
                per_sample: nominal,
            };
        };
        // Relative to the first stamp (precision), at the nominal rate:
        // residuals from it; a line through them, twice, leaving out what
        // lies far from the first line.
        let pts: Vec<(f64, f64)> = self
            .stamps
            .iter()
            .map(|&(i, t)| {
                let x = (i as f64 - i0 as f64) / rate;
                (x, (t - t0) as f64 - x * 1e9)
            })
            .collect();
        let fit = |pts: &[(f64, f64)]| -> Option<(f64, f64)> {
            let n = pts.len() as f64;
            if pts.len() < 2 {
                return None;
            }
            let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
            let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
            let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
            if sxx < 1e-12 {
                return Some((my, 0.0));
            }
            let b = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / sxx;
            Some((my - b * mx, b))
        };
        let Some((a, b)) = fit(&pts) else {
            return SampleClock {
                origin: t0 as f64 - i0 as f64 * nominal,
                per_sample: nominal,
            };
        };
        let mut res: Vec<f64> = pts.iter().map(|p| (p.1 - a - b * p.0).abs()).collect();
        let spread = median(&mut res).unwrap_or(0.0);
        let limit = (spread * 4.0).max(200_000.0);
        let kept: Vec<(f64, f64)> = pts
            .iter()
            .copied()
            .filter(|p| (p.1 - a - b * p.0).abs() <= limit)
            .collect();
        let (a, b) = fit(&kept).unwrap_or((a, b));
        // t(i) = t0 + a + (i − i0)/rate · (1e9 + b)
        let per_sample = (1e9 + b) / rate;
        SampleClock {
            origin: t0 as f64 + a - i0 as f64 * per_sample,
            per_sample,
        }
    }
}

/// When a sample was taken: `origin + i × per_sample` (ns).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleClock {
    pub origin: f64,
    pub per_sample: f64,
}

impl SampleClock {
    pub fn at(&self, i: f64) -> i64 {
        (self.origin + i * self.per_sample).round() as i64
    }
}

/// Where tone bursts of `hz` lasting `seconds` start in `heard` (ns, in
/// time order). Bursts must stand clear of the noise and be at least half a
/// second apart.
pub fn beeps(heard: &Heard, hz: f64, seconds: f64) -> Vec<i64> {
    let rate = f64::from(heard.rate.max(1));
    let x = &heard.samples;
    let w = ((seconds * rate).round() as usize).max(8);
    if x.len() < w * 4 {
        return Vec::new();
    }
    // The tone's I and Q summed over the burst's length.
    let step = std::f64::consts::TAU * hz / rate;
    let (mut si, mut sq) = (0.0f64, 0.0f64);
    let mut iq = vec![(0.0f64, 0.0f64); x.len()];
    let mut env = vec![0.0f64; x.len()];
    for (n, s) in x.iter().enumerate() {
        let (sin, cos) = (step * n as f64).sin_cos();
        let v = f64::from(*s);
        iq[n] = (v * cos, v * sin);
        si += iq[n].0;
        sq += iq[n].1;
        if n >= w {
            si -= iq[n - w].0;
            sq -= iq[n - w].1;
        }
        env[n] = (si * si + sq * sq).sqrt() * 2.0 / w as f64;
    }
    let mut sorted = env.clone();
    let floor = median(&mut sorted).unwrap_or(0.0);
    let mut dev: Vec<f64> = env.iter().map(|e| (e - floor).abs()).collect();
    let noise = median(&mut dev).unwrap_or(0.0) * 1.4826;
    let top = env.iter().copied().fold(0.0, f64::max);
    if top - floor <= noise * 8.0 || top <= 1e-6 {
        return Vec::new();
    }
    let threshold = floor + 0.3 * (top - floor);
    let clock = heard.clock();
    let gap = (0.5 * rate) as usize;
    let mut out: Vec<i64> = Vec::new();
    let mut last_end = 0usize;
    let mut n = w;
    while n < env.len() {
        if env[n] <= threshold {
            n += 1;
            continue;
        }
        // The burst's region and its peak.
        let a = n;
        while n < env.len() && env[n] > threshold {
            n += 1;
        }
        let (peak_at, peak) = (a..n)
            .map(|i| (i, env[i]))
            .fold((a, 0.0), |m, v| if v.1 > m.1 { v } else { m });
        if a < last_end + gap && !out.is_empty() {
            continue;
        }
        last_end = n;
        // The first milliseconds of the rise, from where it clears the
        // noise: the direct sound alone (a reflection arriving later adds
        // to the slope, which would put the start late).
        let low = floor + (8.0 * noise).max(0.04 * (peak - floor));
        let from = peak_at.saturating_sub(w * 2);
        let mut n0 = peak_at;
        while n0 > from && env[n0 - 1] > low {
            n0 -= 1;
        }
        let span = ((RISE_SECONDS * rate) as usize).max(4);
        let rise: Vec<(f64, f64)> = (n0..(n0 + span).min(peak_at + 1))
            .map(|i| (i as f64, env[i]))
            .collect();
        let start = if rise.len() >= 3 {
            let k = rise.len() as f64;
            let mx = rise.iter().map(|p| p.0).sum::<f64>() / k;
            let my = rise.iter().map(|p| p.1).sum::<f64>() / k;
            let sxx: f64 = rise.iter().map(|p| (p.0 - mx).powi(2)).sum();
            let b = rise.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / sxx.max(1e-12);
            if b <= 0.0 {
                continue;
            }
            // Where the rise meets the floor: the sum over the window
            // ending at the burst's first sample holds one sample of it.
            mx + (floor - my) / b
        } else {
            // Too steep to fit (a very short burst): the half crossing.
            let half = floor + 0.5 * (peak - floor);
            let k = (from..=peak_at)
                .find(|&i| env[i] >= half)
                .unwrap_or(peak_at);
            k as f64 - w as f64 / 2.0
        };
        out.push(clock.at(start.max(0.0)));
    }
    out
}

/// The picture's lateness from flash and beep pairs.
#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    /// How much later the picture is than the sound (ms; negative:
    /// earlier).
    pub late_ms: f64,
    /// The spread of the pairs kept (standard deviation, ms).
    pub spread_ms: f64,
    /// Every pair's difference (flash − beep, ms), in time order.
    pub pairs: Vec<f64>,
    /// How many pairs the estimate keeps (outliers left out).
    pub used: usize,
}

/// Pair every flash with the beep nearest it (no more than `apart` ns
/// away) and estimate the picture's lateness: the median of the
/// differences, then the mean of those within three robust deviations of
/// it (and at least 3 ms). `None` without at least `min_pairs` pairs.
pub fn estimate(flashes: &[i64], beeps: &[i64], apart: i64, min_pairs: usize) -> Option<Estimate> {
    let pairs: Vec<f64> = flashes
        .iter()
        .filter_map(|&f| {
            beeps
                .iter()
                .map(|&b| f - b)
                .min_by_key(|d| d.abs())
                .filter(|d| d.abs() <= apart)
                .map(|d| d as f64 / 1e6)
        })
        .collect();
    if pairs.len() < min_pairs.max(1) {
        return None;
    }
    let mut v = pairs.clone();
    let mid = median(&mut v)?;
    let mut dev: Vec<f64> = pairs.iter().map(|d| (d - mid).abs()).collect();
    let mad = median(&mut dev).unwrap_or(0.0) * 1.4826;
    let limit = (3.0 * mad).max(3.0);
    let kept: Vec<f64> = pairs
        .iter()
        .copied()
        .filter(|d| (d - mid).abs() <= limit)
        .collect();
    if kept.len() < min_pairs.max(1) {
        return None;
    }
    let mean = kept.iter().sum::<f64>() / kept.len() as f64;
    let spread = if kept.len() > 1 {
        (kept.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (kept.len() - 1) as f64).sqrt()
    } else {
        0.0
    };
    Some(Estimate {
        late_ms: mean,
        spread_ms: spread,
        pairs,
        used: kept.len(),
    })
}

/// The beeps of the calibration: `count` tone bursts of `hz`, `seconds`
/// long, every `every` seconds from `first`, at amplitude 0.5 — mono
/// samples at `rate`, as long as the last burst plus `tail` seconds.
pub fn beep_track(
    rate: u32,
    first: f64,
    every: f64,
    count: usize,
    hz: f64,
    seconds: f64,
    tail: f64,
) -> Vec<f32> {
    let r = f64::from(rate);
    let total = ((first + every * count.saturating_sub(1) as f64 + seconds + tail) * r) as usize;
    let mut out = vec![0.0f32; total];
    let len = (seconds * r).round() as usize;
    for k in 0..count {
        let at = ((first + every * k as f64) * r).round() as usize;
        for i in 0..len {
            if let Some(s) = out.get_mut(at + i) {
                *s = ((std::f64::consts::TAU * hz * i as f64 / r).sin() * 0.5) as f32;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic noise source.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
        fn noise(&mut self) -> f64 {
            self.next() * 2.0 - 1.0
        }
    }

    /// Camera frames of a screen in rows `screen` (of `grid.rows`) and
    /// columns 4…11 flashing `flash_ns` long at `starts`: frames every
    /// `period` from `t0`, exposure the whole period, each row band's
    /// exposure `readout × y` later (a rolling shutter; y its middle as a
    /// share of the height), the screen's own scan `scan × y` later.
    #[allow(clippy::too_many_arguments)]
    fn camera(
        grid: Grid,
        screen: std::ops::Range<usize>,
        starts: &[i64],
        flash_ns: i64,
        t0: i64,
        period: i64,
        frames: usize,
        readout: f64,
        scan: f64,
        noise: f64,
    ) -> Vec<LightFrame> {
        let mut rng = Lcg(7);
        (0..frames)
            .map(|i| {
                let at = t0 + i as i64 * period;
                let mut cells = vec![0.0f32; grid.cells()];
                for r in 0..grid.rows {
                    let y = (r as f64 + 0.5) / grid.rows as f64;
                    let a = at as f64 + readout * y;
                    let b = a + period as f64;
                    let lit: f64 = starts
                        .iter()
                        .map(|&s| {
                            let s = s as f64 + scan * y;
                            let e = s + flash_ns as f64;
                            (e.min(b) - s.max(a)).max(0.0)
                        })
                        .sum::<f64>()
                        / period as f64;
                    for c in 0..grid.columns {
                        let on = screen.contains(&r) && (4..12).contains(&c);
                        let v = if on { 0.08 + 0.8 * lit } else { 0.05 };
                        cells[r * grid.columns + c] = (v + noise * rng.noise()) as f32;
                    }
                }
                LightFrame {
                    at,
                    exposure: period,
                    cells,
                }
            })
            .collect()
    }

    const GRID: Grid = Grid {
        columns: 16,
        rows: 12,
    };

    #[test]
    fn a_flash_is_timed_within_its_frame() {
        // 30 fps, flashes of 240 ms every second at odd phases.
        let period = 33_333_333;
        let starts: Vec<i64> = (0..8)
            .map(|k| 500_000_000 + k * 1_000_000_000 + (k * 7_919_123) % period)
            .collect();
        let frames = camera(
            GRID,
            3..9,
            &starts,
            240_000_000,
            0,
            period,
            300,
            0.0,
            0.0,
            0.002,
        );
        let seen = flashes(&frames, GRID);
        assert_eq!(seen.flashes.len(), starts.len(), "{seen:?}");
        for (f, s) in seen.flashes.iter().zip(&starts) {
            let err = (f.at - s) as f64 / 1e6;
            assert!(err.abs() < 0.5, "flash at {s}: off by {err:.3} ms");
            assert!(f.frames >= 7, "{} frames", f.frames);
        }
        // Only the screen's cells.
        assert!(seen.cells.iter().all(|j| (3..9).contains(&(j / 16))));
    }

    #[test]
    fn a_rolling_shutter_and_the_screens_scan_are_read_at_the_screens_middle() {
        // Rows read out over 25 ms; the screen scans over 16 ms. A flash
        // reaches the screen's middle (rows 2…9: y = 0.5) at
        // start + 8 ms; the camera stamps its first row.
        let period = 33_333_333;
        let starts: Vec<i64> = (0..8)
            .map(|k| 400_000_000 + k * 1_000_000_000 + (k * 4_567_891) % period)
            .collect();
        let frames = camera(
            GRID,
            2..10,
            &starts,
            240_000_000,
            1_000,
            period,
            300,
            25e6,
            16e6,
            0.002,
        );
        let seen = flashes(&frames, GRID);
        assert_eq!(seen.flashes.len(), starts.len());
        for (f, s) in seen.flashes.iter().zip(&starts) {
            // The middle row of the screen is y = 0.5: it lights at
            // s + 8 ms on the screen; the camera's row there starts its
            // exposure 12.5 ms after the stamp, which the slope cannot
            // tell from the scan — so the time read is the screen's middle
            // less that row's readout.
            let expected = *s as f64 + 8e6 - 12.5e6;
            let err = (f.at as f64 - expected) / 1e6;
            assert!(err.abs() < 1.0, "off by {err:.3} ms");
        }
        // The slope: the scan less the readout over the picture.
        assert!(
            (seen.slope / 1e6 - (16.0 - 25.0)).abs() < 1.5,
            "{}",
            seen.slope
        );
    }

    #[test]
    fn a_short_flash_is_found_too() {
        // The sync test's one-frame flash (40 ms) at 30 fps.
        let period = 33_333_333;
        let starts: Vec<i64> = (0..6).map(|k| 300_000_000 + k * 1_000_000_000).collect();
        let frames = camera(
            GRID,
            0..12,
            &starts,
            40_000_000,
            0,
            period,
            200,
            0.0,
            0.0,
            0.001,
        );
        let seen = flashes(&frames, GRID);
        assert_eq!(seen.flashes.len(), 6);
        for (f, s) in seen.flashes.iter().zip(&starts) {
            // Its level is unknown (no frame fully inside): coarser.
            assert!(((f.at - s) as f64 / 1e6).abs() < 20.0);
        }
    }

    #[test]
    fn nothing_flashing_is_no_flash() {
        let frames = camera(GRID, 0..0, &[], 0, 0, 33_333_333, 100, 0.0, 0.0, 0.01);
        assert!(flashes(&frames, GRID).flashes.is_empty());
    }

    /// `seconds` of microphone at 48 kHz: beeps (1 kHz, 40 ms) starting at
    /// `starts` (seconds), noise, an echo 9 ms later at half the level and
    /// a tail; blocks of 256 stamped `t0` on with up to 0.3 ms of late
    /// wake-ups.
    fn microphone(starts: &[f64], seconds: f64, t0: i64, noise: f64) -> Heard {
        let rate = 48_000u32;
        let n = (seconds * rate as f64) as usize;
        let mut rng = Lcg(3);
        let mut x = vec![0.0f32; n];
        for &s in starts {
            let a = s * rate as f64;
            for (i, v) in x.iter_mut().enumerate() {
                let t = (i as f64 - a) / rate as f64;
                let burst = |t: f64| {
                    if (0.0..0.04).contains(&t) {
                        (std::f64::consts::TAU * 1000.0 * t).sin() * 0.2
                    } else {
                        0.0
                    }
                };
                let tail = if t > 0.0 && t < 0.3 {
                    rng.noise() * 0.01 * (-t * 20.0).exp()
                } else {
                    0.0
                };
                *v += (burst(t) + 0.5 * burst(t - 0.009) + tail) as f32;
            }
        }
        for v in &mut x {
            *v += (noise * rng.noise()) as f32;
        }
        let stamps = (0..n / 256)
            .map(|b| {
                let late = (rng.next() * 300_000.0) as i64;
                (
                    b * 256,
                    t0 + (b as f64 * 256.0 * 1e9 / rate as f64) as i64 + late,
                )
            })
            .collect();
        Heard {
            rate,
            samples: x,
            stamps,
        }
    }

    #[test]
    fn a_beep_is_timed_to_a_fraction_of_a_millisecond() {
        let starts = [0.5, 1.5, 2.5003, 3.4991, 4.5];
        let heard = microphone(&starts, 5.5, 2_000_000_000, 0.01);
        let found = beeps(&heard, 1000.0, 0.04);
        assert_eq!(found.len(), starts.len(), "{found:?}");
        for (f, s) in found.iter().zip(&starts) {
            let expected = 2_000_000_000.0 + s * 1e9;
            let err = (*f as f64 - expected) / 1e6;
            assert!(err.abs() < 0.3, "beep at {s}: off by {err:.3} ms");
        }
    }

    #[test]
    fn silence_has_no_beeps() {
        let heard = microphone(&[], 3.0, 0, 0.01);
        assert!(beeps(&heard, 1000.0, 0.04).is_empty());
    }

    #[test]
    fn the_estimate_leaves_out_what_does_not_belong() {
        let ms = 1_000_000;
        let flashes: Vec<i64> = (0..8)
            .map(|k| k * 1000 * ms + 20 * ms + (k % 3) * ms)
            .collect();
        let mut beeps: Vec<i64> = (0..8).map(|k| k * 1000 * ms).collect();
        // One beep heard wrong (an echo taken for it).
        beeps[5] += 80 * ms;
        let e = estimate(&flashes, &beeps, 450 * ms, 4).unwrap();
        assert_eq!(e.pairs.len(), 8);
        assert_eq!(e.used, 7);
        assert!((e.late_ms - 21.0).abs() < 0.6, "{e:?}");
        assert!(e.spread_ms < 1.0);
        // Too few pairs: nothing.
        assert!(estimate(&flashes[..2], &beeps, 450 * ms, 4).is_none());
        // Nothing near enough: nothing.
        assert!(estimate(&flashes, &[100_000 * ms], 450 * ms, 1).is_none());
    }

    #[test]
    fn the_beep_track_beeps_where_asked() {
        let t = beep_track(48_000, 1.0, 1.0, 3, 1000.0, 0.04, 0.5);
        assert_eq!(t.len(), (3.54 * 48_000.0) as usize);
        assert_eq!(t[47_999], 0.0);
        assert!(t[48_012].abs() > 0.1);
        assert_eq!(t[48_000 + 1920], 0.0);
        assert!(t[96_012].abs() > 0.1);
    }
}
