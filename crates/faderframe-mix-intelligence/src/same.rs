//! Same audio: where two tracks carry one audio — a copy (panned, at
//! another gain, through an EQ, a few samples later), one source processed
//! two ways (a dry track and its parallel-compressed copy), a stereo
//! recording split into two mono tracks — as opposed to two sounds that
//! merely overlap. Masking between them is meaningless; the masking rule
//! leaves such stretches out ([`crate::rules`]), judged window by window.
//!
//! **Measure.** From each profile's per-frame band sums of the mono
//! fold-down ([`crate::profile::BandFrames::mid_sum`]), over the frames of a
//! window (about [`SameRule::window_seconds`], sliding by half) where both
//! play, per band the averaged cross-spectrum `S = Σ a·conj(b)` and powers
//! give the magnitude-squared coherence `|S|² / (Σ|a|² · Σ|b|²)`, its bias
//! with `n` frames taken off (`(γ² − 1/n) / (1 − 1/n)`, as Auto-Align's
//! estimator does). Coherence is 1 for audio linearly related to the other
//! (gain, pan, a filter smooth across the band) and near 0 for independent
//! sounds, whatever their levels and spectra.
//!
//! **A small delay.** A band sum adds the band's bins: a delay `τ` turns
//! each bin by its own angle, so the band's coherence falls to the
//! *ceiling* `|Σ w_k² e^{jω_k τ}|² / (Σ w_k²)²` (a flat spectrum within the
//! band; `w_k` the bin's share). The delay is found per window as the
//! peak of `|Σ_b Re(S_b e^{−jω_b τ})| / √(P_a P_b)` over the coherent bands
//! (`ω_b` the band's centroid) within ±[`SameRule::max_lag_ms`] — the
//! band-domain version of the cross-correlation Auto-Align uses on the
//! waveform —, each band's coherence is divided by its ceiling there, and
//! bands whose ceiling is under ½ are not judged (the delay smears them).
//!
//! **Channel by channel.** Two copies of a stereo source panned (or
//! balanced) differently are different mixes of its left and right; where
//! those differ — a wide recording, a stereo chorus — the copies' mono
//! fold-downs are not linearly related, though each channel is the other's
//! at another gain. So stereo profiles also keep each channel's band sums
//! ([`crate::profile::BandFrames::channel_sum`]), and a window is also the
//! same audio when left against left and right against right are: every
//! channel pair with something to judge (a hard-panned copy's silent side
//! has nothing) reaching the threshold, at least one of them.
//!
//! **Decide.** The bands that matter in a window are those where both
//! tracks are within [`SameRule::relevance_db`] of their own loudest band
//! and above the floor. A window is the same audio when at least
//! [`SameRule::min_bands`] bands matter and both the power-weighted and the
//! plain mean of their (delay-corrected) coherence reach
//! [`SameRule::coherence`]: one loud coherent band (a shared kick under two
//! different performances) does not make the whole the same audio. Each
//! frame takes the decision of the window whose middle is nearest: a pair
//! sharing material only in some sections is judged per section. A window
//! with too little to judge (a fade, a clip's ends) is undecided and takes
//! the verdict of the nearest window that was judged, same or different.
//!
//! Cost: `O(frames · bands)` per pair, plus a delay search (`O(bands · 8
//! · max_lag)`) only in windows already coherent at zero delay. A
//! profile's band sums are decoded once ([`BandSums`]) for all its pairs.

use crate::config::SameRule;
use crate::finding::TimeSpan;
use crate::profile::{FLOOR_DB, TrackProfile, db_power};

/// Frames under this broadband power (mean square) are silence.
const SILENCE: f64 = 1e-10;

/// Which band sums a judgement reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    /// The mono fold-down.
    Mid,
    /// A channel (0 left, 1 right) of a stereo profile.
    Channel(usize),
}

/// A profile's band sums, decoded once.
#[derive(Clone, Debug)]
pub struct BandSums {
    frames: usize,
    bands: usize,
    z: Vec<(f32, f32)>,
    /// Stereo profiles: each channel's (left, then right).
    channels: Option<[Vec<(f32, f32)>; 2]>,
    /// Frames it plays in (within 40 dB of its loudest frame).
    active: Vec<bool>,
}

