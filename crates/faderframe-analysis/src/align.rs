//! Microphone alignment: how much later, and in which polarity, each of
//! several microphones on one source hears it than the others (a drum
//! kit's close, overhead and room microphones, a guitar cabinet's close
//! and room ones, a bass's DI and microphone).
//!
//! Every pair of tracks is compared by generalised cross-correlation: the
//! cross-spectrum averaged over Hann frames where both are active
//! (Welch), weighted by the maximum-likelihood (Hannan–Thomson) weight
//! `γ² / (1 − γ²) / |G|` — the phase transform (PHAT) scaled by how
//! coherent the two are in each band — so level and filtering do not
//! count (only the phase does), bands where one microphone hears only
//! noise or bleed fall away, and the room's reflections stay sharp peaks
//! of their own, after the direct sound's. The band is 30 Hz to 18 kHz
//! with soft edges.
//!
//! The strongest peak within `±max_lag` gives the lag and the polarity
//! (its sign); its height over the correlation's spread (a robust
//! z-score) gives the confidence: unrelated material stays near the
//! noise of the search window. The lag is then refined on the continuous
//! (band-limited) correlation between the samples, and once more with
//! the second track's frames moved by the whole samples found, so the
//! frames overlap fully (a Hann frame's lag smears line spectra towards
//! zero lag otherwise): sub-sample precision.
//!
//! Tracks related to everyone through one *hub* (the track sharing the
//! most with the others, usually the overheads) get an arrival time; the
//! default reference is the earliest (the closest microphone). Against
//! another reference a track is measured directly, or through the hub
//! where that is surer (a kick microphone and a snare microphone share
//! only bleed; both share the kit with the overheads).

use crate::fft;
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};

/// What to search.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignSettings {
    /// The furthest lag searched either way (seconds): 30 ms is about
    /// ten metres of air.
    pub max_lag: f64,
    /// The band compared (Hz).
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Default for AlignSettings {
    fn default() -> Self {
        Self {
            max_lag: 0.030,
            low_hz: 30.0,
            high_hz: 18_000.0,
        }
    }
}

/// Least confidence a lag is used with (below it: no common signal).
pub const MIN_CONFIDENCE: f32 = 0.25;

/// z-scores mapped to confidence 0 and 1.
const Z_NONE: f64 = 7.0;
const Z_SURE: f64 = 18.0;

/// Most frames compared (the busiest of the analysed audio).
const MAX_FRAMES: usize = 160;

/// Frames quieter than this under a track's loudest (dB) are not active.
const ACTIVE_DB: f64 = 50.0;

/// Frames quieter than this (mean square, about −100 dBFS) are silence.
const SILENCE: f64 = 1e-10;

/// One pair's finding: how many samples later `b` hears what `a` hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairLag {
    /// Samples (fractional): positive when `b` is later.
    pub lag: f64,
    /// `b` is the other way up.
    pub inverted: bool,
    /// 0 … 1.
    pub confidence: f32,
    /// The peak's height over the correlation's spread.
    pub score: f64,
    /// How far the peak stands over the next one (0: another as high,
    /// 1: alone) — low for two microphones sharing only bleed, each
    /// hearing the other's source.
    pub clarity: f32,
}

impl PairLag {
    /// The same finding seen from `b`.
    fn reversed(self) -> Self {
        Self {
            lag: -self.lag,
            ..self
        }
    }

    pub fn related(&self) -> bool {
        self.confidence >= MIN_CONFIDENCE
    }

    /// How much to trust it when choosing between findings.
    pub fn sureness(&self) -> f32 {
        self.confidence * (0.2 + 0.8 * self.clarity)
    }
}

/// Every pair of tracks compared.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub rate: f64,
    pub tracks: usize,
    /// The upper triangle by `index(a, b)` (`a < b`): `None` when either
    /// is silent where the other plays.
    pairs: Vec<Option<PairLag>>,
    /// Silent tracks (nothing to compare).
    pub silent: Vec<bool>,
    /// The track related to the most others (arrival times are its lags).
    pub hub: Option<usize>,
}

/// A track's place against the reference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackLag {
    /// Samples it hears the source after the reference (`None`: no common
    /// signal): advancing it this much aligns it.
    pub lag: Option<f64>,
    /// The other way up from the reference.
    pub inverted: bool,
    pub confidence: f32,
    /// Found through the hub rather than against the reference directly.
    pub via_hub: bool,
}

impl Analysis {
    fn index(&self, a: usize, b: usize) -> usize {
        debug_assert!(a < b && b < self.tracks);
        a * self.tracks - a * (a + 1) / 2 + (b - a - 1)
    }

