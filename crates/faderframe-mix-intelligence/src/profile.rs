//! A track's measurements over the analysed range: band levels per frame
//! for each stereo channel and their correlation, and the levels of the
//! whole range.
//!
//! **Spectral frames.** Each channel is cut into Hann-windowed frames of
//! `fft_size` samples every `hop` samples (the last frame zero-padded) and
//! transformed ([`faderframe_analysis::fft`]). A band's level is the
//! *mean-square power* of the signal within the band: the one-sided power
//! spectrum scaled by `2 / (N · Σw²)` (DC and Nyquist bins by half that),
//! so that summed over all bins it equals the frame's windowed mean square
//! (Parseval), summed over a band's bins with [`crate::bands::BinWeights`].
//! In dB that is dBFS RMS: a full-scale sine reads −3.01 dB in its band,
//! white noise of RMS σ spreads σ² over the bands by their widths. It is
//! power, not a power spectral density: wider bands hold more of a noise.
//!
//! Per frame and band three numbers are kept: the left and right channels'
//! levels and their correlation (the band's cross power over the
//! geometric mean of the channels' powers, −1 to 1). A mono signal reads
//! the same left and right, correlation 1. Levels are stored in hundredths
//! of a dB from [`FLOOR_DB`] (silence and anything quieter) up, the
//! correlation in steps of 1/32767: 6 bytes per band and frame — a
//! five-minute track at 48 kHz with the default settings (3 516 frames,
//! 30 bands) takes 0.6 MB.
//!
//! **Levels.** The whole range is also measured by
//! [`faderframe_analysis::levels::FileMeter`]: sample peak, true peak (4×
//! oversampled, BS.1770 Annex 2), RMS, samples at full scale and overs,
//! and BS.1770-4 loudness (integrated, range, highest momentary and
//! short-term) with both channels weighted 1; and optionally the
//! integrated loudness of given spans (sections).

use crate::bands::{BandLayout, BinWeights};
use crate::config::{AnalysisConfig, ConfigError};
use faderframe_analysis::levels::{FileLevels, FileMeter};
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

/// Levels at or under this read as silence (dB).
pub const FLOOR_DB: f64 = -150.0;
/// Powers under this are silence (−150 dB).
const FLOOR_POWER: f64 = 1e-15;
const MAX_CDB: i32 = 6_000;
const MIN_CDB: i32 = (FLOOR_DB * 100.0) as i32;

#[derive(Debug, Error, PartialEq)]
pub enum ProfileError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("no audio channels to analyse")]
    NoChannels,
    #[error("the channels differ in length")]
    Ragged,
    #[error("the analysis was cancelled")]
    Cancelled,
}

fn to_cdb(power: f64) -> i16 {
    // (NaN falls through to the floor too.)
    if power.is_nan() || power <= FLOOR_POWER {
        return MIN_CDB as i16;
    }
    let db = 10.0 * power.log10();
    ((db * 100.0).round() as i32).clamp(MIN_CDB, MAX_CDB) as i16
}

fn from_cdb(v: i16) -> f64 {
    f64::from(v) / 100.0
}

/// dB of a power (the floor for silence).
pub fn power_db(power: f64) -> f64 {
    if power > FLOOR_POWER {
        10.0 * power.log10()
    } else {
        FLOOR_DB
    }
}

/// Power of a level in dB (0 for the floor).
pub fn db_power(db: f64) -> f64 {
    if db <= FLOOR_DB {
        0.0
    } else {
        10f64.powf(db / 10.0)
    }
}

/// Band levels per frame (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct BandFrames {
    bands: usize,
    frames: usize,
    left: Vec<i16>,
    right: Vec<i16>,
    corr: Vec<i16>,
}