impl BandSums {
    pub fn of(p: &TrackProfile) -> Self {
        let (frames, bands) = (p.frames.frames(), p.frames.bands());
        let mut z = Vec::with_capacity(frames * bands);
        let mut energy = vec![0.0f64; frames];
        for (t, e) in energy.iter_mut().enumerate() {
            for b in 0..bands {
                let (re, im) = p.frames.mid_sum(t, b);
                *e += re * re + im * im;
                z.push((re as f32, im as f32));
            }
        }
        let channels = p.frames.has_channel_sums().then(|| {
            [0, 1].map(|c| {
                let mut v = Vec::with_capacity(frames * bands);
                for t in 0..frames {
                    for b in 0..bands {
                        let (re, im) = p.frames.channel_sum(t, b, c).unwrap_or((0.0, 0.0));
                        v.push((re as f32, im as f32));
                    }
                }
                v
            })
        });
        let top = energy.iter().copied().fold(0.0, f64::max);
        let floor = (top * 1e-4).max(SILENCE);
        Self {
            frames,
            bands,
            z,
            channels,
            active: energy.iter().map(|e| *e >= floor && *e > SILENCE).collect(),
        }
    }

    fn at(&self, stream: Stream, t: usize, b: usize) -> (f64, f64) {
        let z = match (stream, &self.channels) {
            (Stream::Channel(c), Some(ch)) => &ch[c.min(1)],
            _ => &self.z,
        };
        let (re, im) = z[t * self.bands + b];
        (f64::from(re), f64::from(im))
    }
}

/// Each band's bins (angular frequency per sample, squared share) and
/// centroid, for the delay's ceiling.
#[derive(Clone, Debug)]
struct Dispersion {
    bins: Vec<Vec<(f64, f64)>>,
    centroid: Vec<f64>,
}

impl Dispersion {
    fn of(p: &TrackProfile) -> Self {
        let n = p.fft_size.max(2);
        let w = p.layout.bin_weights(n, f64::from(p.rate));
        let bins: Vec<Vec<(f64, f64)>> = (0..p.layout.len())
            .map(|b| {
                w.band(b)
                    .iter()
                    .map(|&(k, s)| {
                        (
                            std::f64::consts::TAU * k as f64 / n as f64,
                            f64::from(s) * f64::from(s),
                        )
                    })
                    .collect()
            })
            .collect();
        let centroid = bins
            .iter()
            .map(|v| {
                let (num, den) = v
                    .iter()
                    .fold((0.0, 0.0), |a, (o, s)| (a.0 + o * s, a.1 + s));
                if den > 0.0 { num / den } else { 0.0 }
            })
            .collect();
        Self { bins, centroid }
    }

    /// The coherence a pure delay of `tau` samples leaves band `b` (flat
    /// spectrum within it).
    fn ceiling(&self, b: usize, tau: f64) -> f64 {
        let v = &self.bins[b];
        let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
        for (o, s) in v {
            re += s * (o * tau).cos();
            im += s * (o * tau).sin();
            sum += s;
        }
        if sum > 0.0 {
            (re * re + im * im) / (sum * sum)
        } else {
            0.0
        }
    }
}

/// One window's judgement.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// Power-weighted and plain mean coherence over the bands judged.
    pub coherence: f64,
    pub band_coherence: f64,
    pub bands: usize,
    /// The delay found (samples, positive: `b` later).
    pub lag: f64,
    /// Per band: the delay-corrected coherence (`NaN`: not judged).
    pub per_band: Vec<f64>,
    /// Sums over the bands judged: powers and the cross term.
    pub power_a: f64,
    pub power_b: f64,
    pub cross: f64,
}

impl Window {
    pub fn same(&self, rule: &SameRule) -> bool {
        self.bands >= rule.min_bands
            && self.coherence >= rule.coherence
            && self.band_coherence >= rule.coherence
    }
}