    /// How much later `b` hears it than `a` (`None`: nothing in common
    /// to compare).
    pub fn pair(&self, a: usize, b: usize) -> Option<PairLag> {
        use std::cmp::Ordering as O;
        match a.cmp(&b) {
            O::Equal => Some(PairLag {
                lag: 0.0,
                inverted: false,
                confidence: 1.0,
                score: f64::INFINITY,
                clarity: 1.0,
            }),
            O::Less => self.pairs.get(self.index(a, b)).copied().flatten(),
            O::Greater => self
                .pairs
                .get(self.index(b, a))
                .copied()
                .flatten()
                .map(PairLag::reversed),
        }
    }

    /// When each track hears the source, in samples after the hub (`None`:
    /// unrelated to the hub).
    pub fn arrivals(&self) -> Vec<Option<f64>> {
        let Some(hub) = self.hub else {
            return vec![None; self.tracks];
        };
        (0..self.tracks)
            .map(|i| self.pair(hub, i).filter(PairLag::related).map(|p| p.lag))
            .collect()
    }

    /// The track that hears the source first (the closest microphone).
    pub fn earliest(&self) -> Option<usize> {
        self.arrivals()
            .into_iter()
            .enumerate()
            .filter_map(|(i, t)| t.map(|t| (i, t)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Every track against `reference`: directly, or through the hub
    /// where that is surer.
    pub fn relative_to(&self, reference: usize) -> Vec<TrackLag> {
        (0..self.tracks)
            .map(|i| {
                if i == reference {
                    return TrackLag {
                        lag: Some(0.0),
                        inverted: false,
                        confidence: 1.0,
                        via_hub: false,
                    };
                }
                let direct = self.pair(reference, i).filter(PairLag::related);
                let through = self
                    .hub
                    .filter(|h| *h != reference && *h != i)
                    .and_then(|h| {
                        let to_ref = self.pair(h, reference).filter(PairLag::related)?;
                        let to_i = self.pair(h, i).filter(PairLag::related)?;
                        Some(PairLag {
                            lag: to_i.lag - to_ref.lag,
                            inverted: to_i.inverted != to_ref.inverted,
                            // Two findings: a little less sure than either.
                            confidence: to_i.confidence.min(to_ref.confidence) * 0.9,
                            score: to_i.score.min(to_ref.score),
                            clarity: to_i.clarity.min(to_ref.clarity),
                        })
                    });
                match (direct, through) {
                    (Some(d), Some(t)) if t.sureness() > d.sureness() => TrackLag {
                        lag: Some(t.lag),
                        inverted: t.inverted,
                        confidence: t.confidence,
                        via_hub: true,
                    },
                    (Some(d), _) => TrackLag {
                        lag: Some(d.lag),
                        inverted: d.inverted,
                        confidence: d.confidence,
                        via_hub: false,
                    },
                    (None, Some(t)) => TrackLag {
                        lag: Some(t.lag),
                        inverted: t.inverted,
                        confidence: t.confidence,
                        via_hub: true,
                    },
                    (None, None) => TrackLag {
                        lag: None,
                        inverted: false,
                        confidence: direct
                            .or(self.pair(reference, i))
                            .map_or(0.0, |p| p.confidence),
                        via_hub: false,
                    },
                }
            })
            .collect()
    }
}

/// The band's bins of one frame's spectrum (re, im).
type Bins = Vec<(f64, f64)>;

/// A track's analysed frames: their index and spectrum.
type Spectra = Vec<(usize, Bins)>;

/// Frames of one analysis: their size, hop and the band.
struct Frames {
    size: usize,
    hop: usize,
    window: Vec<f64>,
    /// Bins compared and their weight (soft band edges).
    lo: usize,
    hi: usize,
    taper: Vec<f64>,
}

impl Frames {
    fn new(rate: f64, max_lag: usize, settings: &AlignSettings) -> Self {
        // Long enough that the furthest lag keeps most of a frame in
        // common, about 0.17 s anyway.
        let size = ((rate * 0.17) as usize)
            .max(6 * max_lag)
            .max(1024)
            .next_power_of_two();
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / size as f64).cos())
            .collect();
        let bin = |hz: f64| ((hz * size as f64 / rate).round() as usize).clamp(1, size / 2 - 1);
        let high = settings.high_hz.min(rate * 0.45);
        let (lo, hi) = (
            bin(settings.low_hz),
            bin(high).max(bin(settings.low_hz) + 2),
        );
        // An octave of raised cosine above the low edge, 15 % below the
        // high one.
        let lo_full = bin(settings.low_hz * 2.0).min(hi);
        let hi_full = bin(high * 0.85).max(lo_full);
        let taper = (lo..=hi)
            .map(|k| {
                let rise = if k < lo_full {
                    let x = (k - lo) as f64 / (lo_full - lo).max(1) as f64;
                    0.5 - 0.5 * (PI * x).cos()
                } else {
                    1.0
                };
                let fall = if k > hi_full {
                    let x = (hi - k) as f64 / (hi - hi_full).max(1) as f64;
                    0.5 - 0.5 * (PI * x).cos()
                } else {
                    1.0
                };
                rise * fall
            })
            .collect();
        Self {
            size,
            hop: size / 2,
            window,
            lo,
            hi,
            taper,
        }
    }

    /// Frames in `len` samples (one, padded, when shorter than a frame).
    fn count(&self, len: usize) -> usize {
        if len <= self.size {
            usize::from(len > 0)
        } else {
            (len - self.size) / self.hop + 1
        }
    }

    /// Windowed samples of frame `f` of `x` starting `shift` samples later.
    fn fill(&self, x: &[f32], f: usize, shift: i64, out: &mut [f64]) {
        let start = (f * self.hop) as i64 + shift;
        for (i, (o, w)) in out.iter_mut().zip(&self.window).enumerate() {
            let at = start + i as i64;
            *o = if at >= 0 {
                x.get(at as usize).map_or(0.0, |v| f64::from(*v) * w)
            } else {
                0.0
            };
        }
    }

    /// The band's bins of two frames at once (one complex FFT).
    fn spectra(&self, a: &[f64], b: &[f64], re: &mut [f64], im: &mut [f64]) -> [Bins; 2] {
        re.copy_from_slice(a);
        im.copy_from_slice(b);
        fft(re, im);
        let n = self.size;
        let mut sa = Vec::with_capacity(self.hi - self.lo + 1);
        let mut sb = Vec::with_capacity(self.hi - self.lo + 1);
        for k in self.lo..=self.hi {
            let (zr, zi) = (re[k], im[k]);
            let (cr, ci) = (re[n - k], -im[n - k]);
            // A = (Z + conj Z[n−k]) / 2, B = (Z − conj Z[n−k]) / 2j.
            sa.push(((zr + cr) * 0.5, (zi + ci) * 0.5));
            sb.push(((zi - ci) * 0.5, -(zr - cr) * 0.5));
        }
        [sa, sb]
    }
}

/// Mean square of every frame of `x`.
fn frame_energies(x: &[f32], frames: &Frames, count: usize) -> Vec<f64> {
    (0..count)
        .map(|f| {
            let a = (f * frames.hop).min(x.len());
            let b = (a + frames.size).min(x.len());
            let sum: f64 = x[a..b].iter().map(|v| f64::from(*v).powi(2)).sum();
            sum / frames.size as f64
        })
        .collect()
}

/// The weighted cross-spectrum of a pair over `frames` (`None`: fewer
/// than two of them).
struct Cross {
    /// Weighted cross-spectrum per band bin.
    s: Bins,
}

fn cross(frames: &Frames, a: &[Bins], b: &[Bins]) -> Option<Cross> {
    let k = a.len().min(b.len());
    if k < 2 {
        return None;
    }
    let bins = frames.hi - frames.lo + 1;
    let mut g = vec![(0.0f64, 0.0f64); bins];
    let mut paa = vec![0.0f64; bins];
    let mut pbb = vec![0.0f64; bins];
    for (fa, fb) in a.iter().zip(b) {
        for i in 0..bins {
            let (ar, ai) = fa[i];
            let (br, bi) = fb[i];
            // conj(A)·B
            g[i].0 += ar * br + ai * bi;
            g[i].1 += ar * bi - ai * br;
            paa[i] += ar * ar + ai * ai;
            pbb[i] += br * br + bi * bi;
        }
    }
    let kf = k as f64;
    let s = (0..bins)
        .map(|i| {
            let (gr, gi) = g[i];
            let mag = (gr * gr + gi * gi).sqrt();
            let den = paa[i] * pbb[i];
            if mag <= 0.0 || den <= 0.0 {
                return (0.0, 0.0);
            }
            let weight = if k >= 4 {
                // Magnitude-squared coherence, its bias with `k` frames
                // taken off (unrelated bins weigh nothing).
                let c = (mag * mag / den).min(1.0);
                let c = ((c - 1.0 / kf) / (1.0 - 1.0 / kf)).clamp(0.0, 0.98);
                c / (1.0 - c)
            } else {
                1.0
            } * frames.taper[i]
                / mag;
            (gr * weight, gi * weight)
        })
        .collect();
    Some(Cross { s })
}

impl Cross {
    /// The correlation at whole lags −`max`…`max` (index `max + lag`).
    fn correlation(&self, frames: &Frames, max: usize) -> Vec<f64> {
        let n = frames.size;
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        for (i, (sr, si)) in self.s.iter().enumerate() {
            let k = frames.lo + i;
            // Inverse transform through the forward one: conjugate in,
            // Hermitian spectrum (the other half mirrored).
            re[k] = *sr;
            im[k] = -*si;
            re[n - k] = *sr;
            im[n - k] = *si;
        }
        fft(&mut re, &mut im);
        (0..=2 * max)
            .map(|i| {
                let lag = i as i64 - max as i64;
                re[lag.rem_euclid(n as i64) as usize]
            })
            .collect()
    }