impl BandFrames {
    pub fn bands(&self) -> usize {
        self.bands
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    fn at(&self, frame: usize, band: usize) -> usize {
        frame * self.bands + band
    }

    /// Left and right levels (dB).
    pub fn levels(&self, frame: usize, band: usize) -> (f64, f64) {
        let i = self.at(frame, band);
        (from_cdb(self.left[i]), from_cdb(self.right[i]))
    }

    /// The left and right powers.
    pub fn powers(&self, frame: usize, band: usize) -> (f64, f64) {
        let (l, r) = self.levels(frame, band);
        (db_power(l), db_power(r))
    }

    /// The mean of the two channels' powers (what a band holds, wherever
    /// it is placed).
    pub fn power(&self, frame: usize, band: usize) -> f64 {
        let (l, r) = self.powers(frame, band);
        0.5 * (l + r)
    }

    /// The band's level (dB of [`Self::power`]).
    pub fn level(&self, frame: usize, band: usize) -> f64 {
        power_db(self.power(frame, band))
    }

    /// Correlation of left and right in the band (−1 to 1; 0 in silence).
    pub fn correlation(&self, frame: usize, band: usize) -> f64 {
        f64::from(self.corr[self.at(frame, band)]) / 32_767.0
    }

    /// The mid ((L+R)/2) and side ((L−R)/2) powers.
    pub fn mid_side(&self, frame: usize, band: usize) -> (f64, f64) {
        let (l, r) = self.powers(frame, band);
        let cross = self.correlation(frame, band) * (l * r).sqrt();
        (
            ((l + r + 2.0 * cross) / 4.0).max(0.0),
            ((l + r - 2.0 * cross) / 4.0).max(0.0),
        )
    }

    /// Memory the frames take (bytes).
    pub fn bytes(&self) -> usize {
        6 * self.left.len()
    }
}

/// A track's measurements (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct TrackProfile {
    pub rate: u32,
    pub fft_size: usize,
    pub hop: usize,
    /// Channels analysed (1 or 2; more were reduced to the first two).
    pub channels: usize,
    /// Channels beyond two were left out.
    pub reduced: bool,
    /// Frames of audio analysed.
    pub length: usize,
    pub layout: BandLayout,
    pub frames: BandFrames,
    pub levels: FileLevels,
    /// Integrated loudness (LUFS) of each requested span (`None`: too
    /// short or silent).
    pub span_loudness: Vec<Option<f64>>,
    /// Samples that were not finite numbers (NaN, infinity: a device
    /// failing); measured as silence.
    pub non_finite: u64,
}

impl TrackProfile {
    pub fn seconds(&self) -> f64 {
        self.length as f64 / f64::from(self.rate.max(1))
    }

    pub fn hop_seconds(&self) -> f64 {
        self.hop as f64 / f64::from(self.rate.max(1))
    }

    /// The time (seconds from the start) frame `t` is centred on.
    pub fn frame_time(&self, t: usize) -> f64 {
        ((t * self.hop) as f64 + self.fft_size as f64 / 2.0) / f64::from(self.rate.max(1))
    }

    /// The span of time frame `t` stands for: half a hop either side of its
    /// centre, inside the analysed range.
    pub fn frame_span(&self, t: usize) -> (f64, f64) {
        let c = self.frame_time(t);
        let h = self.hop_seconds() / 2.0;
        ((c - h).max(0.0), (c + h).min(self.seconds()))
    }

    /// The frames whose centres lie in `[a, b)` seconds.
    pub fn frames_in(&self, a: f64, b: f64) -> std::ops::Range<usize> {
        let rate = f64::from(self.rate.max(1));
        let first = ((a * rate - self.fft_size as f64 / 2.0) / self.hop as f64)
            .ceil()
            .max(0.0) as usize;
        let last = ((b * rate - self.fft_size as f64 / 2.0) / self.hop as f64)
            .ceil()
            .max(0.0) as usize;
        first.min(self.frames.frames())..last.min(self.frames.frames())
    }

    /// Mean power per band over `frames` (all frames when `None`).
    pub fn mean_powers(&self, frames: Option<std::ops::Range<usize>>) -> Vec<f64> {
        let range = frames.unwrap_or(0..self.frames.frames());
        let n = range.len().max(1) as f64;
        (0..self.frames.bands())
            .map(|b| range.clone().map(|t| self.frames.power(t, b)).sum::<f64>() / n)
            .collect()
    }

