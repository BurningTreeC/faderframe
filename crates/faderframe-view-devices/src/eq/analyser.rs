//! The EQ's analyser: the spectra of what goes in, what comes out and an
//! external signal (the sidechain, or another EQ's output), with the usual
//! settings — resolution, how fast levels fall, range, tilt, freeze — plus
//! long-term averages (for EQ Match), the peaks Spectrum Grab offers and
//! where two spectra collide.

use faderframe_analysis::fft;
use faderframe_plugin_host::tap::{AnalysisTap, AudioRing, RING_FRAMES};
use std::sync::Arc;
use std::time::Instant;

/// FFT sizes of the resolutions.
pub(crate) const RESOLUTIONS: [usize; 4] = [1024, 2048, 4096, 8192];
pub(crate) const RESOLUTION_NAMES: [&str; 4] = ["Low", "Medium", "High", "Maximum"];
/// How fast levels fall (dB per second).
pub(crate) const SPEEDS: [(&str, f32); 5] = [
    ("Very Slow", 8.0),
    ("Slow", 16.0),
    ("Medium", 32.0),
    ("Fast", 64.0),
    ("Very Fast", 128.0),
];
/// The analyser's ranges (dB under full scale) and tilts (dB/oct).
pub(crate) const RANGES: [f32; 3] = [60.0, 90.0, 120.0];
pub(crate) const TILTS: [f32; 5] = [0.0, 1.5, 3.0, 4.5, 6.0];
/// The floor of every spectrum (dBFS).
pub(crate) const FLOOR: f32 = -150.0;

/// One signal's spectrum (of its mid).
pub(crate) struct Line {
    size: usize,
    rate: f64,
    window: Vec<f64>,
    input: Vec<f32>,
    re: Vec<f64>,
    im: Vec<f64>,
    /// Level per bin (dBFS).
    level: Vec<f32>,
    /// Frames of the ring read so far.
    seen: u64,
    /// Long-term average power per bin, while averaging.
    avg: Vec<f64>,
    avg_frames: u64,
    pub averaging: bool,
    /// Whether audio arrived at the last feed, and when it last did.
    pub alive: bool,
    last_alive: Option<Instant>,
}