    /// The band-limited correlation at a fractional lag.
    fn at(&self, frames: &Frames, lag: f64) -> f64 {
        let n = frames.size as f64;
        let step = 2.0 * PI * lag / n;
        let (sr, si) = (step.cos(), step.sin());
        let start = step * frames.lo as f64;
        let (mut pr, mut pi) = (start.cos(), start.sin());
        let mut sum = 0.0;
        for (gr, gi) in &self.s {
            // Re(G · e^{jωτ})
            sum += gr * pr - gi * pi;
            let next = pr * sr - pi * si;
            pi = pr * si + pi * sr;
            pr = next;
        }
        sum
    }

    /// The peak of `sign × correlation` between `from` and `to`
    /// (golden-section search).
    fn refine(&self, frames: &Frames, from: f64, to: f64, sign: f64) -> f64 {
        let g = 0.5 * (5f64.sqrt() - 1.0);
        let (mut a, mut b) = (from, to);
        let mut c = b - g * (b - a);
        let mut d = a + g * (b - a);
        let mut fc = sign * self.at(frames, c);
        let mut fd = sign * self.at(frames, d);
        for _ in 0..40 {
            if fc > fd {
                b = d;
                d = c;
                fd = fc;
                c = b - g * (b - a);
                fc = sign * self.at(frames, c);
            } else {
                a = c;
                c = d;
                fc = fd;
                d = a + g * (b - a);
                fd = sign * self.at(frames, d);
            }
        }
        0.5 * (a + b)
    }
}

/// The peak of a correlation (whole lags, index `max + lag`): its lag,
/// sign, z-score and clarity (against the highest other local maximum
/// outside `guard`).
fn peak(r: &[f64], max: usize, guard: usize) -> (i64, f64, f64, f32) {
    let Some((i, v)) = r
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map(|(i, v)| (i, *v))
    else {
        return (0, 1.0, 0.0, 0.0);
    };
    let far = |j: usize| j.abs_diff(i) > guard;
    let mut rest: Vec<f64> = r
        .iter()
        .enumerate()
        .filter(|(j, _)| far(*j))
        .map(|(_, v)| v.abs())
        .collect();
    let z = if rest.is_empty() {
        0.0
    } else {
        let mid = rest.len() / 2;
        let (_, median, _) = rest.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        let sigma = *median / 0.6745;
        if sigma > 0.0 { v.abs() / sigma } else { 0.0 }
    };
    let second = (1..r.len().saturating_sub(1))
        .filter(|j| far(*j))
        .map(|j| r[j].abs())
        .zip((1..r.len().saturating_sub(1)).filter(|j| far(*j)))
        .filter(|(m, j)| *m >= r[j - 1].abs() && *m >= r[j + 1].abs())
        .map(|(m, _)| m)
        .fold(0.0, f64::max);
    let clarity = if v != 0.0 {
        (1.0 - second / v.abs()).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    (i as i64 - max as i64, v.signum(), z, clarity)
}

fn confidence(z: f64) -> f32 {
    ((z - Z_NONE) / (Z_SURE - Z_NONE)).clamp(0.0, 1.0) as f32
}

/// Spectra of `frames` of `x` (the band's bins), `shift` samples later.
fn spectra_of(frames: &Frames, x: &[f32], which: &[usize], shift: i64) -> Vec<Bins> {
    let n = frames.size;
    let (mut a, mut b) = (vec![0.0; n], vec![0.0; n]);
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    let mut out = Vec::with_capacity(which.len());
    for pair in which.chunks(2) {
        frames.fill(x, pair[0], shift, &mut a);
        if let Some(f) = pair.get(1) {
            frames.fill(x, *f, shift, &mut b);
        } else {
            b.fill(0.0);
        }
        let [sa, sb] = frames.spectra(&a, &b, &mut re, &mut im);
        out.push(sa);
        if pair.len() > 1 {
            out.push(sb);
        }
    }
    out
}

/// Compare every pair of `tracks` (mono, the same span, at `rate`).
/// `progress` (0 … 1000) follows the work.
pub fn analyse(
    tracks: &[&[f32]],
    rate: f64,
    settings: &AlignSettings,
    progress: Option<&AtomicU32>,
) -> Analysis {
    let n = tracks.len();
    let report = |v: u32| {
        if let Some(p) = progress {
            p.store(v.min(1000), Ordering::Relaxed);
        }
    };
    let max_lag = ((settings.max_lag.max(0.0005) * rate).ceil() as usize).max(2);
    let frames = Frames::new(rate, max_lag, settings);
    let len = tracks.iter().map(|t| t.len()).max().unwrap_or(0);
    let count = frames.count(len);
    // Which frames each track is active in.
    let energies: Vec<Vec<f64>> = tracks
        .iter()
        .map(|t| frame_energies(t, &frames, count))
        .collect();
    let active: Vec<Vec<bool>> = energies
        .iter()
        .map(|e| {
            let loudest = e.iter().copied().fold(0.0, f64::max);
            let floor = (loudest * 10f64.powf(-ACTIVE_DB / 10.0)).max(SILENCE);
            e.iter().map(|v| *v >= floor && *v > SILENCE).collect()
        })
        .collect();
    let silent: Vec<bool> = active.iter().map(|a| !a.iter().any(|x| *x)).collect();
    // The busiest frames: where most tracks play, loudest among them.
    let mut chosen: Vec<usize> = (0..count)
        .filter(|f| active.iter().filter(|a| a[*f]).count() >= 2)
        .collect();
    if chosen.len() > MAX_FRAMES {
        let score = |f: usize| -> f64 {
            let playing = active.iter().filter(|a| a[f]).count() as f64;
            let level: f64 = energies
                .iter()
                .zip(&active)
                .filter(|(_, a)| a[f])
                .map(|(e, _)| {
                    let loudest = e.iter().copied().fold(SILENCE, f64::max);
                    (e[f] / loudest).max(SILENCE).log10()
                })
                .sum::<f64>()
                / playing.max(1.0);
            playing * 100.0 + level
        };
        chosen.sort_by(|a, b| score(*b).total_cmp(&score(*a)));
        chosen.truncate(MAX_FRAMES);
        chosen.sort_unstable();
    }
    // Each track's spectra of the chosen frames it is active in.
    let mut spectra: Vec<Spectra> = Vec::with_capacity(n);
    for (t, x) in tracks.iter().enumerate() {
        let mine: Vec<usize> = chosen.iter().copied().filter(|f| active[t][*f]).collect();
        let s = spectra_of(&frames, x, &mine, 0);
        spectra.push(mine.into_iter().zip(s).collect());
        report((300 * (t + 1) / n.max(1)) as u32);
    }
    let guard = ((rate * 0.0005) as usize).max(16);
    let pairs_total = n * n.saturating_sub(1) / 2;
    let mut pairs = Vec::with_capacity(pairs_total);
    let mut done = 0;
    for a in 0..n {
        for b in a + 1..n {
            pairs.push(compare(
                &frames,
                tracks,
                &spectra,
                &active,
                &chosen,
                (a, b),
                max_lag,
                guard,
            ));
            done += 1;
            report(300 + (700 * done / pairs_total.max(1)) as u32);
        }
    }
    let mut analysis = Analysis {
        rate,
        tracks: n,
        pairs,
        silent,
        hub: None,
    };
    // The hub: related to the most (surest) others.
    analysis.hub = (0..n)
        .filter(|i| !analysis.silent[*i])
        .map(|i| {
            let sum: f32 = (0..n)
                .filter(|j| *j != i)
                .filter_map(|j| analysis.pair(i, j))
                .filter(PairLag::related)
                .map(|p| p.sureness())
                .sum();
            (i, sum)
        })
        .filter(|(_, s)| *s > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(i, _)| i);
    report(1000);
    analysis
}

/// One pair: the coarse peak on frames as they are, then again on `b`'s
/// frames moved by the whole samples found.
#[allow(clippy::too_many_arguments)]
fn compare(
    frames: &Frames,
    tracks: &[&[f32]],
    spectra: &[Spectra],
    active: &[Vec<bool>],
    chosen: &[usize],
    (a, b): (usize, usize),
    max_lag: usize,
    guard: usize,
) -> Option<PairLag> {
    let common = |t: usize| -> Vec<Bins> {
        spectra[t]
            .iter()
            .filter(|(f, _)| active[a][*f] && active[b][*f])
            .map(|(_, s)| s.clone())
            .collect()
    };
    let cx = cross(frames, &common(a), &common(b))?;
    let r = cx.correlation(frames, max_lag);
    let (lag, sign, z, clarity) = peak(&r, max_lag, guard);
    let coarse = cx.refine(frames, lag as f64 - 1.0, lag as f64 + 1.0, sign);
    let mut found = PairLag {
        lag: coarse,
        inverted: sign < 0.0,
        confidence: confidence(z),
        score: z,
        clarity,
    };
    if !found.related() || lag == 0 {
        return Some(found);
    }
    // Again with `b`'s frames that much later: the frames overlap fully.
    let both: Vec<usize> = chosen
        .iter()
        .copied()
        .filter(|f| active[a][*f] && active[b][*f])
        .collect();
    let moved = spectra_of(frames, tracks[b], &both, lag);
    if let Some(cx) = cross(frames, &common(a), &moved) {
        let fine = cx.refine(frames, -1.5, 1.5, sign);
        if fine.abs() < 1.49 {
            found.lag = lag as f64 + fine;
        }
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic noise source.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        }
    }

    const RATE: f64 = 48_000.0;

    /// A drum-like source: kicks (a falling sine and a click), snares
    /// (noise and a tone) and hats (bright noise), with silence between.
    fn drums(seconds: f64, seed: u64) -> Vec<f64> {
        let len = (seconds * RATE) as usize;
        let mut x = vec![0.0; len];
        let mut noise = Noise(seed);
        let beat = (RATE * 0.25) as usize;
        let mut t = 0;
        let mut i = 0;
        while t < len {
            let kind = i % 4;
            let vel = 0.6 + 0.4 * noise.next().abs();
            for k in 0..(RATE * 0.3) as usize {
                if t + k >= len {
                    break;
                }
                let s = k as f64 / RATE;
                let v = match kind {
                    0 => {
                        let f = 50.0 + 90.0 * (-s * 30.0).exp();
                        (2.0 * PI * f * s).sin() * (-s * 12.0).exp()
                            + 0.3 * noise.next() * (-s * 400.0).exp()
                    }
                    2 => {
                        noise.next() * 0.6 * (-s * 25.0).exp()
                            + 0.4 * (2.0 * PI * 190.0 * s).sin() * (-s * 30.0).exp()
                    }
                    _ => noise.next() * 0.25 * (-s * 60.0).exp(),
                };
                x[t + k] += v * vel;
            }
            t += beat + (noise.next() * 400.0) as usize;
            i += 1;
        }
        x
    }

    /// Delay by `d` samples (fractional) and shape by `gain(hz)` (a
    /// zero-phase filter), in the frequency domain.
    fn delayed(x: &[f64], d: f64, gain: impl Fn(f64) -> f64) -> Vec<f64> {
        let n = (x.len() + 8192).next_power_of_two();
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        re[..x.len()].copy_from_slice(x);
        fft(&mut re, &mut im);
        for k in 0..n {
            let signed = if k <= n / 2 {
                k as f64
            } else {
                k as f64 - n as f64
            };
            let hz = signed.abs() * RATE / n as f64;
            let w = -2.0 * PI * signed * d / n as f64;
            let g = gain(hz);
            let (c, s) = if k == n / 2 {
                (w.cos(), 0.0)
            } else {
                (w.cos(), w.sin())
            };
            let (r, i) = (re[k], im[k]);
            re[k] = g * (r * c - i * s);
            im[k] = g * (r * s + i * c);
        }
        // Inverse through the forward transform.
        for v in im.iter_mut() {
            *v = -*v;
        }
        fft(&mut re, &mut im);
        re.truncate(x.len());
        re.iter_mut().for_each(|v| *v /= n as f64);
        re
    }

    /// One microphone: the source delayed `d`, filtered, its reflections
    /// (delay, gain), at a level, maybe the other way up, plus noise.
    struct Mic {
        delay: f64,
        level: f64,
        inverted: bool,
        reflections: Vec<(f64, f64)>,
        noise: f64,
        filter: fn(f64) -> f64,
    }

    impl Mic {
        fn new(delay: f64) -> Self {
            Self {
                delay,
                level: 1.0,
                inverted: false,
                reflections: Vec::new(),
                noise: 0.0005,
                filter: |_| 1.0,
            }
        }

        fn record(&self, source: &[f64], seed: u64) -> Vec<f32> {
            let mut y = delayed(source, self.delay, self.filter);
            for (d, g) in &self.reflections {
                let r = delayed(source, self.delay + d, |hz| {
                    (self.filter)(hz) * 0.7f64.powf(hz / 4000.0)
                });
                for (a, b) in y.iter_mut().zip(r) {
                    *a += g * b;
                }
            }
            let mut noise = Noise(seed);
            let sign = if self.inverted { -1.0 } else { 1.0 };
            y.iter()
                .map(|v| (sign * self.level * v + self.noise * noise.next()) as f32)
                .collect()
        }
    }

    fn close(found: f64, want: f64, tolerance: f64) -> bool {
        (found - want).abs() <= tolerance
    }

    #[test]
    fn a_kit_s_microphones_are_found_to_a_twentieth_of_a_sample() {
        let source = drums(8.0, 7);
        let mics = [
            // Kick in: close, boomy.
            Mic {
                level: 1.4,
                filter: |hz| 1.0 / (1.0 + (hz / 3000.0).powi(2)).sqrt() + 0.3,
                ..Mic::new(12.0)
            },
            // Snare bottom: the other way up, bright.
            Mic {
                inverted: true,
                level: 0.5,
                filter: |hz| (hz / 400.0).min(1.0) * 0.9 + 0.1,
                ..Mic::new(23.37)
            },
            // Overheads: later, the high end, a floor reflection.
            Mic {
                level: 0.3,
                reflections: vec![(150.0, 0.45)],
                filter: |hz| 0.3 + (hz / 2000.0).min(1.0),
                ..Mic::new(171.62)
            },
            // Room: much later, dark, early reflections, noisier.
            Mic {
                level: 0.12,
                reflections: vec![(210.5, 0.6), (390.25, 0.4), (611.0, 0.3)],
                noise: 0.002,
                filter: |hz| 1.0 / (1.0 + (hz / 1500.0).powi(2)).sqrt(),
                ..Mic::new(950.81)
            },
        ];
        let recorded: Vec<Vec<f32>> = mics
            .iter()
            .enumerate()
            .map(|(i, m)| m.record(&source, 100 + i as u64))
            .collect();
        let views: Vec<&[f32]> = recorded.iter().map(Vec::as_slice).collect();
        let a = analyse(&views, RATE, &AlignSettings::default(), None);
        // The kick microphone hears it first.
        assert_eq!(a.earliest(), Some(0));
        let lags = a.relative_to(0);
        for (i, (m, l)) in mics.iter().zip(&lags).enumerate() {
            let want = m.delay - mics[0].delay;
            let lag = l.lag.expect("related");
            assert!(
                close(lag, want, 0.05),
                "mic {i}: {lag:.4} for {want:.4} ({l:?})"
            );
            assert_eq!(l.inverted, m.inverted != mics[0].inverted, "mic {i}");
            assert!(l.confidence > 0.8, "mic {i}: {l:?}");
        }
    }

    #[test]
    fn whole_and_fractional_lags_either_way() {
        let source = drums(6.0, 3);
        for (d, inverted) in [
            (0.0, false),
            (0.5, true),
            (1.0, false),
            (37.25, false),
            (-12.75, true),
            (-1_000.1, false),
            (1_400.6, true),
        ] {
            let a = Mic::new(2_000.0).record(&source, 1);
            let b = Mic {
                inverted,
                level: 0.2,
                ..Mic::new(2_000.0 + d)
            }
            .record(&source, 2);
            let r = analyse(&[&a, &b], RATE, &AlignSettings::default(), None);
            let p = r.pair(0, 1).expect("compared");
            assert!(close(p.lag, d, 0.02), "{d}: {p:?}");
            assert_eq!(p.inverted, inverted, "{d}");
            assert!(p.related(), "{d}: {p:?}");
            // And seen from the other track.
            let q = r.pair(1, 0).expect("compared");
            assert!(close(q.lag, -d, 0.02));
        }
    }

    #[test]
    fn independent_noise_and_bleed_still_find_the_lag() {
        let source = drums(8.0, 11);
        let bleed = drums(8.0, 12);
        let mut a = Mic::new(100.0).record(&source, 3);
        let mut b = Mic {
            noise: 0.02,
            ..Mic::new(345.3)
        }
        .record(&source, 4);
        // The other drum bleeds into both, from other places.
        for (x, y) in a.iter_mut().zip(delayed(&bleed, 400.0, |_| 0.3)) {
            *x += y as f32;
        }
        for (x, y) in b.iter_mut().zip(delayed(&bleed, 120.0, |_| 0.25)) {
            *x += y as f32;
        }
        let r = analyse(&[&a, &b], RATE, &AlignSettings::default(), None);
        let p = r.pair(0, 1).expect("compared");
        assert!(close(p.lag, 245.3, 0.05), "{p:?}");
        assert!(p.related(), "{p:?}");
    }

    #[test]
    fn unrelated_material_is_refused() {
        let a = Mic::new(0.0).record(&drums(6.0, 21), 5);
        let b = Mic::new(0.0).record(&drums(6.0, 22), 6);
        let mut noise = Noise(9);
        let c: Vec<f32> = (0..a.len()).map(|_| (0.1 * noise.next()) as f32).collect();
        let r = analyse(&[&a, &b, &c], RATE, &AlignSettings::default(), None);
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            let p = r.pair(i, j).expect("compared");
            assert!(!p.related(), "{i}/{j}: {p:?}");
            assert!(p.confidence < 0.1, "{i}/{j}: {p:?}");
        }
        assert!(r.relative_to(0)[1].lag.is_none());
    }