    /// Memory the profile takes, roughly (bytes).
    pub fn bytes(&self) -> usize {
        self.frames.bytes() + 512 + 64 * self.layout.len()
    }
}

/// What [`analyse`] works with.
pub struct ProfileRequest<'a> {
    /// Planar channels of equal length (at `rate`).
    pub channels: &'a [&'a [f32]],
    pub rate: u32,
    pub config: &'a AnalysisConfig,
    /// Spans (frames from the start) whose integrated loudness is wanted.
    pub spans: &'a [(usize, usize)],
    /// Set from another thread to stop; checked every few frames.
    pub cancel: Option<&'a AtomicBool>,
}

struct Stft {
    window: Vec<f64>,
    norm: f64,
    weights: BinWeights,
    re: [Vec<f64>; 2],
    im: [Vec<f64>; 2],
}

impl Stft {
    fn new(config: &AnalysisConfig, layout: &BandLayout, rate: u32) -> Self {
        let n = config.fft_size;
        let window: Vec<f64> = (0..n)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos())
            .collect();
        let s2: f64 = window.iter().map(|w| w * w).sum();
        Self {
            norm: 1.0 / (n as f64 * s2.max(1e-12)),
            window,
            weights: layout.bin_weights(n, f64::from(rate)),
            re: [vec![0.0; n], vec![0.0; n]],
            im: [vec![0.0; n], vec![0.0; n]],
        }
    }

    /// Transform a frame of each channel (`channels[c]` from the frame's
    /// first sample; zero past its end).
    fn frame(&mut self, channels: &[&[f32]]) {
        for (c, ch) in channels.iter().enumerate().take(2) {
            let (re, im) = (&mut self.re[c], &mut self.im[c]);
            for (i, (r, w)) in re.iter_mut().zip(&self.window).enumerate() {
                *r = ch.get(i).map_or(0.0, |&v| f64::from(v)) * w;
            }
            im.fill(0.0);
            faderframe_analysis::fft(re, im);
        }
    }

    /// One-sided power scale of bin `k`.
    fn scale(&self, k: usize) -> f64 {
        let n = self.window.len();
        if k == 0 || k == n / 2 {
            self.norm
        } else {
            2.0 * self.norm
        }
    }

    /// The frame just transformed, into `out` (left, right, correlation per
    /// band).
    fn bands(&self, stereo: bool, out: &mut BandFrames) {
        for b in 0..out.bands {
            let (mut pl, mut pr, mut cross) = (0.0, 0.0, 0.0);
            for &(k, w) in self.weights.band(b) {
                let s = self.scale(k) * f64::from(w);
                let (lr, li) = (self.re[0][k], self.im[0][k]);
                pl += s * (lr * lr + li * li);
                if stereo {
                    let (rr, ri) = (self.re[1][k], self.im[1][k]);
                    pr += s * (rr * rr + ri * ri);
                    // Re(L · conj(R)).
                    cross += s * (lr * rr + li * ri);
                }
            }
            if !stereo {
                pr = pl;
                cross = pl;
            }
            let rho = if pl > FLOOR_POWER && pr > FLOOR_POWER {
                (cross / (pl * pr).sqrt()).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            out.left.push(to_cdb(pl));
            out.right.push(to_cdb(pr));
            out.corr.push((rho * 32_767.0).round() as i16);
        }
        out.frames += 1;
    }
}

/// A span whose loudness is measured: its sample range and meter.
type Span = (usize, usize, faderframe_analysis::WeightedLoudness);

/// Measures audio fed piece by piece as it is rendered (see the module
/// docs): only a frame's worth of samples is held, whatever the length.
pub struct ProfileBuilder {
    rate: u32,
    fft_size: usize,
    hop: usize,
    channels: usize,
    layout: BandLayout,
    stft: Stft,
    /// Samples not yet past (the first two channels): `pending[c][0]` is
    /// sample `base`.
    pending: [Vec<f32>; 2],
    base: usize,
    /// Where the next frame starts.
    next: usize,
    length: usize,
    frames: BandFrames,
    meter: FileMeter,
    clean: Vec<Vec<f32>>,
    spans: Vec<Span>,
    non_finite: u64,
}

