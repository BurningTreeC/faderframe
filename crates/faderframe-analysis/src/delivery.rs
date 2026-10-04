//! Preparing finished audio for delivery (offline, on whole files):
//! measuring it ([`Measurement`], [`measure`]: BS.1770 integrated loudness,
//! loudness range, true and sample peak), bringing it to a loudness target
//! ([`normalize_loudness`]) and keeping its true peak under a ceiling with a
//! lookahead limiter ([`limit_true_peak`]).
//!
//! Audio is planar (`audio[channel][frame]`); loudness is measured on the
//! first two channels (one channel counts once, as BS.1770 weighs it).

use crate::loudness::{LoudnessMeter, TP_LATENCY, TruePeak};
use std::collections::VecDeque;

/// What finished audio measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessReport {
    /// Integrated loudness (LUFS; `-inf` for silence or under 400 ms).
    pub integrated: f64,
    /// Loudness range (LU).
    pub range: f64,
    /// Highest true peak (dBTP).
    pub true_peak: f64,
    /// Highest sample peak (dBFS).
    pub sample_peak: f64,
    /// Highest short-term loudness (LUFS).
    pub max_short_term: f64,
}

impl LoudnessReport {
    /// Peak to loudness ratio (LU).
    pub fn plr(&self) -> f64 {
        self.true_peak - self.integrated
    }
}

fn db(v: f64) -> f64 {
    if v > 0.0 {
        20.0 * v.log10()
    } else {
        f64::NEG_INFINITY
    }
}

fn gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A loudness measurement fed piece by piece (one song, or every song of
/// an album in order).
#[derive(Clone, Debug)]
pub struct Measurement {
    meter: LoudnessMeter,
    sample_peak: f32,
    silence: Vec<f32>,
}

impl Measurement {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            meter: LoudnessMeter::new(sample_rate),
            sample_peak: 0.0,
            silence: Vec::new(),
        }
    }

    /// Add audio (planar; channels beyond the second are not measured).
    pub fn add(&mut self, audio: &[Vec<f32>]) {
        const CHUNK: usize = 4096;
        let frames = audio.first().map_or(0, Vec::len);
        if self.silence.len() < CHUNK {
            self.silence = vec![0.0; CHUNK];
        }
        let mut at = 0;
        while at < frames {
            let n = CHUNK.min(frames - at);
            let l = &audio[0][at..at + n];
            let r = match audio.get(1) {
                Some(r) => &r[at..at + n],
                None => &self.silence[..n],
            };
            self.meter.process(l, r, true);
            at += n;
        }
        self.sample_peak = audio
            .iter()
            .flatten()
            .fold(self.sample_peak, |m, s| m.max(s.abs()));
    }

    pub fn report(&self) -> LoudnessReport {
        let read = self.meter.read();
        LoudnessReport {
            integrated: read.integrated,
            range: read.range,
            true_peak: read.true_peak,
            sample_peak: db(self.sample_peak as f64),
            max_short_term: read.max_short_term,
        }
    }
}

/// Measure finished audio.
pub fn measure(audio: &[Vec<f32>], sample_rate: u32) -> LoudnessReport {
    let mut m = Measurement::new(sample_rate);
    m.add(audio);
    m.report()
}

/// Multiply every sample by `db` decibels.
pub fn apply_gain(audio: &mut [Vec<f32>], db: f64) {
    let g = gain(db);
    if (g - 1.0).abs() < 1e-12 {
        return;
    }
    for ch in audio {
        for s in ch.iter_mut() {
            *s = (*s as f64 * g) as f32;
        }
    }
}

/// The true-peak envelope: per frame, the highest magnitude (4×
/// oversampled) of any channel around it.
fn true_peak_envelope(audio: &[Vec<f32>], out: &mut [f32]) {
    out.fill(0.0);
    let len = out.len();
    for ch in audio {
        let mut tp = TruePeak::new();
        for n in 0..len + TP_LATENCY {
            let p = tp.run(ch.get(n).copied().unwrap_or(0.0)) as f32;
            let Some(i) = n.checked_sub(TP_LATENCY) else {
                continue;
            };
            if i < len {
                out[i] = out[i].max(p).max(ch[i].abs());
            }
            if i + 1 < len {
                out[i + 1] = out[i + 1].max(p);
            }
        }
    }
}

