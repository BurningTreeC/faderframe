//! Spectral masking between two tracks: where in frequency and time one
//! track (the *masker*) sounds at a level close to or above another's (the
//! *target*) in a band that is a prominent part of the target, at a
//! similar stereo placement.
//!
//! Per frame `t` and band `b` (levels in dB, see [`crate::profile`]):
//!
//! * the target **counts** when its band level is above `floor_db` and
//!   within `prominence_db` of its own loudest band in that frame;
//! * the masker **covers** it by `cover = clamp((Lm − Lt − lo) / (hi − lo),
//!   0, 1)` (`lo`, `hi` = `cover_lo_db`, `cover_hi_db`: none when the
//!   masker is 12 dB or more under the target, fully at the target's level
//!   or above), when the masker is above the floor itself;
//! * the stereo placements' **likeness** `s` is the cosine of the two
//!   tracks' (√P_left, √P_right) vectors (1 for the same placement, 0 for
//!   hard left against hard right), weighted `w = floor + (1 − floor)·s`;
//! * the cell's **conflict** is `k = cover · w` where the target counts.
//!
//! Over time, bands where the target is covered (`k ≥ strong`) for at
//! least half of `min_seconds` are grouped into contiguous ranges (at most
//! two octaves: the two-octave window covered most). Per range, a frame's
//! conflict is the target-power-weighted mean of `k` over the range's
//! bands; covered frames make events (closer than `merge_gap_seconds`
//! merged). A range is kept when it is covered for at least `min_seconds`
//! in all; how much of the time the target plays there it is covered (the
//! persistence) goes into the severity, so an isolated overlap is still
//! found and located, only ranked lower.
//!
//! The result is evidence of a *potential* conflict (overlap in level,
//! frequency, time and placement), not a psychoacoustic masking model.

use crate::config::MaskingRule;
use crate::finding::{MAX_EVENTS, Pattern, TimeSpan};
use crate::profile::{TrackProfile, power_db};
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum MaskingError {
    #[error("the two profiles were measured differently (rate, frames or bands)")]
    Mismatch,
    #[error("the analysis was cancelled")]
    Cancelled,
}

/// One frequency range where the target is covered.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictRange {
    /// Bands `a..=b` of the layout.
    pub bands: (usize, usize),
    /// Their edges (Hz).
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// When candidate bands spanned more than two octaves: their whole
    /// extent (Hz).
    pub wider: Option<(f64, f64)>,
    pub events: Vec<TimeSpan>,
    /// Seconds covered, and seconds the target counts in the range.
    pub conflict_seconds: f64,
    pub active_seconds: f64,
    /// `conflict_seconds / active_seconds`.
    pub persistence: f64,
    /// Mean conflict over the covered frames (`strong` to 1).
    pub depth: f64,
    /// The target's energy in the range over its energy in all bands
    /// (whole analysed range).
    pub share: f64,
    /// Target and masker levels in the range while covered (dB of the
    /// range's mean power per frame).
    pub target_db: f64,
    pub masker_db: f64,
    /// Mean stereo likeness while covered (0 to 1).
    pub spatial: f64,
    pub pattern: Pattern,
    /// Mean level per band while covered (dB), target and masker.
    pub target_spectrum: Vec<f64>,
    pub masker_spectrum: Vec<f64>,
}

impl ConflictRange {
    /// Masker minus target (dB) while covered.
    pub fn margin_db(&self) -> f64 {
        self.masker_db - self.target_db
    }
}