impl ProfileBuilder {
    /// `channels` channels at `rate`; `spans` (sample ranges) whose
    /// integrated loudness is wanted.
    pub fn new(
        rate: u32,
        channels: usize,
        config: &AnalysisConfig,
        spans: &[(usize, usize)],
    ) -> Result<Self, ProfileError> {
        config.validate(rate)?;
        if channels == 0 {
            return Err(ProfileError::NoChannels);
        }
        let layout = config.layout(rate);
        let weights = vec![1.0; channels];
        Ok(Self {
            rate,
            fft_size: config.fft_size,
            hop: config.hop,
            channels,
            stft: Stft::new(config, &layout, rate),
            frames: BandFrames {
                bands: layout.len(),
                frames: 0,
                left: Vec::new(),
                right: Vec::new(),
                corr: Vec::new(),
            },
            layout,
            pending: [Vec::new(), Vec::new()],
            base: 0,
            next: 0,
            length: 0,
            // Levels of the whole range (all channels, BS.1770 weight 1
            // each).
            meter: FileMeter::new(rate, channels, Some(&weights), 1.0),
            clean: vec![Vec::new(); channels],
            spans: spans
                .iter()
                .map(|&(a, b)| {
                    (
                        a,
                        b,
                        faderframe_analysis::WeightedLoudness::new(rate, &weights),
                    )
                })
                .collect(),
            non_finite: 0,
        })
    }

    /// The next piece of every channel (equal lengths).
    pub fn push(&mut self, piece: &[&[f32]]) -> Result<(), ProfileError> {
        if piece.len() != self.channels {
            return Err(ProfileError::NoChannels);
        }
        let n = piece.first().map_or(0, |c| c.len());
        if piece.iter().any(|c| c.len() != n) {
            return Err(ProfileError::Ragged);
        }
        // Samples that are not numbers count as silence (and are counted).
        let mut bad = 0u64;
        for (c, ch) in self.clean.iter_mut().zip(piece) {
            c.clear();
            c.extend(ch.iter().map(|&v| {
                if v.is_finite() {
                    v
                } else {
                    bad += 1;
                    0.0
                }
            }));
        }
        self.non_finite += bad;
        let clean: Vec<&[f32]> = self.clean.iter().map(Vec::as_slice).collect();
        self.meter.add(&clean);
        let (from, to) = (self.length, self.length + n);
        for (a, b, l) in &mut self.spans {
            let (x, y) = ((*a).max(from), (*b).min(to));
            if y > x {
                let part: Vec<&[f32]> = clean.iter().map(|c| &c[x - from..y - from]).collect();
                l.add(&part);
            }
        }
        for (p, c) in self.pending.iter_mut().zip(&clean) {
            p.extend_from_slice(c);
        }
        self.length = to;
        let stereo = self.channels >= 2;
        let used = self.channels.min(2);
        while self.next + self.fft_size <= self.length {
            let off = self.next - self.base;
            let views: Vec<&[f32]> = self.pending[..used].iter().map(|p| &p[off..]).collect();
            self.stft.frame(&views);
            self.stft.bands(stereo, &mut self.frames);
            self.next += self.hop;
        }
        // Keep from the next frame's start on.
        let keep_from = self.next.min(self.length) - self.base;
        if keep_from > 0 {
            for p in &mut self.pending {
                p.drain(..keep_from.min(p.len()));
            }
            self.base += keep_from;
        }
        Ok(())
    }