impl Line {
    pub fn new(size: usize, rate: f64) -> Self {
        let size = size.next_power_of_two().max(256);
        Self {
            size,
            rate,
            window: (0..size)
                .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / size as f64).cos())
                .collect(),
            input: Vec::with_capacity(2 * size),
            re: vec![0.0; size],
            im: vec![0.0; size],
            level: vec![FLOOR; size / 2],
            seen: 0,
            avg: vec![0.0; size / 2],
            avg_frames: 0,
            averaging: false,
            alive: false,
            last_alive: None,
        }
    }

    /// Whether audio arrived within the last second.
    pub fn alive_recently(&self) -> bool {
        self.last_alive
            .is_some_and(|t| t.elapsed().as_secs_f64() < 1.0)
    }

    /// Read what `ring` got since the last call; levels fall by `fall` dB
    /// a second (or hold their maximum when `freeze`).
    pub fn feed(&mut self, ring: &AudioRing, scratch: &mut [Vec<f32>; 2], fall: f32, freeze: bool) {
        let written = ring.written();
        if written < self.seen {
            // A new ring (the plugin was loaded again).
            self.seen = 0;
        }
        let new = (written - self.seen).min(RING_FRAMES as u64) as usize;
        self.seen = written;
        self.alive = new > 0;
        if new == 0 {
            return;
        }
        self.last_alive = Some(Instant::now());
        let [l, r] = scratch;
        ring.latest(&mut l[..new], &mut r[..new]);
        self.input
            .extend(l[..new].iter().zip(&r[..new]).map(|(a, b)| 0.5 * (a + b)));
        let hop = self.size / 4;
        let per_frame = fall * hop as f32 / self.rate as f32;
        // Keep the work bounded after a long pause.
        let excess = self.input.len().saturating_sub(4 * self.size);
        self.input.drain(..excess);
        while self.input.len() >= self.size {
            self.analyse(per_frame, freeze);
            self.input.drain(..hop);
        }
    }

    fn analyse(&mut self, fall: f32, freeze: bool) {
        for i in 0..self.size {
            self.re[i] = f64::from(self.input[i]) * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        // Hann's coherent gain is a half, and a sine splits between the
        // positive and negative frequencies: a full scale sine reads 0 dBFS.
        let norm = 4.0 / self.size as f64;
        for b in 0..self.size / 2 {
            let power = (self.re[b].powi(2) + self.im[b].powi(2)) * norm * norm;
            if self.averaging {
                self.avg[b] += power;
            }
            let db = (10.0 * power.max(1e-20).log10()) as f32;
            let l = &mut self.level[b];
            *l = if db >= *l {
                db
            } else if freeze {
                *l
            } else {
                (*l - fall).max(db)
            };
        }
        if self.averaging {
            self.avg_frames += 1;
        }
    }

    pub fn clear(&mut self) {
        self.level.fill(FLOOR);
        self.input.clear();
    }

    /// Start reading a ring from `written` frames on (not from its past).
    pub fn skip_to(&mut self, written: u64) {
        self.seen = written.saturating_sub((self.size / 2) as u64);
    }

    pub fn reset_average(&mut self) {
        self.avg.fill(0.0);
        self.avg_frames = 0;
    }

    /// Seconds of audio in the average.
    pub fn averaged_seconds(&self) -> f64 {
        self.avg_frames as f64 * (self.size / 4) as f64 / self.rate
    }

    /// Bins between the geometric midpoints round each frequency.
    fn spans(&self, freqs: &[f64]) -> Vec<(f64, f64)> {
        let per_bin = self.size as f64 / self.rate;
        let n = freqs.len();
        (0..n)
            .map(|i| {
                let lo = if i == 0 {
                    freqs[0] * freqs[0] / freqs.get(1).copied().unwrap_or(freqs[0] * 1.01)
                } else {
                    freqs[i - 1]
                };
                let hi = if i + 1 == n {
                    freqs[i] * freqs[i] / lo.max(1e-3)
                } else {
                    freqs[i + 1]
                };
                (
                    (freqs[i] * lo).sqrt() * per_bin,
                    (freqs[i] * hi).sqrt() * per_bin,
                )
            })
            .collect()
    }

    /// The level (dBFS) at each of `freqs`: the loudest of the bins round
    /// it (so a tone reads its level whatever the resolution), interpolated
    /// where bins are sparser than the points.
    pub fn curve(&self, freqs: &[f64]) -> Vec<f32> {
        let bins = self.level.len();
        self.spans(freqs)
            .iter()
            .zip(freqs)
            .map(|(&(a, b), f)| {
                if b - a < 1.5 {
                    let x = (f * self.size as f64 / self.rate).clamp(0.0, (bins - 1) as f64);
                    let k = x.floor() as usize;
                    let t = (x - k as f64) as f32;
                    let next = self.level[(k + 1).min(bins - 1)];
                    self.level[k] + (next - self.level[k]) * t
                } else {
                    let (k0, k1) = (
                        (a.round() as usize).min(bins - 1),
                        (b.round() as usize).clamp(1, bins),
                    );
                    self.level[k0..k1.max(k0 + 1)]
                        .iter()
                        .copied()
                        .fold(FLOOR, f32::max)
                }
            })
            .collect()
    }

    /// The long-term average (dBFS) at `freqs`, once anything is in it.
    pub fn average(&self, freqs: &[f64]) -> Option<Vec<f64>> {
        if self.avg_frames == 0 {
            return None;
        }
        let bins = self.avg.len();
        let n = self.avg_frames as f64;
        Some(
            self.spans(freqs)
                .iter()
                .map(|&(a, b)| {
                    let k0 = (a.floor().max(1.0) as usize).min(bins - 1);
                    let k1 = (b.ceil() as usize).clamp(k0 + 1, bins);
                    let p = self.avg[k0..k1].iter().sum::<f64>() / ((k1 - k0) as f64 * n);
                    10.0 * p.max(1e-20).log10()
                })
                .collect(),
        )
    }
}

/// Where the external spectrum is read from: the EQ's own sidechain, or
/// another EQ's output.
pub(crate) type External = Result<Arc<AnalysisTap>, Arc<AnalysisTap>>;

/// The three spectra.
pub(crate) struct Analyser {
    pub rate: u32,
    pub size: usize,
    pub pre: Line,
    pub post: Line,
    pub ext: Line,
    /// Which ring the external line reads (it starts over on a change).
    ext_from: usize,
    scratch: [Vec<f32>; 2],
}

impl Analyser {
    pub fn new(rate: u32, size: usize) -> Self {
        let r = f64::from(rate.max(8_000));
        Self {
            rate,
            size,
            pre: Line::new(size, r),
            post: Line::new(size, r),
            ext: Line::new(size, r),
            ext_from: 0,
            scratch: [vec![0.0; RING_FRAMES], vec![0.0; RING_FRAMES]],
        }
    }