/// Stereo likeness of two placements (see the module docs).
pub fn likeness(a: (f64, f64), b: (f64, f64)) -> f64 {
    let num = (a.0 * b.0).sqrt() + (a.1 * b.1).sqrt();
    let den = ((a.0 + a.1) * (b.0 + b.1)).sqrt();
    if den > 0.0 {
        (num / den).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn compatible(a: &TrackProfile, b: &TrackProfile) -> bool {
    a.rate == b.rate
        && a.hop == b.hop
        && a.fft_size == b.fft_size
        && a.frames.frames() == b.frames.frames()
        && a.frames.bands() == b.frames.bands()
}

/// Where `masker` may cover `target` (see the module docs), the most
/// covered ranges first, at most `rule.max_ranges`.
pub fn conflicts(
    target: &TrackProfile,
    masker: &TrackProfile,
    rule: &MaskingRule,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<ConflictRange>, MaskingError> {
    conflicts_except(target, masker, rule, &[], cancel)
}

/// [`conflicts`] with the frames marked in `skip` left out (the target does
/// not count there): where the two carry the same audio
/// ([`crate::same`]), masking is not judged.
pub fn conflicts_except(
    target: &TrackProfile,
    masker: &TrackProfile,
    rule: &MaskingRule,
    skip: &[bool],
    cancel: Option<&AtomicBool>,
) -> Result<Vec<ConflictRange>, MaskingError> {
    if !compatible(target, masker) {
        return Err(MaskingError::Mismatch);
    }
    let (frames, bands) = (target.frames.frames(), target.frames.bands());
    if frames == 0 || bands == 0 {
        return Ok(Vec::new());
    }
    let hop_s = target.hop_seconds();
    // Per cell: the conflict (negative where the target does not count).
    let mut k = vec![-1.0f32; frames * bands];
    let mut active = vec![0usize; bands];
    let mut covered = vec![0usize; bands];
    for t in 0..frames {
        if t % 256 == 0 && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(MaskingError::Cancelled);
        }
        if skip.get(t) == Some(&true) {
            continue;
        }
        let top = (0..bands)
            .map(|b| target.frames.level(t, b))
            .fold(f64::NEG_INFINITY, f64::max);
        for b in 0..bands {
            let lt = target.frames.level(t, b);
            if lt < rule.floor_db || lt < top - rule.prominence_db {
                continue;
            }
            active[b] += 1;
            let lm = masker.frames.level(t, b);
            let cell = if lm < rule.floor_db {
                0.0
            } else {
                let span = (rule.cover_hi_db - rule.cover_lo_db).max(1e-6);
                let cover = ((lm - lt - rule.cover_lo_db) / span).clamp(0.0, 1.0);
                let s = likeness(target.frames.powers(t, b), masker.frames.powers(t, b));
                cover * (rule.spatial_floor + (1.0 - rule.spatial_floor) * s)
            };
            k[t * bands + b] = cell as f32;
            if cell >= rule.strong {
                covered[b] += 1;
            }
        }
    }
    // Candidate bands: covered for half the time threshold at least (a
    // range may spread its time over its bands).
    let candidate: Vec<bool> = (0..bands)
        .map(|b| active[b] > 0 && covered[b] as f64 * hop_s >= rule.min_seconds * 0.5)
        .collect();
    let per_octave = {
        let o = target
            .layout
            .bands()
            .first()
            .map_or(1.0 / 3.0, |b| b.octaves());
        (1.0 / o.max(1e-3)).round().max(1.0) as usize
    };
    let max_bands = 2 * per_octave;
    let mut groups = Vec::new();
    let mut b = 0;
    while b < bands {
        if !candidate[b] {
            b += 1;
            continue;
        }
        let start = b;
        while b < bands && candidate[b] {
            b += 1;
        }
        let end = b - 1;
        if end - start < max_bands {
            groups.push((start, end, None));
        } else {
            // The two-octave window covered most.
            let best = (start..=end + 1 - max_bands)
                .max_by_key(|&a| {
                    (
                        covered[a..a + max_bands].iter().sum::<usize>(),
                        usize::MAX - a,
                    )
                })
                .unwrap_or(start);
            let whole = target.layout.span(start, end);
            groups.push((best, best + max_bands - 1, whole));
        }
    }
    let total_target: f64 = (0..frames)
        .map(|t| (0..bands).map(|b| target.frames.power(t, b)).sum::<f64>())
        .sum();
    let mut out = Vec::new();
    for (a, z, wider) in groups {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(MaskingError::Cancelled);
        }
        let mut active_frames = 0usize;
        let mut covered_frames = Vec::new();
        let (mut sum_k, mut sum_s) = (0.0, 0.0);
        let (mut pt_sum, mut pm_sum) = (0.0, 0.0);
        let mut spec_t = vec![0.0; bands];
        let mut spec_m = vec![0.0; bands];
        let mut range_energy = 0.0;
        for t in 0..frames {
            let (mut wt, mut wk, mut ws) = (0.0, 0.0, 0.0);
            for b in a..=z {
                let pt = target.frames.power(t, b);
                range_energy += pt;
                let cell = k[t * bands + b];
                if cell < 0.0 {
                    continue;
                }
                wt += pt;
                wk += pt * f64::from(cell);
                ws += pt * likeness(target.frames.powers(t, b), masker.frames.powers(t, b));
            }
            if wt <= 0.0 {
                continue;
            }
            active_frames += 1;
            let kr = wk / wt;
            if kr < rule.strong {
                continue;
            }
            covered_frames.push(t);
            sum_k += kr;
            sum_s += ws / wt;
            for b in a..=z {
                pt_sum += target.frames.power(t, b);
                pm_sum += masker.frames.power(t, b);
            }
            for b in 0..bands {
                spec_t[b] += target.frames.power(t, b);
                spec_m[b] += masker.frames.power(t, b);
            }
        }
        let n = covered_frames.len();
        let conflict_seconds = n as f64 * hop_s;
        let active_seconds = active_frames as f64 * hop_s;
        let persistence = if active_frames > 0 {
            n as f64 / active_frames as f64
        } else {
            0.0
        };
        if n == 0 || conflict_seconds < rule.min_seconds {
            continue;
        }
        let events = events_of(target, &covered_frames, rule.merge_gap_seconds);
        let pattern = if persistence >= 0.5 {
            Pattern::Persistent
        } else if events.len() >= 3 {
            Pattern::Recurring
        } else {
            Pattern::Isolated
        };
        let nf = n as f64;
        let (lo_hz, hi_hz) = target.layout.span(a, z).unwrap_or((0.0, 0.0));
        out.push(ConflictRange {
            bands: (a, z),
            lo_hz,
            hi_hz,
            wider: wider.filter(|w| *w != (lo_hz, hi_hz)),
            events,
            conflict_seconds,
            active_seconds,
            persistence,
            depth: sum_k / nf,
            share: if total_target > 0.0 {
                (range_energy / total_target).clamp(0.0, 1.0)
            } else {
                0.0
            },
            target_db: power_db(pt_sum / nf),
            masker_db: power_db(pm_sum / nf),
            spatial: sum_s / nf,
            pattern,
            target_spectrum: spec_t.iter().map(|p| power_db(p / nf)).collect(),
            masker_spectrum: spec_m.iter().map(|p| power_db(p / nf)).collect(),
        });
    }
    out.sort_by(|x, y| {
        (y.conflict_seconds * y.depth)
            .total_cmp(&(x.conflict_seconds * x.depth))
            .then(x.bands.cmp(&y.bands))
    });
    out.truncate(rule.max_ranges);
    Ok(out)
}

/// Runs of frames as time spans, gaps up to `merge_gap` seconds closed, at
/// most [`MAX_EVENTS`] (the last absorbs the rest).
pub(crate) fn events_of(p: &TrackProfile, frames: &[usize], merge_gap: f64) -> Vec<TimeSpan> {
    let mut out: Vec<TimeSpan> = Vec::new();
    for &t in frames {
        let (a, b) = p.frame_span(t);
        match out.last_mut() {
            Some(last) if a - last.end <= merge_gap.max(0.0) + 1e-9 => last.end = last.end.max(b),
            _ => out.push(TimeSpan::new(a, b)),
        }
    }
    if out.len() > MAX_EVENTS {
        let end = out.last().map_or(0.0, |e| e.end);
        out.truncate(MAX_EVENTS);
        if let Some(last) = out.last_mut() {
            last.end = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AnalysisConfig;
    use crate::profile::{ProfileRequest, analyse};
    use std::f64::consts::TAU;

    const SR: u32 = 48_000;

    /// A tone at `hz` of amplitude `amp` sounding from `a` to `b` seconds
    /// of a `len`-second signal (short fades).
    fn tone(hz: f64, amp: f64, a: f64, b: f64, len: f64) -> Vec<f32> {
        let fade = 0.01 * f64::from(SR);
        (0..(len * f64::from(SR)) as usize)
            .map(|i| {
                let t = i as f64 / f64::from(SR);
                if t < a || t >= b {
                    return 0.0;
                }
                let into = (t - a) * f64::from(SR);
                let left = (b - t) * f64::from(SR);
                let g = (into / fade).min(left / fade).min(1.0);
                (amp * g * (TAU * hz * t).sin()) as f32
            })
            .collect()
    }

    fn mix(parts: &[&[f32]]) -> Vec<f32> {
        let n = parts.iter().map(|p| p.len()).max().unwrap_or(0);
        (0..n)
            .map(|i| parts.iter().map(|p| p.get(i).copied().unwrap_or(0.0)).sum())
            .collect()
    }

    pub(crate) fn profile(channels: &[&[f32]]) -> TrackProfile {
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

    fn found(target: &[f32], masker: &[f32]) -> Vec<ConflictRange> {
        conflicts(
            &profile(&[target]),
            &profile(&[masker]),
            &MaskingRule::default(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    const LEN: f64 = 10.0;

    #[test]
    fn different_frequency_regions_do_not_conflict() {
        let low = tone(80.0, 0.3, 0.0, LEN, LEN);
        let high = tone(3000.0, 0.3, 0.0, LEN, LEN);
        assert!(found(&low, &high).is_empty());
        assert!(found(&high, &low).is_empty());
    }

    #[test]
    fn the_same_band_at_different_times_does_not_conflict() {
        let a = tone(80.0, 0.3, 0.0, 4.0, LEN);
        let b = tone(84.0, 0.3, 5.0, 9.0, LEN);
        assert!(found(&a, &b).is_empty());
    }

    #[test]
    fn the_same_band_at_the_same_time_conflicts_persistently() {
        // Target: 80 Hz with a harmonic; masker in the same third octave
        // (71–89 Hz) at the same level, all the time.
        let a = mix(&[
            &tone(80.0, 0.3, 0.0, LEN, LEN),
            &tone(1000.0, 0.05, 0.0, LEN, LEN),
        ]);
        let b = tone(84.0, 0.3, 0.0, LEN, LEN);
        let c = found(&a, &b);
        assert_eq!(c.len(), 1, "{c:?}");
        let r = &c[0];
        assert!(r.lo_hz < 80.0 && r.hi_hz > 84.0, "{r:?}");
        assert_eq!(r.pattern, Pattern::Persistent);
        assert!(r.persistence > 0.9, "{}", r.persistence);
        assert!(r.conflict_seconds > 8.0);
        assert!(r.margin_db().abs() < 1.0, "{}", r.margin_db());
        assert!(r.depth > 0.9 && r.spatial > 0.99);
        assert!(r.share > 0.9, "most of the target is there: {}", r.share);
        assert_eq!(r.events.len(), 1);
        // Deterministic: the same input gives the same result.
        assert_eq!(found(&a, &b), c);
    }

    #[test]
    fn a_much_quieter_masker_does_not_conflict() {
        let a = tone(80.0, 0.3, 0.0, LEN, LEN);
        // 20 dB under.
        let b = tone(84.0, 0.03, 0.0, LEN, LEN);
        assert!(found(&a, &b).is_empty());
        // 4 dB under: partly covered (cover 2/3), still counted.
        let b = tone(84.0, 0.3 * 10f64.powf(-4.0 / 20.0), 0.0, LEN, LEN);
        let c = found(&a, &b);
        assert_eq!(c.len(), 1);
        assert!((c[0].depth - 2.0 / 3.0).abs() < 0.05, "{}", c[0].depth);
        assert!((c[0].margin_db() + 4.0).abs() < 0.5, "{}", c[0].margin_db());
    }

    #[test]
    fn a_brief_overlap_is_not_reported_and_a_short_one_is_isolated() {
        let a = tone(80.0, 0.3, 0.0, LEN, LEN);
        // Half a second: under the minimum.
        let b = tone(84.0, 0.3, 3.0, 3.5, LEN);
        assert!(found(&a, &b).is_empty());
        // Two seconds: reported, isolated, a fifth of the time.
        let b = tone(84.0, 0.3, 3.0, 5.0, LEN);
        let c = found(&a, &b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].pattern, Pattern::Isolated);
        assert!(c[0].persistence < 0.35, "{}", c[0].persistence);
        assert_eq!(c[0].events.len(), 1);
        let e = c[0].events[0];
        assert!(
            (e.start - 3.0).abs() < 0.2 && (e.end - 5.0).abs() < 0.2,
            "{e:?}"
        );
    }

    #[test]
    fn repeated_overlaps_recur() {
        let a = tone(80.0, 0.3, 0.0, LEN, LEN);
        let hits: Vec<Vec<f32>> = [0.5, 3.5, 6.5]
            .iter()
            .map(|&s| tone(84.0, 0.3, s, s + 1.0, LEN))
            .collect();
        let refs: Vec<&[f32]> = hits.iter().map(Vec::as_slice).collect();
        let b = mix(&refs);
        let c = found(&a, &b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].events.len(), 3, "{:?}", c[0].events);
        assert_eq!(c[0].pattern, Pattern::Recurring);
    }

    #[test]
    fn silence_and_very_quiet_audio_find_nothing() {
        let z = vec![0.0f32; (LEN * f64::from(SR)) as usize];
        let a = tone(80.0, 0.3, 0.0, LEN, LEN);
        assert!(found(&z, &a).is_empty());
        assert!(found(&a, &z).is_empty());
        assert!(found(&z, &z).is_empty());
        // −90 dBFS tones: under the floor.
        let qa = tone(80.0, 3e-5, 0.0, LEN, LEN);
        let qb = tone(84.0, 3e-5, 0.0, LEN, LEN);
        assert!(found(&qa, &qb).is_empty());
    }

    #[test]
    fn stereo_separation_lessens_the_conflict() {
        let s = tone(80.0, 0.3, 0.0, LEN, LEN);
        let m = tone(84.0, 0.3, 0.0, LEN, LEN);
        let z = vec![0.0f32; s.len()];
        let rule = MaskingRule::default();
        let together =
            conflicts(&profile(&[&s, &s]), &profile(&[&m, &m]), &rule, None).unwrap_or_default();
        let apart =
            conflicts(&profile(&[&s, &z]), &profile(&[&z, &m]), &rule, None).unwrap_or_default();
        assert_eq!(together.len(), 1);
        assert_eq!(apart.len(), 1, "separation lessens, does not remove");
        assert!(apart[0].spatial < 0.05 && together[0].spatial > 0.99);
        assert!(apart[0].depth < together[0].depth - 0.3);
    }

    #[test]
    fn mismatched_profiles_and_cancelling_are_errors() {
        let a = profile(&[&tone(80.0, 0.3, 0.0, 2.0, 2.0)]);
        let b = profile(&[&tone(80.0, 0.3, 0.0, 3.0, 3.0)]);
        assert_eq!(
            conflicts(&a, &b, &MaskingRule::default(), None),
            Err(MaskingError::Mismatch)
        );
        let stop = AtomicBool::new(true);
        assert_eq!(
            conflicts(&a, &a, &MaskingRule::default(), Some(&stop)),
            Err(MaskingError::Cancelled)
        );
        assert_eq!(likeness((0.0, 0.0), (1.0, 1.0)), 0.0);
        assert!((likeness((1.0, 1.0), (2.0, 2.0)) - 1.0).abs() < 1e-12);
    }
}