    /// Everything pushed so far, measured: the last frames padded with
    /// silence.
    pub fn finish(mut self) -> TrackProfile {
        let (n, hop) = (self.fft_size, self.hop);
        let total = if self.length <= n {
            usize::from(self.length > 0)
        } else {
            (self.length - n).div_ceil(hop) + 1
        };
        let stereo = self.channels >= 2;
        let used = self.channels.min(2);
        while self.frames.frames < total {
            let off = self.next.saturating_sub(self.base);
            let views: Vec<&[f32]> = self.pending[..used]
                .iter()
                .map(|p| p.get(off..).unwrap_or(&[]))
                .collect();
            self.stft.frame(&views);
            self.stft.bands(stereo, &mut self.frames);
            self.next += hop;
        }
        let length = self.length;
        TrackProfile {
            rate: self.rate,
            fft_size: n,
            hop,
            channels: used,
            reduced: self.channels > 2,
            length,
            layout: self.layout,
            frames: self.frames,
            levels: self.meter.finish(),
            span_loudness: self
                .spans
                .into_iter()
                .map(|(a, b, l)| {
                    (b.min(length) > a)
                        .then(|| l.integrated())
                        .filter(|v| v.is_finite())
                })
                .collect(),
            non_finite: self.non_finite,
        }
    }

    /// Memory held now (bytes, roughly).
    pub fn bytes(&self) -> usize {
        self.frames.bytes() + 4 * (self.pending[0].capacity() + self.pending[1].capacity())
    }
}