    /// Read what arrived since the last frame.
    pub fn update(
        &mut self,
        tap: &AnalysisTap,
        external: Option<&External>,
        external_on: bool,
        fall: f32,
        frozen: bool,
    ) {
        self.pre.feed(&tap.input, &mut self.scratch, fall, frozen);
        self.post.feed(&tap.output, &mut self.scratch, fall, frozen);
        let ring = match external {
            Some(Ok(own)) if external_on => Some(&own.sidechain),
            Some(Err(other)) if external_on => {
                other.watch();
                Some(&other.output)
            }
            _ => None,
        };
        match ring {
            Some(ring) => {
                let from = std::ptr::from_ref(ring) as usize;
                if from != self.ext_from {
                    self.ext_from = from;
                    self.ext = Line::new(self.size, f64::from(self.rate.max(8_000)));
                    self.ext.skip_to(ring.written());
                }
                self.ext.feed(ring, &mut self.scratch, fall, frozen);
            }
            None => {
                self.ext_from = 0;
                self.ext.clear();
            }
        }
    }
}

/// The peaks of a spectrum Spectrum Grab offers: local maxima standing at
/// least `prominence` dB over their surroundings (a third of an octave each
/// way), loudest first, at most `max`.
pub(crate) fn peaks(freqs: &[f64], levels: &[f32], prominence: f32, max: usize) -> Vec<(f64, f32)> {
    let n = freqs.len().min(levels.len());
    let mut out: Vec<(f64, f32)> = Vec::new();
    for i in 1..n.saturating_sub(1) {
        let v = levels[i];
        if v <= FLOOR + 30.0 || v < levels[i - 1] || v < levels[i + 1] {
            continue;
        }
        let near = |j: usize| (freqs[j] / freqs[i]).log2().abs() <= 1.0 / 3.0;
        let around = (0..n)
            .filter(|j| *j != i && near(*j))
            .map(|j| levels[j])
            .fold(f32::INFINITY, f32::min);
        let higher_near = (0..n).any(|j| j != i && near(j) && levels[j] > v);
        if !higher_near && around.is_finite() && v - around >= prominence {
            out.push((freqs[i], v));
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(max);
    out
}

/// How strongly two (tilted) spectra collide at each point, 0 to 1: both
/// near their own loudest (within `depth` dB).
pub(crate) fn collisions(ours: &[f32], theirs: &[f32], depth: f32) -> Vec<f32> {
    let top = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (a, b) = (top(ours), top(theirs));
    ours.iter()
        .zip(theirs)
        .map(|(x, y)| {
            let s = |v: f32, t: f32| ((v - (t - depth)) / depth).clamp(0.0, 1.0);
            (s(*x, a) * s(*y, b)).sqrt()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_with(f: f64, amp: f32, frames: usize) -> AudioRing {
        let ring = AudioRing::default();
        let v: Vec<f32> = (0..frames)
            .map(|i| amp * (std::f64::consts::TAU * f * i as f64 / 48_000.0).sin() as f32)
            .collect();
        ring.push(&v, &v);
        ring
    }

    #[test]
    fn a_sine_reads_its_level_at_its_frequency() {
        let ring = ring_with(1_000.0, 0.5, 16_384);
        let mut line = Line::new(4096, 48_000.0);
        let mut scratch = [vec![0.0; RING_FRAMES], vec![0.0; RING_FRAMES]];
        line.averaging = true;
        line.feed(&ring, &mut scratch, 30.0, false);
        let freqs = [500.0, 1_000.0, 2_000.0];
        let c = line.curve(&freqs);
        assert!((c[1] + 6.02).abs() < 1.6, "{c:?}");
        assert!(c[0] < c[1] - 30.0 && c[2] < c[1] - 30.0);
        // The average is a density (the power of each point's band, per
        // bin): a tone stands out of it, both spectra of a match alike.
        let avg = line.average(&freqs).unwrap();
        assert!(avg[1] > avg[0] + 40.0 && avg[1] > avg[2] + 40.0, "{avg:?}");
        assert!(line.averaged_seconds() > 0.1);
        // Nothing new: nothing changes, and it is not alive.
        line.feed(&ring, &mut scratch, 30.0, false);
        assert!(!line.alive);
    }

    #[test]
    fn peaks_stand_out_and_collisions_need_both() {
        let freqs: Vec<f64> = (0..120)
            .map(|i| 20.0 * 1000f64.powf(i as f64 / 119.0))
            .collect();
        let mut levels = vec![-60.0f32; 120];
        levels[40] = -20.0;
        levels[80] = -30.0;
        levels[81] = -31.0;
        let p = peaks(&freqs, &levels, 6.0, 8);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, freqs[40]);
        let other = vec![-20.0f32; 120];
        let c = collisions(&levels, &other, 24.0);
        assert!(c[40] > 0.9 && c[10] == 0.0);
    }
}