    #[test]
    fn silence_has_nothing_in_common() {
        let a = Mic::new(0.0).record(&drums(3.0, 1), 1);
        let b = vec![0.0f32; a.len()];
        let r = analyse(&[&a, &b], RATE, &AlignSettings::default(), None);
        assert!(r.silent[1]);
        assert!(r.pair(0, 1).is_none());
        assert_eq!(r.relative_to(0)[1].lag, None);
    }

    #[test]
    fn lags_beyond_the_search_are_not_found() {
        let source = drums(6.0, 5);
        let a = Mic::new(0.0).record(&source, 1);
        // 50 ms: beyond the 30 ms searched.
        let b = Mic::new(2_400.0).record(&source, 2);
        let r = analyse(&[&a, &b], RATE, &AlignSettings::default(), None);
        let p = r.pair(0, 1).expect("compared");
        assert!(!p.related() || p.lag.abs() <= 1440.0, "{p:?}");
        // A wider search finds it.
        let wide = AlignSettings {
            max_lag: 0.06,
            ..AlignSettings::default()
        };
        let p = analyse(&[&a, &b], RATE, &wide, None)
            .pair(0, 1)
            .expect("compared");
        assert!(close(p.lag, 2_400.0, 0.05), "{p:?}");
    }

    #[test]
    fn a_close_and_a_room_microphone_through_minimum_phase_filters() {
        // A guitar cabinet: the close microphone bright (a first-order
        // high-pass), the room one dark (a first-order low-pass) and
        // reflected: real filters shift the phase, so less exactly.
        let source = drums(6.0, 31);
        let close_mic = Mic::new(10.0).record(&source, 1);
        let room = Mic {
            reflections: vec![(180.0, 0.5)],
            ..Mic::new(310.4)
        }
        .record(&source, 2);
        let one_pole = |x: &[f32], hz: f64, high: bool| -> Vec<f32> {
            let a = (-2.0 * PI * hz / RATE).exp();
            let mut y = 0.0f64;
            x.iter()
                .map(|v| {
                    y = (1.0 - a) * f64::from(*v) + a * y;
                    (if high { f64::from(*v) - y } else { y }) as f32
                })
                .collect()
        };
        let a = one_pole(&close_mic, 80.0, true);
        let b = one_pole(&room, 6_000.0, false);
        let r = analyse(&[&a, &b], RATE, &AlignSettings::default(), None);
        let p = r.pair(0, 1).expect("compared");
        assert!(close(p.lag, 300.4, 0.5), "{p:?}");
        assert!(!p.inverted);
    }