/// Keep the true peak at or below `ceiling_db` (dBTP): a lookahead limiter
/// whose gain is down where a peak needs it (smoothly, over `lookahead_ms`
/// before it) and recovers with `release_ms`. Returns the largest gain
/// reduction (dB, ≥ 0).
pub fn limit_true_peak(
    audio: &mut [Vec<f32>],
    sample_rate: u32,
    ceiling_db: f64,
    lookahead_ms: f64,
    release_ms: f64,
) -> f64 {
    let len = audio.first().map_or(0, Vec::len);
    if len == 0 {
        return 0.0;
    }
    let rate = sample_rate.max(1) as f64;
    let look = ((lookahead_ms / 1000.0 * rate).round() as usize).clamp(1, len);
    let release = 1.0 - (-1.0 / (release_ms.max(1.0) / 1000.0 * rate)).exp();
    let mut need = vec![0.0f32; len];
    let mut held = vec![0.0f32; len];
    let mut total = vec![1.0f32; len];
    let ceiling = gain(ceiling_db) as f32;
    // Aim a hair (0.01 dB) under the ceiling; the signal between samples
    // moves with the gain, so a further pass catches what is left over.
    let aim = ceiling * 0.9989;
    for _ in 0..4 {
        true_peak_envelope(audio, &mut need);
        let mut over = false;
        for v in &mut need {
            over |= *v > ceiling;
            *v = if *v > aim { aim / *v } else { 1.0 };
        }
        if !over {
            break;
        }
        // held[k] = the lowest gain needed in k..=k+look.
        let mut window: VecDeque<usize> = VecDeque::new();
        for k in (0..len).rev() {
            while window.back().is_some_and(|&b| need[b] >= need[k]) {
                window.pop_back();
            }
            window.push_back(k);
            while window.front().is_some_and(|&f| f > k + look) {
                window.pop_front();
            }
            held[k] = window.front().map_or(1.0, |&f| need[f]);
        }
        // Averaged over the `look + 1` frames up to i: every one of them
        // holds i's need or less, so the gain is low enough at i and
        // reaches it smoothly. Before the start counts as the start (a
        // peak right at the beginning is reached in time).
        let span = (look + 1) as f64;
        let before = held[0] as f64;
        let mut sum = look as f64 * before;
        let mut g = before;
        for i in 0..len {
            sum += held[i] as f64;
            let target = (sum / span).min(1.0);
            sum -= if i >= look {
                held[i - look] as f64
            } else {
                before
            };
            g = if target < g {
                target
            } else {
                g + (target - g) * release
            };
            total[i] *= g as f32;
            for ch in audio.iter_mut() {
                ch[i] = (ch[i] as f64 * g) as f32;
            }
        }
    }
    let lowest = total.iter().fold(1.0f32, |m, g| m.min(*g));
    -db(lowest as f64).min(0.0)
}

/// How [`normalize_loudness`] treats a true peak the gain would push over
/// the ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeakHandling {
    /// Limit the peaks: the loudness reaches the target.
    #[default]
    Limit,
    /// Use less gain: the loudness may stay below the target.
    LowerGain,
}

/// What [`normalize_loudness`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Normalized {
    /// Gain applied (dB).
    pub gain_db: f64,
    /// Largest limiter gain reduction (dB).
    pub limited_db: f64,
    pub before: LoudnessReport,
    pub after: LoudnessReport,
}

/// Limiter timing for delivery: 1.5 ms lookahead, 60 ms release.
pub const LOOKAHEAD_MS: f64 = 1.5;
pub const RELEASE_MS: f64 = 60.0;

/// Apply `gain_db`, then limit to `ceiling_db` where the true peak goes
/// over it. Returns the limiter's largest gain reduction (dB).
pub fn gain_and_limit(
    audio: &mut [Vec<f32>],
    sample_rate: u32,
    gain_db: f64,
    ceiling_db: f64,
) -> f64 {
    apply_gain(audio, gain_db);
    limit_true_peak(audio, sample_rate, ceiling_db, LOOKAHEAD_MS, RELEASE_MS)
}