/// Judge frames `range` (see the module docs; `None`: fewer than
/// `rule.min_frames` frames both play in, or no band matters).
fn judge(
    a: &BandSums,
    b: &BandSums,
    disp: &Dispersion,
    range: std::ops::Range<usize>,
    rate: f64,
    rule: &SameRule,
    stream: Stream,
) -> Option<Window> {
    let frames: Vec<usize> = range.filter(|t| a.active[*t] && b.active[*t]).collect();
    let n = frames.len();
    if n < rule.min_frames.max(2) {
        return None;
    }
    let bands = a.bands;
    let (mut sa, mut sb) = (vec![0.0; bands], vec![0.0; bands]);
    let (mut cr, mut ci) = (vec![0.0; bands], vec![0.0; bands]);
    for &t in &frames {
        for k in 0..bands {
            let (ar, ai) = a.at(stream, t, k);
            let (br, bi) = b.at(stream, t, k);
            sa[k] += ar * ar + ai * ai;
            sb[k] += br * br + bi * bi;
            // a · conj(b).
            cr[k] += ar * br + ai * bi;
            ci[k] += ai * br - ar * bi;
        }
    }
    let nf = n as f64;
    let floor = db_power(rule.floor_db.max(FLOOR_DB + 1.0));
    let top = |s: &[f64]| s.iter().copied().fold(0.0, f64::max);
    let (ta, tb) = (top(&sa), top(&sb));
    let rel = 10f64.powf(-rule.relevance_db / 10.0);
    let matters: Vec<bool> = (0..bands)
        .map(|k| {
            sa[k] / nf >= floor && sb[k] / nf >= floor && sa[k] >= ta * rel && sb[k] >= tb * rel
        })
        .collect();
    let msc: Vec<f64> = (0..bands)
        .map(|k| {
            let den = sa[k] * sb[k];
            if den <= 0.0 {
                return 0.0;
            }
            let m = ((cr[k] * cr[k] + ci[k] * ci[k]) / den).min(1.0);
            ((m - 1.0 / nf) / (1.0 - 1.0 / nf)).clamp(0.0, 1.0)
        })
        .collect();
    let judged = |tau: f64| -> Vec<Option<f64>> {
        (0..bands)
            .map(|k| {
                if !matters[k] {
                    return None;
                }
                let c = disp.ceiling(k, tau);
                (c >= 0.5).then(|| (msc[k] / c).min(1.0))
            })
            .collect()
    };
    let means = |q: &[Option<f64>]| -> (f64, f64, usize) {
        let (mut w, mut wq, mut sum, mut cnt) = (0.0, 0.0, 0.0, 0usize);
        for (k, v) in q.iter().enumerate() {
            if let Some(v) = v {
                let u = (sa[k] * sb[k]).sqrt();
                w += u;
                wq += u * v;
                sum += v;
                cnt += 1;
            }
        }
        if cnt == 0 {
            (0.0, 0.0, 0)
        } else {
            (wq / w.max(1e-300), sum / cnt as f64, cnt)
        }
    };
    let mut lag = 0.0;
    let mut q = judged(0.0);
    let (w0, p0, _) = means(&q);
    // A delay is looked for only where something is coherent already.
    if w0.min(p0) >= 0.5 * rule.coherence {
        let reach = rule.max_lag_ms / 1000.0 * rate;
        let steps = (reach * 4.0).ceil() as i64;
        let score = |tau: f64| -> f64 {
            (0..bands)
                .filter(|k| matters[*k] && msc[*k] >= 0.2)
                .map(|k| {
                    let o = disp.centroid[k] * tau;
                    (cr[k] * o.cos() + ci[k] * o.sin()) / (sa[k] * sb[k]).sqrt().max(1e-300)
                })
                .sum::<f64>()
                .abs()
        };
        let (mut best, mut at) = (score(0.0), 0.0);
        for i in -steps..=steps {
            let tau = i as f64 / 4.0;
            let s = score(tau);
            if s > best + 1e-12 {
                best = s;
                at = tau;
            }
        }
        if at != 0.0 {
            lag = at;
            q = judged(at);
        }
    }
    let (coherence, band_coherence, count) = means(&q);
    if count == 0 {
        return None;
    }
    let (mut pa, mut pb, mut x) = (0.0, 0.0, 0.0);
    for (k, v) in q.iter().enumerate() {
        if v.is_some() {
            pa += sa[k] / nf;
            pb += sb[k] / nf;
            x += cr[k] / nf;
        }
    }
    Some(Window {
        coherence,
        band_coherence,
        bands: count,
        lag,
        per_band: q.into_iter().map(|v| v.unwrap_or(f64::NAN)).collect(),
        power_a: pa,
        power_b: pb,
        cross: x,
    })
}