/// Measure `req.channels` at once (see [`ProfileBuilder`] for audio
/// arriving in pieces).
pub fn analyse(req: &ProfileRequest<'_>) -> Result<TrackProfile, ProfileError> {
    let first = *req.channels.first().ok_or(ProfileError::NoChannels)?;
    if req.channels.iter().any(|c| c.len() != first.len()) {
        return Err(ProfileError::Ragged);
    }
    let mut b = ProfileBuilder::new(req.rate, req.channels.len(), req.config, req.spans)?;
    const PIECE: usize = 65_536;
    let mut at = 0;
    while at < first.len() {
        if req.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(ProfileError::Cancelled);
        }
        let end = (at + PIECE).min(first.len());
        let piece: Vec<&[f32]> = req.channels.iter().map(|c| &c[at..end]).collect();
        b.push(&piece)?;
        at = end;
    }
    if req.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(ProfileError::Cancelled);
    }
    Ok(b.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    pub(crate) fn sine(hz: f64, amp: f64, seconds: f64) -> Vec<f32> {
        (0..(seconds * f64::from(SR)) as usize)
            .map(|i| (amp * (TAU * hz * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    }

    fn profile(channels: &[&[f32]]) -> TrackProfile {
        let config = AnalysisConfig::for_rate(SR);
        analyse(&ProfileRequest {
            channels,
            rate: SR,
            config: &config,
            spans: &[],
            cancel: None,
        })
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn band_of(p: &TrackProfile, hz: f64) -> usize {
        p.layout.index_of(hz).unwrap_or(usize::MAX)
    }

    #[test]
    fn silence_reads_the_floor_everywhere() {
        let z = vec![0.0f32; SR as usize];
        let p = profile(&[&z]);
        assert!(p.frames.frames() > 5);
        for t in 0..p.frames.frames() {
            for b in 0..p.frames.bands() {
                assert_eq!(p.frames.level(t, b), FLOOR_DB);
                assert_eq!(p.frames.correlation(t, b), 0.0);
            }
        }
        assert_eq!(p.levels.sample_peak, f64::NEG_INFINITY);
    }

    #[test]
    fn a_sine_reads_its_mean_square_in_its_band() {
        // Amplitude 0.5: mean square 0.125 = −9.03 dB.
        let s = sine(1000.0, 0.5, 2.0);
        let p = profile(&[&s]);
        let b = band_of(&p, 1000.0);
        let t = p.frames.frames() / 2;
        let lvl = p.frames.level(t, b);
        assert!((lvl - -9.03).abs() < 0.1, "{lvl}");
        // Far bands hold (almost) nothing.
        assert!(p.frames.level(t, band_of(&p, 100.0)) < lvl - 80.0);
        assert!(p.frames.level(t, band_of(&p, 8000.0)) < lvl - 80.0);
        // Mono: same both sides, fully correlated.
        let (l, r) = p.frames.levels(t, b);
        assert_eq!(l, r);
        assert!((p.frames.correlation(t, b) - 1.0).abs() < 1e-4);
        // The whole range's levels.
        assert!((p.levels.sample_peak - -6.02).abs() < 0.01);
        assert!((p.levels.rms - -9.03).abs() < 0.02);
    }

    #[test]
    fn two_sines_and_their_ratio() {
        // 100 Hz at 0.4 and 2 kHz at 0.1: 12.04 dB apart.
        let a = sine(100.0, 0.4, 2.0);
        let b = sine(2000.0, 0.1, 2.0);
        let mix: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let p = profile(&[&mix]);
        let t = p.frames.frames() / 2;
        let d = p.frames.level(t, band_of(&p, 100.0)) - p.frames.level(t, band_of(&p, 2000.0));
        assert!((d - 12.04).abs() < 0.15, "{d}");
    }

    #[test]
    fn white_noise_spreads_over_the_bands_by_width() {
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..SR as usize * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0) * 0.5
            })
            .collect();
        let p = profile(&[&noise]);
        // Uniform in ±0.5: σ² = 1/12; per band σ²·width/(rate/2).
        let mean = p.mean_powers(None);
        for (b, band) in p.layout.bands().iter().enumerate() {
            if band.centre < 200.0 {
                continue;
            }
            let want = (1.0 / 12.0) * (band.hi - band.lo) / 24_000.0;
            let got = mean[b];
            let err = 10.0 * (got / want).log10();
            assert!(err.abs() < 0.6, "{} Hz: {err} dB", band.centre);
        }
    }

    #[test]
    fn stereo_relationships_show_in_the_correlation() {
        let s = sine(500.0, 0.5, 1.0);
        let inverted: Vec<f32> = s.iter().map(|v| -v).collect();
        let z = vec![0.0f32; s.len()];
        let b500 = |p: &TrackProfile| band_of(p, 500.0);
        let same = profile(&[&s, &s]);
        let t = same.frames.frames() / 2;
        assert!((same.frames.correlation(t, b500(&same)) - 1.0).abs() < 1e-3);
        let (m, side) = same.frames.mid_side(t, b500(&same));
        assert!(side < m * 1e-6);
        let inv = profile(&[&s, &inverted]);
        assert!((inv.frames.correlation(t, b500(&inv)) + 1.0).abs() < 1e-3);
        let (m, side) = inv.frames.mid_side(t, b500(&inv));
        assert!(m < side * 1e-6, "an inverted pair cancels in mono");
        // Left only: right silent, no correlation.
        let left = profile(&[&s, &z]);
        let (l, r) = left.frames.levels(t, b500(&left));
        assert!(l > -10.0 && r == FLOOR_DB);
        assert_eq!(left.frames.correlation(t, b500(&left)), 0.0);
        // A quarter-period delay (90°): uncorrelated in the band.
        let shift = (f64::from(SR) / 500.0 / 4.0) as usize;
        let delayed: Vec<f32> = (0..s.len())
            .map(|i| if i >= shift { s[i - shift] } else { 0.0 })
            .collect();
        let quad = profile(&[&s, &delayed]);
        assert!(quad.frames.correlation(t, b500(&quad)).abs() < 0.05);
    }

    #[test]
    fn an_impulse_and_a_transient_are_found_in_their_frames() {
        let mut x = vec![0.0f32; SR as usize * 2];
        x[SR as usize] = 1.0;
        let p = profile(&[&x]);
        let loud: Vec<usize> = (0..p.frames.frames())
            .filter(|&t| p.frames.level(t, band_of(&p, 1000.0)) > -120.0)
            .collect();
        assert!(!loud.is_empty());
        // The frames that saw it are the ones whose windows cover 1 s.
        for t in loud {
            let start = t * p.hop;
            assert!(
                start <= SR as usize && start + p.fft_size > SR as usize,
                "{t}"
            );
        }
        // An impulse is flat: neighbouring bands read alike.
        let t = p.frames_in(0.99, 1.01).start;
        let a = p.frames.level(t, band_of(&p, 1000.0));
        let b = p.frames.level(t, band_of(&p, 2000.0));
        // Third octaves double their width per octave: +3 dB.
        assert!((b - a - 3.0).abs() < 0.5, "{a} {b}");
    }

    #[test]
    fn clipping_and_span_loudness_are_measured() {
        let hot: Vec<f32> = sine(100.0, 1.5, 6.0)
            .into_iter()
            .map(|v| v.clamp(-1.0, 1.0))
            .collect();
        let config = AnalysisConfig::for_rate(SR);
        let p = analyse(&ProfileRequest {
            channels: &[&hot],
            rate: SR,
            config: &config,
            spans: &[
                (0, SR as usize * 3),
                (SR as usize * 3, SR as usize * 6),
                (5, 5),
            ],
            cancel: None,
        })
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(p.levels.overs > 100 && p.levels.full_scale_samples > 1000);
        assert_eq!(p.span_loudness.len(), 3);
        assert!(p.span_loudness[0].is_some() && p.span_loudness[2].is_none());
        let (a, b) = (
            p.span_loudness[0].unwrap_or(0.0),
            p.span_loudness[1].unwrap_or(1.0),
        );
        assert!((a - b).abs() < 0.1, "steady: {a} {b}");
    }

    #[test]
    fn pieces_of_any_size_measure_the_same() {
        let a = sine(440.0, 0.3, 3.0);
        let b = sine(97.0, 0.2, 3.0);
        let config = AnalysisConfig::for_rate(SR);
        let whole = analyse(&ProfileRequest {
            channels: &[&a, &b],
            rate: SR,
            config: &config,
            spans: &[(1000, 100_000)],
            cancel: None,
        })
        .unwrap_or_else(|e| panic!("{e}"));
        for piece in [1, 777, 4096, 100_000] {
            let mut builder = ProfileBuilder::new(SR, 2, &config, &[(1000, 100_000)])
                .unwrap_or_else(|e| panic!("{e}"));
            let mut at = 0;
            while at < a.len() {
                let end = (at + piece).min(a.len());
                builder
                    .push(&[&a[at..end], &b[at..end]])
                    .unwrap_or_else(|e| panic!("{e}"));
                at = end;
            }
            assert!(builder.bytes() < 1_000_000, "bounded memory");
            assert_eq!(builder.finish(), whole, "pieces of {piece}");
        }
    }

    #[test]
    fn bad_input_is_refused_and_cancelling_stops() {
        let config = AnalysisConfig::for_rate(SR);
        let req = |ch: &'static [&'static [f32]]| ProfileRequest {
            channels: ch,
            rate: SR,
            config: &config,
            spans: &[],
            cancel: None,
        };
        assert_eq!(analyse(&req(&[])).err(), Some(ProfileError::NoChannels));
        assert_eq!(
            analyse(&req(&[&[0.0, 0.0], &[0.0]])).err(),
            Some(ProfileError::Ragged)
        );
        let s = sine(100.0, 0.5, 1.0);
        let stop = AtomicBool::new(true);
        let r = analyse(&ProfileRequest {
            channels: &[&s],
            rate: SR,
            config: &config,
            spans: &[],
            cancel: Some(&stop),
        });
        assert_eq!(r.err(), Some(ProfileError::Cancelled));
        // NaNs never reach the levels; they are counted.
        let nan = vec![f32::NAN; 20_000];
        let p = analyse(&ProfileRequest {
            channels: &[&nan],
            rate: SR,
            config: &config,
            spans: &[],
            cancel: None,
        })
        .unwrap_or_else(|e| panic!("{e}"));
        for t in 0..p.frames.frames() {
            for b in 0..p.frames.bands() {
                assert!(p.frames.level(t, b).is_finite());
                assert!(p.frames.correlation(t, b).is_finite());
            }
        }
        assert_eq!(p.non_finite, 20_000);
        assert!(!p.levels.rms.is_nan());
    }
}