/// Bring `audio` to `target_lufs` with its true peak at most `ceiling_db`.
/// Silence is left alone. With [`PeakHandling::Limit`] the gain is refined
/// (up to six times) so that the limited result meets the target.
pub fn normalize_loudness(
    audio: &mut [Vec<f32>],
    sample_rate: u32,
    target_lufs: f64,
    ceiling_db: f64,
    peaks: PeakHandling,
) -> Normalized {
    let before = measure(audio, sample_rate);
    if !before.integrated.is_finite() {
        return Normalized {
            gain_db: 0.0,
            limited_db: 0.0,
            before,
            after: before,
        };
    }
    let mut gain_db = target_lufs - before.integrated;
    if peaks == PeakHandling::LowerGain {
        gain_db = gain_db.min(ceiling_db - before.true_peak);
        apply_gain(audio, gain_db);
        let after = measure(audio, sample_rate);
        // The true peak of the scaled signal can land a hair above.
        let limited_db = if after.true_peak > ceiling_db {
            limit_true_peak(audio, sample_rate, ceiling_db, LOOKAHEAD_MS, RELEASE_MS)
        } else {
            0.0
        };
        return Normalized {
            gain_db,
            limited_db,
            before,
            after: if limited_db > 0.0 {
                measure(audio, sample_rate)
            } else {
                after
            },
        };
    }
    if before.true_peak + gain_db <= ceiling_db {
        apply_gain(audio, gain_db);
        let after = measure(audio, sample_rate);
        return Normalized {
            gain_db,
            limited_db: 0.0,
            before,
            after,
        };
    }
    // Limiting takes some loudness away again, the more the harder it
    // works: aim again from the original, along the measured slope.
    let original = audio.to_vec();
    let mut result = None;
    let mut last: Option<(f64, f64)> = None;
    for _ in 0..6 {
        for (ch, o) in audio.iter_mut().zip(&original) {
            ch.copy_from_slice(o);
        }
        let limited_db = gain_and_limit(audio, sample_rate, gain_db, ceiling_db);
        let after = measure(audio, sample_rate);
        let miss = target_lufs - after.integrated;
        result = Some(Normalized {
            gain_db,
            limited_db,
            before,
            after,
        });
        if miss.abs() < 0.05 || !miss.is_finite() {
            break;
        }
        // LU gained per dB of gain (1 without limiting, less with it).
        let slope = last
            .map(|(g, l)| (after.integrated - l) / (gain_db - g))
            .filter(|s| s.is_finite())
            .map_or(1.0, |s| s.clamp(0.2, 1.0));
        last = Some((gain_db, after.integrated));
        gain_db += miss / slope;
    }
    result.unwrap_or(Normalized {
        gain_db: 0.0,
        limited_db: 0.0,
        before,
        after: before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const SR: u32 = 48_000;

    fn sine(freq: f64, amp: f64, secs: f64) -> Vec<f32> {
        (0..(secs * SR as f64) as usize)
            .map(|i| (amp * (2.0 * PI * freq * i as f64 / SR as f64).sin()) as f32)
            .collect()
    }

    /// A sine with bursts: loud peaks over a steady level.
    fn punchy(secs: f64) -> Vec<Vec<f32>> {
        let mut l = sine(220.0, 0.25, secs);
        for (i, s) in l.iter_mut().enumerate() {
            if i % 12_000 < 300 {
                *s += 0.7 * (2.0 * PI * 3000.0 * i as f64 / SR as f64).sin() as f32;
            }
        }
        vec![l.clone(), l]
    }

    #[test]
    fn a_sine_measures_as_the_standard_says() {
        // EBU Tech 3341: a 1 kHz sine at −18 dBFS in both channels reads
        // −18 LUFS (±0.1).
        let s = sine(1000.0, gain(-18.0), 10.0);
        let r = measure(&[s.clone(), s.clone()], SR);
        assert!((r.integrated - -18.0).abs() < 0.1, "{}", r.integrated);
        assert!((r.sample_peak - -18.0).abs() < 0.05);
        assert!((r.true_peak - -18.0).abs() < 0.3);
        assert!(r.range < 0.5);
        // One channel counts once: 3 dB less.
        let m = measure(&[s], SR);
        assert!((m.integrated - -21.0).abs() < 0.1, "{}", m.integrated);
    }

    #[test]
    fn measuring_in_pieces_is_measuring_the_whole() {
        let a = punchy(5.0);
        let whole = measure(&a, SR);
        let mut m = Measurement::new(SR);
        let half = a[0].len() / 2;
        m.add(&[a[0][..half].to_vec(), a[1][..half].to_vec()]);
        m.add(&[a[0][half..].to_vec(), a[1][half..].to_vec()]);
        let parts = m.report();
        assert!((whole.integrated - parts.integrated).abs() < 0.01);
        assert_eq!(whole.sample_peak, parts.sample_peak);
    }

    #[test]
    fn loudness_targets_are_reached_and_peaks_held() {
        let audio = punchy(8.0);
        // A loud master: −9 LUFS, −1 dBTP, limited.
        let mut a = audio.clone();
        let n = normalize_loudness(&mut a, SR, -9.0, -1.0, PeakHandling::Limit);
        assert!(n.limited_db > 0.5, "{n:?}");
        assert!((n.after.integrated - -9.0).abs() < 0.1, "{:?}", n.after);
        assert!(n.after.true_peak <= -1.0 + 0.02, "{:?}", n.after);
        // Less gain instead: the ceiling decides.
        let mut b = audio.clone();
        let m = normalize_loudness(&mut b, SR, -8.0, -1.0, PeakHandling::LowerGain);
        assert!(m.after.true_peak <= -1.0 + 0.02, "{:?}", m.after);
        assert!(m.after.integrated < -8.5);
        // Quiet enough already: plain gain, no limiting.
        let mut c = audio.clone();
        let q = normalize_loudness(&mut c, SR, -30.0, -1.0, PeakHandling::Limit);
        assert_eq!(q.limited_db, 0.0);
        assert!((q.after.integrated - -30.0).abs() < 0.05);
        // Silence stays silence.
        let mut z = vec![vec![0.0f32; 48_000]; 2];
        let s = normalize_loudness(&mut z, SR, -14.0, -1.0, PeakHandling::Limit);
        assert_eq!(s.gain_db, 0.0);
        assert!(z[0].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn the_limiter_holds_inter_sample_peaks() {
        // fs/4 with a 45° phase: every sample misses the crests, the true
        // peak is 3 dB above the sample peak.
        let x: Vec<f32> = (0..48_000)
            .map(|i| (0.99 * (PI / 2.0 * i as f64 + PI / 4.0).sin()) as f32)
            .collect();
        let mut a = vec![x.clone(), x];
        let before = measure(&a, SR);
        assert!(before.true_peak > before.sample_peak + 2.5, "{before:?}");
        let reduced = limit_true_peak(&mut a, SR, -1.0, LOOKAHEAD_MS, RELEASE_MS);
        let after = measure(&a, SR);
        assert!(after.true_peak <= -1.0 + 0.02, "{after:?}");
        assert!((0.8..1.5).contains(&reduced), "{reduced}");
    }

    #[test]
    fn the_limiter_leaves_what_fits_alone_and_ramps_into_peaks() {
        let quiet = sine(440.0, 0.5, 1.0);
        let mut a = vec![quiet.clone(), quiet.clone()];
        assert_eq!(limit_true_peak(&mut a, SR, -1.0, 1.5, 60.0), 0.0);
        assert_eq!(a[0], quiet);
        // One loud burst in the middle: before it the gain is untouched
        // beyond the lookahead, and it never steps by more than a smooth
        // ramp allows.
        let mut b = sine(440.0, 0.3, 1.0);
        for s in &mut b[24_000..24_480] {
            *s *= 5.0;
        }
        let orig = b.clone();
        let mut c = vec![b.clone(), b];
        limit_true_peak(&mut c, SR, -1.0, 1.5, 60.0);
        let look = (0.0015 * SR as f64) as usize;
        assert_eq!(&c[0][..24_000 - look - 8], &orig[..24_000 - look - 8]);
        // The gain per frame (where the signal is far enough from zero
        // to read it), compared between neighbouring frames.
        let gain_at = |i: usize| (orig[i].abs() > 0.05).then(|| (c[0][i] / orig[i]) as f64);
        let biggest_step = (1..orig.len())
            .filter_map(|i| Some((gain_at(i)? - gain_at(i - 1)?).abs()))
            .fold(0.0, f64::max);
        assert!(biggest_step < 0.01, "{biggest_step}");
    }
}