/// Where two tracks carry the same audio (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SameAudio {
    /// Per frame of the profiles: the same audio there.
    pub frames: Vec<bool>,
    /// Those frames as stretches (seconds from the analysed range's
    /// start).
    pub spans: Vec<TimeSpan>,
    /// Seconds both play in them.
    pub seconds: f64,
    /// Over the same windows: the power-weighted and plain mean coherence,
    /// the mean number of bands judged.
    pub coherence: f64,
    pub band_coherence: f64,
    pub bands: f64,
    /// The median delay found (samples; `b` later when positive).
    pub lag: f64,
    /// `b`'s level against `a`'s over the bands judged (dB).
    pub level_db: f64,
    /// Their mono sum against the louder of the two alone (dB: +6 for two
    /// equal copies in phase, below 0 where they cancel).
    pub mono_gain_db: f64,
    /// Per band, the mean coherence over the same windows (`NaN`: never
    /// judged there).
    pub per_band: Vec<f64>,
}

impl SameAudio {
    pub fn any(&self) -> bool {
        self.seconds > 0.0
    }
}

/// Where `a` and `b` (profiles of the same range and settings, their band
/// sums decoded) carry the same audio. Incompatible profiles: none.
pub fn same_audio(
    pa: &TrackProfile,
    a: &BandSums,
    pb: &TrackProfile,
    b: &BandSums,
    rule: &SameRule,
) -> SameAudio {
    let frames = a.frames;
    let none = || SameAudio {
        frames: vec![false; frames],
        spans: Vec::new(),
        seconds: 0.0,
        coherence: 0.0,
        band_coherence: 0.0,
        bands: 0.0,
        lag: 0.0,
        level_db: 0.0,
        mono_gain_db: 0.0,
        per_band: vec![f64::NAN; a.bands],
    };
    if a.frames != b.frames
        || a.bands != b.bands
        || pa.rate != pb.rate
        || pa.hop != pb.hop
        || pa.fft_size != pb.fft_size
        || frames == 0
    {
        return none();
    }
    let disp = Dispersion::of(pa);
    let hop_s = pa.hop_seconds();
    let w = ((rule.window_seconds / hop_s).round() as usize).max(rule.min_frames.max(2));
    let step = (w / 2).max(1);
    let stereo = a.channels.is_some() && b.channels.is_some();
    let rate = f64::from(pa.rate);
    // Per window: the mono fold-downs' judgement, and (both stereo) each
    // channel pair's.
    let mut windows: Vec<Option<Window>> = Vec::new();
    let mut by_channel: Vec<[Option<Window>; 2]> = Vec::new();
    let mut s = 0;
    loop {
        let e = (s + w).min(frames);
        windows.push(judge(a, b, &disp, s..e, rate, rule, Stream::Mid));
        by_channel.push(if stereo {
            [0, 1].map(|c| judge(a, b, &disp, s..e, rate, rule, Stream::Channel(c)))
        } else {
            [None, None]
        });
        if e >= frames {
            break;
        }
        s += step;
    }
    // A channel pair with nothing to judge (a hard-panned copy's silent
    // side) does not count against it.
    let judged = |x: &Option<Window>| x.as_ref().is_some_and(|x| x.bands >= rule.min_bands);
    let channels_same = |pair: &[Option<Window>; 2]| {
        let same = |x: &Option<Window>| x.as_ref().is_some_and(|x| x.same(rule));
        pair.iter().any(same) && pair.iter().all(|x| same(x) || !judged(x))
    };
    let same_window: Vec<bool> = windows
        .iter()
        .zip(&by_channel)
        .map(|(x, ch)| x.as_ref().is_some_and(|x| x.same(rule)) || channels_same(ch))
        .collect();
    // Windows with too little to judge (a fade, the ends of a clip, a
    // quiet stretch) are undecided, not different: they take the verdict
    // of the nearest window that was judged — a copy's swell stays the
    // same audio, a section that differs stays different.
    let known: Vec<bool> = windows
        .iter()
        .zip(&by_channel)
        .map(|(x, ch)| judged(x) || ch.iter().any(judged))
        .collect();
    let decided: Vec<bool> = (0..same_window.len())
        .map(|i| {
            if known[i] {
                return same_window[i];
            }
            (1..same_window.len())
                .flat_map(|d| [i.checked_sub(d), Some(i + d)])
                .flatten()
                .find(|&j| j < known.len() && known[j])
                .is_some_and(|j| same_window[j])
        })
        .collect();
    // The windows found the same by their channels stand for them where
    // their mono fold-downs did not (coherence, bands, delay from the
    // channels; levels and the mono sum from the fold-downs where judged).
    let windows: Vec<Option<Window>> = windows
        .into_iter()
        .zip(&by_channel)
        .zip(&same_window)
        .map(|((mid, ch), same)| {
            if !*same || mid.as_ref().is_some_and(|x| x.same(rule)) {
                return mid;
            }
            let same: Vec<&Window> = ch
                .iter()
                .filter_map(|x| x.as_ref().filter(|x| x.same(rule)))
                .collect();
            let k = same.len() as f64;
            let first = same.first()?;
            let per_band = (0..a.bands)
                .map(|band| {
                    let v: Vec<f64> = same
                        .iter()
                        .map(|x| x.per_band[band])
                        .filter(|v| v.is_finite())
                        .collect();
                    if v.is_empty() {
                        f64::NAN
                    } else {
                        v.iter().sum::<f64>() / v.len() as f64
                    }
                })
                .collect();
            let (power_a, power_b, cross) = match &mid {
                Some(m) => (m.power_a, m.power_b, m.cross),
                None => same.iter().fold((0.0, 0.0, 0.0), |acc, x| {
                    (acc.0 + x.power_a, acc.1 + x.power_b, acc.2 + x.cross)
                }),
            };
            Some(Window {
                coherence: same.iter().map(|x| x.coherence).sum::<f64>() / k,
                band_coherence: same.iter().map(|x| x.band_coherence).sum::<f64>() / k,
                bands: same.iter().map(|x| x.bands).max().unwrap_or(0),
                lag: first.lag,
                per_band,
                power_a,
                power_b,
                cross,
            })
        })
        .collect();
    if !decided.contains(&true) {
        return none();
    }
    // Each frame: the window whose middle is nearest.
    let last = windows.len() - 1;
    let marks: Vec<bool> = (0..frames)
        .map(|t| {
            let i = ((t as f64 - w as f64 / 2.0) / step as f64)
                .round()
                .clamp(0.0, last as f64) as usize;
            decided[i]
        })
        .collect();
    let mut spans: Vec<TimeSpan> = Vec::new();
    let mut seconds = 0.0;
    for (t, m) in marks.iter().enumerate() {
        if !*m {
            continue;
        }
        if a.active[t] && b.active[t] {
            seconds += hop_s;
        }
        let (x, y) = pa.frame_span(t);
        match spans.last_mut() {
            Some(l) if x - l.end <= 1e-9 => l.end = y,
            _ => spans.push(TimeSpan::new(x, y)),
        }
    }
    let same: Vec<&Window> = windows
        .iter()
        .zip(&same_window)
        .filter_map(|(x, s)| x.as_ref().filter(|_| *s))
        .collect();
    let k = same.len() as f64;
    let (pa_sum, pb_sum, x_sum) = same.iter().fold((0.0, 0.0, 0.0), |acc, x| {
        (acc.0 + x.power_a, acc.1 + x.power_b, acc.2 + x.cross)
    });
    let mut lags: Vec<f64> = same.iter().map(|x| x.lag).collect();
    let per_band = (0..a.bands)
        .map(|band| {
            let v: Vec<f64> = same
                .iter()
                .map(|x| x.per_band[band])
                .filter(|v| v.is_finite())
                .collect();
            if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        })
        .collect();
    let db = |v: f64| crate::profile::power_db(v.max(0.0));
    SameAudio {
        frames: marks,
        spans,
        seconds,
        coherence: same.iter().map(|x| x.coherence).sum::<f64>() / k,
        band_coherence: same.iter().map(|x| x.band_coherence).sum::<f64>() / k,
        bands: same.iter().map(|x| x.bands as f64).sum::<f64>() / k,
        lag: crate::dynamics::median(&mut lags).unwrap_or(0.0),
        level_db: db(pb_sum) - db(pa_sum),
        mono_gain_db: db(pa_sum + pb_sum + 2.0 * x_sum) - db(pa_sum.max(pb_sum)),
        per_band,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::phase::tests::{Noise, delayed, drums};
    use crate::test_support::{SR, profile};

    pub(crate) fn same(a: &[&[f32]], b: &[&[f32]]) -> SameAudio {
        let (pa, pb) = (profile(a), profile(b));
        same_audio(
            &pa,
            &BandSums::of(&pa),
            &pb,
            &BandSums::of(&pb),
            &SameRule::default(),
        )
    }

    /// A first-order low-pass at `hz` and a +6 dB bell-ish boost: an EQ.
    pub(crate) fn eq(x: &[f32], hz: f64) -> Vec<f32> {
        let a = (-std::f64::consts::TAU * hz / f64::from(SR)).exp();
        let mut y = 0.0f64;
        x.iter()
            .map(|v| {
                y = (1.0 - a) * f64::from(*v) + a * y;
                (0.5 * f64::from(*v) + 1.5 * y) as f32
            })
            .collect()
    }

    const LEN: f64 = 12.0;

    /// One stretch over the whole range (the gaps between hits, where a
    /// track is quiet, are not counted in `seconds`).
    fn covers(s: &SameAudio) -> bool {
        s.spans.len() == 1
            && s.spans[0].start < 0.5
            && s.spans[0].end > LEN - 0.5
            && s.seconds > 9.0
    }

    #[test]
    fn copies_are_the_same_audio_however_they_are_placed() {
        let x = drums(LEN, 0.25, 61);
        // Panned to one side, at another gain.
        let quiet: Vec<f32> = x.iter().map(|v| 0.3 * v).collect();
        let z = vec![0.0f32; x.len()];
        let s = same(&[&x], &[&quiet, &z]);
        assert!(covers(&s), "{s:?}");
        assert!(s.coherence > 0.95 && s.band_coherence > 0.95, "{s:?}");
        // Compared as mono fold-downs: the copy on one side is half its
        // channel there (0.3 / 2).
        assert!((s.level_db - 20.0 * 0.15f64.log10()).abs() < 0.5, "{s:?}");
        // In mono the copy adds in phase: (1 + 0.15)², +1.2 dB over the
        // louder alone.
        assert!(
            (s.mono_gain_db - 20.0 * 1.15f64.log10()).abs() < 0.3,
            "{s:?}"
        );
        // Two equal copies: +6 dB in mono.
        let s = same(&[&x], &[&x]);
        assert!((s.mono_gain_db - 6.02).abs() < 0.05, "{s:?}");
        // Through an EQ.
        let s = same(&[&x], &[&eq(&x, 800.0)]);
        assert!(covers(&s), "{s:?}");
        // A few samples later (and 0.4 ms: within the allowance).
        for d in [3.0, 19.2] {
            let s = same(&[&x], &[&delayed(&x, d)]);
            assert!(covers(&s), "{d}: {s:?}");
            assert!((s.lag - d).abs() <= 1.0, "{d}: {s:?}");
        }
        // Inverted: still one audio (it cancels in mono).
        let inv: Vec<f32> = x.iter().map(|v| -v).collect();
        let s = same(&[&x], &[&inv]);
        assert!(covers(&s) && s.mono_gain_db < -40.0, "{s:?}");
    }

    /// A wide stereo source (its sides different sounds, as a stereo
    /// chorus or a spaced pair makes them) copied and the copies panned
    /// opposite ways: their mono fold-downs are different mixes of the two
    /// sides, but side by side each channel is the other's at another
    /// gain — one audio. A copy panned hard to one side too; two
    /// independent stereo sources are not.
    #[test]
    fn copies_of_a_wide_source_panned_apart_are_one_audio() {
        let left = drums(LEN, 0.25, 91);
        let right = drums(LEN, 0.375, 92);
        let pan = |l: f32, r: f32| -> [Vec<f32>; 2] {
            [
                left.iter().map(|v| l * v).collect(),
                right.iter().map(|v| r * v).collect(),
            ]
        };
        let [al, ar] = pan(1.0, 0.25);
        let [bl, br] = pan(0.25, 1.0);
        let s = same(&[&al, &ar], &[&bl, &br]);
        assert!(covers(&s), "{s:?}");
        // The mono fold-downs alone would not have said so.
        let (pa, pb) = (profile(&[&al, &ar]), profile(&[&bl, &br]));
        let (sa, sb) = (BandSums::of(&pa), BandSums::of(&pb));
        let disp = Dispersion::of(&pa);
        let mid = judge(
            &sa,
            &sb,
            &disp,
            0..sa.frames,
            f64::from(SR),
            &SameRule::default(),
            Stream::Mid,
        );
        assert!(
            mid.as_ref().is_none_or(|w| !w.same(&SameRule::default())),
            "the fold-downs differ: {mid:?}"
        );
        // Hard left: its right side silent, judged on the left alone.
        let [hl, hr] = pan(1.0, 0.0);
        let s = same(&[&al, &ar], &[&hl, &hr]);
        assert!(covers(&s), "{s:?}");
        // Two independent wide sources.
        let other_l = drums(LEN, 0.3, 93);
        let other_r = drums(LEN, 0.45, 94);
        let s = same(&[&al, &ar], &[&other_l, &other_r]);
        assert!(!s.any(), "{s:?}");
    }

    #[test]
    fn a_stereo_recording_split_in_two_is_one_audio() {
        let x = drums(LEN, 0.25, 62);
        let mut n1 = Noise(63);
        let mut n2 = Noise(64);
        // Left: the source and a little of its own room; right: quieter,
        // two samples later, its own room.
        let l: Vec<f32> = x.iter().map(|v| v + (0.01 * n1.next()) as f32).collect();
        let r: Vec<f32> = delayed(&x, 2.0)
            .iter()
            .map(|v| 0.6 * v + (0.01 * n2.next()) as f32)
            .collect();
        let s = same(&[&l], &[&r]);
        assert!(covers(&s), "{s:?}");
    }

    #[test]
    fn different_audio_is_not_the_same() {
        let mut n1 = Noise(71);
        let mut n2 = Noise(72);
        let a: Vec<f32> = (0..(LEN * 48_000.0) as usize)
            .map(|_| (0.3 * n1.next()) as f32)
            .collect();
        let b: Vec<f32> = (0..(LEN * 48_000.0) as usize)
            .map(|_| (0.3 * n2.next()) as f32)
            .collect();
        assert!(!same(&[&a], &[&b]).any());
        // Two performances of one part: the same rhythm, every hit its own
        // few milliseconds off and its own noise.
        let one = drums(LEN, 0.5, 73);
        let mut jitter = Noise(74);
        let hit = drums(0.5, 1.0, 75);
        let mut two = vec![0.0f32; one.len()];
        for k in 0..(LEN / 0.5) as usize {
            let at = ((k as f64 * 0.5 + 0.008 * jitter.next()) * 48_000.0).max(0.0) as usize;
            for (i, v) in hit.iter().enumerate() {
                if at + i < two.len() {
                    two[at + i] += v;
                }
            }
        }
        let s = same(&[&one], &[&two]);
        assert!(!s.any(), "{s:?}");
        // Silence on either side: nothing.
        let z = vec![0.0f32; a.len()];
        assert!(!same(&[&a], &[&z]).any());
        assert!(!same(&[&z], &[&z]).any());
    }

    /// A copy that swells in (a long fade from silence) is the same audio
    /// through its fade too: the quiet windows are undecided and follow
    /// the judged ones, not counted as different.
    #[test]
    fn a_copy_is_the_same_audio_through_its_fade_in() {
        let x = drums(LEN, 0.25, 95);
        let fade = 4.0 * f64::from(SR);
        let swell: Vec<f32> = x
            .iter()
            .enumerate()
            .map(|(i, v)| v * ((i as f64 / fade).min(1.0).powi(3)) as f32)
            .collect();
        let copy: Vec<f32> = swell.iter().map(|v| 0.5 * v).collect();
        let s = same(&[&swell], &[&copy]);
        assert_eq!(s.spans.len(), 1, "{s:?}");
        assert!(s.spans[0].start < 0.5, "{:?}", s.spans[0]);
        assert!(s.spans[0].end > LEN - 0.5, "{:?}", s.spans[0]);
    }

    #[test]
    fn the_same_in_one_section_and_different_in_another() {
        let x = drums(LEN, 0.25, 81);
        // Another part (another rhythm: drum loops sharing a kick sample on
        // the same beats would be the same audio there, rightly).
        let y = drums(LEN, 0.37, 82);
        let half = (LEN / 2.0 * 48_000.0) as usize;
        // The same for six seconds, then the other part.
        let b: Vec<f32> = x[..half].iter().chain(&y[half..]).copied().collect();
        let mut n = Noise(83);
        let b: Vec<f32> = b
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if i < half {
                    *v
                } else {
                    v + (0.2 * n.next()) as f32
                }
            })
            .collect();
        let s = same(&[&x], &[&b]);
        assert_eq!(s.spans.len(), 1, "{s:?}");
        let sp = s.spans[0];
        assert!(sp.start < 0.5 && (sp.end - 6.0).abs() < 2.5, "{sp:?}");
        assert!(s.seconds > 3.0 && s.seconds < 8.5, "{s:?}");
    }
}