    #[test]
    fn three_microphones_meet_through_the_one_hearing_everything() {
        // A kick and a snare microphone hear their own drum (and a little
        // of the other); the overheads hear both.
        let kick = drums(8.0, 41);
        let snare = drums(8.0, 42);
        let mix = |a: &[f64], da: f64, ga: f64, b: &[f64], db: f64, gb: f64, seed: u64| {
            let x = delayed(a, da, |_| ga);
            let y = delayed(b, db, |_| gb);
            let mut noise = Noise(seed);
            x.iter()
                .zip(&y)
                .map(|(p, q)| (p + q + 0.0005 * noise.next()) as f32)
                .collect::<Vec<f32>>()
        };
        let kick_in = mix(&kick, 10.0, 1.0, &snare, 60.0, 0.1, 1);
        let snare_top = mix(&snare, 12.0, 1.0, &kick, 70.0, 0.1, 2);
        let overheads = mix(&kick, 150.0, 0.5, &snare, 120.5, 0.5, 3);
        let r = analyse(
            &[&kick_in, &snare_top, &overheads],
            RATE,
            &AlignSettings::default(),
            None,
        );
        assert_eq!(r.hub, Some(2), "{r:?}");
        // Against the overheads: each close microphone by its own drum.
        let lags = r.relative_to(2);
        assert!(close(lags[0].lag.expect("kick"), -140.0, 0.05), "{lags:?}");
        assert!(close(lags[1].lag.expect("snare"), -108.5, 0.05), "{lags:?}");
    }

    #[test]
    fn progress_reaches_the_end() {
        let source = drums(2.0, 1);
        let a = Mic::new(0.0).record(&source, 1);
        let b = Mic::new(5.0).record(&source, 2);
        let p = AtomicU32::new(0);
        analyse(&[&a, &b], RATE, &AlignSettings::default(), Some(&p));
        assert_eq!(p.load(Ordering::Relaxed), 1000);
    }
}
